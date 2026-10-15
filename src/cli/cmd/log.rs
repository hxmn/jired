//! `jired log [ISSUE]` [M3.7] — who changed what, and when.
//!
//! With no issue named it reads like `git log`: everything the workspace knows
//! happened, merged into one newest-first list. Name an issue and it narrows to
//! that issue's history.
//!
//! "Happened" is wider than Jira's changelog, which records field changes and
//! nothing else. A comment posted and time booked are as much of an issue's
//! history as a status that moved, so they are merged into the same timeline —
//! out of the snapshot `fetch` already stores, not a second round of requests.
//!
//! A field whose old and new values are short reads as `from → to`. One whose
//! values are prose — a description, an instance's own "Acceptance Criteria" —
//! reads as a unified diff instead: Jira hands back the whole text on both
//! sides, and a page of it does not fit through a column layout.
//!
//! Neighbouring events by the same person on the same issue share one heading.
//! Jira records a transition, the comment explaining it and the time booked
//! against it as three separate things, and printing three identical headings
//! makes one afternoon's work look like three afternoons'.
//!
//! Changelogs are fetched per issue and cached, rather than pulled for every
//! issue during `fetch`: they are large, rarely needed, and asking for them
//! wholesale is the difference between a fast sync and a slow one. So the first
//! run over a workspace fetches what it is missing; every run after it is
//! offline until `--refresh`.

use crate::api::model::{ChangelogEntry, ChangelogItem, RemoteComment, RemoteIssue, RemoteWorklog};
use crate::cli::context::Context;
use crate::cli::GlobalArgs;
use crate::error::{ExitCode, JiredError, Result};
use crate::json::Out;
use crate::store::state::IssueRow;
use crate::style::{self, BOLD, CYAN, DIM, GREEN, RED};
use crate::sync::progress::Progress;
use crate::sync::Engine;
use chrono::{DateTime, Datelike, Duration, Local, Utc};
use clap::Args;
use similar::TextDiff;

#[derive(Args, Debug, Clone)]
pub struct LogArgs {
    /// Issue to show history for: a key, or the path to its file
    ///
    /// Omitted, every issue in the workspace is shown, newest edit first.
    pub issue: Option<String>,
    /// How much to show: a count of entries, or a span back from now
    ///
    /// `--limit 20` is the newest twenty entries; `--limit 1d` is everything
    /// from the last day, however many that is. Spans are written in weeks,
    /// days, hours and minutes — `2w`, `1d12h`, `36h`, `90m`.
    #[arg(long, value_name = "N|SPAN", default_value = "20", value_parser = parse_limit)]
    pub limit: Limit,
    /// Make every issue key a link to the issue in a browser
    ///
    /// On a terminal the key itself becomes clickable and the rows stay the
    /// width they were; into a pipe or a file, where an escape sequence is no
    /// use to anyone, the URL is printed instead.
    #[arg(long)]
    pub links: bool,
    /// Ignore the cached copy and ask the server
    #[arg(long)]
    pub refresh: bool,
}

/// What `--limit` asked for: how many entries, or how far back.
///
/// The two are one flag rather than two because they answer the same question —
/// how much of the log do I want — and a `git log` reader reaches for `--limit`
/// either way. Which one was meant is unambiguous from the spelling: a bare
/// number is a count, anything with a unit is a span.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Limit {
    Count(usize),
    Span(Duration),
}

pub fn parse_limit(text: &str) -> std::result::Result<Limit, String> {
    let text = text.trim();
    if text.chars().all(|c| c.is_ascii_digit()) && !text.is_empty() {
        return text
            .parse()
            .map(Limit::Count)
            .map_err(|_| format!("{text} is more entries than there could ever be"));
    }
    parse_span(text).map(Limit::Span).ok_or_else(|| {
        format!(
            "`{text}` is neither a count nor a span — write `20` for the newest \
             twenty entries, or `1d`, `2w`, `36h`, `90m` for a span back from now"
        )
    })
}

/// A span in calendar time: `1d` is twenty-four hours, `1w` is seven days.
///
/// Deliberately *not* `doc::parse_duration`, which reads the same letters as
/// Jira's working time — there a day is eight hours and a week is five days.
/// That scale is right for booked work and wrong for "what happened since
/// yesterday", and sharing the parser would silently give `--limit 1d` a
/// third of the window it says.
fn parse_span(text: &str) -> Option<Duration> {
    let mut seconds: i64 = 0;
    let mut digits = String::new();
    let mut any = false;

    for c in text.chars() {
        match c {
            '0'..='9' => digits.push(c),
            ' ' | ',' => {}
            _ => {
                let scale: i64 = match c.to_ascii_lowercase() {
                    'w' => 7 * 24 * 3600,
                    'd' => 24 * 3600,
                    'h' => 3600,
                    'm' => 60,
                    _ => return None,
                };
                let n: i64 = digits.parse().ok()?;
                digits.clear();
                seconds = seconds.checked_add(n.checked_mul(scale)?)?;
                any = true;
            }
        }
    }
    // A trailing number has no unit, and a span of nothing would show nothing.
    (any && digits.is_empty() && seconds > 0).then(|| Duration::seconds(seconds))
}

/// A span written the shortest way that stays exact: `172800s` reads as `2d`.
fn describe(span: Duration) -> String {
    let seconds = span.num_seconds().max(60);
    for (unit, scale) in [("w", 7 * 24 * 3600), ("d", 24 * 3600), ("h", 3600)] {
        if seconds >= scale && seconds % scale == 0 {
            return format!("{}{unit}", seconds / scale);
        }
    }
    format!("{}m", seconds / 60)
}

