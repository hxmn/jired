//! `jired status` [M2.10] — offline overview of the workspace.

use crate::cli::context::Context;
use crate::cli::GlobalArgs;
use crate::error::{ExitCode, Result};
use crate::json::Out;
use clap::Args;

#[derive(Args, Debug, Clone, Default)]
pub struct StatusArgs {
    /// Include unchanged issues in the listing
    #[arg(long)]
    pub all: bool,
}

impl StatusArgs {
    pub fn options(&self) -> crate::sync::status::StatusOptions {
        crate::sync::status::StatusOptions { all: self.all }
    }
}

pub fn run(g: &GlobalArgs, args: &StatusArgs, out: Out) -> Result<ExitCode> {
    let mut ctx = Context::open(g, out)?;
    let mut report = crate::sync::status::run(&mut ctx, &args.options())?;
    report.stale_agent_docs = stale_agent_docs(ctx.workspace.root());
    report.render(out);
    out.emit("status", &report);
    Ok(ExitCode::Success)
}

/// Generated instructions in this workspace that another jired wrote.
///
/// Only a file that already carries the block is reported: a workspace that
/// never had one asked for that, and `status` is not the place to sell it.
fn stale_agent_docs(root: &std::path::Path) -> Vec<crate::sync::status::StaleAgentDoc> {
    use crate::cli::cmd::init_agents::DocState;

    crate::cli::cmd::init_agents::doc_states(root)
        .into_iter()
        .filter_map(|(file, state)| match state {
            DocState::Stale { generated_by } => Some(crate::sync::status::StaleAgentDoc {
                file: file.to_string(),
                generated_by,
                expected: env!("CARGO_PKG_VERSION").to_string(),
            }),
            DocState::Current | DocState::Missing => None,
        })
        .collect()
}
