//! `jired log [ISSUE]` [M3.7] — who changed what, and when.
//!
//! With no issue named it reads like `git log`: every edit the workspace knows
//! about, merged into one newest-first list. Name an issue and it narrows to
//! that issue's history.
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
use chrono::{DateTime, Utc};
use clap::Args;

#[derive(Args, Debug, Clone)]
pub struct LogArgs {
    /// Issue to show history for: a key, or the path to its file
    ///
    /// Omitted, every issue in the workspace is shown, newest edit first.
    pub issue: Option<String>,
    /// Show at most this many entries (newest first)
    #[arg(long, default_value_t = 20)]
    pub limit: usize,
    /// Ignore the cached copy and ask the server
    #[arg(long)]
    pub refresh: bool,
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
    entries.truncate(args.limit);

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

fn render(entries: &[LogEntry], total: usize, one: Option<&str>, args: &LogArgs, out: Out) {
    if entries.is_empty() {
        match one {
            Some(key) => out.say(format!("{key} has no recorded history")),
            None => {
                out.say("no recorded history in this workspace");
                out.hint("run `jired pull` to bring issues in, then `jired log`");
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
            args.limit * 2
        ));
    }
}
