//! `jired log [ISSUE]` [M3.7] — who changed what, and when.
//!
//! With no issue named it reads like `git log`: every edit the workspace knows
//! about, merged into one newest-first list. Name an issue and it narrows to
//! that issue's history.
//!
//! A field whose old and new values are short reads as `from → to`. One whose
//! values are prose — a description, an instance's own "Acceptance Criteria" —
//! reads as a unified diff instead: Jira hands back the whole text on both
//! sides, and a page of it does not fit through a column layout.
//!
//! Changelogs are fetched per issue and cached, rather than pulled for every
//! issue during `fetch`: they are large, rarely needed, and asking for them
//! wholesale is the difference between a fast sync and a slow one. So the first
//! run over a workspace fetches what it is missing; every run after it is
//! offline until `--refresh`.

use crate::api::model::ChangelogEntry;
use crate::cli::context::Context;
use crate::cli::GlobalArgs;
use crate::error::{ExitCode, JiredError, Result};
use crate::json::Out;
use crate::store::state::IssueRow;
use crate::style::{self, BOLD, CYAN, DIM, GREEN, RED};
use crate::sync::progress::Progress;
use crate::sync::Engine;
use chrono::{DateTime, Duration, Utc};
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

fn parse_limit(text: &str) -> std::result::Result<Limit, String> {
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

/// One changelog entry, tagged with the issue it belongs to.
///
/// The tag is what makes a merged log readable; it is emitted in single-issue
/// mode too so a consumer never has to special-case the two shapes.
#[derive(serde::Serialize)]
struct LogEntry<'a> {
    key: &'a str,
    #[serde(flatten)]
    entry: &'a ChangelogEntry,
}

pub fn run(g: &GlobalArgs, args: &LogArgs, out: Out) -> Result<ExitCode> {
    let mut ctx = Context::open(g, out)?;
    let engine = Engine::open(&mut ctx)?;

    let one = args
        .issue
        .as_deref()
        .map(crate::project::naming::key_from_input);
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

    let mut history: Vec<(String, Vec<ChangelogEntry>)> = Vec::new();
    let mut stale: Vec<(String, Option<DateTime<Utc>>)> = Vec::new();
    for row in rows {
        if args.refresh || is_stale(&engine, &row)? {
            stale.push((row.key.clone(), issue_updated(&row)));
        }
        let cached = engine.state.changelog(&row.key)?;
        history.push((row.key, cached));
    }

    if !stale.is_empty() {
        fetch_into(&mut ctx, &engine, &stale, &mut history, one.is_some(), out)?;
    }

    let mut entries: Vec<LogEntry> = history
        .iter()
        .flat_map(|(key, entries)| entries.iter().map(move |e| LogEntry { key, entry: e }))
        .collect();
    // Newest first: the last thing that happened is the thing being asked
    // about. Ties break on the entry id, which Jira hands out in order, so a
    // batch of same-second edits still reads in the order it happened.
    entries.sort_by(|a, b| {
        b.entry
            .created
            .cmp(&a.entry.created)
            .then_with(|| b.entry.id.cmp(&a.entry.id))
            .then_with(|| a.key.cmp(b.key))
    });
    let total = entries.len();
    apply_limit(&mut entries, args.limit, Utc::now());

    render(&entries, total, one.as_deref(), args, out);

    out.emit(
        "log",
        &serde_json::json!({
            "key": one,
            "scope": if one.is_some() { "issue" } else { "workspace" },
            "entries": entries,
        }),
    );
    Ok(ExitCode::Success)
}