impl Limit {
    /// Cut a newest-first list down to this limit, dating each item with `at`.
    ///
    /// Shared with `jired watch`, which asks the same question of the same
    /// spelling — how much of the recent end do I want — over a different kind
    /// of row. An item nothing dates cannot be shown to fall inside a span, so
    /// a span drops it; a count still counts it.
    pub(crate) fn keep<T>(
        self,
        items: &mut Vec<T>,
        now: DateTime<Utc>,
        at: impl Fn(&T) -> Option<DateTime<Utc>>,
    ) {
        match self {
            Limit::Count(n) => items.truncate(n),
            Limit::Span(span) => {
                let cutoff = now - span;
                items.retain(|item| at(item).is_some_and(|when| when >= cutoff));
            }
        }
    }

    /// The limit to suggest when there is more to see: twice this one.
    ///
    /// Doubling is what makes the hint worth printing — it is a limit the
    /// reader can paste, not a unit conversion they have to do first.
    fn wider(self) -> String {
        match self {
            Limit::Count(n) => n.saturating_mul(2).max(1).to_string(),
            Limit::Span(s) => describe(s.checked_mul(2).unwrap_or(s)),
        }
    }
}

/// Everything one issue contributes to the log.
struct Known {
    key: String,
    changelog: Vec<ChangelogEntry>,
    /// The last snapshot `fetch` stored: where the comments, the worklogs and
    /// the summary come from. `None` when it cannot be parsed, which costs this
    /// issue its comments and its summary but not its changelog.
    snapshot: Option<RemoteIssue>,
}

impl Known {
    fn summary(&self) -> &str {
        self.snapshot
            .as_ref()
            .map(|i| i.fields.summary.as_str())
            .unwrap_or("")
    }
}

/// One thing that happened to an issue, whatever kind of thing it was.
struct Event<'a> {
    key: &'a str,
    /// The issue's summary *now* — a rename shows up as its own row anyway.
    summary: &'a str,
    at: Option<DateTime<Utc>>,
    /// Jira's own id, which is also how same-second events are ordered.
    id: &'a str,
    author: Option<&'a str>,
    what: What<'a>,
}

enum What<'a> {
    Change(&'a [ChangelogItem]),
    Comment(&'a RemoteComment),
    Worklog(&'a RemoteWorklog),
}

impl What<'_> {
    fn kind(&self) -> &'static str {
        match self {
            What::Change(_) => "change",
            What::Comment(_) => "comment",
            What::Worklog(_) => "worklog",
        }
    }
}

impl Event<'_> {
    /// The record a consumer parses, which is not what the terminal shows: the
    /// texts go out whole and undiffed, because a diff is a rendering and the
    /// two sides are the fact.
    fn to_json(&self) -> serde_json::Value {
        let mut doc = serde_json::json!({
            "key": self.key,
            "kind": self.what.kind(),
            "id": self.id,
            "summary": self.summary,
            "author": self.author,
            "created": self.at.as_ref().map(crate::api::model::format_timestamp),
            "items": [],
        });
        match self.what {
            What::Change(items) => {
                doc["items"] = serde_json::json!(items
                    .iter()
                    .map(|i| serde_json::json!({
                        "field": i.field,
                        "from": i.from,
                        "to": i.to,
                    }))
                    .collect::<Vec<_>>());
            }
            // Trimmed at the end only: the trailing newline is the markdown
            // renderer's, not the author's, and it is the one part of the text
            // that would differ from what the issue file holds.
            What::Comment(c) => {
                doc["text"] =
                    serde_json::json!(crate::convert::to_markdown(&c.body).markdown.trim_end());
            }
            What::Worklog(w) => {
                doc["text"] =
                    serde_json::json!(crate::convert::to_markdown(&w.comment).markdown.trim_end());
                doc["time_spent"] = serde_json::json!(w.time_spent);
            }
        }
        doc
    }
}

/// An issue whose snapshot holds fewer comments or worklogs than the server
/// said it had, so the timeline is knowingly short.
struct Partial {
    key: String,
    what: &'static str,
    held: usize,
    total: usize,
}

pub fn run(g: &GlobalArgs, args: &LogArgs, out: Out) -> Result<ExitCode> {
    let mut ctx = Context::open(g, out)?;
    let engine = Engine::open(&mut ctx)?;
    let base_url = engine.base_url().to_string();

    let one = args
        .issue
        .as_deref()
        .map(crate::project::naming::key_from_input)
        .transpose()?;
    let rows = match &one {
        Some(key) => match engine.state.issue(key)? {
            Some(row) => vec![row],
            None => {
                return Err(JiredError::usage(format!("{key} is not in this workspace"))
                    .with_detail("issue", key.clone())
                    .with_hint("run `jired pull` first, or check the key"))
            }
        },
        // A locally created issue has no server history to ask for yet.
        None => engine
            .state
            .issues()?
            .into_iter()
            .filter(|row| !row.key.starts_with("NEW-"))
            .collect(),
    };

    let markup = engine.markup();
    let field_map = engine.field_map()?;
    let mut known: Vec<Known> = Vec::new();
    let mut stale: Vec<(String, Option<DateTime<Utc>>)> = Vec::new();
    for row in rows {
        if args.refresh || is_stale(&engine, &row)? {
            stale.push((row.key.clone(), issue_updated(&row)));
        }
        let changelog = engine.state.changelog(&row.key)?;
        // The newer of the two snapshots: `remote` is what the last fetch saw,
        // and only an issue that has never been fetched falls back to `base`.
        let raw = row
            .remote
            .as_ref()
            .map(|r| &r.raw)
            .or(row.base.as_ref().map(|b| &b.raw));
        let snapshot = raw
            .and_then(|raw| crate::api::normalize::issue_from_json(raw, markup, &field_map).ok());
        known.push(Known {
            key: row.key,
            changelog,
            snapshot,
        });
    }

    if !stale.is_empty() {
        fetch_into(&mut ctx, &engine, &stale, &mut known, one.is_some(), out)?;
    }

    let partial = partial_snapshots(&known);
    let mut events = timeline(&known);
    // Newest first: the last thing that happened is the thing being asked
    // about. Ties break on the entry id, which Jira hands out in order, so a
    // batch of same-second edits still reads in the order it happened.
    events.sort_by(|a, b| {
        b.at.cmp(&a.at)
            .then_with(|| b.id.cmp(a.id))
            .then_with(|| a.key.cmp(b.key))
    });
    let total = events.len();
    let now = Utc::now();
    apply_limit(&mut events, args.limit, now);

    render(
        &events,
        total,
        &partial,
        now,
        one.as_deref(),
        args,
        &base_url,
        out,
    );

    out.emit(
        "log",
        &serde_json::json!({
            "key": one,
            "scope": if one.is_some() { "issue" } else { "workspace" },
            "entries": events.iter().map(Event::to_json).collect::<Vec<_>>(),
        }),
    );
    Ok(ExitCode::Success)
}

