//! `jired pull` [M1.11, M2.4] — fetch, then merge into the working copy.

use crate::cli::cmd::fetch::FetchArgs;
use crate::cli::context::Context;
use crate::cli::GlobalArgs;
use crate::error::{ExitCode, Result};
use crate::json::Out;
use clap::{Args, ValueEnum};

#[derive(ValueEnum, Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Strategy {
    /// Stop before writing anything if any conflict is detected
    #[default]
    Abort,
    /// Write conflict markers and record conflicts for `jired resolve`
    Merge,
    /// Keep the local value wherever both sides changed
    Ours,
    /// Take the remote value wherever both sides changed
    Theirs,
}

#[derive(Args, Debug, Clone, Default)]
pub struct PullArgs {
    #[command(flatten)]
    pub scope: FetchArgs,

    /// Merge the existing snapshot without contacting the server
    #[arg(long)]
    pub no_fetch: bool,

    /// How to handle fields changed on both sides
    #[arg(long, value_enum, default_value_t = Strategy::Abort)]
    pub strategy: Strategy,

    /// Overwrite local changes with remote state (equivalent to --strategy theirs)
    #[arg(long)]
    pub force: bool,
}

impl PullArgs {
    pub fn effective_strategy(&self) -> Strategy {
        if self.force {
            Strategy::Theirs
        } else {
            self.strategy
        }
    }
}

pub fn run(g: &GlobalArgs, args: &PullArgs, out: Out) -> Result<ExitCode> {
    let mut ctx = Context::open(g, out)?;
    ctx.lock()?;
    let report = crate::sync::pull::run(&mut ctx, args)?;
    report.render(out);
    out.emit("pull", &report);
    // Conflicts are a distinct, scriptable outcome rather than an error.
    Ok(if report.conflicts.is_empty() {
        ExitCode::Success
    } else {
        ExitCode::Conflict
    })
}
