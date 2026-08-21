//! `jired reset` — drop local changes and go back to the last sync.

use crate::cli::cmd::prompt::confirm;
use crate::cli::context::Context;
use crate::cli::GlobalArgs;
use crate::error::{ExitCode, Result};
use crate::json::Out;
use clap::Args;

#[derive(Args, Debug, Clone, Default)]
pub struct ResetArgs {
    /// Issues to reset: keys or file paths (default: everything with local changes)
    #[arg(value_delimiter = ',')]
    pub issues: Vec<String>,

    /// List what would be dropped without touching anything
    #[arg(long, short = 'n')]
    pub dry_run: bool,

    /// Delete locally created issues too — they have no earlier state to go back to
    #[arg(long)]
    pub include_new: bool,
}

pub fn run(g: &GlobalArgs, args: &ResetArgs, out: Out) -> Result<ExitCode> {
    let mut ctx = Context::open(g, out)?;
    ctx.lock()?;

    let mut plan = crate::sync::reset::plan(&mut ctx, args)?;
    if plan.report.is_empty() {
        out.say("nothing to reset — the working copy matches the last sync");
        out.emit("reset", &plan.report);
        return Ok(ExitCode::Success);
    }

    // Listed before the question, so what is being agreed to is on screen
    // rather than summarised as a number.
    plan.report.render(out);

    if args.dry_run {
        plan.report.dry_run = true;
        out.say("dry run — nothing was changed");
        out.emit("reset", &plan.report);
        return Ok(ExitCode::Success);
    }

    // Everything left standing is `kept` or already clean: there is nothing to
    // ask about, and asking would invite a "yes" that does nothing.
    if plan.issues_affected() == 0 {
        out.say("nothing to reset");
        out.emit("reset", &plan.report);
        return Ok(ExitCode::Success);
    }

    let question = format!("drop local changes to {} issue(s)?", plan.issues_affected());
    if !confirm(&question, ctx.yes, ctx.resolver.interactive())? {
        out.say("cancelled — nothing was changed");
        out.emit(
            "reset",
            &serde_json::json!({"cancelled": true, "planned": plan.report}),
        );
        return Ok(ExitCode::Success);
    }

    let report = crate::sync::reset::apply(&mut ctx, plan)?;
    out.say(report.summary());
    out.emit("reset", &report);
    Ok(ExitCode::Success)
}
