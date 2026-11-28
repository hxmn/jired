//! Values the wire model and the file model both speak of.
//!
//! `api` is how Jira spells an issue and `doc` is how a file does; neither
//! should need the other to say what a user, a checklist line or a timestamp
//! is. These lived on one side or the other and each side reached across for
//! them, which made the two one tangle. Both import them from here, and the
//! old paths re-export them so callers elsewhere did not move.

use chrono::{DateTime, Local, TimeZone, Utc};
use serde::{Deserialize, Serialize};

// ---- People ------------------------------------------------------------------

/// A user reference. Cloud identifies by `accountId`, Data Center by `name`;
/// both land here so mention rendering has one shape.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UserRef {
    pub id: String,
    pub display_name: String,
}

/// Render a mention the way the file format spells it: `@[Name](id)`.
pub fn format_user(user: Option<&UserRef>) -> Option<String> {
    user.map(|u| format!("@[{}]({})", u.display_name, u.id))
}

/// Recover the account id from `@[Name](id)`; a bare string is taken as an id.
pub fn parse_user(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    if let Some(rest) = value.strip_prefix("@[") {
        if let Some(close) = rest.find("](") {
            let id = &rest[close + 2..];
            return Some(id.trim_end_matches(')').to_string());
        }
    }
    Some(value.to_string())
}

// ---- Issues ------------------------------------------------------------------

pub fn is_issue_key(s: &str) -> bool {
    let Some((project, number)) = s.split_once('-') else {
        return false;
    };
    !project.is_empty()
        && project
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
        && project
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_uppercase())
        && !number.is_empty()
        && number.chars().all(|c| c.is_ascii_digit())
}

/// One line of `## Checklist`: an item, or the header of a group of items.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChecklistEntry {
    Header(String),
    Item(ChecklistItem),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChecklistItem {
    pub checked: bool,
    /// Status name, kept only when the checkbox does not already say it —
    /// `IN PROGRESS`, `SKIPPED`, or whatever the instance has configured.
    pub status: Option<String>,
    pub title: String,
    /// The item's note, if it has one.
    pub description: Option<String>,
}

// ---- Links -------------------------------------------------------------------

/// The `links:` key a web (remote) link is written under, in place of a
/// direction: `- web: https://wiki.example/x "Release notes"`.
///
/// One list for both kinds rather than a second key, because both are "what
/// this issue points at" and an agent adding one should not have to know Jira
/// keeps them in two places.
pub const WEB: &str = "web";

/// A web link as `links:` spells it: the URL, then the title in quotes when
/// there is one that says more than the URL does.
pub fn web_value(url: &str, title: &str) -> String {
    let (url, title) = (url.trim(), title.trim());
    match title.is_empty() || title == url {
        true => url.to_string(),
        false => format!("{url} \"{title}\""),
    }
}

// ---- Time --------------------------------------------------------------------

/// Parse a Jira timestamp into UTC.
///
/// Jira Cloud emits ISO-8601 with an offset; Data Center emits the same shape
/// but in server-local time, and some proxies drop the colon from the offset.
pub fn parse_timestamp(s: &str) -> Option<DateTime<Utc>> {
    if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
        return Some(dt.with_timezone(&Utc));
    }
    for fmt in [
        "%Y-%m-%dT%H:%M:%S%.3f%z",
        "%Y-%m-%dT%H:%M:%S%z",
        "%Y-%m-%d %H:%M:%S%.3f%z",
    ] {
        if let Ok(dt) = DateTime::parse_from_str(s, fmt) {
            return Some(dt.with_timezone(&Utc));
        }
    }
    // Date-only values appear in duedate and custom date fields.
    if let Ok(d) = chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d") {
        return d.and_hms_opt(0, 0, 0).map(|dt| dt.and_utc());
    }
    None
}

/// Parse a timestamp a person typed, reading a missing offset as their own.
///
/// The server always says which zone it means; a person does not, and having
/// just been shown a local time they will type one back. Anything with an
/// offset keeps it — an explicit zone is never second-guessed — so this only
/// ever fills in a blank.
pub fn parse_local_timestamp(s: &str) -> Option<DateTime<Utc>> {
    let s = s.trim();
    // Naive shapes first: none of them match a string that carries an offset,
    // and going the other way would let `parse_timestamp` read a bare date as
    // midnight UTC before this ever saw it.
    for fmt in [
        "%Y-%m-%d %H:%M:%S",
        "%Y-%m-%dT%H:%M:%S",
        "%Y-%m-%d %H:%M",
        "%Y-%m-%dT%H:%M",
    ] {
        if let Ok(naive) = chrono::NaiveDateTime::parse_from_str(s, fmt) {
            return local_to_utc(naive);
        }
    }
    if let Ok(date) = chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d") {
        if let Some(naive) = date.and_hms_opt(0, 0, 0) {
            return local_to_utc(naive);
        }
    }
    parse_timestamp(s)
}

