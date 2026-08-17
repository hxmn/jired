//! `jired diff` [M2.5] — offline comparison of the three states.

use crate::cli::context::Context;
use crate::cli::GlobalArgs;
use crate::error::{ExitCode, Result};
use crate::json::Out;
use clap::Args;

#[derive(Args, Debug, Clone, Default)]
pub struct DiffArgs {
    /// Restrict the diff to these issues
    pub issues: Vec<String>,

    /// Compare the last fetched remote state against base
    #[arg(long, conflicts_with = "all")]
    pub remote: bool,

    /// Compare the working copy against the last fetched remote state
    #[arg(long)]
    pub all: bool,

    /// Print a summary instead of full diffs
    #[arg(long)]
    pub stat: bool,

    /// Exit with code 1 when differences exist (like `git diff --exit-code`)
    #[arg(long)]
    pub exit_code: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum DiffMode {
    WorkingBase,
    BaseRemote,
    WorkingRemote,
}

impl DiffArgs {
    pub fn mode(&self) -> DiffMode {
        match (self.remote, self.all) {
            (true, _) => DiffMode::BaseRemote,
            (_, true) => DiffMode::WorkingRemote,
            _ => DiffMode::WorkingBase,
        }
    }
}

pub fn run(g: &GlobalArgs, args: &DiffArgs, out: Out) -> Result<ExitCode> {
    let mut ctx = Context::open(g, out)?;
    let report = crate::sync::diff::run(&mut ctx, args)?;
    report.render(out, args.stat);
    out.emit("diff", &report);
    Ok(if args.exit_code && !report.issues.is_empty() {
        ExitCode::Generic
    } else {
        ExitCode::Success
    })
}
