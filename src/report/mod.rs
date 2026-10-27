//! `jired report` — where the booked time went.
//!
//! Read off the working copy, like every other read: the `## Worklogs` sections
//! of the files already on disk say who booked what, when, and against which
//! issue, so a report needs no network and costs one pass over the files. What
//! it cannot know is time booked since the last `pull`, which is why every
//! rendering says how old the snapshot is.
//!
//! The shape is a grid: a row per person, a column per day. Aggregating is
//! cheap enough — one pass over documents already in memory — that the screen
//! re-runs it on every keystroke that moves the window, which is what lets the
//! window be moved at all.

pub mod issues;

use crate::doc::{parse_duration, IssueDoc, WorklogId};
use crate::json::Out;
use chrono::{DateTime, Datelike, Duration, NaiveDate, NaiveTime, Utc, Weekday};
use serde::Serialize;
use std::collections::HashMap;
use unicode_width::UnicodeWidthStr;

/// Who a worklog belongs to when the file does not say and the account has no
/// name either. Only reachable for an entry the server sent without an author.
const NOBODY: &str = "(unknown)";

/// The most days a window may cover, here and on the command line: past this a
/// "report" is an export, and the grid has nothing left to draw.
pub const MAX_DAYS: u32 = 3650;

/// The stretch of time a report covers: whole UTC days, one per column.
///
/// Whole days rather than a rolling `N × 24h`, because the question is asked in
/// days: an entry booked at 09:00 seven days ago should not drop out of a
/// seven-day report merely because it is now 10:00. Bounded at both ends, so
/// the window can be moved back over earlier weeks and every entry still has
/// exactly one column it belongs to.
#[derive(Debug, Clone, Copy)]
pub struct Window {
    pub days: u32,
    /// Midnight UTC of the first day counted.
    pub since: DateTime<Utc>,
    /// Midnight UTC of the day after the last one counted.
    pub until: DateTime<Utc>,
    /// When the report was taken, which is what dates an entry that names no
    /// start of its own.
    pub taken_at: DateTime<Utc>,
}

impl Window {
    /// The `days` days ending `back` days before today.
    pub fn ending(days: u32, now: DateTime<Utc>, back: u32) -> Self {
        let days = days.clamp(1, MAX_DAYS);
        let tomorrow = now.date_naive().and_time(NaiveTime::MIN).and_utc() + Duration::days(1);
        let until = tomorrow - Duration::days(i64::from(back));
        Self {
            days,
            since: until - Duration::days(i64::from(days)),
            until,
            taken_at: now,
        }
    }

    /// The `days` days ending today.
    pub fn last(days: u32, now: DateTime<Utc>) -> Self {
        Self::ending(days, now, 0)
    }

    /// From `first` to `last`, both counted: the window a person names when
    /// the question is "the week of the 19th" rather than "the last N days".
    ///
    /// `None` when the dates run backwards or span more than [`MAX_DAYS`].
    pub fn between(first: NaiveDate, last: NaiveDate, now: DateTime<Utc>) -> Option<Self> {
        let days = (last - first).num_days() + 1;
        if days < 1 || days > i64::from(MAX_DAYS) {
            return None;
        }
        let since = first.and_time(NaiveTime::MIN).and_utc();
        Some(Self {
            days: days as u32,
            since,
            until: since + Duration::days(days),
            taken_at: now,
        })
    }

    /// How many days before today the window's last column is, the offset
    /// [`Window::ending`] takes; `None` for a window that ends in the future.
    pub fn back(&self, now: DateTime<Utc>) -> Option<u32> {
        let last = (self.until - Duration::days(1)).date_naive();
        u32::try_from((now.date_naive() - last).num_days()).ok()
    }

    /// Which column `when` falls in, or `None` when it falls outside.
    fn column(&self, when: DateTime<Utc>) -> Option<usize> {
        if when < self.since || when >= self.until {
            return None;
        }
        Some((when - self.since).num_days().max(0) as usize)
    }

    /// The day each column stands for, oldest first.
    pub fn dates(&self) -> Vec<NaiveDate> {
        (0..i64::from(self.days))
            .map(|i| (self.since + Duration::days(i)).date_naive())
            .collect()
    }
}

/// What the files cannot say about a worklog or an issue, read out of the
/// snapshots in `state.db`.
///
/// A file names a worklog's author by display name only, and a display name is
/// not a person: somebody renamed in Jira shows up under the old name in every
/// file pulled before the rename and the new one after, and was counted as two
/// people. The snapshot carries the account id behind each entry.
#[derive(Debug, Clone, Default)]
pub struct Lookup {
    /// A worklog's author, by worklog id — unique across an instance, so the
    /// issue it sits on is not needed to find it.
    pub authors: HashMap<String, Author>,
    /// The account running the report, which staged entries belong to.
    pub me_id: Option<String>,
    /// Issues the snapshots know, for a parent that has no file here: what a
    /// rolled-up row is called and where its own parent is.
    pub known: HashMap<String, IssueInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Author {
    pub id: String,
    pub display_name: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IssueInfo {
    pub summary: String,
    pub issue_type: String,
    pub parent: Option<String>,
}

impl Lookup {
    /// Read the worklog authors and the issue outlines out of one snapshot.
    ///
    /// Called oldest snapshot first, so a later one — the remote after the
    /// base — has the last word on a name.
    pub fn learn(&mut self, raw: &serde_json::Value) {
        let fields = raw.get("fields");
        if let Some(key) = raw.get("key").and_then(serde_json::Value::as_str) {
            let text = |v: Option<&serde_json::Value>| {
                v.and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_string()
            };
            self.known.insert(
                key.to_string(),
                IssueInfo {
                    summary: text(fields.and_then(|f| f.get("summary"))),
                    issue_type: text(
                        fields
                            .and_then(|f| f.get("issuetype"))
                            .and_then(|t| t.get("name")),
                    ),
                    parent: fields
                        .and_then(|f| f.get("parent"))
                        .and_then(|p| p.get("key"))
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_string),
                },
            );
        }
        let worklogs = fields
            .and_then(|f| f.get("worklog"))
            .and_then(|w| w.get("worklogs"))
            .and_then(serde_json::Value::as_array);
        for w in worklogs.into_iter().flatten() {
            let id = match w.get("id") {
                Some(serde_json::Value::String(s)) => s.clone(),
                Some(serde_json::Value::Number(n)) => n.to_string(),
                _ => continue,
            };
            if let Some(user) = crate::api::normalize::user_ref(w.get("author")) {
                self.authors.insert(
                    id,
                    Author {
                        id: user.id,
                        display_name: user.display_name,
                    },
                );
            }
        }
    }
}

/// Everything a report needs besides its window.
///
/// Held apart from the report itself so a screen can re-aggregate as the window
/// moves without going back to the disk for the files or to the database for
/// the context around them.
pub struct Source<'a> {
    pub issues: &'a [IssueDoc],
    /// Display name of the account, for entries staged with no author yet.
    pub me: String,
    pub lookup: Lookup,
    /// Fold each sub-task's time into the issue at the top of its `parent:`
    /// chain.
    pub rollup: bool,
    pub now: DateTime<Utc>,
    pub project: Option<String>,
    /// When the snapshot was last refreshed. Kept as the instant it is: the
    /// document writes it as UTC and the printed line as the reader's clock.
    pub last_fetch: Option<DateTime<Utc>>,
    pub last_fetch_age_seconds: Option<i64>,
}

