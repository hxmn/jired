//! `jired transition` [M3.6] — validated workflow status change.

use crate::cli::cmd::edit::{edit_issue, push_one, report_push};
use crate::cli::context::Context;
use crate::cli::GlobalArgs;
use crate::error::{ErrorKind, ExitCode, JiredError, Result};
use crate::json::Out;
use crate::sync::Engine;
use clap::Args;

#[derive(Args, Debug, Clone)]
pub struct TransitionArgs {
    /// Issue to transition: a key, or the path to its file
    pub issue: String,
    /// Target status name
    #[arg(long)]
    pub to: Option<String>,
    /// List the transitions available from the current status
    #[arg(long)]
    pub list: bool,
    /// Fields required by the transition screen, as key=value
    #[arg(long = "field", value_name = "KEY=VALUE")]
    pub fields: Vec<String>,
    /// Apply immediately instead of leaving the change staged
    #[arg(long)]
    pub push: bool,
    /// With --push, show the API call without sending it
    #[arg(long, requires = "push")]
    pub dry_run: bool,
}

pub fn run(g: &GlobalArgs, args: &TransitionArgs, out: Out) -> Result<ExitCode> {
    let mut ctx = Context::open(g, out)?;
    let key = crate::project::naming::key_from_input(&args.issue);

    // Transitions depend on the issue's current status and the workflow, so the
    // server is the authority — but staging an edit is otherwise an offline
    // operation, and a plane should not stop someone moving a ticket.
    let engine = Engine::open(&mut ctx)?;
    let available = match engine
        .client(&mut ctx)
        .and_then(|c| crate::api::block_on(c.transitions(&key)))
    {
        Ok(list) => {
            engine.state.cache_transitions(&key, &list)?;
            list
        }
        Err(e) if e.kind == ErrorKind::Network => match engine.state.cached_transitions(&key)? {
            Some((list, at)) => {
                out.warn(format!(
                    "the server is unreachable; using the workflow cached at {at}"
                ));
                list
            }
            None => return Err(e),
        },
        Err(e) => return Err(e),
    };

    if args.list {
        render_list(&available, out);
        out.emit(
            "transitions",
            &serde_json::json!({ "key": key, "transitions": available }),
        );
        return Ok(ExitCode::Success);
    }

    if args.to.is_none() {
        // Only one document may reach stdout, so the options go to stderr as
        // part of the error rather than as a second result document.
        for t in &available {
            out.warn(format!("available: {} → {}", t.name, t.to_status));
        }
        return Err(JiredError::usage("no target status given")
            .with_detail("issue", key)
            .with_detail(
                "available",
                serde_json::json!(available.iter().map(|t| &t.to_status).collect::<Vec<_>>()),
            )
            .with_hint("pass --to \"In Review\", or --list to see the options"));
    }

    let to = args.to.as_ref().expect("checked above");
    let target = available
        .iter()
        .find(|t| t.to_status.eq_ignore_ascii_case(to))
        .or_else(|| available.iter().find(|t| t.name.eq_ignore_ascii_case(to)))
        .ok_or_else(|| {
            JiredError::usage(format!("no transition from here leads to `{to}`"))
                .with_detail("issue", key.clone())
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
                .with_detail("issue", key.clone())
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

    ctx.lock()?;
    let to_status = target.to_status.clone();
    let file = edit_issue(&mut ctx, &key, |doc| {
        doc.frontmatter.status = to_status.clone();
        Ok(())
    })?;

    let pushed = if args.push {
        let report = push_one(&mut ctx, &key, args.dry_run)?;
        report_push(&report, out);
        Some(report)
    } else {
        None
    };

    match &pushed {
        Some(r) if r.dry_run => {}
        Some(r) if r.results.iter().all(|x| x.ok) => {
            out.say(format!("{key} → {}", target.to_status))
        }
        Some(_) => out.say(format!(
            "{key} → {} is staged in {file}, but the push failed — \
             fix the cause and run `jired push`",
            target.to_status
        )),
        None => out.say(format!(
            "staged {key} → {} in {file} — run `jired push` to apply it",
            target.to_status
        )),
    }
    out.emit(
        "transition",
        &serde_json::json!({
            "key": key,
            "to": target.to_status,
            "transition_id": target.id,
            "file": file,
            "pushed": pushed.is_some(),
            "push": pushed,
        }),
    );
    Ok(match pushed {
        Some(report) => report.exit_code(),
        None => ExitCode::Success,
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
        out.say(format!("{:<width$} → {}{}", t.name, t.to_status, required));
    }
}
