//! `jired sprint` — make a sprint, and move issues in and out of one.
//!
//! `create` is the odd one out among the writing commands: a sprint has no file
//! to stage it in, so it goes straight to the server rather than through
//! `push`, and `-n` prints the call it would make instead — the same escape
//! hatch `push` offers, for the same reason. Everything can come from the
//! command line, and with a name and a board in hand nothing is asked. Without
//! them there are three rungs, in the order the terminal can support them: the
//! full-screen form, line prompts, and exit 7 naming what is missing.
//!
//! `add` and `remove` are the opposite: an issue's sprint *is* a file field, so
//! they behave like `transition` and `assign` — write the frontmatter, let it
//! show up in `status` and `diff`, and go out through the normal `push`, or
//! immediately with `--push`. What they save is knowing where `sprint:` goes in
//! the key order and that its last entry is the one a move can act on.

use crate::api::{Board, JiraApi, Sprint, SprintInput};
use crate::cli::cmd::edit::{edit_issue, push_these, report_push};
use crate::cli::cmd::list::{board_names, pick_board};
use crate::cli::cmd::prompt::Ui;
use crate::cli::context::Context;
use crate::cli::GlobalArgs;
use crate::error::{ExitCode, JiredError, Result};
use crate::json::Out;
use crate::store::state::SprintRow;
use crate::style::{self, CYAN};
use crate::sync::push::PushReport;
use crate::tui::sprint::{BoardSprint, KnownBoard, NewSprint, Seed, SprintScreen};
use chrono::{DateTime, Utc};
use clap::{Args, Subcommand};

#[derive(Subcommand, Debug)]
pub enum SprintCmd {
    /// Create a sprint on a board
    Create(CreateArgs),
    /// Move issues into a sprint
    Add(AddArgs),
    /// Move issues out of every sprint, back to the backlog
    Remove(RemoveArgs),
}

#[derive(Args, Debug, Clone, Default)]
pub struct AddArgs {
    /// Issues to move: keys, or paths to their files
    #[arg(value_name = "ISSUES", required = true, value_delimiter = ',')]
    pub issues: Vec<String>,

    /// Sprint to move them into: a name, or an id
    #[arg(long, value_name = "SPRINT", required = true)]
    pub sprint: String,

    /// Apply immediately instead of leaving the change staged
    #[arg(long)]
    pub push: bool,

    /// With --push, show the API call without sending it
    #[arg(long, requires = "push")]
    pub dry_run: bool,
}

#[derive(Args, Debug, Clone, Default)]
pub struct RemoveArgs {
    /// Issues to send back to the backlog: keys, or paths to their files
    #[arg(value_name = "ISSUES", required = true, value_delimiter = ',')]
    pub issues: Vec<String>,

    /// Apply immediately instead of leaving the change staged
    #[arg(long)]
    pub push: bool,

    /// With --push, show the API call without sending it
    #[arg(long, requires = "push")]
    pub dry_run: bool,
}

#[derive(Args, Debug, Clone, Default)]
pub struct CreateArgs {
    /// What the sprint is called
    ///
    /// On a form this defaults to the number the board is due: a board whose
    /// last sprint is `Sprint 42` is offered `Sprint 43`.
    #[arg(value_name = "NAME")]
    pub name: Option<String>,

    /// Board to create it on: an id or a name
    ///
    /// Only needed when the project has more than one.
    #[arg(long, value_name = "BOARD")]
    pub board: Option<String>,

    /// When the sprint starts
    #[arg(long, value_name = "TIMESTAMP")]
    pub start: Option<String>,

    /// When the sprint ends (Jira wants both dates or neither)
    #[arg(long, value_name = "TIMESTAMP")]
    pub end: Option<String>,

    /// What the sprint is for
    #[arg(long, value_name = "TEXT")]
    pub goal: Option<String>,

    /// Issues to move into it once it exists, as `jired sprint add` would
    #[arg(long, value_name = "ISSUES", value_delimiter = ',')]
    pub issues: Vec<String>,