/// Every event the workspace knows about, in no particular order yet.
fn timeline(known: &[Known]) -> Vec<Event<'_>> {
    let mut events = Vec::new();
    for k in known {
        let summary = k.summary();
        for entry in &k.changelog {
            events.push(Event {
                key: &k.key,
                summary,
                at: entry.created,
                id: &entry.id,
                author: entry.author.as_deref(),
                what: What::Change(&entry.items),
            });
        }
        let Some(issue) = &k.snapshot else { continue };
        for c in &issue.comments {
            events.push(Event {
                key: &k.key,
                summary,
                at: c.created,
                id: &c.id,
                author: c.author.as_ref().map(|a| a.display_name.as_str()),
                what: What::Comment(c),
            });
        }
        for w in &issue.worklogs {
            events.push(Event {
                key: &k.key,
                summary,
                // Dated by when the booking happened, not by when the work did:
                // a log answers "what happened today", and time booked this
                // morning for last Tuesday happened this morning. `report time`
                // is the command that reads a worklog the other way round.
                at: w.created.or(w.started),
                id: &w.id,
                author: w.author.as_ref().map(|a| a.display_name.as_str()),
                what: What::Worklog(w),
            });
        }
    }
    events
}

/// Issues whose snapshot holds only part of what the server said it had.
///
/// Both listings are completed page by page when an issue is fetched, so this
/// is what a completion that failed at the time looks like weeks later: a
/// timeline missing comments nobody was told about. Rare, and worth a line
/// rather than silence.
fn partial_snapshots(known: &[Known]) -> Vec<Partial> {
    let mut out = Vec::new();
    for k in known {
        let Some(issue) = &k.snapshot else { continue };
        if issue.comment_total > issue.comments.len() {
            out.push(Partial {
                key: k.key.clone(),
                what: "comments",
                held: issue.comments.len(),
                total: issue.comment_total,
            });
        }
        if issue.worklog_total > issue.worklogs.len() {
            out.push(Partial {
                key: k.key.clone(),
                what: "worklogs",
                held: issue.worklogs.len(),
                total: issue.worklog_total,
            });
        }
    }
    out
}

/// Cut a newest-first list of events down to what `--limit` asked for.
///
/// `now` is passed in rather than read here so the window can be tested against
/// a fixed clock.
fn apply_limit(events: &mut Vec<Event>, limit: Limit, now: DateTime<Utc>) {
    limit.keep(events, now, |e| e.at);
}

/// True when the server has edited this issue since its changelog was cached.
///
/// Comparing against the issue's own `updated` — not against an empty cache —
/// is what keeps a repeat `jired log` offline: an issue nothing has happened to
/// is never asked about twice, and one that moved is asked about exactly once.
fn is_stale(engine: &Engine, row: &IssueRow) -> Result<bool> {
    let Some(synced) = engine.state.changelog_synced_at(&row.key)? else {
        return Ok(true);
    };
    Ok(issue_updated(row).is_some_and(|u| u > synced))
}

/// The newest `updated` jired knows for this issue, in server time.
fn issue_updated(row: &IssueRow) -> Option<DateTime<Utc>> {
    row.remote
        .as_ref()
        .map(|r| r.updated)
        .or_else(|| row.base.as_ref().map(|b| b.updated))
}

/// Fetch the changelogs that are out of date, storing each as it arrives.
///
/// In workspace mode a single unreadable issue must not cost the user the
/// whole log — the history that *was* readable is still worth printing — so
/// the first failure stops the fetch with one warning rather than aborting.
/// Asking for one issue by name is different: there the failure is the answer.
fn fetch_into(
    ctx: &mut Context,
    engine: &Engine,
    stale: &[(String, Option<DateTime<Utc>>)],
    known: &mut [Known],
    single: bool,
    out: Out,
) -> Result<()> {
    let client = engine.client(ctx)?;
    let progress = Progress::bar(out, stale.len(), "fetching history");
    for (key, updated) in stale {
        let fetched = match crate::api::block_on(client.changelog(key)) {
            Ok(fetched) => fetched,
            Err(e) if single => return Err(e),
            Err(e) => {
                progress.finish();
                out.warn(format!(
                    "stopped at {key}: {} — showing the history already cached",
                    e.message
                ));
                return Ok(());
            }
        };
        engine.state.save_changelog(key, &fetched)?;
        if let Some(at) = updated {
            engine.state.set_changelog_synced_at(key, at)?;
        }
        if let Some(slot) = known.iter_mut().find(|k| &k.key == key) {
            slot.changelog = fetched;
        }
        progress.inc();
    }
    progress.finish();
    Ok(())
}

/// The unified diff between the two values of one changed field.
struct Prose {
    added: usize,
    removed: usize,
    /// Unified diff lines, signs and `@@` headers included, indented by nobody.
    lines: Vec<String>,
    /// How many lines the cap left out.
    hidden: usize,
}

/// Everything either value may spend on the `field: from → to` row before the
/// field is treated as prose.
///
/// It is a budget rather than a list of field names on purpose: an instance's
/// own "Acceptance Criteria" is as much prose as `description` is, and jired
/// does not know its name — but it can see that what came back does not fit on
/// a line.
const ARROW_BUDGET: usize = 56;

