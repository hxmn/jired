//! `jired push` [M2.8, M2.9] — apply the working-copy delta to Jira.

use crate::cli::context::Context;
use crate::cli::GlobalArgs;
use crate::error::{ExitCode, Result};
use crate::json::Out;
use clap::Args;

#[derive(Args, Debug, Clone, Default)]
pub struct PushArgs {
    /// Push only these issues: keys or file paths (default: every dirty issue)
    #[arg(value_delimiter = ',')]
    pub issues: Vec<String>,

    /// Show the exact API calls and diffs without sending anything
    #[arg(long, short = 'n', visible_alias = "preview")]
    pub dry_run: bool,

    /// Push even when the server changed since the last sync
    #[arg(long)]
    pub force: bool,

    /// Allow pushing regions whose original rich-text markup cannot be preserved
    #[arg(long)]
    pub allow_lossy: bool,

    /// Allow deleting comments removed from the file
    #[arg(long)]
    pub allow_comment_delete: bool,

    /// Allow deleting worklogs removed from the file
    #[arg(long)]
    pub allow_worklog_delete: bool,

    /// Replay operations left pending by an interrupted push
    #[arg(long)]
    pub resume: bool,

    /// Re-fetch issues refused as stale and retry the ones that still merge
    ///
    /// A worklog somebody added has nothing to do with the labels you are
    /// sending; dropping the issue over it is stricter than it needs to be.
    /// An issue whose incoming change touches a field you also changed is a
    /// real conflict and is still refused.
    #[arg(long, conflicts_with = "force")]
    pub refresh_stale: bool,
}

pub fn run(g: &GlobalArgs, args: &PushArgs, out: Out) -> Result<ExitCode> {
    let mut ctx = Context::open(g, out)?;
    ctx.lock()?;

    // Push is the one command that changes somebody else's data, and it used to
    // send everything dirty with no preview and no question. Planning first
    // costs nothing — a dry run makes no calls — and turns the confirmation into
    // an accurate list rather than a count.
    if !args.dry_run && ctx.resolver.interactive() && !ctx.yes {
        let plan_args = PushArgs {
            dry_run: true,
            ..args.clone()
        };
        let plan = crate::sync::push::run(&mut ctx, &plan_args)?;
        if plan.results.is_empty() {
            plan.render(out);
            out.emit("push", &plan);
            return Ok(ExitCode::Success);
        }
        for line in plan.plan_lines(false) {
            out.say(line);
        }
        let question = format!(
            "send {} operation(s) across {} issue(s)?",
            plan.op_count(),
            plan.results.len()
        );
        if !crate::cli::cmd::prompt::confirm(&question, false, true)? {
            out.say("cancelled — nothing was sent");
            out.emit(
                "push",
                &serde_json::json!({"cancelled": true, "planned": plan}),
            );
            return Ok(ExitCode::Success);
        }
    }

    let report = crate::sync::push::run(&mut ctx, args)?;
    report.render(out);
    out.emit("push", &report);
    Ok(report.exit_code())
}