    /// With --issues, send the moves instead of leaving them staged
    #[arg(long, requires = "issues")]
    pub push: bool,

    /// Fill in the form even when the arguments would have been enough
    #[arg(long, short = 'i')]
    pub interactive: bool,

    /// Show what would be done without creating or sending anything
    #[arg(long, short = 'n')]
    pub dry_run: bool,
}

pub fn run(g: &GlobalArgs, cmd: &SprintCmd, out: Out) -> Result<ExitCode> {
    match cmd {
        SprintCmd::Create(args) => create(g, args, out),
        SprintCmd::Add(args) => {
            let mut ctx = Context::open(g, out)?;
            ctx.lock()?;
            let target = resolve_sprint(&ctx, &args.sprint)?;
            let moved = stage_moves(&mut ctx, &args.issues, Some(&target), out)?;
            finish(&mut ctx, moved, Some(target), args.push, args.dry_run, out)
        }
        SprintCmd::Remove(args) => {
            let mut ctx = Context::open(g, out)?;
            ctx.lock()?;
            let moved = stage_moves(&mut ctx, &args.issues, None, out)?;
            finish(&mut ctx, moved, None, args.push, args.dry_run, out)
        }
    }
}

/// One issue's staged move.
#[derive(Debug, Clone, serde::Serialize)]
struct Moved {
    key: String,
    file: String,
    /// False when the issue was already where it was being sent.
    changed: bool,
}

/// The sprint a `--sprint` value names, as this workspace knows it.
///
/// A name or an id, because both are things a person has: an id from `jired
/// sprints`, a name from the board. Resolving here rather than at push time is
/// what turns "the change was not sent" — a warning after the fact — into an
/// error while the typo is still on screen.
fn resolve_sprint(ctx: &Context, wanted: &str) -> Result<SprintRow> {
    let wanted = wanted.trim();
    let sprints = crate::store::state::StateStore::open(&ctx.workspace)?.sprints()?;
    if let Some(by_id) = sprints.iter().find(|s| s.id == wanted) {
        return Ok(by_id.clone());
    }
    // An open sprint wins over a closed one of the same name: names repeat
    // across the years and nobody means the finished one.
    let mut named: Vec<&SprintRow> = sprints
        .iter()
        .filter(|s| s.name.eq_ignore_ascii_case(wanted))
        .collect();
    named.sort_by_key(|s| match s.state.as_str() {
        "active" => 0,
        "future" => 1,
        _ => 2,
    });
    if let Some(hit) = named.first() {
        return Ok((*hit).clone());
    }

    let open: Vec<String> = sprints
        .iter()
        .filter(|s| s.state != "closed")
        .map(|s| format!("{} ({})", s.name, s.id))
        .collect();
    Err(
        JiredError::usage(format!("`{wanted}` is not a sprint this workspace knows"))
            .with_detail("sprint", wanted.to_string())
            .with_detail("known", serde_json::json!(open))
            .with_hint(if open.is_empty() {
                "run `jired fetch` to pick up the boards and their sprints".to_string()
            } else {
                format!("one of: {}", open.join(", "))
            }),
    )
}

/// Write the sprint change into each issue's file.
///
/// One bad issue does not stop the others, the same promise `push` makes: a key
/// the workspace does not have is reported and the rest are still staged.
fn stage_moves(
    ctx: &mut Context,
    issues: &[String],
    target: Option<&SprintRow>,
    out: Out,
) -> Result<Vec<Moved>> {
    let mut staged = Vec::new();
    let mut failed = 0;
    for issue in issues {
        let key = crate::project::naming::key_from_input(issue);
        let mut changed = false;
        let name = target.map(|t| t.name.clone());
        let edited = edit_issue(ctx, &key, |doc| {
            changed = set_current_sprint(&mut doc.frontmatter.sprint, name.as_deref());
            Ok(())
        });
        match edited {
            Ok(file) => staged.push(Moved { key, file, changed }),
            Err(e) => {
                failed += 1;
                out.warn(format!("{key}: {}", e.message));
            }
        }
    }
    // Every one of them failing is the command failing, not a quiet success.
    if failed > 0 && staged.is_empty() {
        return Err(JiredError::usage(format!(
            "none of the {failed} issue(s) named are in this workspace"
        ))
        .with_hint("run `jired pull` first, or check the keys"));
    }
    Ok(staged)
}

