//! `jired projects|boards|sprints list` [M1.13, M4.3].

use crate::cli::context::Context;
use crate::cli::GlobalArgs;
use crate::error::{ExitCode, Result};
use crate::json::Out;
use clap::Args;

// These three always query the server: there is no cached list to fall back on,
// so the `--refresh` flag they used to carry meant nothing and is gone.

#[derive(Args, Debug, Clone, Default)]
pub struct ProjectsArgs {}

#[derive(Args, Debug, Clone, Default)]
pub struct BoardsArgs {
    /// Only boards for this project (defaults to the workspace project)
    #[arg(long = "for-project", value_name = "KEY")]
    pub project: Option<String>,
}

#[derive(Args, Debug, Clone, Default)]
pub struct SprintsArgs {
    /// Board to list sprints for
    #[arg(long, value_name = "BOARD")]
    pub board: Option<String>,
    /// Include closed sprints [config: sprints.include_closed]
    #[arg(long)]
    pub all: bool,
}

pub fn run_projects(g: &GlobalArgs, _args: &ProjectsArgs, out: Out) -> Result<ExitCode> {
    let mut ctx = Context::open(g, out)?;
    let session = crate::store::session::SessionStore::open(&ctx.workspace)?;
    let account = session.require_account()?;
    let client = crate::api::build_client(&mut ctx, &account)?;
    let projects = crate::api::block_on(client.projects())?;
    if projects.is_empty() {
        out.say("this account cannot see any projects");
    }
    let width = column(projects.iter().map(|p| p.key.len()), 12);
    for p in &projects {
        out.say(format!("{:<width$} {}", p.key, p.name));
    }
    out.emit("projects", &serde_json::json!({ "projects": projects }));
    Ok(ExitCode::Success)
}

/// Width for a left-hand column: as wide as its widest value, within reason.
pub(crate) fn column(widths: impl Iterator<Item = usize>, cap: usize) -> usize {
    widths.max().unwrap_or(cap).clamp(4, cap.max(24))
}

pub fn run_boards(g: &GlobalArgs, args: &BoardsArgs, out: Out) -> Result<ExitCode> {
    let mut ctx = Context::open(g, out)?;
    let session = crate::store::session::SessionStore::open(&ctx.workspace)?;
    let account = session.require_account()?;
    let client = crate::api::build_client(&mut ctx, &account)?;
    let project = match &args.project {
        Some(p) => Some(p.clone()),
        None => ctx.resolver.get_quiet("workspace.project")?,
    };
    let boards = crate::api::block_on(client.boards(project.as_deref()))?;
    if boards.is_empty() {
        out.say(match &project {
            Some(p) => format!("no boards for project {p}"),
            None => "no boards visible to this account".to_string(),
        });
    }
    for b in &boards {
        out.say(format!("{:<8} {:<10} {}", b.id, b.board_type, b.name));
    }
    out.emit("boards", &serde_json::json!({ "boards": boards }));
    Ok(ExitCode::Success)
}

pub fn run_sprints(g: &GlobalArgs, args: &SprintsArgs, out: Out) -> Result<ExitCode> {
    let mut ctx = Context::open(g, out)?;
    let session = crate::store::session::SessionStore::open(&ctx.workspace)?;
    let account = session.require_account()?;
    let client = crate::api::build_client(&mut ctx, &account)?;
    let board = match &args.board {
        Some(b) => b.clone(),
        None => {
            return Err(crate::error::JiredError::missing_value(
                "a board is required to list sprints",
            )
            .with_detail("flag", "--board")
            .with_hint("run `jired boards` to see available boards"))
        }
    };
    let include_closed = args.all
        || ctx
            .resolver
            .get_bool("sprints.include_closed")?
            .unwrap_or(false);
    let sprints = crate::api::block_on(client.sprints(&board))?;
    let shown: Vec<_> = sprints
        .iter()
        .filter(|s| include_closed || s.state != "closed")
        .collect();
    if shown.is_empty() {
        out.say(if sprints.is_empty() {
            format!("board {board} has no sprints")
        } else {
            format!("board {board} has no open sprints")
        });
        if !sprints.is_empty() && !include_closed {
            out.hint("use --all to include closed sprints");
        }
    }
    for s in &shown {
        out.say(format!("{:<8} {:<10} {}", s.id, s.state, s.name));
    }
    out.emit("sprints", &serde_json::json!({ "sprints": sprints }));
    Ok(ExitCode::Success)
}