/// How much of one field's diff a log is willing to spend lines on.
///
/// `jired diff` shows one issue at a time and can print everything; a log is a
/// scan over many entries, and a description rewritten wholesale would bury the
/// twenty other things that happened that week.
const MAX_DIFF_LINES: usize = 30;

/// The same, for a comment or a worklog note, which are quoted rather than
/// diffed: enough to recognise the comment, not enough to become the screen.
const MAX_BODY_LINES: usize = 6;

/// How wide the summary column pads to. A longer summary is printed in full and
/// pushes that row's author along; it is never cut.
const SUMMARY_COLUMN: usize = 48;

/// The diff to show for a field, or `None` when `from → to` still reads.
///
/// Prose is decided by the values, not the field: Jira hands back the whole old
/// and new text of a description in one changelog item, and printing that as
/// `from → to` puts a page of text through a column layout — the newlines land
/// in the middle of the row and the reader has to spot the arrow to know which
/// half is which.
fn prose_diff(from: &str, to: &str) -> Option<Prose> {
    let long = from.chars().count() > ARROW_BUDGET || to.chars().count() > ARROW_BUDGET;
    if !(long || from.contains('\n') || to.contains('\n')) {
        return None;
    }
    // A last line that ends like every other one, so that appending a paragraph
    // is not reported as an edit to the paragraph above it.
    let (from, to) = (line_terminated(from), line_terminated(to));
    let diff = TextDiff::from_lines(from.as_ref(), to.as_ref());
    let mut added = 0;
    let mut removed = 0;
    for change in diff.iter_all_changes() {
        match change.tag() {
            similar::ChangeTag::Insert => added += 1,
            similar::ChangeTag::Delete => removed += 1,
            similar::ChangeTag::Equal => {}
        }
    }

    let body = diff
        .unified_diff()
        .context_radius(2)
        // The marker says something about the text jired synthesised, not about
        // the text Jira stored.
        .missing_newline_hint(false)
        .to_string();
    let mut lines: Vec<String> = body.lines().map(str::to_string).collect();
    // Values that differ only in a trailing newline diff to nothing; the arrow
    // form still describes them, so let it.
    if lines.is_empty() {
        return None;
    }
    let hidden = lines.len().saturating_sub(MAX_DIFF_LINES);
    lines.truncate(MAX_DIFF_LINES);
    Some(Prose {
        added,
        removed,
        lines,
        hidden,
    })
}

/// Text whose last line ends like every other line. Empty text stays empty, so
/// a field that had no value diffs as additions alone.
fn line_terminated(s: &str) -> std::borrow::Cow<'_, str> {
    if s.is_empty() || s.ends_with('\n') {
        std::borrow::Cow::Borrowed(s)
    } else {
        std::borrow::Cow::Owned(format!("{s}\n"))
    }
}

/// A timestamp as somebody scanning a log wants it.
///
/// The date of something that happened in the last day is "today", which the
/// reader knows; the year of something that happened this year is this year,
/// which they also know. So each is printed only once it stops being obvious,
/// and what is left is the part that tells them something. Local time, like
/// everything else jired puts on a screen.
fn format_when(at: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let local = at.with_timezone(&Local);
    // A timestamp from the future is a clock disagreeing with the server's, not
    // a reason to print a date; it reads as "just now" either way.
    if now.signed_duration_since(at) < Duration::hours(24) {
        local.format("%H:%M").to_string()
    } else if local.year() == now.with_timezone(&Local).year() {
        local.format("%m-%d %H:%M").to_string()
    } else {
        local.format("%Y-%m-%d %H:%M").to_string()
    }
}

/// Neighbouring events that share an issue and an author, as one block each.
///
/// Only neighbours: the list is already in the order it will be printed, so a
/// block is a run of rows whose headings would have been identical — never a
/// gathering that moves an event away from where its timestamp put it.
///
/// An author nobody recorded joins another one, because the heading it would
/// have printed is the same heading. That is a claim about the row, not about
/// the person: two unattributed events on one issue are not knowably the same
/// hand, and the block does not say they are.
fn blocks<'a, 'e>(events: &'e [Event<'a>]) -> Vec<&'e [Event<'a>]> {
    let mut blocks: Vec<&[Event]> = Vec::new();
    for (i, e) in events.iter().enumerate() {
        match blocks.last_mut() {
            Some(block) if block[0].key == e.key && block[0].author == e.author => {
                let start = i - block.len();
                *block = &events[start..=i];
            }
            _ => blocks.push(&events[i..=i]),
        }
    }
    blocks
}

/// The one timestamp a block is headed with: when it happened, or — when it did
/// not all happen at once — the span from its oldest event to its newest.
///
/// A block usually *is* one moment, and then it reads as one moment. Saying so
/// when it is not would be the heading claiming an issue was touched once at
/// the newest of several times, which is the fact a log exists to carry.
fn block_when(block: &[Event], now: DateTime<Utc>) -> String {
    // Newest first, so the block's own ends are its first and last rows.
    let newest = block.first().and_then(|e| e.at);
    let oldest = block.last().and_then(|e| e.at);
    let (Some(newest), Some(oldest)) = (newest, oldest) else {
        return newest.map(|at| format_when(at, now)).unwrap_or_default();
    };
    let (from, to) = (format_when(oldest, now), format_when(newest, now));
    if from == to {
        return to;
    }
    // Within one day the date is already on the left of the dash, and printing
    // it twice buys nothing but a column every other row has to pad out to.
    let to =
        if oldest.with_timezone(&Local).date_naive() == newest.with_timezone(&Local).date_naive() {
            newest.with_timezone(&Local).format("%H:%M").to_string()
        } else {
            to
        };
    format!("{from}–{to}")
}