/// Cut a newest-first list down to what `--limit` asked for.
///
/// `now` is passed in rather than read here so the window can be tested against
/// a fixed clock. An entry the server dated nothing cannot be shown to fall
/// inside a span, so a span drops it; a count still counts it.
fn apply_limit(entries: &mut Vec<LogEntry>, limit: Limit, now: DateTime<Utc>) {
    match limit {
        Limit::Count(n) => entries.truncate(n),
        Limit::Span(span) => {
            let cutoff = now - span;
            entries.retain(|e| e.entry.created.is_some_and(|c| c >= cutoff));
        }
    }
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
    history: &mut [(String, Vec<ChangelogEntry>)],
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
        if let Some(slot) = history.iter_mut().find(|(k, _)| k == key) {
            slot.1 = fetched;
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

/// Everything either value may spend on the `from → to` line before the field
/// is treated as prose.
///
/// The row already costs four spaces of indent, sixteen of field name and three
/// for the arrow, so this is what is left of a narrow terminal. It is a budget
/// rather than a list of field names on purpose: an instance's own "Acceptance
/// Criteria" is as much prose as `description` is, and jired does not know its
/// name — but it can see that what came back does not fit on a line.
const ARROW_BUDGET: usize = 56;

/// How much of one field's diff a log is willing to spend lines on.
///
/// `jired diff` shows one issue at a time and can print everything; a log is a
/// scan over many entries, and a description rewritten wholesale would bury the
/// twenty other things that happened that week.
const MAX_DIFF_LINES: usize = 30;

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

fn render_prose(field: &str, prose: &Prose, out: Out) {
    out.say(format!(
        "    {} {} {}",
        format_args!("{:<16}", field),
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
        out.say(format!("      {painted}"));
    }
    if prose.hidden > 0 {
        out.say(format!(
            "      {}",
            style::out(
                DIM,
                format!("… {} more lines — --json for the whole text", prose.hidden)
            )
        ));
    }
}

fn render(entries: &[LogEntry], total: usize, one: Option<&str>, args: &LogArgs, out: Out) {
    if entries.is_empty() {
        // History that exists but sits outside the window is a different answer
        // from no history at all, and saying the second when the first is true
        // reads as jired having lost it.
        match (total, one) {
            (0, Some(key)) => out.say(format!("{key} has no recorded history")),
            (0, None) => {
                out.say("no recorded history in this workspace");
                out.hint("run `jired pull` to bring issues in, then `jired log`");
            }
            _ => {
                match args.limit {
                    Limit::Span(span) => {
                        out.say(format!("nothing changed in the last {}", describe(span)))
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

    // Only a merged log needs the key column, and only as wide as it has to be.
    let width = if one.is_some() {
        0
    } else {
        entries.iter().map(|e| e.key.len()).max().unwrap_or(0)
    };

    for e in entries {
        let when = e
            .entry
            .created
            .as_ref()
            .map(crate::api::model::format_local)
            .unwrap_or_default();
        let who = e.entry.author.as_deref().unwrap_or("unknown");
        // Padded before painting: escape codes count towards a format width and
        // would knock every following column out of line.
        let head = format!("{}  {}", style::out(DIM, when), style::out(BOLD, who));
        if width > 0 {
            out.say(format!(
                "{}  {head}",
                style::out(CYAN, format!("{:<width$}", e.key, width = width))
            ));
        } else {
            out.say(head);
        }
        for item in &e.entry.items {
            let from = item.from.as_deref().unwrap_or_default();
            let to = item.to.as_deref().unwrap_or_default();
            if let Some(prose) = prose_diff(from, to) {
                render_prose(&item.field, &prose, out);
                continue;
            }
            // Red out, green in: the same reading `diff` and `pull` already ask
            // for, so a changelog entry needs no second vocabulary.
            out.say(format!(
                "    {} {} {} {}",
                format_args!("{:<16}", item.field),
                style::out(RED, item.from.as_deref().unwrap_or("(none)")),
                style::out(DIM, "→"),
                style::out(GREEN, item.to.as_deref().unwrap_or("(none)"))
            ));
        }
    }

    if total > entries.len() {
        out.hint(format!(
            "{} of {total} entries; --limit {} to see more",
            entries.len(),
            args.limit.wider()
        ));
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
        let entries: Vec<ChangelogEntry> = ["2026-09-07T09:00:00Z", "2026-09-01T09:00:00Z"]
            .iter()
            .map(|iso| ChangelogEntry {
                id: iso.to_string(),
                author: None,
                created: Some(at(iso)),
                items: vec![],
            })
            .collect();
        let now = at("2026-09-07T12:00:00Z");

        let mut day: Vec<LogEntry> = entries
            .iter()
            .map(|entry| LogEntry {
                key: "PROJ-1",
                entry,
            })
            .collect();
        apply_limit(&mut day, Limit::Span(Duration::hours(24)), now);
        assert_eq!(day.len(), 1);

        let mut fortnight: Vec<LogEntry> = entries
            .iter()
            .map(|entry| LogEntry {
                key: "PROJ-1",
                entry,
            })
            .collect();
        apply_limit(&mut fortnight, Limit::Span(Duration::days(14)), now);
        assert_eq!(fortnight.len(), 2);
    }

    /// Nothing dates an entry to inside the window, so a span leaves it out.
    #[test]
    fn a_span_drops_an_entry_the_server_never_dated() {
        let entry = ChangelogEntry {
            id: "1".into(),
            author: None,
            created: None,
            items: vec![],
        };
        let mut entries = vec![LogEntry {
            key: "PROJ-1",
            entry: &entry,
        }];
        apply_limit(
            &mut entries,
            Limit::Span(Duration::days(3650)),
            at("2026-09-07T12:00:00Z"),
        );
        assert!(entries.is_empty());
        entries.push(LogEntry {
            key: "PROJ-1",
            entry: &entry,
        });
        apply_limit(&mut entries, Limit::Count(20), at("2026-09-07T12:00:00Z"));
        assert_eq!(entries.len(), 1);
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
}
