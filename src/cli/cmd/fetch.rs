//! `jired fetch` [M1.8] — refresh the remote snapshot; never touches files.

use crate::cli::context::Context;
use crate::cli::GlobalArgs;
use crate::error::{ExitCode, Result};
use crate::json::Out;
use clap::Args;

#[derive(Args, Debug, Clone, Default)]
pub struct FetchArgs {
    /// Only these issues: keys or file paths
    ///
    /// The same positional form `push`, `diff` and `resolve` take, so
    /// `jired pull PROJ-1` works like `jired push PROJ-1`.
    #[arg(value_name = "ISSUES", value_delimiter = ',')]
    pub positional_issues: Vec<String>,

    /// Restrict the fetch to issues matching this JQL
    #[arg(long, value_name = "JQL")]
    pub jql: Option<String>,

    /// Fetch issues in this sprint (name or id)
    #[arg(long, value_name = "SPRINT")]
    pub sprint: Option<String>,

    /// Fetch issues on this board (name or id)
    #[arg(long, value_name = "BOARD")]
    pub board: Option<String>,

    /// Only these issues (same as the positional form)
    #[arg(long, value_name = "KEYS", value_delimiter = ',')]
    pub issues: Vec<String>,

    /// Fetch issues updated at or after this timestamp
    #[arg(long, value_name = "TIMESTAMP")]
    pub updated_since: Option<String>,

    /// Ignore the incremental watermark and refetch everything in scope
    #[arg(long)]
    pub full: bool,

    /// Skip changelogs to keep the state database small
    #[arg(long)]
    pub slim: bool,
}

impl FetchArgs {
    /// Every issue named, from either spelling, resolved to keys.
    pub fn wanted_issues(&self) -> crate::error::Result<Vec<String>> {
        let mut all: Vec<String> = self
            .positional_issues
            .iter()
            .chain(self.issues.iter())
            .map(|r| crate::project::naming::key_from_input(r))
            .collect::<crate::error::Result<Vec<_>>>()?
            .into_iter()
            .filter(|k| !k.is_empty())
            .collect();
        all.sort();
        all.dedup();
        Ok(all)
    }
}

pub fn run(g: &GlobalArgs, args: &FetchArgs, out: Out) -> Result<ExitCode> {
    let mut ctx = Context::open(g, out)?;
    ctx.lock()?;
    let report = crate::sync::fetch::run(&mut ctx, args)?;
    report.render(out);
    out.emit("fetch", &report);
    Ok(ExitCode::Success)
}
