//! `jired transition` [M3.6] — validated workflow status change.

use crate::cli::cmd::edit::{edit_issue, push_one};
use crate::cli::context::Context;
use crate::cli::GlobalArgs;
use crate::error::{ExitCode, JiredError, Result};
use crate::json::Out;
use crate::sync::Engine;
use clap::Args;

#[derive(Args, Debug, Clone)]
pub struct TransitionArgs {
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
}

pub fn run(g: &GlobalArgs, args: &TransitionArgs, out: Out) -> Result<ExitCode> {
    let mut ctx = Context::open(g, out)?;
    let key = args.issue.to_uppercase();

    // Transitions depend on the issue's current status and the workflow, so
    // they can only come from the server.
    let engine = Engine::open(&ctx)?;
    let client = engine.client(&mut ctx)?;
    let available = crate::api::block_on(client.transitions(&key))?;

    if args.list || args.to.is_none() {
        for t in &available {
            let required = if t.required_fields.is_empty() {
                String::new()
            } else {
                format!("  (requires {})", t.required_fields.join(", "))
            };
            out.say(format!("{:<20} → {}{}", t.name, t.to_status, required));
        }
        out.emit(
            "transitions",
            &serde_json::json!({ "key": key, "transitions": available }),
        );
        if args.to.is_none() && !args.list {
            return Err(JiredError::usage("no target status given")
                .with_detail("issue", key)
                .with_hint("pass --to \"In Review\", or --list to see the options"));
        }
        return Ok(ExitCode::Success);
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
        Some(push_one(&mut ctx, &key)?)
    } else {
        None
    };

    out.say(match &pushed {
        Some(r) if r.results.iter().all(|x| x.ok) => format!("{key} → {}", target.to_status),
        Some(_) => format!("staged {key} → {}; the push failed", target.to_status),
        None => format!(
            "staged {key} → {} in {file} — run `jired push` to apply it",
            target.to_status
        ),
    });
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