impl Source<'_> {
    pub fn report(&self, days: u32, back: u32) -> TimeReport {
        self.report_in(&Window::ending(days, self.now, back))
    }

    pub fn report_in(&self, window: &Window) -> TimeReport {
        let mut report =
            TimeReport::build_with(window, &self.me, self.issues, &self.lookup, self.rollup);
        report.project.clone_from(&self.project);
        report.last_fetch_at = self
            .last_fetch
            .as_ref()
            .map(crate::api::model::format_timestamp);
        report.last_fetch = self.last_fetch;
        report.last_fetch_age_seconds = self.last_fetch_age_seconds;
        report
    }
}

/// Time booked inside a window: by person, by day, and under each person by
/// issue.
#[derive(Debug, Clone, Serialize)]
pub struct TimeReport {
    pub days: u32,
    /// Midnight UTC of the first day counted.
    pub since: String,
    /// Midnight UTC of the day after the last one counted.
    pub until: String,
    /// The day each column stands for, oldest first. Every `by_day` array in
    /// this document is in this order and of this length.
    pub dates: Vec<NaiveDate>,
    pub project: Option<String>,
    /// When the snapshot was last refreshed, so a stale report can say so.
    pub last_fetch_at: Option<String>,
    pub last_fetch_age_seconds: Option<i64>,
    /// The same instant as `last_fetch_at`, kept so the printed line can be
    /// shown in the reader's timezone while the JSON field stays UTC.
    #[serde(skip)]
    pub last_fetch: Option<DateTime<Utc>>,
    /// Everything booked in the window.
    pub seconds: u64,
    /// The part of it still staged locally, which is a subset of `seconds`.
    pub staged_seconds: u64,
    /// Column totals: what everyone booked on each day.
    pub by_day: Vec<u64>,
    pub entries: usize,
    /// Worklogs that could not be counted: no readable start, or a duration in
    /// notation nothing here understands.
    pub skipped: usize,
    /// Most time first, then by name.
    pub people: Vec<Person>,
    /// The same time the other way up: a row per issue, most time first, each
    /// saying who booked it. What a table of issues against people is read
    /// from, without pivoting `people` by hand.
    pub issues: Vec<IssueTotal>,
    /// `parent` when sub-tasks were folded into the issue at the top of their
    /// chain, `None` otherwise.
    pub rollup: Option<&'static str>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Person {
    /// The display name on the person's most recent entry.
    pub name: String,
    /// What the server identifies them by — `accountId` on Cloud, the user
    /// name on Data Center — and what they are grouped by; `None` for time
    /// the snapshots cannot attribute.
    pub account_id: Option<String>,
    /// Earlier display names of the same account, seen on older entries.
    pub also_known_as: Vec<String>,
    pub seconds: u64,
    pub staged_seconds: u64,
    pub by_day: Vec<u64>,
    pub entries: usize,
    /// Most time first, then by key.
    pub issues: Vec<IssueTime>,
}

#[derive(Debug, Clone, Serialize)]
pub struct IssueTime {
    pub key: String,
    pub summary: String,
    #[serde(rename = "type")]
    pub issue_type: String,
    pub parent: Option<String>,
    /// Under `--rollup parent`, the sub-tasks whose time this row carries.
    pub rolled_up: Vec<String>,
    pub seconds: u64,
    pub staged_seconds: u64,
    pub by_day: Vec<u64>,
    pub entries: usize,
}

/// One issue's time across everybody.
#[derive(Debug, Clone, Serialize)]
pub struct IssueTotal {
    pub key: String,
    pub summary: String,
    #[serde(rename = "type")]
    pub issue_type: String,
    pub parent: Option<String>,
    pub rolled_up: Vec<String>,
    pub seconds: u64,
    pub staged_seconds: u64,
    pub by_day: Vec<u64>,
    pub entries: usize,
    /// Who booked it, in the order of `people` at the top of the document.
    pub people: Vec<Share>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Share {
    pub name: String,
    pub account_id: Option<String>,
    pub seconds: u64,
}

/// Where a worklog's time lands as a row: the issue itself, or under
/// `--rollup parent` the one at the top of its `parent:` chain.
struct Rows<'a> {
    own: HashMap<&'a str, IssueInfo>,
    lookup: &'a Lookup,
    rollup: bool,
}

impl<'a> Rows<'a> {
    fn new(issues: &'a [IssueDoc], lookup: &'a Lookup, rollup: bool) -> Self {
        let own = issues
            .iter()
            .map(|d| {
                (
                    d.frontmatter.key.as_str(),
                    IssueInfo {
                        summary: d.frontmatter.summary.clone(),
                        issue_type: d.frontmatter.issue_type.clone(),
                        parent: d.frontmatter.parent.clone().filter(|p| !p.is_empty()),
                    },
                )
            })
            .collect();
        Self {
            own,
            lookup,
            rollup,
        }
    }

    /// The working copy's word first, then the snapshot's.
    fn info(&self, key: &str) -> Option<&IssueInfo> {
        self.own.get(key).or_else(|| self.lookup.known.get(key))
    }

    /// The key the row is filed under.
    fn row_of(&self, key: &str) -> String {
        if !self.rollup {
            return key.to_string();
        }
        let mut at = key.to_string();
        // Bounded: a cycle in the data is not something to loop on for ever.
        for _ in 0..32 {
            match self.info(&at).and_then(|i| i.parent.clone()) {
                Some(parent) if parent != at => at = parent,
                _ => break,
            }
        }
        at
    }
}

impl TimeReport {
    /// Aggregate `issues`, attributing entries nobody has claimed to `me`.
    ///
    /// A staged entry has no author yet — the author is whoever pushes it — so
    /// it counts towards the person running the report.
    pub fn build(window: &Window, me: &str, issues: &[IssueDoc]) -> Self {
        Self::build_with(window, me, issues, &Lookup::default(), false)
    }

