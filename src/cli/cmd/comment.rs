//! `jired comment` [M3.2] — append a comment without hand-editing the file.

use crate::cli::GlobalArgs;
use crate::error::{ExitCode, Result};
use crate::json::Out;
use clap::Args;

#[derive(Args, Debug, Clone)]
pub struct CommentArgs {
    /// Issue to comment on
    pub issue: String,
    /// Comment body (reads stdin when omitted)
    #[arg(short = 'm', long = "message")]
    pub message: Option<String>,
    /// Send the comment immediately instead of leaving it staged
    #[arg(long)]
    pub push: bool,
}

pub fn run(_g: &GlobalArgs, _args: &CommentArgs, _out: Out) -> Result<ExitCode> {
    Err(crate::cli::not_yet("jired comment", "M3.2"))
}
