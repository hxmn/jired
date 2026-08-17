//! `jired push` [M2.8, M2.9] — apply the working-copy delta to Jira.

use crate::cli::context::Context;
use crate::cli::GlobalArgs;
use crate::error::{ExitCode, Result};
use crate::json::Out;
use clap::Args;

#[derive(Args, Debug, Clone, Default)]
pub struct PushArgs {
    /// Push only these issues (default: every dirty issue)
    pub issues: Vec<String>,

    /// Show the exact API calls and diffs without sending anything
    #[arg(long, alias = "preview")]
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

    /// Replay operations left pending by an interrupted push
    #[arg(long)]
    pub resume: bool,
}

pub fn run(g: &GlobalArgs, args: &PushArgs, out: Out) -> Result<ExitCode> {
    let mut ctx = Context::open(g, out)?;
    ctx.lock()?;
    let report = crate::sync::push::run(&mut ctx, args)?;
    report.render(out);
    out.emit("push", &report);
    Ok(report.exit_code())
}
