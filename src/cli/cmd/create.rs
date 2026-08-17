//! `jired create` [M3.5] — a local NEW-* issue that becomes real on push.

use crate::cli::GlobalArgs;
use crate::error::{ExitCode, Result};
use crate::json::Out;
use clap::Args;

#[derive(Args, Debug, Clone)]
pub struct CreateArgs {
    /// Issue type (Story, Bug, Task, …)
    #[arg(long, default_value = "Task")]
    pub r#type: String,
    /// Template file under .jired/templates
    #[arg(long)]
    pub template: Option<String>,
    /// Issue summary
    #[arg(long, short = 's')]
    pub summary: Option<String>,
    /// Parent issue key for a subtask
    #[arg(long)]
    pub parent: Option<String>,
}

pub fn run(_g: &GlobalArgs, _args: &CreateArgs, _out: Out) -> Result<ExitCode> {
    Err(crate::cli::not_yet("jired create", "M3.5"))
}
