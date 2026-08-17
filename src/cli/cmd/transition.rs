//! `jired transition` [M3.6] — validated workflow status change.

use crate::cli::GlobalArgs;
use crate::error::{ExitCode, Result};
use crate::json::Out;
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
}

pub fn run(_g: &GlobalArgs, _args: &TransitionArgs, _out: Out) -> Result<ExitCode> {
    Err(crate::cli::not_yet("jired transition", "M3.6"))
}