    /// [`TimeReport::build`], with the snapshots' word on who booked what and,
    /// with `rollup`, each sub-task's time filed under its top-level parent.
    pub fn build_with(
        window: &Window,
        me: &str,
        issues: &[IssueDoc],
        lookup: &Lookup,
        rollup: bool,
    ) -> Self {
        let columns = window.days as usize;
        let rows = Rows::new(issues, lookup, rollup);
        let mut people: Vec<Person> = Vec::new();
        // Every display name each person was seen under, with the latest entry
        // that used it: the newest is what they are called now.
        let mut names: Vec<Vec<(String, DateTime<Utc>)>> = Vec::new();
        let mut skipped = 0;

        for issue in issues {
            for w in &issue.worklogs {
                let staged = w.id == WorklogId::New;
                let started = match crate::api::model::parse_local_timestamp(&w.started) {
                    Some(t) => t,
                    // A staged entry that names no start is time booked now,
                    // which is what pushing it will date it as.
                    None if staged => window.taken_at,
                    None => {
                        skipped += 1;
                        continue;
                    }
                };
                let Some(column) = window.column(started) else {
                    continue;
                };
                let Some(seconds) = parse_duration(&w.time_spent) else {
                    skipped += 1;
                    continue;
                };

                let author = match &w.id {
                    WorklogId::Existing(id) => lookup.authors.get(id),
                    _ => None,
                };
                let who = match (w.author.trim(), me.trim(), author) {
                    ("", _, Some(a)) => a.display_name.as_str(),
                    ("", "", None) if staged => "you",
                    ("", "", None) => NOBODY,
                    ("", me, None) => me,
                    (author, _, _) => author,
                };
                let account_id = match author {
                    Some(a) => Some(a.id.clone()),
                    None if staged => lookup.me_id.clone(),
                    None => None,
                };
                // By account where there is one; by name only where nothing
                // better is known, which is all a name ever stood in for.
                let found = people
                    .iter()
                    .position(|p| match (&account_id, &p.account_id) {
                        (Some(id), Some(theirs)) => id == theirs,
                        (None, None) => p.name == who,
                        _ => false,
                    });
                let at = match found {
                    Some(at) => at,
                    None => {
                        people.push(Person {
                            name: who.to_string(),
                            account_id: account_id.clone(),
                            also_known_as: Vec::new(),
                            seconds: 0,
                            staged_seconds: 0,
                            by_day: vec![0; columns],
                            entries: 0,
                            issues: Vec::new(),
                        });
                        names.push(Vec::new());
                        people.len() - 1
                    }
                };
                match names[at].iter_mut().find(|(n, _)| n == who) {
                    Some((_, latest)) => *latest = (*latest).max(started),
                    None => names[at].push((who.to_string(), started)),
                }
                let person = &mut people[at];
                person.seconds += seconds;
                person.by_day[column] += seconds;
                person.entries += 1;
                if staged {
                    person.staged_seconds += seconds;
                }

                let own_key = &issue.frontmatter.key;
                let key = rows.row_of(own_key);
                let booked = match person.issues.iter().position(|i| i.key == key) {
                    Some(at) => &mut person.issues[at],
                    None => {
                        let info = rows.info(&key).cloned().unwrap_or_default();
                        person.issues.push(IssueTime {
                            key: key.clone(),
                            summary: info.summary,
                            issue_type: info.issue_type,
                            parent: info.parent,
                            rolled_up: Vec::new(),
                            seconds: 0,
                            staged_seconds: 0,
                            by_day: vec![0; columns],
                            entries: 0,
                        });
                        person.issues.last_mut().expect("just pushed")
                    }
                };
                booked.seconds += seconds;
                booked.by_day[column] += seconds;
                booked.entries += 1;
                if staged {
                    booked.staged_seconds += seconds;
                }
                if *own_key != key && !booked.rolled_up.contains(own_key) {
                    booked.rolled_up.push(own_key.clone());
                    booked.rolled_up.sort();
                }
            }
        }

        for (person, mut seen) in people.iter_mut().zip(names) {
            seen.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
            let mut seen = seen.into_iter().map(|(name, _)| name);
            if let Some(newest) = seen.next() {
                person.name = newest;
            }
            person.also_known_as = seen.collect();
            person.also_known_as.sort();
            person
                .issues
                .sort_by(|a, b| b.seconds.cmp(&a.seconds).then_with(|| a.key.cmp(&b.key)));
        }
        people.sort_by(|a, b| b.seconds.cmp(&a.seconds).then_with(|| a.name.cmp(&b.name)));

        let mut by_day = vec![0u64; columns];
        for person in &people {
            for (day, seconds) in person.by_day.iter().enumerate() {
                by_day[day] += seconds;
            }
        }

        Self {
            days: window.days,
            since: crate::api::model::format_timestamp(&window.since),
            until: crate::api::model::format_timestamp(&window.until),
            dates: window.dates(),
            project: None,
            last_fetch_at: None,
            last_fetch_age_seconds: None,
            last_fetch: None,
            seconds: people.iter().map(|p| p.seconds).sum(),
            staged_seconds: people.iter().map(|p| p.staged_seconds).sum(),
            by_day,
            entries: people.iter().map(|p| p.entries).sum(),
            skipped,
            issues: issue_totals(&people, columns),
            people,
            rollup: rollup.then_some("parent"),
        }
    }

    /// The busiest single cell, which is what a heat map is scaled against.
    pub fn peak(&self) -> u64 {
        self.people
            .iter()
            .flat_map(|p| p.by_day.iter())
            .copied()
            .max()
            .unwrap_or(0)
    }

    /// How much of the total one person accounts for, as a fraction.
    pub fn share(&self, person: &Person) -> f64 {
        self.fraction(person.seconds)
    }

    /// How much of the total `seconds` is, as a fraction.
    pub fn fraction(&self, seconds: u64) -> f64 {
        match self.seconds {
            0 => 0.0,
            total => seconds as f64 / total as f64,
        }
    }

    /// The window in the form a header shows it: `2026-08-16 → 2026-08-22`.
    pub fn span(&self) -> String {
        match (self.dates.first(), self.dates.last()) {
            (Some(first), Some(last)) if first == last => first.to_string(),
            (Some(first), Some(last)) => format!("{first} → {last}"),
            _ => String::new(),
        }
    }

    /// How fresh the snapshot behind the report is, if it has ever been pulled.
    pub fn freshness(&self) -> Option<String> {
        let at = crate::api::model::format_local(self.last_fetch.as_ref()?);
        Some(match self.last_fetch_age_seconds {
            Some(age) => format!(
                "last fetch {at} ({})",
                crate::sync::status::humanize_age(age)
            ),
            None => format!("last fetch {at}"),
        })
    }

