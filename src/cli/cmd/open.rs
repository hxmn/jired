//! `jired open ISSUE-KEY` [M3.7] — browse an issue on the server.

use crate::cli::context::Context;
use crate::cli::GlobalArgs;
use crate::error::{ExitCode, JiredError, Result};
use crate::json::Out;
use clap::Args;

#[derive(Args, Debug, Clone)]
pub struct OpenArgs {
    pub issue: String,
    /// Print the URL instead of launching a browser
    #[arg(long)]
    pub print: bool,
}

pub fn run(g: &GlobalArgs, args: &OpenArgs, out: Out) -> Result<ExitCode> {
    let mut ctx = Context::open(g, out)?;
    let base = ctx.resolver.require("server.url")?;
    let key = crate::project::naming::key_from_input(&args.issue);
    let url = format!("{}/browse/{}", base.trim_end_matches('/'), key);

    // Launching a browser from a script or a JSON pipeline is never wanted.
    let launch = !args.print && !out.json && crate::cli::context::is_interactive();
    if launch {
        open::that_detached(&url)
            .map_err(|e| JiredError::generic(format!("cannot open browser: {e}")))?;
    } else if !out.json {
        // In JSON mode the URL is in the document; a bare line as well would
        // leave stdout holding two things.
        println!("{url}");
    }
    out.emit(
        "open",
        &serde_json::json!({"key": key, "url": url, "launched": launch}),
    );
    Ok(ExitCode::Success)
}
