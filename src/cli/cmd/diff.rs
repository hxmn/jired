//! `jired diff` [M2.5] — offline comparison of the three states.

use crate::cli::context::Context;
use crate::cli::GlobalArgs;
use crate::error::{ExitCode, Result};
use crate::json::Out;
use clap::Args;

/// What the working copy is compared against.
///
/// This used to be spelled `--remote` and `--all`, but `--all` means "widen the
/// scope" on every other command; here it selected a comparison. Naming the
/// choice removes the ambiguity.
#[derive(clap::ValueEnum, Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Against {
    /// The last synced snapshot: what `push` would send
    #[default]
    Base,
    /// The last fetched server state: what `pull` would bring in
    Remote,
}

#[derive(Args, Debug, Clone, Default)]
pub struct DiffArgs {
    /// Restrict the diff to these issues: keys or file paths
    #[arg(value_delimiter = ',')]
    pub issues: Vec<String>,

    /// What to compare the working copy against
    #[arg(long, value_enum, default_value_t = Against::Base, value_name = "WHAT")]
    pub against: Against,

    /// Compare the last fetched remote state against base
    #[arg(long, conflicts_with = "all")]
    pub remote: bool,

    /// Deprecated spelling of `--against remote`
    #[arg(long, hide = true)]
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

impl DiffMode {
    /// What the header says the diff is between.
    pub fn describe(self) -> &'static str {
        match self {
            DiffMode::WorkingBase => "the working copy and the last sync",
            DiffMode::BaseRemote => "the last sync and the last fetch",
            DiffMode::WorkingRemote => "the working copy and the last fetch",
        }
    }
}

impl DiffArgs {
    pub fn mode(&self) -> DiffMode {
        match (self.remote, self.all, self.against) {
            (true, _, _) => DiffMode::BaseRemote,
            (_, true, _) | (_, _, Against::Remote) => DiffMode::WorkingRemote,
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
