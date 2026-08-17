//! `jired projects|boards|sprints list` [M1.13, M4.3].

use crate::cli::context::Context;
use crate::cli::GlobalArgs;
use crate::error::{ExitCode, Result};
use crate::json::Out;
use clap::Args;

#[derive(Args, Debug, Clone, Default)]
pub struct ProjectsArgs {
    /// Query the server instead of using the cached list
    #[arg(long)]
    pub refresh: bool,
}

#[derive(Args, Debug, Clone, Default)]
pub struct BoardsArgs {
    #[arg(long)]
    pub refresh: bool,
    /// Only boards for this project
    #[arg(long, value_name = "KEY")]
    pub project: Option<String>,
}

#[derive(Args, Debug, Clone, Default)]
pub struct SprintsArgs {
    #[arg(long)]
    pub refresh: bool,
    /// Board to list sprints for
    #[arg(long, value_name = "BOARD")]
    pub board: Option<String>,
    /// Include closed sprints
    #[arg(long)]
    pub all: bool,
}

pub fn run_projects(g: &GlobalArgs, args: &ProjectsArgs, out: Out) -> Result<ExitCode> {
    let mut ctx = Context::open(g, out)?;
    let session = crate::store::session::SessionStore::open(&ctx.workspace)?;
    let account = session.require_account()?;
    let client = crate::api::build_client(&mut ctx, &account)?;
    let projects = crate::api::block_on(client.projects())?;
    for p in &projects {
        out.say(format!("{:<12} {}", p.key, p.name));
    }
    out.emit("projects", &serde_json::json!({ "projects": projects }));
    let _ = args.refresh;
    Ok(ExitCode::Success)
}

pub fn run_boards(g: &GlobalArgs, args: &BoardsArgs, out: Out) -> Result<ExitCode> {
    let mut ctx = Context::open(g, out)?;
    let session = crate::store::session::SessionStore::open(&ctx.workspace)?;
    let account = session.require_account()?;
    let client = crate::api::build_client(&mut ctx, &account)?;
    let project = match &args.project {
        Some(p) => Some(p.clone()),
        None => ctx.resolver.get_value("workspace.project")?,
    };
    let boards = crate::api::block_on(client.boards(project.as_deref()))?;
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
    let sprints = crate::api::block_on(client.sprints(&board))?;
    for s in sprints.iter().filter(|s| args.all || s.state != "closed") {
        out.say(format!("{:<8} {:<10} {}", s.id, s.state, s.name));
    }
    out.emit("sprints", &serde_json::json!({ "sprints": sprints }));
    Ok(ExitCode::Success)
}
