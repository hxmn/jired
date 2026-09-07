//! `jired watch` — which issues this account is a watcher of.
//!
//! Jira offers watching one issue at a time, from that issue's own page, which
//! makes "am I watching the things that moved this week" a question you answer
//! by opening twenty tabs. Here it is a list of boxes: the issues the workspace
//! knows, newest first, ticked where the server says this account watches them.
//!
//! Which those are is asked of the server rather than read out of the snapshot
//! — one `watcher = currentUser()` search, which is cheap and, unlike a
//! snapshot, is true right now. Somebody else can add me as a watcher, and a
//! screen about watching that shows a week-old answer is worse than none. The
//! snapshot's own `watches.isWatching` is the fallback when that search cannot
//! be run, and it says so.
//!
//! Nothing is sent while the screen is open: ticking builds a plan, `⏎` sends
//! it, `esc` throws it away. `--all` is the same plan with every box ticked,
//! for a terminal that has no screen to draw on and for a script.

use crate::api::client::{search_all, FieldSet, SearchQuery};
use crate::cli::cmd::log::Limit;
use crate::cli::context::Context;
use crate::cli::GlobalArgs;
use crate::error::{ExitCode, JiredError, Result};
use crate::json::Out;
use crate::style::{self, DIM};
use crate::sync::progress::Progress;
use crate::sync::Engine;
use crate::tui::watch::{Changes, Row, WatchScreen};
#[cfg(test)]
use chrono::DateTime;
use chrono::Utc;
use clap::Args;
use std::collections::BTreeSet;

#[derive(Args, Debug, Clone)]
pub struct WatchArgs {
    /// Watch every issue in the workspace, without opening a screen
    ///
    /// The workspace is the scope: the issues `fetch` has brought in, which for
    /// a workspace made by `jired init` is the project. Issues already watched
    /// are not sent again.
    #[arg(long)]
    pub all: bool,

    /// How much of the workspace the screen offers: a count, or a span back
    ///
    /// `--limit 50` is the fifty most recently touched issues; `--limit 2w` is
    /// everything touched in the last fortnight, however many that is. Spelled
    /// the same way as `jired log --limit`. It shapes the screen's list only:
    /// `--all` means every issue, which is what it says.
    #[arg(long, value_name = "N|SPAN", default_value = "50", value_parser = crate::cli::cmd::log::parse_limit)]
    pub limit: Limit,
}

