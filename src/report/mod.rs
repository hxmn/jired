//! `jired report` — where the booked time went.
//!
//! Read off the working copy, like every other read: the `## Worklogs` sections
//! of the files already on disk say who booked what, when, and against which
//! issue, so a report needs no network and costs one pass over the files. What
//! it cannot know is time booked since the last `pull`, which is why every
//! rendering says how old the snapshot is.

use crate::doc::{parse_duration, IssueDoc, WorklogId};
use crate::json::Out;
use chrono::{DateTime, Duration, NaiveTime, Utc};
use serde::Serialize;
use unicode_width::UnicodeWidthStr;

/// Who a worklog belongs to when the file does not say and the account has no
/// name either. Only reachable for an entry the server sent without an author.
const NOBODY: &str = "(unknown)";

/// The stretch of time a report covers: whole UTC days, the last of them today.
///
/// Whole days rather than a rolling `N × 24h`, because the question is asked in
/// days: an entry booked at 09:00 seven days ago should not drop out of a
/// seven-day report merely because it is now 10:00.
#[derive(Debug, Clone, Copy)]
pub struct Window {
    pub days: u32,
    pub since: DateTime<Utc>,
    /// When the report was taken. Not an upper bound on the entries: time
    /// booked for later today is still time booked, and hiding it would make a
    /// total that does not add up.
    pub until: DateTime<Utc>,
}

impl Window {
    pub fn last(days: u32, now: DateTime<Utc>) -> Self {
        let days = days.max(1);
        let midnight = now.date_naive().and_time(NaiveTime::MIN).and_utc();
        Self {
            days,
            since: midnight - Duration::days(i64::from(days - 1)),
            until: now,
        }
    }

    fn holds(&self, when: DateTime<Utc>) -> bool {
        when >= self.since
    }
}

/// Time booked inside a window, by person, and under each person by issue.
#[derive(Debug, Clone, Serialize)]
pub struct TimeReport {
    pub days: u32,
    /// Midnight UTC of the first day counted.
    pub since: String,
    /// When the report was taken.
    pub until: String,
    pub project: Option<String>,
    /// When the snapshot was last refreshed, so a stale report can say so.
    pub last_fetch_at: Option<String>,
    pub last_fetch_age_seconds: Option<i64>,
    /// Everything booked in the window.
    pub seconds: u64,
    /// The part of it still staged locally, which is a subset of `seconds`.
    pub staged_seconds: u64,
    pub entries: usize,
    /// Worklogs that could not be counted: no readable start, or a duration in
    /// notation nothing here understands.
    pub skipped: usize,
    /// Most time first, then by name.
    pub people: Vec<Person>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Person {
    pub name: String,
    pub seconds: u64,
    pub staged_seconds: u64,
    pub entries: usize,
    /// Most time first, then by key.
    pub issues: Vec<IssueTime>,
}

#[derive(Debug, Clone, Serialize)]
pub struct IssueTime {
    pub key: String,
    pub summary: String,
    pub seconds: u64,
    pub staged_seconds: u64,
    pub entries: usize,
}

impl TimeReport {
    /// Aggregate `issues`, attributing entries nobody has claimed to `me`.
    ///
    /// A staged entry has no author yet — the author is whoever pushes it — so
    /// it counts towards the person running the report.
    pub fn build(window: &Window, me: &str, issues: &[IssueDoc]) -> Self {
        let mut people: Vec<Person> = Vec::new();
        let mut skipped = 0;

        for issue in issues {
            for w in &issue.worklogs {
                let staged = w.id == WorklogId::New;
                let started = match crate::api::model::parse_timestamp(&w.started) {
                    Some(t) => t,
                    // A staged entry that names no start is time booked now,
                    // which is what pushing it will date it as.
                    None if staged => window.until,
                    None => {
                        skipped += 1;
                        continue;
                    }
                };
                if !window.holds(started) {
                    continue;
                }
                let Some(seconds) = parse_duration(&w.time_spent) else {
                    skipped += 1;
                    continue;
                };

                let who = match (w.author.trim(), me.trim()) {
                    ("", "") if staged => "you",
                    ("", "") => NOBODY,
                    ("", me) => me,
                    (author, _) => author,
                };
                let at = match people.iter().position(|p| p.name == who) {
                    Some(at) => at,
                    None => {
                        people.push(Person {
                            name: who.to_string(),
                            seconds: 0,
                            staged_seconds: 0,
                            entries: 0,
                            issues: Vec::new(),
                        });
                        people.len() - 1
                    }
                };
                let person = &mut people[at];
                person.seconds += seconds;
                person.entries += 1;
                if staged {
                    person.staged_seconds += seconds;
                }

                let key = &issue.frontmatter.key;
                match person.issues.iter_mut().find(|i| &i.key == key) {
                    Some(booked) => {
                        booked.seconds += seconds;
                        booked.entries += 1;
                        if staged {
                            booked.staged_seconds += seconds;
                        }
                    }
                    None => person.issues.push(IssueTime {
                        key: key.clone(),
                        summary: issue.frontmatter.summary.clone(),
                        seconds,
                        staged_seconds: if staged { seconds } else { 0 },
                        entries: 1,
                    }),
                }
            }
        }

        for person in &mut people {
            person
                .issues
                .sort_by(|a, b| b.seconds.cmp(&a.seconds).then_with(|| a.key.cmp(&b.key)));
        }
        people.sort_by(|a, b| b.seconds.cmp(&a.seconds).then_with(|| a.name.cmp(&b.name)));

        Self {
            days: window.days,
            since: crate::api::model::format_timestamp(&window.since),
            until: crate::api::model::format_timestamp(&window.until),
            project: None,
            last_fetch_at: None,
            last_fetch_age_seconds: None,
            seconds: people.iter().map(|p| p.seconds).sum(),
            staged_seconds: people.iter().map(|p| p.staged_seconds).sum(),
            entries: people.iter().map(|p| p.entries).sum(),
            skipped,
            people,
        }
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
        format!("{} → {}", day_of(&self.since), day_of(&self.until))
    }

