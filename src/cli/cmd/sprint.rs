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
//! `start`, `edit` and `close` are `create`'s neighbours: a sprint that exists
//! still has no file, so they too post there and then and print the call under
//! `-n`. All three are the same endpoint — `POST …/sprint/{id}`, which merges,
//! so what is not sent is what the sprint keeps — and what they add over that
//! endpoint is the two refusals Jira answers 400 to without saying why: a
//! sprint started with no dates, and a second running sprint on one board.
//!
//! `add` and `remove` are the opposite: an issue's sprint *is* a file field, so
//! they behave like `transition` and `assign` — write the frontmatter, let it
//! show up in `status` and `diff`, and go out through the normal `push`, or
//! immediately with `--push`. What they save is knowing where `sprint:` goes in
//! the key order and that its last entry is the one a move can act on.

use crate::api::{Board, JiraApi, Sprint, SprintInput, SprintUpdate};
use crate::cli::cmd::edit::{push_these, report_push};
use crate::cli::cmd::list::{board_names, pick_board, sprint_row};
use crate::cli::cmd::list_issues::status_categories;
use crate::cli::cmd::prompt::Ui;
use crate::cli::cmd::stage::{self, Staged, Wording};
use crate::cli::context::Context;
use crate::cli::GlobalArgs;
use crate::error::{ExitCode, JiredError, Result};
use crate::json::Out;
use crate::store::state::SprintRow;
use crate::style::{self, CYAN};
use crate::sync::Engine;
use crate::tui::sprint::{BoardSprint, KnownBoard, NewSprint, Seed, SprintScreen};
use chrono::{DateTime, Utc};
use clap::{Args, Subcommand};

#[derive(Subcommand, Debug)]
pub enum SprintCmd {
    /// Create a sprint on a board
    Create(CreateArgs),
    /// Start a sprint that has not been started yet
    Start(StartArgs),
    /// Change a sprint's goal, name or dates
    Edit(EditArgs),
    /// Close a running sprint, optionally carrying its unfinished issues over
    Close(CloseArgs),
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
pub struct StartArgs {
    /// The sprint to start: a name, or an id
    #[arg(value_name = "SPRINT")]
    pub sprint: String,

    /// What the sprint is for, set on the way in
    #[arg(long, value_name = "TEXT")]
    pub goal: Option<String>,

    /// When it starts — required when the sprint has no date yet
    #[arg(long, value_name = "TIMESTAMP")]
    pub start: Option<String>,

    /// When it ends — required when the sprint has no date yet
    #[arg(long, value_name = "TIMESTAMP")]
    pub end: Option<String>,

    /// Show the API call without sending it
    #[arg(long, short = 'n')]
    pub dry_run: bool,
}

#[derive(Args, Debug, Clone, Default)]
pub struct EditArgs {
    /// The sprint to change: a name, or an id
    #[arg(value_name = "SPRINT")]
    pub sprint: String,

    /// What the sprint is for; `--goal ''` clears it
    #[arg(long, value_name = "TEXT")]
    pub goal: Option<String>,

    /// Rename it
    #[arg(long, value_name = "TEXT")]
    pub name: Option<String>,

    /// When it starts
    #[arg(long, value_name = "TIMESTAMP")]
    pub start: Option<String>,

    /// When it ends
    #[arg(long, value_name = "TIMESTAMP")]
    pub end: Option<String>,

    /// Show the API call without sending it
    #[arg(long, short = 'n')]
    pub dry_run: bool,
}

#[derive(Args, Debug, Clone, Default)]
pub struct CloseArgs {
    /// The sprint to close: a name, or an id
    #[arg(value_name = "SPRINT")]
    pub sprint: String,

    /// Carry the issues that are not done into this sprint before closing
    #[arg(long = "move-incomplete-to", value_name = "SPRINT")]
    pub move_incomplete_to: Option<String>,