    /// The plain-text rendering, for terminals the full-screen one cannot run
    /// on and for anyone piping the output somewhere.
    ///
    /// The same grid, while it fits in a sensible line; past that the day
    /// columns are dropped rather than wrapped, since a wrapped grid is worse
    /// than no grid and the totals are what a wide window is asked for.
    pub fn render(&self, out: Out, show_issues: bool) {
        use crate::style::{self, BOLD, CYAN, DIM};

        out.say(style::out(BOLD, self.title()));
        match self.freshness() {
            Some(line) => out.say(style::out(DIM, line)),
            None => out.say(style::out(DIM, "never fetched")),
        }

        if self.people.is_empty() {
            out.say("\nnobody booked any time in this window");
            out.hint("use \"jired pull\" to bring down what the server has");
            return;
        }

        // Padding is applied before painting: escape codes count towards a
        // format width and would knock every following column out of line. The
        // first column holds the issue labels too when they are being shown,
        // or a breakdown would be a column of truncated keys.
        let name_width = self
            .people
            .iter()
            .map(|person| {
                let issues = match show_issues {
                    false => 0,
                    true => person
                        .issues
                        .iter()
                        .map(|i| i.key.width() + 1 + i.summary.width() + 2)
                        .max()
                        .unwrap_or(0),
                };
                person.name.width().max(issues)
            })
            .max()
            .unwrap_or(0)
            .clamp(8, 40);
        let grid = self.days as usize * CELL + name_width + TOTAL <= TEXT_WIDTH;

        out.say("");
        if grid {
            let mut header = " ".repeat(name_width + 2);
            for date in &self.dates {
                header.push_str(&format!("{:>CELL$}", column_head(date)));
            }
            header.push_str(&format!("{:>TOTAL$}", "total"));
            out.say(style::out(DIM, header));
        }

        for person in &self.people {
            let mut row = format!("  {}", pad(&person.name, name_width));
            if grid {
                for seconds in &person.by_day {
                    row.push_str(&format!("{:>CELL$}", format_cell(*seconds)));
                }
            }
            out.say(format!(
                "{}{}{}",
                style::out(CYAN, row),
                style::out(BOLD, format!("{:>TOTAL$}", format_hm(person.seconds))),
                match person.staged_seconds {
                    0 => String::new(),
                    staged => style::out(DIM, format!("  {} staged", format_hm(staged))),
                }
            ));
            if !show_issues {
                continue;
            }
            for issue in &person.issues {
                let mut row = format!(
                    "    {}",
                    pad(
                        &format!("{} {}", issue.key, issue.summary),
                        name_width.saturating_sub(2)
                    )
                );
                if grid {
                    for seconds in &issue.by_day {
                        row.push_str(&format!("{:>CELL$}", format_cell(*seconds)));
                    }
                }
                out.say(format!(
                    "{}{:>TOTAL$}",
                    style::out(DIM, row),
                    format_hm(issue.seconds)
                ));
            }
        }

        if grid {
            let mut totals = format!("  {}", pad("all", name_width));
            for seconds in &self.by_day {
                totals.push_str(&format!("{:>CELL$}", format_cell(*seconds)));
            }
            totals.push_str(&format!("{:>TOTAL$}", format_hm(self.seconds)));
            out.say(style::out(DIM, totals));
        }

        out.say(format!(
            "\n{} · {} · {} entr{}",
            plural(self.people.len(), "person", "people"),
            format_hm(self.seconds),
            self.entries,
            if self.entries == 1 { "y" } else { "ies" }
        ));
        if self.skipped > 0 {
            out.say(style::out(
                DIM,
                format!(
                    "{} worklog{} could not be read and are left out",
                    self.skipped,
                    if self.skipped == 1 { "" } else { "s" }
                ),
            ));
        }
        if !grid {
            out.hint("the window is too wide for a day column each; \"--days 14\" or fewer draws the grid");
        }
        if !show_issues {
            out.hint("use \"--show-issues\" to see which issues the time went to");
        }
    }
}

/// Turn the per-person breakdown the other way up: a row per issue.
fn issue_totals(people: &[Person], columns: usize) -> Vec<IssueTotal> {
    let mut out: Vec<IssueTotal> = Vec::new();
    for person in people {
        for i in &person.issues {
            let at = match out.iter().position(|t| t.key == i.key) {
                Some(at) => at,
                None => {
                    out.push(IssueTotal {
                        key: i.key.clone(),
                        summary: i.summary.clone(),
                        issue_type: i.issue_type.clone(),
                        parent: i.parent.clone(),
                        rolled_up: Vec::new(),
                        seconds: 0,
                        staged_seconds: 0,
                        by_day: vec![0; columns],
                        entries: 0,
                        people: Vec::new(),
                    });
                    out.len() - 1
                }
            };
            let total = &mut out[at];
            total.seconds += i.seconds;
            total.staged_seconds += i.staged_seconds;
            total.entries += i.entries;
            for (day, seconds) in i.by_day.iter().enumerate() {
                total.by_day[day] += seconds;
            }
            for key in &i.rolled_up {
                if !total.rolled_up.contains(key) {
                    total.rolled_up.push(key.clone());
                }
            }
            total.rolled_up.sort();
            total.people.push(Share {
                name: person.name.clone(),
                account_id: person.account_id.clone(),
                seconds: i.seconds,
            });
        }
    }
    out.sort_by(|a, b| b.seconds.cmp(&a.seconds).then_with(|| a.key.cmp(&b.key)));
    out
}

/// Which way up a table is drawn: a column per day, or a column per person.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, clap::ValueEnum)]
pub enum Columns {
    /// People down the side, a column per day
    #[default]
    Days,
    /// Issues down the side, a column per person
    People,
}

/// A plain table: the cells already formatted, the last row the totals.
pub struct Table {
    pub head: Vec<String>,
    pub rows: Vec<Vec<String>>,
}

impl Table {
    /// Markdown, ready to paste into a page: a header, a rule, a row per line.
    /// An empty cell is a day or a person with nothing booked.
    pub fn markdown(&self) -> String {
        let line = |cells: &[String]| {
            let cells: Vec<String> = cells
                .iter()
                .map(|c| c.replace('|', "\\|").replace('\n', " "))
                .collect();
            format!("| {} |", cells.join(" | "))
        };
        let mut out = vec![line(&self.head)];
        // The first column is labels; every other is a figure, so right-aligned.
        let rule: Vec<&str> = (0..self.head.len())
            .map(|i| if i == 0 { "---" } else { "---:" })
            .collect();
        out.push(format!("|{}|", rule.join("|")));
        out.extend(self.rows.iter().map(|r| line(r)));
        out.join("\n")
    }