/// Put `target` at the end of an issue's sprint list, which is where it is now.
///
/// The list is a history — an issue that rolls over stays in the sprint it came
/// from — so a move appends rather than overwrites, and only the last entry is
/// something `push` can act on. `None` empties it, which is how a file says
/// "the backlog". Reports whether anything actually moved.
fn set_current_sprint(sprints: &mut Vec<String>, target: Option<&str>) -> bool {
    let Some(target) = target else {
        let had = !sprints.is_empty();
        sprints.clear();
        return had;
    };
    if sprints
        .last()
        .is_some_and(|s| s.eq_ignore_ascii_case(target))
    {
        return false;
    }
    // An issue coming back to a sprint it has been in before is one entry, not
    // two: the name would otherwise appear twice in its own history.
    sprints.retain(|s| !s.eq_ignore_ascii_case(target));
    sprints.push(target.to_string());
    true
}

/// Report the staged moves, and send them when asked.
fn finish(
    ctx: &mut Context,
    moved: Vec<Moved>,
    target: Option<SprintRow>,
    push: bool,
    dry_run: bool,
    out: Out,
) -> Result<ExitCode> {
    let where_to = target
        .as_ref()
        .map(|t| t.name.clone())
        .unwrap_or_else(|| "the backlog".to_string());
    let changed: Vec<&Moved> = moved.iter().filter(|m| m.changed).collect();

    let pushed: Option<PushReport> = if push && !changed.is_empty() {
        let keys: Vec<String> = changed.iter().map(|m| m.key.clone()).collect();
        let report = push_these(ctx, &keys, dry_run)?;
        report_push(&report, out);
        Some(report)
    } else {
        None
    };

    for m in &moved {
        if !m.changed {
            out.say(format!(
                "{} is already in {where_to}",
                style::out(CYAN, &m.key)
            ));
        }
    }
    match &pushed {
        _ if changed.is_empty() => {}
        Some(r) if r.dry_run => {}
        Some(r) if r.results.iter().all(|x| x.ok) => {
            out.say(format!("moved {} issue(s) to {where_to}", changed.len()))
        }
        Some(_) => out.say(format!(
            "{} issue(s) are staged for {where_to}, but the push failed — \
             fix the cause and run `jired push`",
            changed.len()
        )),
        None => {
            for m in &changed {
                out.say(format!(
                    "staged {} → {where_to} in {}",
                    style::out(CYAN, &m.key),
                    m.file
                ));
            }
            out.hint("run `jired push` to move them");
        }
    }

    out.emit(
        "sprint-move",
        &serde_json::json!({
            "sprint": target.map(|t| serde_json::json!({
                "id": t.id, "name": t.name, "state": t.state, "board_id": t.board_id,
            })),
            "issues": moved,
            "pushed": pushed.is_some(),
            "push": pushed,
        }),
    );
    Ok(match pushed {
        Some(report) => report.exit_code(),
        None => ExitCode::Success,
    })
}