    /// Show the API calls without sending them
    #[arg(long, short = 'n')]
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
        SprintCmd::Start(args) => start(g, args, out),
        SprintCmd::Edit(args) => edit(g, args, out),
        SprintCmd::Close(args) => close(g, args, out),
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
fn stage_moves(
    ctx: &mut Context,
    issues: &[String],
    target: Option<&SprintRow>,
    out: Out,
) -> Result<Vec<Staged>> {
    let name = target.map(|t| t.name.clone());
    stage::stage(ctx, issues, out, move |doc| {
        Ok(set_current_sprint(
            &mut doc.frontmatter.sprint,
            name.as_deref(),
        ))
    })
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
    moved: Vec<Staged>,
    target: Option<SprintRow>,
    push: bool,
    dry_run: bool,
    out: Out,
) -> Result<ExitCode> {
    let where_to = target
        .as_ref()
        .map(|t| t.name.clone())
        .unwrap_or_else(|| "the backlog".to_string());
    let outcome = stage::finish(
        ctx,
        moved,
        &Wording {
            what: &where_to,
            verb: "moved",
            already: &format!("is already in {where_to}"),
        },
        push,
        dry_run,
        out,
        serde_json::json!({
            "sprint": target.map(|t| serde_json::json!({
                "id": t.id, "name": t.name, "state": t.state, "board_id": t.board_id,
            })),
        }),
    )?;
    out.emit("sprint-move", &outcome.doc);
    Ok(outcome.exit)
}

// ---- start, edit, close --------------------------------------------------
//
// The three writes onto a sprint that already exists. None of them has a file
// to stage into, so each sends immediately and `-n` prints the call — the same
// bargain `create` makes, and the reason they are here rather than in `push`.

fn start(g: &GlobalArgs, args: &StartArgs, out: Out) -> Result<ExitCode> {
    let mut ctx = Context::open(g, out)?;
    ctx.lock()?;
    let client = api_client(&mut ctx)?;
    let sprint = live_sprint(&ctx, client.as_ref(), &args.sprint)?;
    match sprint.state.as_str() {
        "active" => return Err(already(&sprint, "is already running")),
        "closed" => {
            return Err(already(
                &sprint,
                "is closed, and a closed sprint cannot be restarted",
            ))
        }
        _ => {}
    }

    // Only the dates that were given go out — the endpoint merges, and resending
    // what the sprint already has is a chance to send it wrong. What the sprint
    // will *have* is a different question, and it is the one Jira checks.
    let given_start = checked_date(args.start.as_deref(), "--start")?;
    let given_end = checked_date(args.end.as_deref(), "--end")?;
    let (start, end) = (
        given_start.or(sprint.start_date),
        given_end.or(sprint.end_date),
    );
    // Jira refuses `state: active` on a sprint with no dates and answers 400
    // naming nothing, so the missing half is named here instead.
    if start.is_none() || end.is_none() {
        return Err(JiredError::usage(format!(
            "{} has no {} date, and Jira will not start a sprint without both",
            sprint.name,
            match (start.is_none(), end.is_none()) {
                (true, true) => "start or end",
                (true, false) => "start",
                _ => "end",
            }
        ))
        .with_detail("sprint", sprint.name.clone())
        .with_hint("pass --start and --end, which set the dates and start it in one call"));
    }
    check_order(start, end)?;
    if let Some(board) = &sprint.board_id {
        check_one_running(client.as_ref(), board, &sprint)?;
    }

    send(
        &ctx,
        client.as_ref(),
        &sprint,
        SprintUpdate {
            goal: args.goal.clone(),
            start: given_start,
            end: given_end,
            state: Some("active".to_string()),
            ..Default::default()
        },
        args.dry_run,
        out,
    )
}

fn edit(g: &GlobalArgs, args: &EditArgs, out: Out) -> Result<ExitCode> {
    let update = SprintUpdate {
        // An empty name is refused where an empty goal is not: a sprint without
        // a goal is an ordinary sprint, and one without a name is nothing
        // anybody can point at again.
        name: match args.name.as_deref().map(str::trim) {
            Some("") => {
                return Err(JiredError::usage("a sprint cannot be renamed to nothing")
                    .with_hint("pass the new name — `jired sprint edit 44 --name \"Sprint 44a\"`"))
            }
            other => other.map(str::to_string),
        },
        // Not filtered for emptiness the way `create` filters it: `--goal ''`
        // on a sprint that has one is the only way to take it back off.
        goal: args.goal.clone(),
        start: checked_date(args.start.as_deref(), "--start")?,
        end: checked_date(args.end.as_deref(), "--end")?,
        state: None,
    };
    // Checked before anything is opened: a command with no flags is a mistake
    // about the command, and a sprint fetched to find that out is a wasted trip.
    if update.is_empty() {
        return Err(JiredError::missing_value("nothing to change").with_hint(
            "pass --goal, --name, --start or --end — \
                 `jired sprint edit \"Sprint 44\" --goal \"Ship SSO.\"`",
        ));
    }

    let mut ctx = Context::open(g, out)?;
    ctx.lock()?;
    let client = api_client(&mut ctx)?;
    let sprint = live_sprint(&ctx, client.as_ref(), &args.sprint)?;
    let (start, end) = (
        update.start.or(sprint.start_date),
        update.end.or(sprint.end_date),
    );
    if start.is_none() && end.is_some() {
        return Err(needs_a_start());
    }
    check_order(start, end)?;

    let renamed = update.name.is_some();
    let exit = send(&ctx, client.as_ref(), &sprint, update, args.dry_run, out);
    if renamed && !args.dry_run && exit.is_ok() {
        // Issue files spell the sprint by name, so a rename leaves every
        // `sprint:` that mentions it one release behind the board.
        out.hint("run `jired pull` to bring `sprint:` in the issue files up to the new name");
    }
    exit
}

fn close(g: &GlobalArgs, args: &CloseArgs, out: Out) -> Result<ExitCode> {
    let mut ctx = Context::open(g, out)?;
    ctx.lock()?;
    let client = api_client(&mut ctx)?;
    let sprint = live_sprint(&ctx, client.as_ref(), &args.sprint)?;
    match sprint.state.as_str() {
        "closed" => return Err(already(&sprint, "is already closed")),
        "future" => {
            return Err(already(
                &sprint,
                "has not been started, so there is nothing to close",
            )
            .with_hint("`jired sprint start` runs it; deleting one is a job for Jira"))
        }
        _ => {}
    }

    let carry_to = args
        .move_incomplete_to
        .as_deref()
        .map(|wanted| carry_target(&ctx, client.as_ref(), wanted, &sprint))
        .transpose()?;

    // Read off the working copy, not the snapshot: these are the files a
    // carry-over edits, so the set that moves is the set that is listed.
    let incomplete = incomplete_in(&mut ctx, &sprint.name)?;
    let keys: Vec<String> = incomplete.iter().map(|i| i.key.clone()).collect();
    let request = close_request(&sprint);
    // Said before anything moves or closes, in both paths: which issues a close
    // is about to strand is the thing worth reading, and reading it afterwards
    // is reading it too late.
    report_incomplete(&incomplete, carry_to.as_ref(), out);

    if args.dry_run {
        if let (Some(target), false) = (&carry_to, keys.is_empty()) {
            out.say(format!("POST /rest/agile/1.0/sprint/{}/issue", target.id));
            out.say(serde_json::to_string_pretty(
                &serde_json::json!({ "issues": keys }),
            )?);
        }
        out.say(format!(
            "POST {}",
            request["path"].as_str().unwrap_or_default()
        ));
        out.say(serde_json::to_string_pretty(&request["body"])?);
        out.emit(
            "sprint-close",
            &serde_json::json!({
                "dry_run": true,
                "closed": false,
                "sprint": serde_json::Value::Null,
                "request": request,
                "incomplete": incomplete,
                "carried_to": carry_to.as_ref().map(brief),
                "issues": [],
            }),
        );
        return Ok(ExitCode::Success);
    }

    // Carried first, and the close abandoned if the carry-over fails: an issue
    // cannot be rescued out of a sprint that is already closed, so the order
    // that leaves a recoverable state is moves, then close.
    let mut moved: Vec<Staged> = Vec::new();
    let mut pushed = None;
    if let (Some(target), false) = (&carry_to, keys.is_empty()) {
        let row = SprintRow {
            id: target.id.clone(),
            board_id: target.board_id.clone(),
            name: target.name.clone(),
            state: target.state.clone(),
        };
        moved = stage_moves(&mut ctx, &keys, Some(&row), out)?;
        let changed: Vec<String> = moved
            .iter()
            .filter(|m| m.changed)
            .map(|m| m.key.clone())
            .collect();
        if !changed.is_empty() {
            let report = push_these(&mut ctx, &changed, false)?;
            report_push(&report, out);
            if !report.results.iter().all(|r| r.ok) {
                out.say(format!(
                    "{} was left open — the carry-over did not go through",
                    sprint.name
                ));
                out.hint("fix the cause, run `jired push`, then close the sprint again");
                out.emit(
                    "sprint-close",
                    &serde_json::json!({
                        "dry_run": false,
                        "closed": false,
                        "sprint": serde_json::Value::Null,
                        "request": request,
                        "incomplete": incomplete,
                        "carried_to": Some(brief(target)),
                        "issues": moved,
                        "push": report,
                    }),
                );
                return Ok(report.exit_code());
            }
            out.say(format!(
                "carried {} issue(s) over to {}",
                changed.len(),
                target.name
            ));
            pushed = Some(report);
        }
    }

    let closed = crate::api::block_on(client.update_sprint(
        &sprint.id,
        &SprintUpdate {
            state: Some("closed".to_string()),
            ..Default::default()
        },
    ))?;
    remember(&ctx, &closed, out);
    out.say(sprint_row(&closed));
    // The closed sprint drops out of `sprints/` on the next pull, and until then
    // its folder reads like a sprint that is still running.
    out.hint("run `jired pull` to refresh the grouping folders");

    out.emit(
        "sprint-close",
        &serde_json::json!({
            "dry_run": false,
            "closed": true,
            "sprint": closed,
            "request": request,
            "incomplete": incomplete,
            "carried_to": carry_to.as_ref().map(brief),
            "issues": moved,
            "push": pushed,
        }),
    );
    Ok(ExitCode::Success)
}

/// One issue a close would leave behind, as the working copy has it.
#[derive(Debug, Clone, serde::Serialize)]
struct Incomplete {
    key: String,
    file: String,
    status: String,
}

/// The issues this workspace has in a sprint that are not finished.
///
/// "Finished" is the status category the last fetch recorded, because a
/// workflow's own names mean nothing across instances — `Done` on one is
/// `Готово` on the next. A status jired has never seen counts as unfinished:
/// carrying an issue that was already done is a move somebody can undo, and
/// stranding one in a closed sprint is the thing this exists to prevent.
fn incomplete_in(ctx: &mut Context, sprint: &str) -> Result<Vec<Incomplete>> {
    let engine = Engine::open(ctx)?;
    let categories = status_categories(&engine.state.issues()?);
    let mut out = Vec::new();
    for (key, file) in engine.projector.scan()? {
        // A file that will not parse is not a reason to fail a close: `doctor`
        // is where a broken file gets reported.
        let Ok(doc) = engine.projector.read(&file.relative) else {
            continue;
        };
        let fm = doc.frontmatter;
        // Where the issue is *now* is the last entry; an earlier one is history.
        if !fm
            .sprint
            .last()
            .is_some_and(|s| s.eq_ignore_ascii_case(sprint))
        {
            continue;
        }
        if categories
            .get(&fm.status.trim().to_lowercase())
            .is_some_and(|c| c == "done")
        {
            continue;
        }
        out.push(Incomplete {
            key,
            file: file.relative,
            status: fm.status,
        });
    }
    Ok(out)
}

/// Say which issues a close leaves unfinished — the view that otherwise means
/// reading `jired list --sprint` and comparing it against the board by eye.
fn report_incomplete(incomplete: &[Incomplete], carried_to: Option<&Sprint>, out: Out) {
    if incomplete.is_empty() {
        return;
    }
    let keys: Vec<&str> = incomplete.iter().map(|i| i.key.as_str()).collect();
    match carried_to {
        Some(target) => out.say(format!(
            "{} issue(s) are not done and go to {}: {}",
            keys.len(),
            target.name,
            keys.join(", ")
        )),
        None => {
            out.warn(format!(
                "{} issue(s) are not done and stay in the closed sprint: {}",
                keys.len(),
                keys.join(", ")
            ));
            out.hint("--move-incomplete-to \"the next sprint\" carries them over instead");
        }
    }
}

/// The sprint a `--move-incomplete-to` names, checked for the two ways it can
/// be the wrong one.
fn carry_target(
    ctx: &Context,
    client: &dyn JiraApi,
    wanted: &str,
    closing: &Sprint,
) -> Result<Sprint> {
    let target = live_sprint(ctx, client, wanted)?;
    if target.id == closing.id {
        return Err(JiredError::usage(format!(
            "{} cannot carry its own issues over to itself",
            closing.name
        ))
        .with_hint("name the sprint they should go to, or leave the flag off"));
    }
    if target.state == "closed" {
        return Err(
            already(&target, "is closed, so nothing can be carried into it")
                .with_hint("carry them into a running or future sprint"),
        );
    }
    Ok(target)
}

/// The client every one of these commands needs, opened the one way.
fn api_client(ctx: &mut Context) -> Result<Box<dyn JiraApi>> {
    let session = crate::store::session::SessionStore::open(&ctx.workspace)?;
    let account = session.require_account()?;
    crate::api::build_client(ctx, &account)
}

/// The sprint a name or an id points at, as the server holds it *now*.
///
/// The local cache carries a name and a state and no dates, and every check
/// these commands make before sending — has it dates, is it already running, is
/// something else running on its board — is a question about the server. So the
/// cache resolves the name and the server answers the question.
fn live_sprint(ctx: &Context, client: &dyn JiraApi, wanted: &str) -> Result<Sprint> {
    let id = match resolve_sprint(ctx, wanted) {
        Ok(row) => row.id,
        // A bare number is an id whatever the cache knows. These commands ask
        // the server for the sprint regardless, so it is the server that says
        // whether the id is real, and refusing it here would make a workspace
        // that has not fetched a dead end.
        Err(_) if is_id(wanted) => wanted.trim().to_string(),
        Err(e) => return Err(e),
    };
    crate::api::block_on(client.sprint(&id))
}

fn is_id(wanted: &str) -> bool {
    let wanted = wanted.trim();
    !wanted.is_empty() && wanted.chars().all(|c| c.is_ascii_digit())
}

/// Send one change to a sprint, or print the call it would have been.
fn send(
    ctx: &Context,
    client: &dyn JiraApi,
    sprint: &Sprint,
    update: SprintUpdate,
    dry_run: bool,
    out: Out,
) -> Result<ExitCode> {
    let request = serde_json::json!({
        "method": "POST",
        "path": format!("/rest/agile/1.0/sprint/{}", sprint.id),
        "body": update.to_body(),
    });
    if dry_run {
        out.say(format!(
            "POST {}",
            request["path"].as_str().unwrap_or_default()
        ));
        out.say(serde_json::to_string_pretty(&request["body"])?);
        out.emit(
            "sprint-update",
            &serde_json::json!({
                "dry_run": true,
                "sprint": serde_json::Value::Null,
                "request": request,
            }),
        );
        return Ok(ExitCode::Success);
    }

    let updated = crate::api::block_on(client.update_sprint(&sprint.id, &update))?;
    remember(ctx, &updated, out);
    out.say(sprint_row(&updated));
    if let (Some(start), Some(end)) = (&updated.start_date, &updated.end_date) {
        out.say(format!(
            "{} → {}",
            crate::api::model::format_local(start),
            crate::api::model::format_local(end)
        ));
    }
    if let Some(goal) = updated.goal.as_deref().filter(|g| !g.trim().is_empty()) {
        out.say(format!("goal: {goal}"));
    }
    out.emit(
        "sprint-update",
        &serde_json::json!({
            "dry_run": false,
            "sprint": updated,
            "request": request,
        }),
    );
    Ok(ExitCode::Success)
}

fn close_request(sprint: &Sprint) -> serde_json::Value {
    serde_json::json!({
        "method": "POST",
        "path": format!("/rest/agile/1.0/sprint/{}", sprint.id),
        "body": { "state": "closed" },
    })
}

/// A sprint as the two things another command needs to address it by.
fn brief(sprint: &Sprint) -> serde_json::Value {
    serde_json::json!({ "id": sprint.id, "name": sprint.name })
}

/// "`Sprint 44` is already running" — the same shape for every state refusal,
/// so the id is always there for whoever is reading the JSON.
fn already(sprint: &Sprint, what: &str) -> JiredError {
    JiredError::usage(format!("{} {what}", sprint.name))
        .with_detail("sprint", sprint.name.clone())
        .with_detail("sprint_id", sprint.id.clone())
        .with_detail("state", sprint.state.clone())
}

/// Jira allows one running sprint per board and answers 400 for the second,
/// naming nothing. Asking first is what turns that into the sprint's name.
///
/// Best-effort: a board that will not answer is not a reason to refuse, because
/// the server makes this check too and its answer is the one that counts.
fn check_one_running(client: &dyn JiraApi, board: &str, starting: &Sprint) -> Result<()> {
    let Ok(sprints) = crate::api::block_on(client.sprints(board)) else {
        return Ok(());
    };
    // Boards over one backlog commonly share a sprint pool, so a running sprint
    // in the answer is only in the way when it is this board's own.
    let running = sprints.iter().find(|s| {
        s.state == "active"
            && s.id != starting.id
            && s.board_id.as_deref().is_none_or(|b| b == board)
    });
    let Some(running) = running else {
        return Ok(());
    };
    Err(
        JiredError::usage(format!("board {board} is already running {}", running.name))
            .with_detail("sprint", running.name.clone())
            .with_detail("sprint_id", running.id.clone())
            .with_detail("board", board.to_string())
            .with_hint(format!(
                "close it first: `jired sprint close \"{}\"`",
                running.name
            )),
    )
}

/// What the sprint's dates would be after this change, checked the way
/// `create` checks the ones it is about to set.
fn check_order(start: Option<DateTime<Utc>>, end: Option<DateTime<Utc>>) -> Result<()> {
    match (start, end) {
        (Some(start), Some(end)) if end <= start => {
            Err(JiredError::usage("the sprint ends before it starts")
                .with_detail("start", crate::api::model::format_timestamp(&start))
                .with_detail("end", crate::api::model::format_timestamp(&end)))
        }
        _ => Ok(()),
    }
}

fn create(g: &GlobalArgs, args: &CreateArgs, out: Out) -> Result<ExitCode> {
    let mut ctx = Context::open(g, out)?;
    ctx.lock()?;
    let client = api_client(&mut ctx)?;
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
    out.say(sprint_row(&sprint));
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
    if input.start.is_none() && input.end.is_some() {
        return Err(needs_a_start());
    }
    check_order(input.start, input.end)
}

fn needs_a_start() -> JiredError {
    JiredError::usage("an end date needs a start date")
        .with_hint("pass --start too, or leave both off and set the dates in Jira")
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
        // The server has the change either way, so this is a stale cache and
        // not a failed command; saying which is the difference between running
        // `fetch` and running the whole thing again.
        out.warn(format!(
            "{}: the change went through, but the local sprint cache could not be \
             updated ({e}) — run `jired fetch` to refresh it",
            sprint.name
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(text: &str) -> Option<DateTime<Utc>> {
        crate::api::model::parse_timestamp(text)
    }

    /// The list is a history, so a move appends — and an issue coming back to a
    /// sprint it has been in before is one entry, not two.
    #[test]
    fn a_move_appends_and_never_names_a_sprint_twice() {
        let mut sprints = vec!["Sprint 41".to_string(), "Sprint 42".to_string()];
        assert!(set_current_sprint(&mut sprints, Some("Sprint 43")));
        assert_eq!(sprints, ["Sprint 41", "Sprint 42", "Sprint 43"]);

        // Already there is not a change, and must not stage one.
        assert!(!set_current_sprint(&mut sprints, Some("sprint 43")));

        assert!(set_current_sprint(&mut sprints, Some("Sprint 41")));
        assert_eq!(sprints, ["Sprint 42", "Sprint 43", "Sprint 41"]);

        // No sprint at all is what the backlog looks like in a file.
        assert!(set_current_sprint(&mut sprints, None));
        assert!(sprints.is_empty());
        assert!(!set_current_sprint(&mut sprints, None));
    }

    /// A sprint with dates that do not make sense is refused before it is sent,
    /// whether the dates are being set or were already there.
    #[test]
    fn dates_out_of_order_are_refused_and_a_half_pair_is_named() {
        assert!(check_order(at("2026-09-01T09:00:00Z"), at("2026-09-15T09:00:00Z")).is_ok());
        assert!(check_order(at("2026-09-15T09:00:00Z"), at("2026-09-01T09:00:00Z")).is_err());
        // The same instant twice is a sprint of no length, which Jira takes and
        // nobody means.
        assert!(check_order(at("2026-09-01T09:00:00Z"), at("2026-09-01T09:00:00Z")).is_err());
        // Half a pair is somebody else's problem: `check_dates` decides whether
        // a missing half is allowed, and this only compares the two.
        assert!(check_order(at("2026-09-01T09:00:00Z"), None).is_ok());
        assert!(check_order(None, None).is_ok());

        let err = check_dates(&SprintInput {
            end: at("2026-09-15T09:00:00Z"),
            ..Default::default()
        })
        .unwrap_err();
        assert!(err.message.contains("needs a start date"));
    }

    /// A name resolves against the cache; a bare number does not have to,
    /// because the server is about to be asked either way.
    #[test]
    fn an_id_is_a_run_of_digits_and_nothing_else() {
        assert!(is_id("43"));
        assert!(is_id(" 43 "));
        assert!(!is_id("Sprint 43"));
        assert!(!is_id("43a"));
        assert!(!is_id(""));
        assert!(!is_id("   "));
    }
}