    /// The same table padded to line up on a terminal, the labels column cut
    /// to `label_width`.
    pub fn text(&self, label_width: usize) -> Vec<String> {
        let mut widths: Vec<usize> = self.head.iter().map(|h| h.width()).collect();
        for row in &self.rows {
            for (i, cell) in row.iter().enumerate() {
                widths[i] = widths[i].max(cell.width());
            }
        }
        widths[0] = widths[0].min(label_width);
        let line = |cells: &[String]| {
            cells
                .iter()
                .enumerate()
                .map(|(i, c)| match i {
                    0 => pad(c, widths[0]),
                    _ => format!("{}{c}", " ".repeat(widths[i].saturating_sub(c.width()))),
                })
                .collect::<Vec<_>>()
                .join("  ")
        };
        let mut out = vec![line(&self.head)];
        out.extend(self.rows.iter().map(|r| line(r)));
        out
    }
}

impl TimeReport {
    /// People down the side, a column per day, a total column and row.
    pub fn days_table(&self) -> Table {
        let mut head = vec!["Person".to_string()];
        head.extend(self.dates.iter().map(|d| d.to_string()));
        head.push("Total".into());
        let mut rows: Vec<Vec<String>> = self
            .people
            .iter()
            .map(|p| {
                let mut row = vec![p.name.clone()];
                row.extend(p.by_day.iter().map(|s| figure(*s)));
                row.push(figure(p.seconds));
                row
            })
            .collect();
        let mut total = vec!["Total".to_string()];
        total.extend(self.by_day.iter().map(|s| figure(*s)));
        total.push(figure(self.seconds));
        rows.push(total);
        Table { head, rows }
    }

    /// Issues down the side, a column per person, a total column and row: the
    /// worklog table a sprint review pastes into its page.
    pub fn people_table(&self) -> Table {
        let mut head = vec!["Issue".to_string()];
        head.extend(self.people.iter().map(|p| p.name.clone()));
        head.push("Total".into());
        let mut rows: Vec<Vec<String>> = self
            .issues
            .iter()
            .map(|i| {
                let label = match i.summary.is_empty() {
                    true => i.key.clone(),
                    false => format!("{} {}", i.key, i.summary),
                };
                let mut row = vec![label];
                for p in &self.people {
                    let booked = i
                        .people
                        .iter()
                        .find(|s| s.name == p.name && s.account_id == p.account_id)
                        .map_or(0, |s| s.seconds);
                    row.push(figure(booked));
                }
                row.push(figure(i.seconds));
                row
            })
            .collect();
        let mut total = vec!["Total".to_string()];
        total.extend(self.people.iter().map(|p| figure(p.seconds)));
        total.push(figure(self.seconds));
        rows.push(total);
        Table { head, rows }
    }

    /// The table `columns` asks for.
    pub fn table(&self, columns: Columns) -> Table {
        match columns {
            Columns::Days => self.days_table(),
            Columns::People => self.people_table(),
        }
    }

    /// The heading and freshness lines, then the table lined up for a
    /// terminal.
    pub fn render_table(&self, out: Out, columns: Columns) {
        use crate::style::{self, BOLD, DIM};
        out.say(style::out(BOLD, self.title()));
        match self.freshness() {
            Some(line) => out.say(style::out(DIM, line)),
            None => out.say(style::out(DIM, "never fetched")),
        }
        if self.people.is_empty() {
            out.say("\nnobody booked any time in this window");
            out.hint("use \"jired pull\" to bring down what the server has");
            return;
        }
        out.say("");
        for line in self.table(columns).text(48) {
            out.say(line);
        }
    }

    /// `time booked over 5 days (2026-10-19 → 2026-10-23), project TSK`.
    pub fn title(&self) -> String {
        format!(
            "time booked over {} day{} ({}){}{}",
            self.days,
            if self.days == 1 { "" } else { "s" },
            self.span(),
            match &self.project {
                Some(p) => format!(", project {p}"),
                None => String::new(),
            },
            match self.rollup {
                Some(_) => ", sub-tasks under their parents",
                None => "",
            }
        )
    }
}

/// A table cell: the duration, or nothing for a day nobody booked — a dot or a
/// `0m` in every empty cell is noise in a pasted table.
fn figure(seconds: u64) -> String {
    match seconds {
        0 => String::new(),
        s => format_hm(s),
    }
}

/// Width of one day column and of the total column, in the printed grid.
const CELL: usize = 6;
const TOTAL: usize = 9;

/// Widest printed line the grid is allowed to want. Past it the day columns go
/// rather than wrap, because a wrapped grid is not a grid.
const TEXT_WIDTH: usize = 100;

/// A day column's heading: `Su16`, `Mo17`. Two letters and the day of the
/// month is the least that still says *which* day without a second header row.
pub fn column_head(date: &NaiveDate) -> String {
    let weekday = match date.weekday() {
        Weekday::Mon => "Mo",
        Weekday::Tue => "Tu",
        Weekday::Wed => "We",
        Weekday::Thu => "Th",
        Weekday::Fri => "Fr",
        Weekday::Sat => "Sa",
        Weekday::Sun => "Su",
    };
    format!("{weekday}{:02}", date.day())
}

pub fn is_weekend(date: &NaiveDate) -> bool {
    matches!(date.weekday(), Weekday::Sat | Weekday::Sun)
}

/// Hours and minutes, never days: how long a working day is is an instance
/// setting, so a total shown in days would mean different things on different
/// servers.
pub fn format_hm(seconds: u64) -> String {
    let minutes = seconds / 60;
    match (minutes / 60, minutes % 60) {
        (0, m) => format!("{m}m"),
        (h, 0) => format!("{h}h"),
        (h, m) => format!("{h}h {m}m"),
    }
}

/// The same duration in a grid cell, where every column is worth two or three
/// characters: `2h30`, `45m`, `8h`, and a dot for a day nothing was booked on.
pub fn format_cell(seconds: u64) -> String {
    let minutes = seconds / 60;
    match (minutes / 60, minutes % 60) {
        (0, 0) => "·".to_string(),
        (0, m) => format!("{m}m"),
        (h, 0) => format!("{h}h"),
        (h, m) => format!("{h}h{m:02}"),
    }
}

/// One cell of a heat map: how full a day was, relative to the busiest one.
///
/// What a window too wide for numbers falls back to. The shape of a month is
/// still readable when the figures in it are not.
pub fn heat(seconds: u64, peak: u64) -> char {
    if seconds == 0 || peak == 0 {
        return '·';
    }
    match (seconds * 4).div_ceil(peak) {
        0 | 1 => '░',
        2 => '▒',
        3 => '▓',
        _ => '█',
    }
}