fn create(g: &GlobalArgs, args: &CreateArgs, out: Out) -> Result<ExitCode> {
    let mut ctx = Context::open(g, out)?;
    ctx.lock()?;
    let session = crate::store::session::SessionStore::open(&ctx.workspace)?;
    let account = session.require_account()?;
    let client = crate::api::build_client(&mut ctx, &account)?;
    let project: Option<String> = ctx.resolver.get_quiet("workspace.project")?;

    // Boards are what a sprint hangs off, so they are worth a request whichever
    // way the rest of the values arrive.
    let boards = crate::api::block_on(client.boards(project.as_deref()))?;

    let Some(wanted) = compose(&mut ctx, args, &boards, client.as_ref(), &project)? else {
        return Err(JiredError::generic("cancelled — no sprint was created"));
    };

    // The last gate, so the flags-only path cannot get past what the form and
    // the prompts both check.
    let (board_id, board_name) = board_for(&boards, &wanted.board)?;
    let input = SprintInput {
        name: checked_name(&wanted.name)?,
        board_id,
        start: checked_date(wanted.start.as_deref(), "--start")?,
        end: checked_date(wanted.end.as_deref(), "--end")?,
        goal: Some(wanted.goal.clone()).filter(|g| !g.trim().is_empty()),
    };
    check_dates(&input)?;

    let request = serde_json::json!({
        "method": "POST",
        "path": "/rest/agile/1.0/sprint",
        "body": input.to_body(),
    });
    if args.dry_run {
        out.say(format!("POST {}", request["path"].as_str().unwrap_or("")));
        out.say(serde_json::to_string_pretty(&request["body"])?);
        // Named, not staged: the sprint does not exist, so there is nothing for
        // the issues to be moved into yet.
        for issue in &args.issues {
            out.say(format!(
                "would move {} into it",
                style::out(CYAN, crate::project::naming::key_from_input(issue))
            ));
        }
        out.emit(
            "sprint-create",
            &serde_json::json!({
                "dry_run": true,
                "sprint": serde_json::Value::Null,
                "board": { "id": input.board_id, "name": board_name },
                "request": request,
                "issues": [],
            }),
        );
        return Ok(ExitCode::Success);
    }

    let sprint = crate::api::block_on(client.create_sprint(&input))?;
    remember(&ctx, &sprint, out);

    // The same row shape `jired sprints` prints, so the sprint that was just
    // made reads as one of the list it has joined.
    out.say(format!(
        "{} {} {}",
        style::out(CYAN, format!("{:<8}", sprint.id)),
        style::status(&format!("{:<10}", sprint.state), "new"),
        sprint.name
    ));
    out.say(format!("created on {board_name}"));

    // Creating a sprint is almost always followed by filling it, and the moves
    // are ordinary file edits: they stage, and go out with the next push or
    // straight away with --push.
    let row = SprintRow {
        id: sprint.id.clone(),
        board_id: sprint.board_id.clone(),
        name: sprint.name.clone(),
        state: sprint.state.clone(),
    };
    let moved = stage_moves(&mut ctx, &args.issues, Some(&row), out)?;
    let pushed = if !moved.is_empty() {
        let keys: Vec<String> = moved
            .iter()
            .filter(|m| m.changed)
            .map(|m| m.key.clone())
            .collect();
        if args.push && !keys.is_empty() {
            let report = push_these(&mut ctx, &keys, false)?;
            report_push(&report, out);
            match report.results.iter().all(|r| r.ok) {
                true => out.say(format!("moved {} issue(s) into it", keys.len())),
                false => out.say(format!(
                    "{} issue(s) are staged for it, but the push failed — \
                     fix the cause and run `jired push`",
                    keys.len()
                )),
            }
            Some(report)
        } else {
            out.say(format!("staged {} issue(s) for it", keys.len()));
            out.hint("run `jired push` to move them");
            None
        }
    } else {
        out.hint(format!(
            "put issues in it with `jired sprint add KEY --sprint \"{}\"`",
            sprint.name
        ));
        // A sprint with nothing in it has no folder to show, which reads like
        // the sprint never happened; saying when the folder arrives spares the
        // hunt.
        out.hint(format!(
            "`{}/{}` appears with the first issue that push moves in",
            crate::project::naming::GROUP_SPRINTS,
            crate::project::naming::group_dir(&sprint.name, None)
        ));
        None
    };

    let exit = match &pushed {
        Some(report) => report.exit_code(),
        None => ExitCode::Success,
    };
    out.emit(
        "sprint-create",
        &serde_json::json!({
            "dry_run": false,
            "sprint": sprint,
            "board": { "id": input.board_id, "name": board_name },
            "request": request,
            "issues": moved,
            "push": pushed,
        }),
    );
    Ok(exit)
}