/// A terminal hyperlink: the text is what shows, the URL is where it goes.
///
/// OSC 8, which terminals that do not know it drop silently — but only when
/// they are terminals at all. Into a file the escapes are noise around a URL
/// nobody can click, which is why the caller prints the URL instead.
fn hyperlink(url: &str, text: &str) -> String {
    format!("\x1b]8;;{url}\x1b\\{text}\x1b]8;;\x1b\\")
}

fn browse_url(base: &str, key: &str) -> String {
    format!("{}/browse/{}", base.trim_end_matches('/'), key)
}

/// Pad to a column width measured in characters.
///
/// Bytes would make a Cyrillic summary half as wide as it looks, and painting
/// before padding would count the escape codes; both knock every column after
/// this one out of line.
fn pad(text: &str, width: usize) -> String {
    let len = text.chars().count();
    format!("{text}{}", " ".repeat(width.saturating_sub(len)))
}

#[allow(clippy::too_many_arguments)]
fn render(
    events: &[Event],
    total: usize,
    partial: &[Partial],
    now: DateTime<Utc>,
    one: Option<&str>,
    args: &LogArgs,
    base_url: &str,
    out: Out,
) {
    if events.is_empty() {
        // History that exists but sits outside the window is a different answer
        // from no history at all, and saying the second when the first is true
        // reads as jired having lost it.
        match (total, one) {
            (0, Some(key)) => out.say(format!("{key} has nothing recorded against it")),
            (0, None) => {
                out.say("nothing recorded in this workspace");
                out.hint("run `jired pull` to bring issues in, then `jired log`");
            }
            _ => {
                match args.limit {
                    Limit::Span(span) => {
                        out.say(format!("nothing happened in the last {}", describe(span)))
                    }
                    Limit::Count(_) => out.say("nothing to show at this limit"),
                }
                out.hint(format!(
                    "{total} older entries; --limit {} to look further back",
                    args.limit.wider()
                ));
            }
        }
        return;
    }

    let blocks = blocks(events);
    // Every column is as wide as its own data and no wider, so a workspace of
    // short keys does not pay for the one long one it does not have.
    let when: Vec<String> = blocks.iter().map(|b| block_when(b, now)).collect();
    let time_width = when.iter().map(|w| w.chars().count()).max().unwrap_or(0);
    let key_width = events
        .iter()
        .map(|e| e.key.chars().count())
        .max()
        .unwrap_or(0);
    let summary_width = events
        .iter()
        .map(|e| e.summary.chars().count())
        .max()
        .unwrap_or(0)
        .min(SUMMARY_COLUMN);
    // An escape sequence is a link only where something can follow it; a file
    // gets the URL itself, which is the part a reader can still use.
    let clickable = args.links && std::io::IsTerminal::is_terminal(&std::io::stdout());

    for (i, (block, when)) in blocks.iter().zip(&when).enumerate() {
        // One blank line between blocks, the way `git log` separates commits:
        // a block is as many rows as it changed things, and without a gap a
        // three-field edit and three one-field edits read the same.
        if i > 0 {
            out.say("");
        }
        let e = &block[0];
        let key = pad(e.key, key_width);
        let key = if clickable {
            hyperlink(&browse_url(base_url, e.key), &key)
        } else {
            key
        };
        let mut row = format!(
            "{}  {}",
            style::out(DIM, pad(when, time_width)),
            // Bold as well as cyan: the key is what the eye lands on to find
            // where one entry starts, so it is the one thing painted twice.
            style::out(&format!("{BOLD}{CYAN}"), key)
        );
        if summary_width > 0 {
            row.push_str(&format!("  {}", pad(e.summary, summary_width)));
        }
        if let Some(author) = e.author {
            row.push_str(&format!("  {}", style::out(DIM, format!("({author})"))));
        }
        if args.links && !clickable {
            row.push_str(&format!(
                "  {}",
                style::out(DIM, browse_url(base_url, e.key))
            ));
        }
        out.say(row.trim_end());

        for e in block.iter() {
            match e.what {
                What::Change(items) => render_change(items, out),
                What::Comment(c) => render_body(
                    "comment",
                    // A comment edited after it was posted is not the comment
                    // that was posted, and the log dates it by when it first
                    // appeared.
                    c.updated
                        .is_some_and(|edited| c.created.is_some_and(|posted| edited > posted)),
                    None,
                    &crate::convert::to_markdown(&c.body).markdown,
                    out,
                ),
                What::Worklog(w) => render_body(
                    "worklog",
                    false,
                    Some(&w.time_spent),
                    &crate::convert::to_markdown(&w.comment).markdown,
                    out,
                ),
            }
        }
    }

    // The trailer is about the log, not about the entry it happens to follow,
    // so it gets the same gap every entry got.
    if total > events.len() || !partial.is_empty() {
        out.say("");
    }
    if total > events.len() {
        out.hint(format!(
            "{} of {total} entries; --limit {} to see more",
            events.len(),
            args.limit.wider()
        ));
    }
    for p in partial.iter().take(3) {
        out.hint(format!(
            "{}: the snapshot holds {} of {} {} — `jired fetch {}` brings the rest",
            p.key, p.held, p.total, p.what, p.key
        ));
    }
}

/// The rows under a changelog entry: one per field it touched.
fn render_change(items: &[ChangelogItem], out: Out) {
    for item in items {
        let from = item.from.as_deref().unwrap_or_default();
        let to = item.to.as_deref().unwrap_or_default();
        if let Some(prose) = prose_diff(from, to) {
            render_prose(&item.field, &prose, out);
            continue;
        }
        // A field that gained a value it did not have, or lost the one it did,
        // is a `+` or a `-` rather than an arrow out of nothing: the sign says
        // in one character what `(none) →` said in seven.
        match (from.is_empty(), to.is_empty()) {
            (true, false) => out.say(format!(
                "  {} {}",
                style::out(GREEN, format!("+{}:", item.field)),
                style::out(GREEN, to)
            )),
            (false, true) => out.say(format!(
                "  {} {}",
                style::out(RED, format!("-{}:", item.field)),
                style::out(RED, from)
            )),
            // Red out, green in: the same reading `diff` and `pull` already ask
            // for, so a changelog entry needs no second vocabulary.
            (false, false) => out.say(format!(
                "  {}: {} {} {}",
                item.field,
                style::out(RED, from),
                style::out(DIM, "→"),
                style::out(GREEN, to)
            )),
            (true, true) => out.say(format!("  {}: {}", item.field, style::out(DIM, "(none)"))),
        }
    }
}