/// Whether a written timestamp says which zone it is in.
///
/// What separates a value to leave in the words it was typed in from one that
/// has to have a zone put on it before it is written down. A bare date says no
/// zone, whatever [`parse_timestamp`] is willing to make of it.
pub fn names_a_zone(s: &str) -> bool {
    let s = s.trim();
    DateTime::parse_from_rfc3339(s).is_ok()
        || ["%Y-%m-%dT%H:%M:%S%.3f%z", "%Y-%m-%dT%H:%M:%S%z"]
            .iter()
            .any(|fmt| DateTime::parse_from_str(s, fmt).is_ok())
}

/// Pin a wall-clock reading to an instant.
///
/// The hour a clock skips forward over never happened and the hour it repeats
/// happened twice; taking the earliest candidate answers both with the reading
/// closest to what was meant, rather than refusing an otherwise good time.
fn local_to_utc(naive: chrono::NaiveDateTime) -> Option<DateTime<Utc>> {
    use chrono::offset::LocalResult;
    match Local.from_local_datetime(&naive) {
        LocalResult::Single(dt) => Some(dt.with_timezone(&Utc)),
        LocalResult::Ambiguous(earlier, _) => Some(earlier.with_timezone(&Utc)),
        LocalResult::None => Local
            .from_local_datetime(&(naive + chrono::Duration::hours(1)))
            .earliest()
            .map(|dt| dt.with_timezone(&Utc)),
    }
}

/// Serialize a timestamp the way jired writes it everywhere.
///
/// UTC, because this is what goes into `state.db`, into issue files, into API
/// payloads and into `--json` documents — records, which have to mean the same
/// thing on every machine that reads them. Text on a screen is the opposite
/// case; see [`format_local`].
pub fn format_timestamp(dt: &DateTime<Utc>) -> String {
    dt.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// The same instant as the person reading it lives it.
///
/// Everything jired prints to a terminal goes through here. On a screen UTC is
/// not a fact, it is a subtraction the reader has to do, and the offset is one
/// they already know — so the zone marker goes too and what is left is the time
/// their own clock shows.
pub fn format_local(dt: &DateTime<Utc>) -> String {
    dt.with_timezone(&Local)
        .format("%Y-%m-%d %H:%M:%S")
        .to_string()
}

/// A local time that also has to survive being read back.
///
/// For the values shown on screen that are *also* records — a worklog's start
/// time, which the form writes into the issue file. It keeps the offset, so it
/// is every bit as unambiguous as a `Z` timestamp while still reading as the
/// clock the person authoring it was looking at.
pub fn format_local_iso(dt: &DateTime<Utc>) -> String {
    dt.with_timezone(&Local)
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// Format a JQL timestamp literal in the shape Jira's parser accepts.
pub fn jql_timestamp(dt: &DateTime<Utc>) -> String {
    dt.format("%Y-%m-%d %H:%M").to_string()
}

/// Check a duration in Jira's notation, returning it in seconds.
///
/// Only the syntax is checked. A week and a day are whatever the instance has
/// them configured as — the seconds here are for comparison and error messages,
/// never for what goes over the wire, which is always the text the user wrote.
pub fn parse_duration(text: &str) -> Option<u64> {
    let mut total: u64 = 0;
    let mut digits = String::new();
    let mut any = false;

    for c in text.trim().chars() {
        match c {
            '0'..='9' => digits.push(c),
            ' ' | ',' => {}
            _ => {
                let scale = match c.to_ascii_lowercase() {
                    'w' => 5 * 8 * 3600,
                    'd' => 8 * 3600,
                    'h' => 3600,
                    'm' => 60,
                    _ => return None,
                };
                let n: u64 = digits.parse().ok()?;
                digits.clear();
                total = total.checked_add(n.checked_mul(scale)?)?;
                any = true;
            }
        }
    }
    // A bare number is ambiguous — Jira reads it as minutes only because of an
    // instance setting — so it is refused rather than assumed.
    (any && digits.is_empty() && total > 0).then_some(total)
}