/// Resolve the arguments into a complete sprint, asking for what is missing.
///
/// `None` means the user backed out.
fn compose(
    ctx: &mut Context,
    args: &CreateArgs,
    boards: &[Board],
    client: &dyn JiraApi,
    project: &Option<String>,
) -> Result<Option<NewSprint>> {
    let interactive = ctx.resolver.interactive();
    if args.interactive && !interactive {
        return Err(
            JiredError::missing_value("--interactive needs a terminal to ask on")
                .with_hint("pass the name and the board as arguments instead"),
        );
    }

    // A board nobody named is only missing when there is a choice to make: one
    // board is no choice, and several is a question for the form. A project
    // with no boards to check against gets what it was given, because that is
    // an id typed by someone who knows the board is there.
    let named_board = match (&args.board, boards) {
        (Some(b), []) => Some(b.clone()),
        (Some(b), _) => Some(pick_board(boards, Some(b))?.name.clone()),
        (None, [only]) => Some(only.name.clone()),
        (None, _) => None,
    };
    let seed = Seed {
        name: args.name.clone(),
        board: named_board.clone(),
        start: normalize(args.start.as_deref(), "--start")?,
        end: normalize(args.end.as_deref(), "--end")?,
        goal: args.goal.clone(),
    };

    let complete = seed.name.is_some() && named_board.is_some();
    if interactive && (args.interactive || !complete) {
        if crate::tui::available() {
            let known: Vec<KnownBoard> = boards
                .iter()
                .map(|b| KnownBoard {
                    id: b.id.clone(),
                    name: b.name.clone(),
                    board_type: b.board_type.clone(),
                })
                .collect();
            let mut screen = SprintScreen::new(
                project.as_deref().unwrap_or_default(),
                &known,
                &existing(client, boards),
                chrono::Utc::now(),
                &seed,
            );
            return crate::tui::run(&mut screen);
        }
        // No alternate screen here — over a dumb terminal the line prompts are
        // the whole interface rather than a consolation prize.
        let ui = Ui::new(true, !ctx.out.json && !ctx.out.quiet);
        return ask(&ui, boards, seed).map(Some);
    }

    let (Some(name), Some(board)) = (seed.name, named_board) else {
        return Err(
            JiredError::missing_value(match (args.name.is_some(), boards) {
                (true, many) => format!("a board is required — one of: {}", board_names(many)),
                (false, [_]) | (false, []) => "a name is required".to_string(),
                (false, _) => "a name and a board are required".to_string(),
            })
            .with_hint(
                "pass what is missing as arguments — \
                 `jired sprint create \"Sprint 43\" --board 12` — \
                 or run `jired sprint create` in a terminal",
            ),
        );
    };
    Ok(Some(NewSprint {
        name,
        board,
        start: seed.start,
        end: seed.end,
        goal: seed.goal.unwrap_or_default(),
    }))
}

/// Every sprint the project's boards already have.
///
/// Best-effort and per board, the way the fetch path caches them: a board that
/// denies the agile API, or has no sprints at all, costs the form nothing more
/// than a name it cannot offer.
fn existing(client: &dyn JiraApi, boards: &[Board]) -> Vec<BoardSprint> {
    let mut out = Vec::new();
    for board in boards {
        let Ok(sprints) = crate::api::block_on(client.sprints(&board.id)) else {
            continue;
        };
        out.extend(sprints.into_iter().map(|s| BoardSprint {
            board_id: s.board_id.unwrap_or_else(|| board.id.clone()),
            name: s.name,
            state: s.state,
        }));
    }
    out
}

