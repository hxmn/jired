//! `jired open [ISSUE-KEY]` [M3.7] — browse an issue on the server.
//!
//! With no key it is the same question `edit` and `comment` ask — which
//! issue? — answered by the same screen. What it offers is wider, though:
//! `open` sends a URL to a browser rather than touching the working copy, so
//! every issue the workspace has a snapshot of is openable, file or no file.
//! Locally created `NEW-*` issues are the exception the catalog already drops:
//! they have no server key, and `/browse/NEW-8bf7b6` is a 404.

use crate::cli::cmd::pick::{self, Listing};
use crate::cli::context::Context;
use crate::cli::GlobalArgs;
use crate::error::{ExitCode, JiredError, Result};
use crate::json::Out;
use crate::store::state::StateStore;
use clap::Args;

#[derive(Args, Debug, Clone, Default)]
pub struct OpenArgs {
    /// Issue to open: a key, or the path to its file; omit to search for one
    pub issue: Option<String>,
    /// Print the URL instead of launching a browser
    #[arg(long)]
    pub print: bool,
}

pub fn run(g: &GlobalArgs, args: &OpenArgs, out: Out) -> Result<ExitCode> {
    let mut ctx = Context::open(g, out)?;
    // Before any screen is drawn: a workspace with no server to browse is a
    // complaint the user should read, not one painted over by a picker.
    let base = ctx.resolver.require("server.url")?;
    let key = match &args.issue {
        Some(issue) => crate::project::naming::key_from_input(issue)?,
        // Nothing named: let them find it. Every issue with a snapshot is
        // offered, file or no file — a browser URL needs neither.
        None => pick::choose(&mut ctx, "Open which issue?", Listing::Known)?,
    };
    let url = format!("{}/browse/{}", base.trim_end_matches('/'), key);

    // Going to an issue is the plainest thing "viewed" can mean, so it is the
    // one command that stamps it without also changing the file.
    if let Ok(state) = StateStore::open(&ctx.workspace) {
        pick::mark_viewed(&state, &key);
    }

    // Launching a browser from a script or a JSON pipeline is never wanted.
    let launch = !args.print && !out.json && crate::cli::context::is_interactive();
    if launch {
        open::that_detached(&url).map_err(|e| {
            JiredError::generic("cannot open a browser")
                .with_source(e)
                .with_hint("use --print to get the URL instead")
        })?;
        // A picked issue got here by closing a full-screen list, so without
        // this line the command's entire visible output is the screen
        // vanishing — and which issue won is exactly what is worth confirming.
        if args.issue.is_none() {
            out.say(format!("opening {url}"));
        }
    } else if !out.json {
        // In JSON mode the URL is in the document; a bare line as well would
        // leave stdout holding two things. Routed through `Out` so `--quiet`
        // and a closed pipe are both honoured.
        out.say(&url);
    }
    out.emit(
        "open",
        &serde_json::json!({"key": key, "url": url, "launched": launch}),
    );
    Ok(ExitCode::Success)
}
