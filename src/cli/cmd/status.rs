//! `jired status` [M2.10] — offline overview of the workspace.

use crate::cli::context::Context;
use crate::cli::GlobalArgs;
use crate::error::{ExitCode, Result};
use crate::json::Out;
use clap::Args;

#[derive(Args, Debug, Clone, Default)]
pub struct StatusArgs {
    /// Include unchanged issues in the listing
    #[arg(long)]
    pub all: bool,
}

pub fn run(g: &GlobalArgs, args: &StatusArgs, out: Out) -> Result<ExitCode> {
    let mut ctx = Context::open(g, out)?;
    let report = crate::sync::status::run(&mut ctx, args)?;
    report.render(out);
    out.emit("status", &report);
    Ok(ExitCode::Success)
}
