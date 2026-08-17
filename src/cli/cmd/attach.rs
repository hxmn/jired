//! `jired attach` and `jired attachments …` [M3.3].

use crate::cli::GlobalArgs;
use crate::error::{ExitCode, Result};
use crate::json::Out;
use clap::{Args, Subcommand};
use std::path::PathBuf;

#[derive(Args, Debug, Clone)]
pub struct AttachArgs {
    pub issue: String,
    /// Files to stage for upload
    #[arg(required = true)]
    pub files: Vec<PathBuf>,
}

#[derive(Subcommand, Debug)]
pub enum AttachmentsCmd {
    /// Download attachment contents, replacing local stubs
    Pull(AttachmentsPullArgs),
    /// List known attachments
    List(AttachmentsListArgs),
}

#[derive(Args, Debug, Clone)]
pub struct AttachmentsPullArgs {
    /// Issues to download for (default: all)
    pub issues: Vec<String>,
    /// Skip attachments larger than this many bytes
    #[arg(long)]
    pub max_size: Option<u64>,
}

#[derive(Args, Debug, Clone)]
pub struct AttachmentsListArgs {
    pub issues: Vec<String>,
}

pub fn run(_g: &GlobalArgs, _args: &AttachArgs, _out: Out) -> Result<ExitCode> {
    Err(crate::cli::not_yet("jired attach", "M3.3"))
}

pub fn run_attachments(_g: &GlobalArgs, _cmd: &AttachmentsCmd, _out: Out) -> Result<ExitCode> {
    Err(crate::cli::not_yet("jired attachments", "M3.3"))
}