/// A proportion drawn in `cells` columns, the staged part of it hollow.
///
/// Staged time is real time — it is in the total either way — so it fills the
/// bar; drawing it differently is what stops "on the server" and "on my disk"
/// from looking like the same claim.
pub fn bar(share: f64, staged: f64, cells: usize) -> String {
    let filled = ((share * cells as f64).round() as usize).min(cells);
    let hollow = ((staged * cells as f64).round() as usize).min(filled);
    format!(
        "{}{}{}",
        "█".repeat(filled - hollow),
        "▒".repeat(hollow),
        "·".repeat(cells - filled)
    )
}

/// `text` in exactly `cells` columns: padded, or cut with an ellipsis.
pub fn pad(text: &str, cells: usize) -> String {
    let text = fit(text, cells);
    let width = text.width();
    format!("{text}{}", " ".repeat(cells.saturating_sub(width)))
}

/// `text` in at most `cells` columns, saying where it was cut.
pub fn fit(text: &str, cells: usize) -> String {
    if text.width() <= cells {
        return text.to_string();
    }
    if cells <= 1 {
        return "…".repeat(cells);
    }
    let mut out = String::new();
    let mut width = 0;
    for g in unicode_segmentation::UnicodeSegmentation::graphemes(text, true) {
        let next = width + g.width();
        if next > cells - 1 {
            break;
        }
        out.push_str(g);
        width = next;
    }
    out.push('…');
    out
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doc::WorklogEntry;

    fn at(text: &str) -> DateTime<Utc> {
        crate::api::model::parse_timestamp(text).expect("a timestamp")
    }

    fn issue(key: &str, summary: &str, worklogs: Vec<WorklogEntry>) -> IssueDoc {
        let mut doc = IssueDoc::parse(&format!(
            "---\nkey: {key}\nid: '1'\nsummary: {summary}\njired: {{}}\n---\n\n# {summary}\n"
        ))
        .expect("a parseable issue");
        doc.worklogs = worklogs;
        doc
    }

    fn booked(author: &str, time: &str, started: &str) -> WorklogEntry {
        WorklogEntry {
            id: WorklogId::Existing(format!("{author}-{started}")),
            time_spent: time.into(),
            author: author.into(),
            started: started.into(),
            body: String::new(),
        }
    }

    fn staged(time: &str, started: &str) -> WorklogEntry {
        WorklogEntry {
            id: WorklogId::New,
            time_spent: time.into(),
            author: String::new(),
            started: started.into(),
            body: String::new(),
        }
    }

    fn week() -> Window {
        Window::last(7, at("2026-08-22T10:00:00Z"))
    }

    /// The whole point: one row per person, most time first.
    #[test]
    fn time_is_totalled_per_person_and_ranked() {
        let issues = vec![
            issue(
                "PROJ-142",
                "Fix login flow",
                vec![
                    booked("Jane D", "2h 30m", "2026-08-20T09:00:00Z"),
                    booked("Ilya A", "1h", "2026-08-21T08:00:00Z"),
                ],
            ),
            issue(
                "PROJ-150",
                "Rotate the certificate",
                vec![booked("Ilya A", "4h", "2026-08-22T08:00:00Z")],
            ),
        ];
        let report = TimeReport::build(&week(), "Ilya A", &issues);

        assert_eq!(report.people.len(), 2);
        assert_eq!(report.people[0].name, "Ilya A");
        assert_eq!(report.people[0].seconds, 5 * 3600);
        assert_eq!(report.people[1].name, "Jane D");
        assert_eq!(report.people[1].seconds, 9000);
        assert_eq!(report.seconds, 5 * 3600 + 9000);
        assert_eq!(report.entries, 3);

        // Each person breaks down into the issues they booked against, biggest
        // first, and only the issues they touched.
        let ilya = &report.people[0];
        assert_eq!(ilya.issues.len(), 2);
        assert_eq!(ilya.issues[0].key, "PROJ-150");
        assert_eq!(ilya.issues[0].summary, "Rotate the certificate");
        assert_eq!(ilya.issues[1].key, "PROJ-142");
        assert_eq!(report.people[1].issues.len(), 1);
    }

    /// The grid: every total is also placed on the day it was booked, at every
    /// level, and the columns run oldest to newest.
    #[test]
    fn time_lands_in_the_column_of_the_day_it_was_booked() {
        let issues = vec![issue(
            "PROJ-142",
            "Fix login flow",
            vec![
                booked("Jane D", "2h 30m", "2026-08-20T09:00:00Z"),
                booked("Jane D", "1h", "2026-08-22T23:59:00Z"),
                booked("Ilya A", "30m", "2026-08-16T00:00:00Z"),
            ],
        )];
        let report = TimeReport::build(&week(), "", &issues);

        assert_eq!(report.dates.len(), 7);
        assert_eq!(report.dates[0].to_string(), "2026-08-16");
        assert_eq!(report.dates[6].to_string(), "2026-08-22");

        let jane = report.people.iter().find(|p| p.name == "Jane D").unwrap();
        assert_eq!(jane.by_day, vec![0, 0, 0, 0, 9000, 0, 3600]);
        assert_eq!(jane.issues[0].by_day, jane.by_day);
        // Column totals are what everyone booked that day.
        assert_eq!(report.by_day, vec![1800, 0, 0, 0, 9000, 0, 3600]);
        assert_eq!(report.by_day.iter().sum::<u64>(), report.seconds);
    }

    /// The window is whole days: an entry from the seventh day back counts
    /// however far into the morning the report is run, and the eighth does not.
    #[test]
    fn the_window_is_counted_in_whole_days() {
        let issues = vec![issue(
            "PROJ-142",
            "Fix login flow",
            vec![
                booked("Jane D", "1h", "2026-08-16T23:00:00Z"),
                booked("Jane D", "2h", "2026-08-16T00:00:00Z"),
                booked("Old Hand", "8h", "2026-08-15T23:59:59Z"),
            ],
        )];
        let report = TimeReport::build(&week(), "", &issues);
        assert_eq!(report.people.len(), 1, "{:?}", report.people);
        assert_eq!(report.people[0].seconds, 3 * 3600);

        // And a shorter window keeps only what falls inside it.
        let today = TimeReport::build(&Window::last(1, at("2026-08-22T10:00:00Z")), "", &issues);
        assert!(today.people.is_empty());
    }

    /// Moving the window back is how earlier weeks are read, and it has to move
    /// both edges: what was in view drops out, what was before it comes in.
    #[test]
    fn a_window_moved_back_shows_the_days_before_it() {
        let issues = vec![issue(
            "PROJ-142",
            "Fix login flow",
            vec![
                booked("Jane D", "1h", "2026-08-20T09:00:00Z"),
                booked("Old Hand", "8h", "2026-08-10T09:00:00Z"),
            ],
        )];
        let now = at("2026-08-22T10:00:00Z");

        let this_week = TimeReport::build(&Window::ending(7, now, 0), "", &issues);
        assert_eq!(this_week.people.len(), 1);
        assert_eq!(this_week.people[0].name, "Jane D");

        let week_before = TimeReport::build(&Window::ending(7, now, 7), "", &issues);
        assert_eq!(week_before.span(), "2026-08-09 → 2026-08-15");
        assert_eq!(week_before.people.len(), 1);
        assert_eq!(week_before.people[0].name, "Old Hand");
        assert_eq!(week_before.people[0].by_day[1], 8 * 3600);
    }

    /// A staged entry belongs to whoever pushes it, so it counts towards them —
    /// and separately, because it is not on the server yet.
    #[test]
    fn staged_time_is_attributed_to_the_account_and_flagged() {
        let issues = vec![issue(
            "PROJ-142",
            "Fix login flow",
            vec![staged("45m", "2026-08-22T08:00:00Z")],
        )];
        let report = TimeReport::build(&week(), "Ilya A", &issues);
        assert_eq!(report.people[0].name, "Ilya A");
        assert_eq!(report.people[0].seconds, 2700);
        assert_eq!(report.people[0].staged_seconds, 2700);
        assert_eq!(report.staged_seconds, 2700);

        // Without a name for the account it is still the reader's own time.
        let anonymous = TimeReport::build(&week(), "", &issues);
        assert_eq!(anonymous.people[0].name, "you");
    }

    /// `### [new] 45m` typed by hand carries no date; the push will date it now,
    /// so the report does too rather than dropping it.
    #[test]
    fn a_staged_entry_without_a_start_is_counted_as_now() {
        let issues = vec![issue("PROJ-142", "Fix login flow", vec![staged("45m", "")])];
        let report = TimeReport::build(&week(), "Ilya A", &issues);
        assert_eq!(report.people[0].seconds, 2700);
        assert_eq!(report.people[0].by_day[6], 2700, "today's column");
        assert_eq!(report.skipped, 0);

        // A window that does not reach today does not reach it either.
        let earlier = TimeReport::build(
            &Window::ending(7, at("2026-08-22T10:00:00Z"), 7),
            "Ilya A",
            &issues,
        );
        assert!(earlier.people.is_empty());
    }

    /// What cannot be counted is said out loud rather than quietly dropped.
    #[test]
    fn unreadable_entries_are_counted_as_skipped() {
        let issues = vec![issue(
            "PROJ-142",
            "Fix login flow",
            vec![
                booked("Jane D", "a while", "2026-08-20T09:00:00Z"),
                booked("Jane D", "1h", "whenever"),
            ],
        )];
        let report = TimeReport::build(&week(), "", &issues);
        assert_eq!(report.skipped, 2);
        assert!(report.people.is_empty());
    }

    /// `--since 2026-10-19 --until 2026-10-23` is Monday to Friday, both ends
    /// counted, whatever day the report is run on.
    #[test]
    fn a_window_between_two_dates_counts_both() {
        let day = |d: &str| NaiveDate::parse_from_str(d, "%Y-%m-%d").unwrap();
        let now = at("2026-10-27T10:00:00Z");
        let w = Window::between(day("2026-10-19"), day("2026-10-23"), now).unwrap();
        assert_eq!(w.days, 5);
        assert_eq!(w.dates().first().unwrap().to_string(), "2026-10-19");
        assert_eq!(w.dates().last().unwrap().to_string(), "2026-10-23");
        assert_eq!(w.back(now), Some(4));
        assert!(Window::between(day("2026-10-23"), day("2026-10-19"), now).is_none());
        // A window reaching past today has nowhere for the screen to start.
        let ahead = Window::between(day("2026-10-26"), day("2026-10-30"), now).unwrap();
        assert_eq!(ahead.back(now), None);
    }

    fn author(id: &str, name: &str) -> Author {
        Author {
            id: id.into(),
            display_name: name.into(),
        }
    }

    /// The field report: Maria Nilsson became Maria Bergman, files pulled
    /// before the rename carry the old name, and the report counted two
    /// people with the same `nilsson.m`. The account is the person.
    #[test]
    fn people_are_grouped_by_account_and_named_by_their_latest_entry() {
        let issues = vec![issue(
            "PROJ-142",
            "Fix login flow",
            vec![
                booked("Maria Nilsson", "2h", "2026-08-17T09:00:00Z"),
                booked("Maria Bergman", "1h", "2026-08-20T09:00:00Z"),
                booked("Jane D", "30m", "2026-08-20T09:00:00Z"),
            ],
        )];
        let mut lookup = Lookup::default();
        for (w, who) in [
            (
                "Maria Nilsson-2026-08-17T09:00:00Z",
                author("nilsson.m", "Maria Bergman"),
            ),
            (
                "Maria Bergman-2026-08-20T09:00:00Z",
                author("nilsson.m", "Maria Bergman"),
            ),
            ("Jane D-2026-08-20T09:00:00Z", author("jane", "Jane D")),
        ] {
            lookup.authors.insert(w.into(), who);
        }
        let report = TimeReport::build_with(&week(), "", &issues, &lookup, false);
        assert_eq!(report.people.len(), 2, "{:?}", report.people);
        let maria = &report.people[0];
        assert_eq!(maria.account_id.as_deref(), Some("nilsson.m"));
        assert_eq!(maria.name, "Maria Bergman");
        assert_eq!(maria.also_known_as, ["Maria Nilsson"]);
        assert_eq!(maria.seconds, 3 * 3600);
    }

    /// Staged time is the account's, and joins the account's booked time
    /// rather than standing beside it under the same name.
    #[test]
    fn staged_time_joins_the_account_it_belongs_to() {
        let issues = vec![issue(
            "PROJ-142",
            "Fix login flow",
            vec![
                booked("Ilya A", "1h", "2026-08-20T09:00:00Z"),
                staged("30m", "2026-08-21T09:00:00Z"),
            ],
        )];
        let mut lookup = Lookup {
            me_id: Some("ilya".into()),
            ..Default::default()
        };
        lookup.authors.insert(
            "Ilya A-2026-08-20T09:00:00Z".into(),
            author("ilya", "Ilya A"),
        );
        let report = TimeReport::build_with(&week(), "Ilya A", &issues, &lookup, false);
        assert_eq!(report.people.len(), 1, "{:?}", report.people);
        assert_eq!(report.people[0].seconds, 5400);
        assert_eq!(report.people[0].staged_seconds, 1800);
    }

    /// `--rollup parent`: a sub-task's time is its parent's row, a parent
    /// with no file of its own is named from the snapshot, and the row says
    /// what it folded in.
    #[test]
    fn rollup_files_sub_task_time_under_the_top_of_the_chain() {
        let mut sub = issue(
            "PROJ-143",
            "Add integration test",
            vec![booked("Jane D", "1h", "2026-08-20T09:00:00Z")],
        );
        sub.frontmatter.parent = Some("PROJ-142".into());
        let mut orphan = issue(
            "PROJ-201",
            "Write the runbook",
            vec![booked("Jane D", "2h", "2026-08-21T09:00:00Z")],
        );
        orphan.frontmatter.parent = Some("PROJ-200".into());
        let issues = vec![
            issue(
                "PROJ-142",
                "Fix login flow",
                vec![booked("Ilya A", "30m", "2026-08-20T09:00:00Z")],
            ),
            sub,
            orphan,
        ];
        let mut lookup = Lookup::default();
        lookup.known.insert(
            "PROJ-200".into(),
            IssueInfo {
                summary: "Release 1.0".into(),
                issue_type: "Story".into(),
                parent: None,
            },
        );

        let report = TimeReport::build_with(&week(), "", &issues, &lookup, true);
        assert_eq!(report.rollup, Some("parent"));
        let keys: Vec<&str> = report.issues.iter().map(|i| i.key.as_str()).collect();
        assert_eq!(keys, ["PROJ-200", "PROJ-142"]);
        assert_eq!(report.issues[0].summary, "Release 1.0");
        assert_eq!(report.issues[0].rolled_up, ["PROJ-201"]);
        assert_eq!(report.issues[1].seconds, 5400);
        assert_eq!(report.issues[1].rolled_up, ["PROJ-143"]);

        // Without it, each issue is its own row and says where it hangs.
        let flat = TimeReport::build_with(&week(), "", &issues, &lookup, false);
        let sub = flat.issues.iter().find(|i| i.key == "PROJ-143").unwrap();
        assert_eq!(sub.parent.as_deref(), Some("PROJ-142"));
        assert!(sub.rolled_up.is_empty());
    }

    /// The table a sprint review pastes: issues down the side, a column per
    /// person, an empty cell for nothing booked, and a total row and column.
    #[test]
    fn the_people_table_is_markdown_with_totals() {
        let issues = vec![
            issue(
                "PROJ-142",
                "Fix | login",
                vec![
                    booked("Jane D", "2h 30m", "2026-08-20T09:00:00Z"),
                    booked("Ilya A", "1h", "2026-08-21T09:00:00Z"),
                ],
            ),
            issue(
                "PROJ-150",
                "Rotate",
                vec![booked("Jane D", "1h", "2026-08-21T09:00:00Z")],
            ),
        ];
        let report = TimeReport::build(&week(), "", &issues);
        assert_eq!(
            report.people_table().markdown(),
            "| Issue | Jane D | Ilya A | Total |\n\
             |---|---:|---:|---:|\n\
             | PROJ-142 Fix \\| login | 2h 30m | 1h | 3h 30m |\n\
             | PROJ-150 Rotate | 1h |  | 1h |\n\
             | Total | 3h 30m | 1h | 4h 30m |"
        );
        let days = report.days_table().markdown();
        assert!(days.starts_with("| Person | 2026-08-16 |"), "{days}");
        assert!(days.ends_with("| 4h 30m |"), "{days}");
    }

    #[test]
    fn durations_are_shown_in_hours_and_minutes() {
        assert_eq!(format_hm(0), "0m");
        assert_eq!(format_hm(2700), "45m");
        assert_eq!(format_hm(3600), "1h");
        assert_eq!(format_hm(9000), "2h 30m");
        // A day is an instance setting, so 40 hours stay 40 hours.
        assert_eq!(format_hm(40 * 3600), "40h");
    }

    /// A cell is two or three characters wide, and an empty day says so rather
    /// than leaving a hole in the grid.
    #[test]
    fn a_grid_cell_is_the_shortest_form_of_a_duration() {
        assert_eq!(format_cell(0), "·");
        assert_eq!(format_cell(2700), "45m");
        assert_eq!(format_cell(3600), "1h");
        assert_eq!(format_cell(9000), "2h30");
        assert_eq!(format_cell(8 * 3600 + 5 * 60), "8h05");
    }

    #[test]
    fn a_heat_cell_is_scaled_against_the_busiest_day() {
        assert_eq!(heat(0, 8 * 3600), '·');
        assert_eq!(heat(3600, 8 * 3600), '░');
        assert_eq!(heat(4 * 3600, 8 * 3600), '▒');
        assert_eq!(heat(6 * 3600, 8 * 3600), '▓');
        assert_eq!(heat(8 * 3600, 8 * 3600), '█');
        assert_eq!(heat(3600, 0), '·', "nothing to scale against");
    }

    #[test]
    fn a_column_heading_names_the_weekday_and_the_day() {
        let date = NaiveDate::from_ymd_opt(2026, 8, 22).unwrap();
        assert_eq!(column_head(&date), "Sa22");
        assert!(is_weekend(&date));
        assert!(!is_weekend(&NaiveDate::from_ymd_opt(2026, 8, 20).unwrap()));
    }

    #[test]
    fn a_bar_is_as_full_as_the_share_it_draws() {
        assert_eq!(bar(0.0, 0.0, 4), "····");
        assert_eq!(bar(0.5, 0.0, 4), "██··");
        assert_eq!(bar(1.0, 0.0, 4), "████");
        assert_eq!(bar(1.4, 0.0, 4), "████", "rounding cannot overflow the bar");
        // The staged part of the share is drawn hollow, inside it.
        assert_eq!(bar(1.0, 0.5, 4), "██▒▒");
        assert_eq!(bar(0.5, 1.0, 4), "▒▒··", "never more hollow than filled");
    }

    #[test]
    fn columns_are_measured_in_terminal_cells() {
        assert_eq!(pad("Jane", 8), "Jane    ");
        assert_eq!(pad("Jane Doe the Third", 8), "Jane Do…");
        assert_eq!(fit("Забота", 4), "Заб…");
        // A wide character takes two cells, so only one fits beside the mark.
        assert_eq!(fit("日本語です", 4), "日…");
        assert_eq!(pad("日本語です", 4).width(), 4);
    }
}
