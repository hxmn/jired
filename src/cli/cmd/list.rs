//! `jired projects|boards|sprints list` [M1.13, M4.3].

use crate::cli::context::Context;
use crate::cli::GlobalArgs;
use crate::error::{ExitCode, Result};
use crate::json::Out;
use crate::rules::sprint::pick_board;
use crate::style::{self, CYAN, DIM};
use clap::Args;

// These three always query the server: there is no cached list to fall back on,
// so the `--refresh` flag they used to carry meant nothing and is gone.

#[derive(Args, Debug, Clone, Default)]
pub struct ProjectsArgs {}

#[derive(Args, Debug, Clone, Default)]
pub struct BoardsArgs {
    /// Only boards for this project (defaults to the workspace project)
    // Not `project`: that id is the global `--project`, which this would
    // replace, leaving `boards --project` refused.
    #[arg(long = "for-project", id = "for_project", value_name = "KEY")]
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
    let client = ctx.client()?;
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
    let client = ctx.client()?;
    let project = match &args.project {
        Some(p) => Some(p.clone()),
        None => ctx.resolver.get_quiet("workspace.project")?,
    };
    let mut boards = crate::api::block_on(client.boards(project.as_deref()))?;
    if boards.is_empty() {
        out.say(match &project {
            Some(p) => format!("no boards for project {p}"),
            None => "no boards visible to this account".to_string(),
        });
    }
    // Data Center leaves the project out of the board listing, which makes a
    // board impossible to place from `--json` alone. It is in the board's own
    // configuration, so the ones that arrived unlabelled are asked directly —
    // a request each, and only for this command, which exists to be read.
    // Best-effort: a board that will not say stays null rather than failing a
    // listing that is otherwise correct.
    for b in boards.iter_mut().filter(|b| b.project_key.is_none()) {
        if let Ok(key) = crate::api::block_on(client.board_project(&b.id)) {
            b.project_key = key;
        }
    }
    // The project is what tells two similarly named boards apart, so it gets a
    // column of its own — and none at all when nothing knows it.
    let key_width = boards
        .iter()
        .filter_map(|b| b.project_key.as_ref().map(|k| k.len()))
        .max();
    for b in &boards {
        let project = match key_width {
            Some(w) => style::out(
                DIM,
                format!("{:<w$} ", b.project_key.as_deref().unwrap_or("")),
            ),
            None => String::new(),
        };
        out.say(format!(
            "{} {} {project}{}",
            style::out(CYAN, format!("{:<8}", b.id)),
            style::out(DIM, format!("{:<10}", b.board_type)),
            b.name
        ));
    }
    out.emit("boards", &serde_json::json!({ "boards": boards }));
    Ok(ExitCode::Success)
}

/// The sprints every board agrees on, when there is such a thing.
///
/// Several boards over one project's backlog commonly share a sprint pool — the
/// sprints even name the board that owns them rather than the board that was
/// asked — and then `--board` is a question with one answer, which is not worth
/// making anybody answer. The first board that disagrees ends it, so boards that
/// really are different cost two requests and then say so, and a board that
/// cannot be asked at all falls back to the error that names the choices.
fn shared_sprints(
    client: &dyn crate::api::JiraApi,
    boards: &[crate::api::Board],
) -> Option<Vec<crate::api::Sprint>> {
    let mut agreed: Option<Vec<crate::api::Sprint>> = None;
    for board in boards {
        let sprints = crate::api::block_on(client.sprints(&board.id)).ok()?;
        match &agreed {
            None => agreed = Some(sprints),
            Some(first) if sprint_ids(first) == sprint_ids(&sprints) => {}
            Some(_) => return None,
        }
    }
    agreed
}

fn sprint_ids(sprints: &[crate::api::Sprint]) -> Vec<&str> {
    let mut ids: Vec<&str> = sprints.iter().map(|s| s.id.as_str()).collect();
    ids.sort_unstable();
    ids
}

pub fn run_sprints(g: &GlobalArgs, args: &SprintsArgs, out: Out) -> Result<ExitCode> {
    let mut ctx = Context::open(g, out)?;
    let client = ctx.client()?;
    // An id is what the sprint endpoint takes and may name a board outside this
    // project, so it goes straight through. Anything else — a name, or nothing
    // at all — only means something against the project's own boards, and that
    // is what costs the extra request.
    let (sprints, source) = match &args.board {
        Some(b) if b.chars().all(|c| c.is_ascii_digit()) && !b.is_empty() => (
            crate::api::block_on(client.sprints(b))?,
            format!("board {b}"),
        ),
        wanted => {
            let project: Option<String> = ctx.resolver.get_quiet("workspace.project")?;
            let boards = crate::api::block_on(client.boards(project.as_deref()))?;
            match pick_board(&boards, wanted.as_deref()) {
                Ok(board) => (
                    crate::api::block_on(client.sprints(&board.id))?,
                    format!("board {}", board.id),
                ),
                // Several boards and none named. Boards over one project's
                // backlog often share a sprint pool, and then every answer to
                // "which board?" is the same answer: ask them, and only insist
                // on the flag when it would actually change what comes back.
                Err(e) if wanted.is_none() => match shared_sprints(client.as_ref(), &boards) {
                    Some(sprints) => (
                        sprints,
                        format!("the {} boards of this project", boards.len()),
                    ),
                    None => return Err(e),
                },
                Err(e) => return Err(e),
            }
        }
    };
    let include_closed = args.all
        || ctx
            .resolver
            .get_bool("sprints.include_closed")?
            .unwrap_or(false);
    let shown: Vec<_> = sprints
        .iter()
        .filter(|s| include_closed || !s.state.is_closed())
        .collect();
    if shown.is_empty() {
        out.say(if sprints.is_empty() {
            format!("no sprints on {source}")
        } else {
            format!("no open sprints on {source}")
        });
        if !sprints.is_empty() && !include_closed {
            out.hint("use --all to include closed sprints");
        }
    }
    for s in &shown {
        out.say(sprint_row(s));
    }
    out.emit("sprints", &serde_json::json!({ "sprints": sprints }));
    Ok(ExitCode::Success)
}

/// One sprint as a row: its id, its state, its name.
///
/// Shared, so a sprint reads the same wherever it appears — in the listing, in
/// the one `sprint create` just made, in the one `sprint start` just started.
/// A sprint's state is the same three-way reading as an issue's status — not
/// started, running, finished — so it gets the same three colours.
pub(crate) fn sprint_row(s: &crate::api::Sprint) -> String {
    let category = match s.state.as_str() {
        "active" => "indeterminate",
        "closed" => "done",
        "future" => "new",
        _ => "",
    };
    format!(
        "{} {} {}",
        style::out(CYAN, format!("{:<8}", s.id)),
        style::status(&format!("{:<10}", s.state), category),
        s.name
    )
}