/// One issue's outcome, for the result document and the summary line.
#[derive(serde::Serialize)]
struct Outcome {
    key: String,
    /// `watching` or `not-watching`: what this account is now, or `failed`.
    status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

pub fn run(g: &GlobalArgs, args: &WatchArgs, out: Out) -> Result<ExitCode> {
    let mut ctx = Context::open(g, out)?;
    let engine = Engine::open(&mut ctx)?;
    let project = ctx.resolver.get_quiet("workspace.project")?;

    let mut rows = candidates(&engine)?;
    if rows.is_empty() {
        out.say("this workspace knows no issues yet");
        out.hint("run `jired pull` first, then `jired watch`");
        out.emit("watch", &report(&[], 0));
        return Ok(ExitCode::Success);
    }

    let client = engine.client(&mut ctx)?;
    let me = crate::api::block_on(client.myself())?;
    // The server's answer overrides the snapshot's, and only where it can be
    // had: an instance that refuses the search still gets a usable screen.
    match watched_now(client.as_ref(), project.as_deref(), out) {
        Ok(watched) => {
            for row in &mut rows {
                row.watching = watched.contains(&row.key);
            }
        }
        Err(e) => out.warn(format!(
            "cannot ask which issues you watch ({}) — going by the last fetch instead",
            e.message
        )),
    }

    let changes = if args.all {
        // Every box ticked, which for issues already watched is no change at
        // all: watching is a set, and re-adding is a request that buys nothing.
        Changes {
            watch: rows
                .iter()
                .filter(|r| !r.watching)
                .map(|r| r.key.clone())
                .collect(),
            unwatch: Vec::new(),
        }
    } else {
        let now = Utc::now();
        args.limit.keep(&mut rows, now, |r| r.updated);
        if !crate::tui::available() {
            return Err(
                JiredError::missing_value("no terminal to show the watch list on")
                    .with_hint("`jired watch --all` watches every issue in the workspace"),
            );
        }
        let mut screen = WatchScreen::new(project.as_deref().unwrap_or(""), rows, now);
        match crate::tui::run(&mut screen)? {
            Some(changes) => changes,
            None => {
                out.say("nothing sent");
                out.emit("watch", &report(&[], 0));
                return Ok(ExitCode::Success);
            }
        }
    };

    if changes.is_empty() {
        out.say(match args.all {
            true => "already watching every issue in this workspace",
            false => "nothing to send",
        });
        out.emit("watch", &report(&[], 0));
        return Ok(ExitCode::Success);
    }

    let outcomes = apply(client.as_ref(), &me.account_id, &changes, out);
    let failed = outcomes.iter().filter(|o| o.status == "failed").count();
    render(&outcomes, failed, out);
    out.emit("watch", &report(&outcomes, failed));

    // A partial failure is not a success, and the outcomes say which half.
    Ok(match failed {
        0 => ExitCode::Success,
        _ => ExitCode::Generic,
    })
}

/// The issues the screen may offer: everything the workspace knows, newest
/// first, with what the last fetch said about watching.
///
/// A `NEW-*` issue is left out for the reason `open` leaves it out — it has no
/// server key, and nobody can watch an issue that does not exist yet.
fn candidates(engine: &Engine) -> Result<Vec<Row>> {
    let mut rows: Vec<Row> = engine
        .state
        .issues()?
        .into_iter()
        .filter(|row| !row.key.starts_with("NEW-"))
        .map(|row| {
            let updated = row
                .remote
                .as_ref()
                .map(|r| r.updated)
                .or(row.base.as_ref().map(|b| b.updated));
            let raw = row
                .remote
                .as_ref()
                .map(|r| &r.raw)
                .or(row.base.as_ref().map(|b| &b.raw));
            Row {
                summary: raw
                    .and_then(|raw| raw.pointer("/fields/summary"))
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string(),
                watching: raw
                    .and_then(|raw| raw.pointer("/fields/watches/isWatching"))
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false),
                key: row.key,
                updated,
            }
        })
        .collect();
    rows.sort_by(|a, b| b.updated.cmp(&a.updated).then_with(|| a.key.cmp(&b.key)));
    Ok(rows)
}

/// The issues this account watches, as the server has it this second.
fn watched_now(
    client: &dyn crate::api::JiraApi,
    project: Option<&str>,
    out: Out,
) -> Result<BTreeSet<String>> {
    let mut jql = String::from("watcher = currentUser()");
    if let Some(project) = project {
        jql = format!("project = {} AND {jql}", crate::sync::jql_quote(project));
    }
    let progress = Progress::spinner(out, "asking what you watch");
    // `Summary` and not `Full`: this search is asking which keys come back, and
    // a full payload would also drag every comment page behind it.
    let issues = crate::api::block_on(search_all(
        client,
        SearchQuery {
            jql,
            fields: FieldSet::Summary,
            ..Default::default()
        },
        |_| {},
    ));
    progress.finish();
    Ok(issues?.into_iter().map(|i| i.key).collect())
}