/// The line-based form, for terminals the full-screen one cannot run on.
///
/// It asks for the two things a sprint cannot do without and takes the rest
/// from the arguments; anything more would be a worse version of the screen.
fn ask(ui: &Ui, boards: &[Board], seed: Seed) -> Result<NewSprint> {
    ui.title("new sprint");
    let name = match seed.name {
        Some(name) => name,
        None => ui.ask("Name", Some("what the team will call it"), None)?,
    };

    // Only asked for when there is a choice to make: the seed already holds the
    // board when one was named, and when the project has only one.
    let board = match seed.board {
        Some(board) => board,
        None => {
            let options: Vec<String> = boards.iter().map(|b| b.name.clone()).collect();
            if options.is_empty() {
                ui.ask("Board", Some("the id of the board it belongs to"), None)?
            } else {
                let chosen = ui.choose("Board", &options, 0)?;
                options[chosen].clone()
            }
        }
    };

    Ok(NewSprint {
        name,
        board,
        start: seed.start,
        end: seed.end,
        goal: seed.goal.unwrap_or_default(),
    })
}

/// The board a form or a flag names, as the id the API wants and the name a
/// person reads.
///
/// A project with no boards to check against gets what it was given: the value
/// is an id typed by someone who knows the board exists, and refusing it would
/// only trade a server error for a local one.
fn board_for(boards: &[Board], named: &str) -> Result<(String, String)> {
    if boards.is_empty() {
        return Ok((named.to_string(), named.to_string()));
    }
    let board = pick_board(boards, Some(named))?;
    Ok((board.id.clone(), board.name.clone()))
}

fn checked_name(name: &str) -> Result<String> {
    let name = name.trim();
    if name.is_empty() {
        return Err(JiredError::missing_value("a sprint name is required")
            .with_hint("`jired sprint create \"Sprint 43\"`"));
    }
    Ok(name.to_string())
}

/// A date as written on the command line, kept in the words it was typed in.
///
/// A value with no offset is one the person meant in their own timezone, which
/// is the one every other screen prints in; one that names a zone is never
/// second-guessed.
fn normalize(text: Option<&str>, flag: &str) -> Result<Option<String>> {
    let Some(text) = text else { return Ok(None) };
    let when = parse(text, flag)?;
    Ok(Some(if crate::api::model::names_a_zone(text) {
        text.trim().to_string()
    } else {
        crate::api::model::format_local_iso(&when)
    }))
}

fn checked_date(text: Option<&str>, flag: &str) -> Result<Option<DateTime<Utc>>> {
    text.map(|t| parse(t, flag)).transpose()
}

fn parse(text: &str, flag: &str) -> Result<DateTime<Utc>> {
    crate::api::model::parse_local_timestamp(text).ok_or_else(|| {
        JiredError::usage(format!("`{text}` is not a date"))
            .with_detail("flag", flag.to_string())
            .with_hint(
                "write it as `2026-09-01` or `2026-09-01 09:00`, which are read in your \
                 own timezone, or name one — `2026-09-01T09:00:00Z`",
            )
    })
}

/// What Jira would refuse, or accept and quietly ignore.
fn check_dates(input: &SprintInput) -> Result<()> {
    match (input.start, input.end) {
        (None, Some(_)) => Err(JiredError::usage("an end date needs a start date")
            .with_hint("pass --start too, or leave both off and set the dates in Jira")),
        (Some(start), Some(end)) if end <= start => {
            Err(JiredError::usage("the sprint ends before it starts")
                .with_detail("start", crate::api::model::format_timestamp(&start))
                .with_detail("end", crate::api::model::format_timestamp(&end)))
        }
        _ => Ok(()),
    }
}

/// Put the new sprint in the local cache, so the issue form offers it and a
/// `sprint:` naming it resolves before the next fetch has run.
///
/// Best-effort on purpose: the sprint exists on the server by now, and failing
/// the command over a cache that would not open would misreport that.
fn remember(ctx: &Context, sprint: &Sprint, out: Out) {
    let saved = crate::store::state::StateStore::open(&ctx.workspace)
        .and_then(|state| state.save_sprints(std::slice::from_ref(sprint)));
    if let Err(e) = saved {
        out.warn(format!(
            "{} was created, but the local sprint cache could not be updated ({e}) — \
             run `jired fetch` to refresh it",
            sprint.name
        ));
    }
}
