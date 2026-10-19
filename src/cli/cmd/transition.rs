//! `jired transition` [M3.6] — validated workflow status change.
//!
//! Takes several issues for the same reason `label` and `set` do: closing out
//! a sprint is a dozen tickets moving to one status, and `jired list --keys`
//! exists to be piped into exactly this. Each issue is validated against its
//! own workflow position — the same `--to` can be a different transition id,
//! or no transition at all, per issue — and one that cannot move does not stop
//! the ones that can.

use crate::cli::cmd::edit::{edit_issue, push_these, report_push};
use crate::cli::context::Context;
use crate::cli::GlobalArgs;
use crate::error::{ErrorKind, ExitCode, JiredError, Result};
use crate::json::Out;
use crate::style::{self, CYAN, DIM};
use crate::sync::Engine;
use clap::Args;

#[derive(Args, Debug, Clone)]
pub struct TransitionArgs {
    /// Issues to transition: keys, or paths to their files
    #[arg(value_name = "ISSUES", required = true, value_delimiter = ',')]
    pub issues: Vec<String>,
    /// Target status name
    #[arg(long)]
    pub to: Option<String>,
    /// List the transitions available from the current status
    #[arg(long)]
    pub list: bool,
    /// Fields required by the transition screen, as key=value
    // An id of its own: clap names an argument after its field, and `fields`
    // is the global `--fields` too — whose definition this one replaced, so
    // `transition --fields` was refused while every other command took it.
    #[arg(long = "field", id = "transition_field", value_name = "KEY=VALUE")]
    pub fields: Vec<String>,
    /// Apply immediately instead of leaving the change staged
    #[arg(long)]
    pub push: bool,
    /// With --push, show the API call without sending it
    #[arg(long, requires = "push")]
    pub dry_run: bool,
}

/// One issue's staged status change: what `stage::Staged` records, plus the
/// transition that was matched — its id differs per issue even under one `--to`.
#[derive(Debug, Clone, serde::Serialize)]
struct StagedIssue {
    key: String,
    file: String,
    /// False when the issue was already in the target status.
    changed: bool,
    /// The status as the workflow spells it, whatever case `--to` came in.
    to: String,
    transition_id: String,
}

pub fn run(g: &GlobalArgs, args: &TransitionArgs, out: Out) -> Result<ExitCode> {
    let mut ctx = Context::open(g, out)?;
    let engine = Engine::open(&mut ctx)?;
    let keys: Vec<String> = args
        .issues
        .iter()
        .map(|k| crate::project::naming::key_from_input(k))
        .collect::<crate::error::Result<_>>()?;

    if args.list {
        // The listing is one issue's position in its workflow; a combined list
        // for several would name transitions that exist for none of them.
        let [key] = &keys[..] else {
            return Err(JiredError::usage("--list reads one issue at a time")
                .with_hint("run `jired transition <issue> --list` for each"));
        };
        let available = available_transitions(&mut ctx, &engine, key, out)?;
        render_list(&available, out);
        out.emit(
            "transitions",
            &serde_json::json!({ "key": key, "transitions": available }),
        );
        return Ok(ExitCode::Success);
    }

    let Some(to) = args.to.as_deref() else {
        // Only one document may reach stdout, so the options go to stderr as
        // part of the error rather than as a second result document. The first
        // issue's options: with no target named there is nothing to loop over.
        let available = available_transitions(&mut ctx, &engine, &keys[0], out)?;
        for t in &available {
            out.warn(format!("available: {} → {}", t.name, t.to_status));
        }
        return Err(JiredError::usage("no target status given")
            .with_detail("issue", keys[0].clone())
            .with_detail(
                "available",
                serde_json::json!(available.iter().map(|t| &t.to_status).collect::<Vec<_>>()),
            )
            .with_hint("pass --to \"In Review\", or --list to see the options"));
    };

    ctx.lock()?;
    let mut staged: Vec<StagedIssue> = Vec::new();
    let mut first_error: Option<JiredError> = None;
    for key in &keys {
        match stage_one(&mut ctx, &engine, key, to, args, out) {
            Ok(row) => staged.push(row),
            // One issue that cannot move must not stop the others — the
            // promise `push` makes — but with a single issue the full error is
            // better than a warning followed by a restatement of it.
            Err(e) if keys.len() == 1 => return Err(e),
            Err(e) => {
                out.warn(format!("{key}: {}", e.message));
                first_error.get_or_insert(e);
            }
        }
    }
    if staged.is_empty() {
        return Err(first_error.expect("no issue staged means at least one failed"));
    }

    let changed: Vec<&StagedIssue> = staged.iter().filter(|s| s.changed).collect();
    let pushed = if args.push && !changed.is_empty() {
        let keys: Vec<String> = changed.iter().map(|s| s.key.clone()).collect();
        let report = push_these(&mut ctx, &keys, args.dry_run)?;
        report_push(&report, out);
        Some(report)
    } else {
        None
    };

    for s in &staged {
        if !s.changed {
            out.say(format!(
                "{} is already in {}",
                style::out(CYAN, &s.key),
                s.to
            ));
        }
    }
    for s in &changed {
        match &pushed {
            Some(r) if r.dry_run => {}
            Some(r) if r.results.iter().any(|x| x.key == s.key && x.ok) => {
                out.say(format!("{} → {}", s.key, s.to));
            }
            Some(_) => out.say(format!(
                "{} → {} is staged in {}, but the push failed — \
                 fix the cause and run `jired push`",
                s.key, s.to, s.file
            )),
            None => out.say(format!(
                "staged {} → {} in {} — run `jired push` to apply it",
                s.key, s.to, s.file
            )),
        }
    }

    out.emit(
        "transition",
        &serde_json::json!({
            "issues": staged,
            "to": to,
            "pushed": pushed.is_some(),
            "push": pushed,
        }),
    );
    Ok(match pushed {
        Some(report) => report.exit_code(),
        None => ExitCode::Success,
    })
}