fn render_prose(field: &str, prose: &Prose, out: Out) {
    out.say(format!(
        "  {field}: {} {}",
        style::out(GREEN, format!("+{}", prose.added)),
        style::out(RED, format!("-{}", prose.removed)),
    ));
    for line in &prose.lines {
        // Colouring the body is the difference between reading a diff and
        // counting `+` signs — the same painting `jired diff` does.
        let painted = match line.chars().next() {
            Some('+') => style::out(GREEN, line),
            Some('-') => style::out(RED, line),
            Some('@') => style::out(CYAN, line),
            _ => line.clone(),
        };
        out.say(quoted(&painted));
    }
    if prose.hidden > 0 {
        out.say(quoted(&style::out(
            DIM,
            format!("⋯ {} more lines — --json for the whole text", prose.hidden),
        )));
    }
}

/// A row of somebody else's text, under a gutter.
///
/// The gutter is what keeps an entry one block: a description with a blank
/// paragraph break in it, or a comment with one, would otherwise print an empty
/// line in the middle of an entry — and an empty line is how the log says the
/// next entry has started. It marks the text as quoted at the same time, which
/// is what it is.
fn quoted(line: &str) -> String {
    format!("    {} {line}", style::out(DIM, "│"))
        .trim_end()
        .to_string()
}

