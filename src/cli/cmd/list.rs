//! `jired projects|boards|sprints list` [M1.13, M4.3].

use crate::cli::context::Context;
use crate::cli::GlobalArgs;
use crate::error::{ExitCode, Result};
use crate::json::Out;
use crate::style::{self, CYAN, DIM};
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
        // Padded before painting: escape codes count towards a format width
        // and would knock every following column out of line.
        out.say(format!(
            "{} {}",
            style::out(CYAN, format!("{:<width$}", p.key)),
            p.name
        ));
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
        out.say(format!(
            "{} {} {}",
            style::out(CYAN, format!("{:<8}", b.id)),
            style::out(DIM, format!("{:<10}", b.board_type)),
            b.name
        ));
    }
    out.emit("boards", &serde_json::json!({ "boards": boards }));
    Ok(ExitCode::Success)
}

/// The board a `--board` value names, out of the ones a project has.
///
/// Matching is by id first and then by name, because an id is what the API
/// wants and a name is what a person has. With nothing named, a project with
/// one board needs no flag — there is no choice to make.
pub(crate) fn pick_board<'a>(
    boards: &'a [crate::api::Board],
    wanted: Option<&str>,
) -> Result<&'a crate::api::Board> {
    let Some(wanted) = wanted.map(str::trim).filter(|w| !w.is_empty()) else {
        return match boards {
            [only] => Ok(only),
            [] => Err(
                crate::error::JiredError::missing_value("this project has no boards")
                    .with_hint("run `jired boards` to see what this account can address"),
            ),
            many => Err(
                crate::error::JiredError::missing_value("a board is required")
                    .with_detail("flag", "--board")
                    .with_hint(format!("one of: {}", board_names(many))),
            ),
        };
    };
    if let Some(by_id) = boards.iter().find(|b| b.id == wanted) {
        return Ok(by_id);
    }
    let mut named = boards
        .iter()
        .filter(|b| b.name.eq_ignore_ascii_case(wanted));
    let Some(first) = named.next() else {
        return Err(
            crate::error::JiredError::usage(format!("no board called `{wanted}`"))
                .with_detail("board", wanted.to_string())
                .with_hint(format!("one of: {}", board_names(boards))),
        );
    };
    match named.next() {
        None => Ok(first),
        // Two boards with one name: an id is the only thing that tells them
        // apart, and guessing would create the sprint on the wrong one.
        Some(_) => Err(crate::error::JiredError::usage(format!(
            "several boards are called `{wanted}`"
        ))
        .with_detail("board", wanted.to_string())
        .with_hint("name the one you mean by its id — `jired boards` lists them")),
    }
}

/// Boards by name, for an error that has to say what the choices are.
pub(crate) fn board_names(boards: &[crate::api::Board]) -> String {
    boards
        .iter()
        .map(|b| format!("{} ({})", b.name, b.id))
        .collect::<Vec<_>>()
        .join(", ")
}

pub fn run_sprints(g: &GlobalArgs, args: &SprintsArgs, out: Out) -> Result<ExitCode> {
    let mut ctx = Context::open(g, out)?;
    let session = crate::store::session::SessionStore::open(&ctx.workspace)?;
    let account = session.require_account()?;
    let client = crate::api::build_client(&mut ctx, &account)?;
    // An id is what the sprint endpoint takes and may name a board outside this
    // project, so it goes straight through. Anything else — a name, or nothing
    // at all — only means something against the project's own boards, and that
    // is what costs the extra request.
    let board = match &args.board {
        Some(b) if b.chars().all(|c| c.is_ascii_digit()) && !b.is_empty() => b.clone(),
        wanted => {
            let project: Option<String> = ctx.resolver.get_quiet("workspace.project")?;
            let boards = crate::api::block_on(client.boards(project.as_deref()))?;
            pick_board(&boards, wanted.as_deref())?.id.clone()
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
        // A sprint's state is the same three-way reading as an issue's status —
        // not started, running, finished — so it gets the same three colours.
        let state = match s.state.as_str() {
            "active" => "indeterminate",
            "closed" => "done",
            "future" => "new",
            _ => "",
        };
        out.say(format!(
            "{} {} {}",
            style::out(CYAN, format!("{:<8}", s.id)),
            style::status(&format!("{:<10}", s.state), state),
            s.name
        ));
    }
    out.emit("sprints", &serde_json::json!({ "sprints": sprints }));
    Ok(ExitCode::Success)
}
