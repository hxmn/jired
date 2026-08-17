//! `jired assign` [M3.6].

use crate::cli::GlobalArgs;
use crate::error::{ExitCode, Result};
use crate::json::Out;
use clap::Args;

#[derive(Args, Debug, Clone)]
pub struct AssignArgs {
    pub issue: String,
    /// Assign to the authenticated account
    #[arg(long, conflicts_with = "user")]
    pub me: bool,
    /// Assign to a user (name, email, or account id)
    #[arg(long)]
    pub user: Option<String>,
    /// Remove the current assignee
    #[arg(long, conflicts_with_all = ["me", "user"])]
    pub none: bool,
}

pub fn run(_g: &GlobalArgs, _args: &AssignArgs, _out: Out) -> Result<ExitCode> {
    Err(crate::cli::not_yet("jired assign", "M3.6"))
}
