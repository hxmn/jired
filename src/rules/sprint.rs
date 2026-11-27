//! The rules of a sprint: what a name, a pair of dates and a board have to be,
//! what Jira allows to run at once, and the request that closes one.
//!
//! Kept apart from the command, which parses its flags, calls these and
//! prints what came back, so the rules can be read and tested without one.

use crate::api::{Agile, Board, JiraApi, Sprint, SprintInput};
use crate::error::{JiredError, Result};
use crate::tui::sprint::BoardSprint;
use chrono::{DateTime, Utc};

/// Put `target` at the end of an issue's sprint list, which is where it is now.
///
/// The list is a history — an issue that rolls over stays in the sprint it came
/// from — so a move appends rather than overwrites, and only the last entry is
/// something `push` can act on. `None` empties it, which is how a file says
/// "the backlog". Reports whether anything actually moved.
pub(crate) fn set_current_sprint(sprints: &mut Vec<String>, target: Option<&str>) -> bool {
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

pub(crate) fn is_id(wanted: &str) -> bool {
    let wanted = wanted.trim();
    !wanted.is_empty() && wanted.chars().all(|c| c.is_ascii_digit())
}

pub(crate) fn close_request(sprint: &Sprint) -> serde_json::Value {
    serde_json::json!({
        "method": "POST",
        "path": format!("/rest/agile/1.0/sprint/{}", sprint.id),
        "body": { "state": "closed" },
    })
}

/// "`Sprint 44` is already running" — the same shape for every state refusal,
/// so the id is always there for whoever is reading the JSON.
pub(crate) fn already(sprint: &Sprint, what: &str) -> JiredError {
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
pub(crate) fn check_one_running(
    client: &dyn JiraApi,
    board: &str,
    starting: &Sprint,
) -> Result<()> {
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
pub(crate) fn check_order(start: Option<DateTime<Utc>>, end: Option<DateTime<Utc>>) -> Result<()> {
    match (start, end) {
        (Some(start), Some(end)) if end <= start => {
            Err(JiredError::usage("the sprint ends before it starts")
                .with_detail("start", crate::api::model::format_timestamp(&start))
                .with_detail("end", crate::api::model::format_timestamp(&end)))
        }
        _ => Ok(()),
    }
}

/// The board a form or a flag names, as the id the API wants and the name a
/// person reads.
///
/// A project with no boards to check against gets what it was given: the value
/// is an id typed by someone who knows the board exists, and refusing it would
/// only trade a server error for a local one.
pub(crate) fn board_for(boards: &[Board], named: &str) -> Result<(String, String)> {
    if boards.is_empty() {
        return Ok((named.to_string(), named.to_string()));
    }
    let board = pick_board(boards, Some(named))?;
    Ok((board.id.clone(), board.name.clone()))
}

pub(crate) fn checked_name(name: &str) -> Result<String> {
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
pub(crate) fn normalize(text: Option<&str>, flag: &str) -> Result<Option<String>> {
    let Some(text) = text else { return Ok(None) };
    let when = parse(text, flag)?;
    Ok(Some(if crate::api::model::names_a_zone(text) {
        text.trim().to_string()
    } else {
        crate::api::model::format_local_iso(&when)
    }))
}

pub(crate) fn checked_date(text: Option<&str>, flag: &str) -> Result<Option<DateTime<Utc>>> {
    text.map(|t| parse(t, flag)).transpose()
}

pub(crate) fn parse(text: &str, flag: &str) -> Result<DateTime<Utc>> {
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
pub(crate) fn check_dates(input: &SprintInput) -> Result<()> {
    if input.start.is_none() && input.end.is_some() {
        return Err(needs_a_start());
    }
    check_order(input.start, input.end)
}

pub(crate) fn needs_a_start() -> JiredError {
    JiredError::usage("an end date needs a start date")
        .with_hint("pass --start too, or leave both off and set the dates in Jira")
}

/// Every sprint the project's boards already have.
///
/// Best-effort and per board, the way the fetch path caches them: a board that
/// denies the agile API, or has no sprints at all, costs the form nothing more
/// than a name it cannot offer.
pub(crate) fn existing(client: &dyn Agile, boards: &[Board]) -> Vec<BoardSprint> {
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
