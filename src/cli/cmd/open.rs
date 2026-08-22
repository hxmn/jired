//! `jired open [ISSUE-KEY]` [M3.7] — browse an issue on the server.
//!
//! With no key it is the same question `edit` asks — which issue? — answered
//! by the same filtered list. What it offers is wider, though: `open` sends a
//! URL to a browser rather than touching the working copy, so every issue the
//! workspace has a snapshot of is openable, file or no file.

use crate::cli::context::Context;
use crate::cli::GlobalArgs;
use crate::error::{ExitCode, JiredError, Result};
use crate::json::Out;
use crate::store::state::StateStore;
use crate::tui::catalog::Catalog;
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
        Some(issue) => crate::project::naming::key_from_input(issue),
        None => search(&mut ctx)?,
    };
    let url = format!("{}/browse/{}", base.trim_end_matches('/'), key);

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

/// Ask which issue, listing everything the workspace has pulled.
///
/// The state store is read directly rather than through [`crate::sync::Engine`]:
/// nothing here talks to Jira, and requiring a stored account to draw a list of
/// keys would turn a browser shortcut into a login.
///
/// Locally created `NEW-*` issues are absent — the catalog drops them — because
/// they have no server key, and `/browse/NEW-8bf7b6` is a 404 with the user's
/// name on it.
fn search(ctx: &mut Context) -> Result<String> {
    if !crate::tui::available() {
        return Err(
            JiredError::missing_value("no issue given, and no terminal to search on")
                .with_hint("name the issue: `jired open PROJ-142`, or a file name"),
        );
    }

    let state = StateStore::open(&ctx.workspace)?;
    let catalog = Catalog::from_rows(
        &state.issues()?,
        ctx.resolver.get_quiet("workspace.project")?.as_deref(),
    );

    let mut screen = crate::tui::pick::PickScreen::new("Open which issue?", &catalog.issues);
    if screen.is_empty() {
        return Err(JiredError::usage("this workspace knows no issues yet")
            .with_hint("run `jired pull` first, or name the issue: `jired open PROJ-142`"));
    }
    crate::tui::run(&mut screen)?
        .ok_or_else(|| JiredError::usage("no issue chosen").with_hint("nothing was opened"))
}
