//! `jired log ISSUE-KEY` [M3.7] — cached changelog, no network.

use crate::cli::GlobalArgs;
use crate::error::{ExitCode, Result};
use crate::json::Out;
use clap::Args;

#[derive(Args, Debug, Clone)]
pub struct LogArgs {
    /// Issue to show history for
    pub issue: String,
    /// Show at most this many entries (newest first)
    #[arg(long, default_value_t = 20)]
    pub limit: usize,
}

pub fn run(_g: &GlobalArgs, _args: &LogArgs, _out: Out) -> Result<ExitCode> {
    Err(crate::cli::not_yet("jired log", "M3.7"))
}