/// The transitions the workflow offers this issue right now.
///
/// Transitions depend on the issue's current status and the workflow, so the
/// server is the authority — but staging an edit is otherwise an offline
/// operation, and a plane should not stop someone moving a ticket.
fn available_transitions(
    ctx: &mut Context,
    engine: &Engine,
    key: &str,
    out: Out,
) -> Result<Vec<crate::api::Transition>> {
    match engine
        .client(ctx)
        .and_then(|c| crate::api::block_on(c.transitions(key)))
    {
        Ok(list) => {
            engine.state.cache_transitions(key, &list)?;
            Ok(list)
        }
        Err(e) if e.kind == ErrorKind::Network => match engine.state.cached_transitions(key)? {
            Some((list, at)) => {
                out.warn(format!(
                    "the server is unreachable; using the workflow cached at {at}"
                ));
                Ok(list)
            }
            None => Err(e),
        },
        Err(e) => Err(e),
    }
}

/// Validate `to` against one issue's workflow and stage the status edit.
fn stage_one(
    ctx: &mut Context,
    engine: &Engine,
    key: &str,
    to: &str,
    args: &TransitionArgs,
    out: Out,
) -> Result<StagedIssue> {
    let available = available_transitions(ctx, engine, key, out)?;
    let target = available
        .iter()
        .find(|t| t.to_status.eq_ignore_ascii_case(to))
        .or_else(|| available.iter().find(|t| t.name.eq_ignore_ascii_case(to)))
        .ok_or_else(|| {
            JiredError::usage(format!("no transition from here leads to `{to}`"))
                .with_detail("issue", key.to_string())
                .with_detail(
                    "available",
                    serde_json::json!(available.iter().map(|t| &t.to_status).collect::<Vec<_>>()),
                )
                .with_hint("run `jired transition <issue> --list` to see the options")
        })?;

    // Surfacing this before anything is written is the point of validating
    // here rather than letting push discover it.
    if !target.required_fields.is_empty() && args.fields.is_empty() {
        return Err(
            JiredError::usage(format!("the transition to `{to}` requires fields"))
                .with_detail("issue", key.to_string())
                .with_detail("required", serde_json::json!(target.required_fields))
                .with_hint(format!(
                    "pass --field {}=<value>",
                    target
                        .required_fields
                        .first()
                        .map(String::as_str)
                        .unwrap_or("name")
                )),
        );
    }

    let to_status = target.to_status.clone();
    let mut changed = false;
    let value = to_status.clone();
    let file = edit_issue(ctx, key, |doc| {
        changed = doc.frontmatter.status != value;
        doc.frontmatter.status = value.clone();
        Ok(())
    })?;
    Ok(StagedIssue {
        key: key.to_string(),
        file,
        changed,
        to: to_status,
        transition_id: target.id.clone(),
    })
}

fn render_list(available: &[crate::api::Transition], out: Out) {
    if available.is_empty() {
        out.say("no transitions are available from this status");
        return;
    }
    let width = available.iter().map(|t| t.name.len()).max().unwrap_or(20);
    for t in available {
        let required = if t.required_fields.is_empty() {
            String::new()
        } else {
            format!("  (requires {})", t.required_fields.join(", "))
        };
        // Padded before painting: escape codes count towards a format width.
        out.say(format!(
            "{} {} {}{}",
            style::out(CYAN, format!("{:<width$}", t.name)),
            style::out(DIM, "→"),
            t.to_status,
            style::out(DIM, required)
        ));
    }
}