/// Send the plan, one issue at a time, and never stop at the first refusal.
///
/// An issue somebody may not watch — a permission, an issue security level —
/// must not cost the other forty their change: the failure is recorded against
/// its own key and the rest go on.
fn apply(
    client: &dyn crate::api::JiraApi,
    user_id: &str,
    changes: &Changes,
    out: Out,
) -> Vec<Outcome> {
    let progress = Progress::bar(out, changes.len(), "sending");
    let mut outcomes = Vec::with_capacity(changes.len());
    for (key, watching) in changes
        .watch
        .iter()
        .map(|k| (k, true))
        .chain(changes.unwatch.iter().map(|k| (k, false)))
    {
        let sent = match watching {
            true => crate::api::block_on(client.watch(key, user_id)),
            false => crate::api::block_on(client.unwatch(key, user_id)),
        };
        outcomes.push(match sent {
            Ok(()) => Outcome {
                key: key.clone(),
                status: if watching { "watching" } else { "not-watching" },
                error: None,
            },
            Err(e) => Outcome {
                key: key.clone(),
                status: "failed",
                error: Some(e.message.clone()),
            },
        });
        progress.inc();
    }
    progress.finish();
    outcomes
}

fn render(outcomes: &[Outcome], failed: usize, out: Out) {
    let watching = outcomes.iter().filter(|o| o.status == "watching").count();
    let stopped = outcomes
        .iter()
        .filter(|o| o.status == "not-watching")
        .count();
    let mut said = Vec::new();
    if watching > 0 {
        said.push(format!("now watching {watching} issue(s)"));
    }
    if stopped > 0 {
        said.push(format!("stopped watching {stopped}"));
    }
    if !said.is_empty() {
        out.say(said.join(", "));
    }
    for outcome in outcomes.iter().filter(|o| o.status == "failed") {
        out.warn(format!(
            "{}: {}",
            outcome.key,
            outcome.error.as_deref().unwrap_or("could not be changed")
        ));
    }
    if failed > 0 {
        out.say(style::out(
            DIM,
            format!("  ({failed} issue(s) unchanged, listed above)"),
        ));
    }
}

fn report(outcomes: &[Outcome], failed: usize) -> serde_json::Value {
    serde_json::json!({
        "watched": outcomes.iter().filter(|o| o.status == "watching").count(),
        "unwatched": outcomes.iter().filter(|o| o.status == "not-watching").count(),
        "failed": failed,
        "issues": outcomes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(key: &str, watching: bool, at: Option<&str>) -> Row {
        Row {
            key: key.into(),
            summary: String::new(),
            updated: at.map(|iso| {
                DateTime::parse_from_rfc3339(iso)
                    .unwrap()
                    .with_timezone(&Utc)
            }),
            watching,
        }
    }

    /// `--all` is a plan like any other, and an issue already watched is not in
    /// it: watching is a set, so re-adding buys a request and nothing else.
    #[test]
    fn all_plans_only_the_issues_not_already_watched() {
        let rows = [
            row("PROJ-1", true, None),
            row("PROJ-2", false, None),
            row("PROJ-3", false, None),
        ];
        let changes = Changes {
            watch: rows
                .iter()
                .filter(|r| !r.watching)
                .map(|r| r.key.clone())
                .collect(),
            unwatch: Vec::new(),
        };
        assert_eq!(changes.watch, ["PROJ-2", "PROJ-3"]);
        assert!(changes.unwatch.is_empty());
    }

    #[test]
    fn the_screen_is_offered_the_recent_end_of_the_workspace() {
        let now = DateTime::parse_from_rfc3339("2026-09-07T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let mut rows = vec![
            row("PROJ-1", false, Some("2026-09-07T09:00:00Z")),
            row("PROJ-2", false, Some("2026-08-01T09:00:00Z")),
        ];
        Limit::Span(chrono::Duration::days(7)).keep(&mut rows, now, |r| r.updated);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].key, "PROJ-1");

        let mut rows = vec![
            row("PROJ-1", false, Some("2026-09-07T09:00:00Z")),
            row("PROJ-2", false, Some("2026-08-01T09:00:00Z")),
        ];
        Limit::Count(1).keep(&mut rows, now, |r| r.updated);
        assert_eq!(rows.len(), 1);
    }
}