    /// How fresh the snapshot behind the report is, if it has ever been pulled.
    pub fn freshness(&self) -> Option<String> {
        let at = self.last_fetch_at.as_ref()?;
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
    pub fn render(&self, out: Out, show_issues: bool) {
        use crate::style::{self, BOLD, CYAN, DIM};

        out.say(style::out(
            BOLD,
            format!(
                "time booked over {} day{} ({}){}",
                self.days,
                if self.days == 1 { "" } else { "s" },
                self.span(),
                match &self.project {
                    Some(p) => format!(", project {p}"),
                    None => String::new(),
                }
            ),
        ));
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
        // format width and would knock every following column out of line.
        let name_width = self
            .people
            .iter()
            .map(|p| p.name.width())
            .max()
            .unwrap_or(0)
            .clamp(8, 32);
        let key_width = self
            .people
            .iter()
            .flat_map(|p| p.issues.iter())
            .map(|i| i.key.width())
            .max()
            .unwrap_or(0)
            .clamp(4, 20);

        out.say("");
        for person in &self.people {
            let share = self.share(person);
            out.say(format!(
                "  {}  {:>9}  {} {:>3.0}%{}",
                style::out(CYAN, pad(&person.name, name_width)),
                format_hm(person.seconds),
                style::out(DIM, bar(share, self.fraction(person.staged_seconds), 12)),
                share * 100.0,
                match person.staged_seconds {
                    0 => String::new(),
                    staged => style::out(DIM, format!("  {} staged", format_hm(staged))),
                }
            ));
            if !show_issues {
                continue;
            }
            for issue in &person.issues {
                out.say(format!(
                    "      {}  {}  {}",
                    pad(&issue.key, key_width),
                    style::out(DIM, pad(&issue.summary, 40)),
                    format_hm(issue.seconds),
                ));
            }
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
        if !show_issues {
            out.hint("use \"--show-issues\" to see which issues the time went to");
        }
    }
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

/// The date part of a timestamp jired wrote.
fn day_of(timestamp: &str) -> &str {
    timestamp.get(..10).unwrap_or(timestamp)
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

    /// Entries from the same person on the same issue are one line, not many.
    #[test]
    fn repeated_bookings_on_one_issue_add_up() {
        let issues = vec![issue(
            "PROJ-142",
            "Fix login flow",
            vec![
                booked("Jane D", "30m", "2026-08-20T09:00:00Z"),
                booked("Jane D", "45m", "2026-08-21T09:00:00Z"),
            ],
        )];
        let report = TimeReport::build(&week(), "", &issues);
        assert_eq!(report.people.len(), 1);
        assert_eq!(report.people[0].issues.len(), 1);
        assert_eq!(report.people[0].issues[0].seconds, 75 * 60);
        assert_eq!(report.people[0].issues[0].entries, 2);
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
        assert_eq!(report.skipped, 0);
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

    #[test]
    fn durations_are_shown_in_hours_and_minutes() {
        assert_eq!(format_hm(0), "0m");
        assert_eq!(format_hm(2700), "45m");
        assert_eq!(format_hm(3600), "1h");
        assert_eq!(format_hm(9000), "2h 30m");
        // A day is an instance setting, so 40 hours stay 40 hours.
        assert_eq!(format_hm(40 * 3600), "40h");
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
