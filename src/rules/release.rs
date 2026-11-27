//! The rules of a release: what a version's name and days have to be, where
//! unfixed issues go when one ships, and the requests that change one.
//!
//! Kept apart from the command, which parses its flags, calls these and
//! prints what came back, so the rules can be read and tested without one.

use crate::api::{JiraApi, Server, Version, VersionUpdate};
use crate::error::{JiredError, Result};
use chrono::NaiveDate;

/// The version a `--move-unfixed-to` names, checked for the ways it can be the
/// wrong one.
pub(crate) fn carry_target(
    project: &str,
    versions: &[Version],
    wanted: &str,
    shipping: &Version,
) -> Result<Version> {
    let target = pick(project, versions, wanted)?;
    if target.id == shipping.id {
        return Err(JiredError::usage(format!(
            "{} cannot move its own issues to itself",
            shipping.name
        ))
        .with_hint("name the version they should go to, or leave the flag off"));
    }
    if target.released || target.archived {
        return Err(already(
            &target,
            &format!("is {}, so nothing can be moved into it", target.state()),
        )
        .with_hint("move them to a version that has not shipped yet"));
    }
    Ok(target)
}

pub(crate) fn update_request(
    client: &dyn JiraApi,
    version: &Version,
    update: &VersionUpdate,
) -> serde_json::Value {
    serde_json::json!({
        "method": "PUT",
        "path": format!("{}/version/{}", api_prefix(client), version.id),
        "body": update.to_body(),
    })
}

/// "`0.4.0` is already released" — one shape for every state refusal, so the id
/// is always there for whoever is reading the JSON.
pub(crate) fn already(version: &Version, what: &str) -> JiredError {
    JiredError::usage(format!("{} {what}", version.name))
        .with_detail("version", version.name.clone())
        .with_detail("version_id", version.id.clone())
        .with_detail("state", version.state())
}

pub(crate) fn checked_name(name: &str) -> Result<String> {
    let name = name.trim();
    if name.is_empty() {
        return Err(JiredError::missing_value("a version name is required")
            .with_hint("`jired release create 0.5.0`"));
    }
    Ok(name.to_string())
}

pub(crate) fn checked_day(text: Option<&str>, flag: &str) -> Result<Option<NaiveDate>> {
    let Some(text) = text else { return Ok(None) };
    crate::api::model::parse_day(text).map(Some).ok_or_else(|| {
        JiredError::usage(format!("`{text}` is not a date"))
            .with_detail("flag", flag.to_string())
            .with_hint("write it as `2026-10-29`: a version's dates are days, with no time")
    })
}

/// Jira refuses a version due out before it starts, with a message that does
/// not say which version.
pub(crate) fn check_order(start: Option<NaiveDate>, release: Option<NaiveDate>) -> Result<()> {
    match (start, release) {
        (Some(start), Some(release)) if release < start => {
            Err(JiredError::usage("the version is due out before it starts")
                .with_detail("start", day(&start))
                .with_detail("release_date", day(&release)))
        }
        _ => Ok(()),
    }
}

/// Add `name` to an issue's versions, unless it is there already. Appended,
/// because the list is in the order the issue gained them.
pub(crate) fn add_version(versions: &mut Vec<String>, name: &str) -> bool {
    if versions.iter().any(|v| v.eq_ignore_ascii_case(name)) {
        return false;
    }
    versions.push(name.to_string());
    true
}

/// Swap one version for another in an issue's list, keeping its place.
///
/// An issue can be in several fix versions — a fix that also goes into a
/// maintenance branch — so only the one being released is moved, and the
/// target is not added twice when the issue already names it.
pub(crate) fn move_version(versions: &mut Vec<String>, from: &str, to: &str) -> bool {
    let Some(at) = versions.iter().position(|v| v.eq_ignore_ascii_case(from)) else {
        return false;
    };
    if versions.iter().any(|v| v.eq_ignore_ascii_case(to)) {
        versions.remove(at);
    } else {
        versions[at] = to.to_string();
    }
    true
}

/// The version a name or an id points at, out of a project's listing.
///
/// An id first, because it is unambiguous and is what `--json` hands a script;
/// then the name, without regard to case, because that is how Jira matches it.
pub(crate) fn pick(project: &str, versions: &[Version], wanted: &str) -> Result<Version> {
    let wanted = wanted.trim();
    if let Some(hit) = versions
        .iter()
        .find(|v| v.id == wanted)
        .or_else(|| versions.iter().find(|v| v.name == wanted))
        .or_else(|| {
            versions
                .iter()
                .find(|v| v.name.eq_ignore_ascii_case(wanted))
        })
    {
        return Ok(hit.clone());
    }
    let open: Vec<String> = versions
        .iter()
        .filter(|v| !v.released && !v.archived)
        .map(|v| format!("{} ({})", v.name, v.id))
        .collect();
    Err(
        JiredError::usage(format!("`{wanted}` is not a version of {project}"))
            .with_detail("version", wanted.to_string())
            .with_detail("known", serde_json::json!(open))
            .with_hint(if open.is_empty() {
                format!("`jired releases --all` lists every version {project} has")
            } else {
                format!("one of: {}", open.join(", "))
            }),
    )
}

pub(crate) fn api_prefix(client: &dyn Server) -> &'static str {
    client.deployment().api_prefix()
}

pub(crate) fn day(d: &NaiveDate) -> String {
    crate::api::model::format_day(d)
}
