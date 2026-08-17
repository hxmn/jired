//! `jired resolve` [M3.8] — clear recorded conflicts.

use crate::cli::GlobalArgs;
use crate::error::{ExitCode, Result};
use crate::json::Out;
use clap::Args;

#[derive(Args, Debug, Clone)]
pub struct ResolveArgs {
    /// Issues to resolve (default: all conflicted)
    pub issues: Vec<String>,
    /// Keep the local value
    #[arg(long, conflicts_with = "theirs")]
    pub ours: bool,
    /// Take the remote value
    #[arg(long)]
    pub theirs: bool,
    /// Resolve only this field
    #[arg(long)]
    pub field: Option<String>,
    /// List conflicts without resolving anything
    #[arg(long)]
    pub list: bool,
}

pub fn run(_g: &GlobalArgs, _args: &ResolveArgs, _out: Out) -> Result<ExitCode> {
    Err(crate::cli::not_yet("jired resolve", "M3.8"))
}
