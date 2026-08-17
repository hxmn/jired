//! `jired clean` [M3.9, M4.5].

use crate::cli::GlobalArgs;
use crate::error::{ExitCode, Result};
use crate::json::Out;
use clap::Args;

#[derive(Args, Debug, Clone)]
pub struct CleanArgs {
    /// Rebuild every grouping folder from the state database
    #[arg(long)]
    pub regroup: bool,
    /// Report what would be removed without touching anything
    #[arg(long)]
    pub dry_run: bool,
    /// Remove orphaned files even when they have unpushed edits
    #[arg(long)]
    pub force: bool,
}

pub fn run(_g: &GlobalArgs, _args: &CleanArgs, _out: Out) -> Result<ExitCode> {
    Err(crate::cli::not_yet("jired clean", "M3.9"))
}