/// The rows under a comment or a worklog: what it was, then what it said.
///
/// Quoted rather than diffed, and capped: a comment is worth recognising in a
/// log, and worth reading in the issue file.
fn render_body(kind: &str, edited: bool, duration: Option<&str>, text: &str, out: Out) {
    let head = style::out(
        GREEN,
        format!("+{kind}{}:", if edited { " (edited)" } else { "" }),
    );
    let mut lines: Vec<&str> = text.lines().map(str::trim_end).collect();
    while lines.last().is_some_and(|l| l.is_empty()) {
        lines.pop();
    }
    let mut rest = lines.iter();
    let first = rest.next().copied().unwrap_or_default();
    // The duration is what a worklog *is*, so it leads; the note it was booked
    // with follows on the same row while there is one line of it.
    let opening = match duration {
        Some(d) if first.is_empty() => d.to_string(),
        Some(d) => format!("{d} — {first}"),
        None => first.to_string(),
    };
    out.say(format!("  {head} {opening}").trim_end());

    let rest: Vec<&str> = rest.copied().collect();
    for (i, line) in rest.iter().enumerate() {
        if i == MAX_BODY_LINES {
            out.say(quoted(&style::out(
                DIM,
                format!("⋯ {} more lines", rest.len() - i),
            )));
            break;
        }
        out.say(quoted(line));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::model::ChangelogEntry;

    fn at(iso: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(iso)
            .unwrap()
            .with_timezone(&Utc)
    }

    fn change(id: &str, iso: &str) -> ChangelogEntry {
        ChangelogEntry {
            id: id.to_string(),
            author: None,
            created: Some(at(iso)),
            items: vec![],
        }
    }

    #[test]
    fn a_bare_number_is_a_count_and_a_unit_makes_a_span() {
        assert_eq!(parse_limit("20"), Ok(Limit::Count(20)));
        assert_eq!(parse_limit(" 5 "), Ok(Limit::Count(5)));
        assert_eq!(parse_limit("1d"), Ok(Limit::Span(Duration::hours(24))));
        assert_eq!(parse_limit("2w"), Ok(Limit::Span(Duration::days(14))));
        assert_eq!(parse_limit("36h"), Ok(Limit::Span(Duration::hours(36))));
        assert_eq!(parse_limit("90m"), Ok(Limit::Span(Duration::minutes(90))));
        assert_eq!(parse_limit("1D"), Ok(Limit::Span(Duration::hours(24))));
        assert_eq!(parse_limit("1d 12h"), Ok(Limit::Span(Duration::hours(36))));
    }

    /// A day here is a day, not the eight hours `logwork "1d"` books.
    #[test]
    fn a_span_is_calendar_time_not_working_time() {
        assert_eq!(parse_limit("1d"), Ok(Limit::Span(Duration::hours(24))));
        assert_eq!(crate::doc::parse_duration("1d"), Some(8 * 3600));
    }

    #[test]
    fn nonsense_is_refused_rather_than_read_as_something() {
        // `0` stays a legal count: it was one before spans existed.
        for bad in ["", "banana", "1x", "1d5", "-3", "0d", "d", "1.5d"] {
            assert!(parse_limit(bad).is_err(), "{bad} should not parse");
        }
    }

    #[test]
    fn a_span_keeps_the_entries_inside_the_window() {
        let entries = [
            change("1", "2026-09-07T09:00:00Z"),
            change("2", "2026-09-01T09:00:00Z"),
        ];
        let now = at("2026-09-07T12:00:00Z");
        let build = || -> Vec<Event> {
            entries
                .iter()
                .map(|entry| Event {
                    key: "PROJ-1",
                    summary: "",
                    at: entry.created,
                    id: &entry.id,
                    author: None,
                    what: What::Change(&entry.items),
                })
                .collect()
        };

        let mut day = build();
        apply_limit(&mut day, Limit::Span(Duration::hours(24)), now);
        assert_eq!(day.len(), 1);

        let mut fortnight = build();
        apply_limit(&mut fortnight, Limit::Span(Duration::days(14)), now);
        assert_eq!(fortnight.len(), 2);
    }

    /// Nothing dates an event to inside the window, so a span leaves it out.
    #[test]
    fn a_span_drops_an_event_the_server_never_dated() {
        let entry = change("1", "2026-09-07T09:00:00Z");
        let undated = || Event {
            key: "PROJ-1",
            summary: "",
            at: None,
            id: &entry.id,
            author: None,
            what: What::Change(&entry.items),
        };
        let mut events = vec![undated()];
        apply_limit(
            &mut events,
            Limit::Span(Duration::days(3650)),
            at("2026-09-07T12:00:00Z"),
        );
        assert!(events.is_empty());

        events.push(undated());
        apply_limit(&mut events, Limit::Count(20), at("2026-09-07T12:00:00Z"));
        assert_eq!(events.len(), 1);
    }

    /// A status or a priority is exactly what the arrow form is for.
    #[test]
    fn a_short_change_stays_on_one_line() {
        assert!(prose_diff("To Do", "In Progress").is_none());
        assert!(prose_diff("", "auth backend").is_none());
        assert!(prose_diff("Medium", "High").is_none());
    }

    #[test]
    fn a_change_that_does_not_fit_the_row_becomes_a_diff() {
        let from = "The login form rejects a correct password on the second attempt.";
        let to = "The login form rejects a correct password on every attempt after the first.";
        let prose = prose_diff(from, to).expect("too long for the arrow");
        assert_eq!((prose.added, prose.removed), (1, 1));
        assert!(prose.lines.iter().any(|l| l.starts_with("@@")));
        assert!(prose.lines.contains(&format!("-{from}")));
        assert!(prose.lines.contains(&format!("+{to}")));
    }

    /// The point of the whole thing: only the edited line is red and green, and
    /// the paragraphs around it are printed once rather than twice.
    #[test]
    fn a_multi_line_change_shows_only_what_moved() {
        let from = "Steps:\n\n1. Log in\n2. Log out\n3. Log in again\n";
        let to = "Steps:\n\n1. Log in\n2. Wait five minutes\n3. Log in again\n";
        let prose = prose_diff(from, to).expect("newlines make it prose");
        assert_eq!((prose.added, prose.removed), (1, 1));
        assert!(prose.lines.contains(&"-2. Log out".to_string()));
        assert!(prose.lines.contains(&"+2. Wait five minutes".to_string()));
        assert!(prose.lines.contains(&" 3. Log in again".to_string()));
        assert_eq!(
            prose.lines.iter().filter(|l| l.starts_with('+')).count(),
            1,
            "the unchanged lines are context, not additions"
        );
    }

    /// A description written where there was none is all additions, and the
    /// empty side must not show up as a line that was removed.
    #[test]
    fn a_field_filled_in_from_nothing_is_additions_alone() {
        let prose = prose_diff("", "One line.\nAnd another.\n").expect("prose");
        assert_eq!((prose.added, prose.removed), (2, 0));
        assert!(!prose.lines.iter().any(|l| l.starts_with('-')));
    }

    /// A log is a scan over many entries, so one rewritten description is
    /// capped rather than allowed to bury the week.
    #[test]
    fn a_wholesale_rewrite_is_capped_and_says_so() {
        let from: String = (0..80).map(|i| format!("old line {i}\n")).collect();
        let to: String = (0..80).map(|i| format!("new line {i}\n")).collect();
        let prose = prose_diff(&from, &to).expect("prose");
        assert_eq!((prose.added, prose.removed), (80, 80));
        assert_eq!(prose.lines.len(), MAX_DIFF_LINES);
        assert_eq!(
            prose.hidden,
            161 - MAX_DIFF_LINES,
            "one @@ header and 160 lines"
        );
    }

    /// Nothing to show is the arrow's business: a header reading `+0 -0` over
    /// an empty body would be a change jired invented.
    #[test]
    fn a_value_that_only_gained_a_line_ending_is_not_a_diff() {
        let long = "x".repeat(ARROW_BUDGET + 1);
        assert!(prose_diff(&long, &format!("{long}\n")).is_none());
    }

    #[test]
    fn the_hint_offers_twice_what_was_asked_for() {
        assert_eq!(Limit::Count(20).wider(), "40");
        assert_eq!(Limit::Span(Duration::hours(24)).wider(), "2d");
        assert_eq!(Limit::Span(Duration::days(7)).wider(), "2w");
        assert_eq!(Limit::Span(Duration::minutes(90)).wider(), "3h");
        assert_eq!(Limit::Span(Duration::hours(36)).wider(), "3d");
        assert_eq!(Limit::Span(Duration::minutes(50)).wider(), "100m");
    }

    /// The date appears when "today" stops being the answer, and the year when
    /// "this year" does.
    ///
    /// The shape is asserted rather than the digits: the clock a reader sees is
    /// their own, so the exact numbers belong to the machine running the test.
    /// Which zone that is, `tests/e2e.rs` pins with `TZ` on a subprocess — the
    /// process env is not a thing to move under the other tests in here.
    #[test]
    fn a_timestamp_says_only_what_the_reader_does_not_already_know() {
        fn shape(s: &str) -> String {
            s.chars()
                .map(|c| if c.is_ascii_digit() { 'N' } else { c })
                .collect()
        }
        let now = at("2026-09-07T12:00:00Z");

        assert_eq!(
            shape(&format_when(at("2026-09-07T09:30:00Z"), now)),
            "NN:NN"
        );
        // Under twenty-four hours old, however the local date reads.
        assert_eq!(
            shape(&format_when(at("2026-09-06T13:00:00Z"), now)),
            "NN:NN"
        );
        assert_eq!(
            shape(&format_when(at("2026-09-06T11:00:00Z"), now)),
            "NN-NN NN:NN"
        );
        let old = format_when(at("2025-11-02T16:20:00Z"), now);
        assert_eq!(shape(&old), "NNNN-NN-NN NN:NN");
        assert!(old.starts_with("2025-"), "another year, so it is printed");
        // A server clock ahead of this one is not a reason to print a date.
        assert_eq!(
            shape(&format_when(at("2026-09-07T12:30:00Z"), now)),
            "NN:NN"
        );
    }

    #[test]
    fn a_link_wraps_the_text_and_not_the_padding() {
        let url = browse_url("https://jira.example.com/", "PROJ-1");
        assert_eq!(url, "https://jira.example.com/browse/PROJ-1");
        let cell = hyperlink(&url, &pad("PROJ-1", 10));
        assert!(cell.starts_with("\x1b]8;;https://jira.example.com/browse/PROJ-1\x1b\\"));
        assert!(cell.contains("PROJ-1    "));
    }

    /// An entry is separated from the next by a blank line, so nothing inside
    /// one may print an empty row: a blank paragraph inside a quoted comment or
    /// description would otherwise split the entry in two.
    #[test]
    fn a_quoted_line_is_never_empty_however_empty_the_text_is() {
        // Asserted without forcing the colour mode: it is process-global, and
        // moving it under the tests running beside this one is how flakes start.
        assert!(quoted("-2. Log out").ends_with("│ -2. Log out"));
        assert!(!quoted("").trim().is_empty(), "a gutter is still a mark");
        assert_eq!(quoted(" "), quoted(""), "a diff's blank context line");
        assert!(
            !quoted("text").ends_with(' '),
            "and none of it is whitespace"
        );
    }

    /// Build a timeline the way `render` sees one: newest first, already sorted.
    fn events<'a>(rows: &'a [(&'a str, Option<&'a str>, &'a str)]) -> Vec<Event<'a>> {
        rows.iter()
            .map(|(key, author, iso)| Event {
                key,
                summary: "",
                at: Some(at(iso)),
                id: "1",
                author: *author,
                what: What::Change(&[]),
            })
            .collect()
    }

    fn shapes(blocks: &[&[Event]]) -> Vec<usize> {
        blocks.iter().map(|b| b.len()).collect()
    }

    /// A transition, the comment explaining it and the time booked against it
    /// are one person's afternoon, not three afternoons.
    #[test]
    fn neighbours_by_one_person_on_one_issue_are_one_block() {
        let timeline = events(&[
            ("PROJ-1", Some("Jane D"), "2026-09-07T12:03:00Z"),
            ("PROJ-1", Some("Jane D"), "2026-09-07T12:02:00Z"),
            ("PROJ-1", Some("Jane D"), "2026-09-07T12:01:00Z"),
        ]);
        assert_eq!(shapes(&blocks(&timeline)), [3]);
    }

    /// Somebody else, or somewhere else, is a heading of its own — and a run
    /// that has been interrupted does not resume, because a block is a run of
    /// neighbours and not a gathering of everything that matches.
    #[test]
    fn a_different_hand_or_issue_starts_a_new_block() {
        let by_author = events(&[
            ("PROJ-1", Some("Jane D"), "2026-09-07T12:03:00Z"),
            ("PROJ-1", Some("Ilya A"), "2026-09-07T12:02:00Z"),
            ("PROJ-1", Some("Jane D"), "2026-09-07T12:01:00Z"),
        ]);
        assert_eq!(shapes(&blocks(&by_author)), [1, 1, 1]);

        let by_issue = events(&[
            ("PROJ-1", Some("Jane D"), "2026-09-07T12:03:00Z"),
            ("PROJ-2", Some("Jane D"), "2026-09-07T12:02:00Z"),
            ("PROJ-2", Some("Jane D"), "2026-09-07T12:01:00Z"),
        ]);
        assert_eq!(shapes(&blocks(&by_issue)), [1, 2]);

        assert!(blocks(&[]).is_empty());
    }

    /// One moment reads as one moment; several read as the span they covered,
    /// because the newest of them alone would be the heading saying the issue
    /// was touched once when it was touched three times.
    #[test]
    fn a_block_is_headed_with_the_span_it_covered() {
        let now = at("2026-09-07T12:10:00Z");
        let same_minute = events(&[
            ("PROJ-1", None, "2026-09-07T12:03:40Z"),
            ("PROJ-1", None, "2026-09-07T12:03:10Z"),
        ]);
        assert!(!block_when(&same_minute, now).contains('–'));

        let one = events(&[("PROJ-1", None, "2026-09-07T12:03:00Z")]);
        assert_eq!(
            block_when(&one, now),
            format_when(at("2026-09-07T12:03:00Z"), now)
        );
    }

    /// Both ends of the span carry the date only when they fall on different
    /// days; otherwise the second one is a clock reading under a date already
    /// printed, and the column stays as narrow as the rows that do not span.
    #[test]
    fn a_span_within_one_day_prints_its_date_once() {
        fn shape(s: &str) -> String {
            s.chars()
                .map(|c| if c.is_ascii_digit() { 'N' } else { c })
                .collect()
        }
        let now = at("2026-09-30T12:00:00Z");
        // Noon to noon UTC, so the pair falls on one local day in every zone
        // that is a whole hour from it, this test's own included.
        let within = events(&[
            ("PROJ-1", None, "2026-09-07T12:00:00Z"),
            ("PROJ-1", None, "2026-09-07T11:00:00Z"),
        ]);
        assert_eq!(shape(&block_when(&within, now)), "NN-NN NN:NN–NN:NN");

        let across = events(&[
            ("PROJ-1", None, "2026-09-07T12:00:00Z"),
            ("PROJ-1", None, "2026-09-04T12:00:00Z"),
        ]);
        assert_eq!(
            shape(&block_when(&across, now)),
            "NN-NN NN:NN–NN-NN NN:NN",
            "three days apart is not one afternoon"
        );
    }

    #[test]
    fn columns_are_measured_in_characters_not_bytes() {
        assert_eq!(pad("Пишем", 7), "Пишем  ");
        assert_eq!(pad("wide enough already", 3), "wide enough already");
    }
}
