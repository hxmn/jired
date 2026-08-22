//! Deployment-neutral shapes the sync engine works with.
//!
//! Every timestamp is normalized to UTC on ingest (decision D22) so diffs are
//! stable regardless of the server's timezone. UTC is what jired *keeps*;
//! [`format_local`] is what it *shows*, and the two must not be confused —
//! a record that moves with whoever reads it is not a record.

use crate::api::Markup;
use chrono::{DateTime, Local, TimeZone, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Myself {
    pub account_id: String,
    pub display_name: Option<String>,
    pub email: Option<String>,
    pub time_zone: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ServerInfo {
    pub deployment: Option<crate::api::Deployment>,
    pub version: Option<String>,
    pub title: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
    pub id: String,
    pub key: String,
    pub name: String,
    #[serde(default)]
    pub project_type: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Board {
    pub id: String,
    pub name: String,
    #[serde(rename = "type")]
    pub board_type: String,
    #[serde(default)]
    pub project_key: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Sprint {
    pub id: String,
    pub name: String,
    /// `active`, `future`, or `closed`.
    pub state: String,
    #[serde(default)]
    pub board_id: Option<String>,
    #[serde(default)]
    pub start_date: Option<DateTime<Utc>>,
    #[serde(default)]
    pub end_date: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BoardColumn {
    pub name: String,
    /// Status ids mapped onto this column.
    pub status_ids: Vec<String>,
}

/// A user reference. Cloud identifies by `accountId`, Data Center by `name`;
/// both land here so mention rendering has one shape.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UserRef {
    pub id: String,
    pub display_name: String,
}

/// Rich text in whichever markup the server uses.
#[derive(Debug, Clone, PartialEq)]
pub enum RichText {
    Adf(serde_json::Value),
    Wiki(String),
    Empty,
}

impl RichText {
    pub fn markup(&self) -> Option<Markup> {
        match self {
            RichText::Adf(_) => Some(Markup::Adf),
            RichText::Wiki(_) => Some(Markup::Wiki),
            RichText::Empty => None,
        }
    }

    pub fn is_empty(&self) -> bool {
        match self {
            RichText::Empty => true,
            RichText::Wiki(s) => s.trim().is_empty(),
            RichText::Adf(v) => v
                .get("content")
                .and_then(|c| c.as_array())
                .is_none_or(|a| a.is_empty()),
        }
    }

    /// Serialized form stored in `state.db` and compared for change detection.
    pub fn to_storage(&self) -> String {
        match self {
            RichText::Adf(v) => v.to_string(),
            RichText::Wiki(s) => s.clone(),
            RichText::Empty => String::new(),
        }
    }

    pub fn from_storage(markup: Markup, raw: &str) -> Self {
        if raw.is_empty() {
            return RichText::Empty;
        }
        match markup {
            Markup::Adf => serde_json::from_str(raw)
                .map(RichText::Adf)
                .unwrap_or(RichText::Empty),
            Markup::Wiki => RichText::Wiki(raw.to_string()),
        }
    }

    /// The value to send in a field payload for this markup.
    pub fn to_field_value(&self) -> serde_json::Value {
        match self {
            RichText::Adf(v) => v.clone(),
            RichText::Wiki(s) => serde_json::Value::String(s.clone()),
            RichText::Empty => serde_json::Value::Null,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct IssueLink {
    /// Outward or inward description, e.g. `blocks` / `is blocked by`.
    pub link_type: String,
    pub other_key: String,
}

/// Normalized issue fields. `raw` on [`RemoteIssue`] remains the source of
/// truth for anything not modelled here.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct IssueFields {
    pub summary: String,
    pub issue_type: String,
    pub status: String,
    pub status_id: Option<String>,
    pub priority: Option<String>,
    pub labels: Vec<String>,
    pub components: Vec<String>,
    pub assignee: Option<UserRef>,
    pub reporter: Option<UserRef>,
    pub created: Option<DateTime<Utc>>,
    pub updated: Option<DateTime<Utc>>,
    /// Sprints the issue belongs to, oldest first. Jira's field is a list: an
    /// issue that rolls over stays in the sprint it came from.
    pub sprint: Vec<String>,
    pub parent: Option<String>,
    pub epic: Option<String>,
    pub fix_versions: Vec<String>,
    pub affects_versions: Vec<String>,
    pub links: Vec<IssueLink>,
    pub story_points: Option<f64>,
    pub subtasks: Vec<SubtaskRef>,
    /// The checklist plugin's list, if the instance has one [M5.4].
    pub checklist: Vec<crate::doc::ChecklistEntry>,
    /// Custom fields with no well-known mapping, keyed by field id.
    pub custom: std::collections::BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubtaskRef {
    pub key: String,
    pub summary: String,
    pub status: String,
    pub done: bool,
}

#[derive(Debug, Clone)]
pub struct RemoteIssue {
    pub key: String,
    pub id: String,
    pub updated: DateTime<Utc>,
    pub description: RichText,
    pub fields: IssueFields,
    pub comments: Vec<RemoteComment>,
    pub worklogs: Vec<RemoteWorklog>,
    /// How many worklogs the server says the issue has.
    ///
    /// An issue payload carries only the first page of them, so this is what
    /// says whether the list above is the whole story.
    pub worklog_total: usize,
    pub attachments: Vec<RemoteAttachment>,
    /// The untouched server payload, kept so unmodelled data round-trips.
    pub raw: serde_json::Value,
}

impl RemoteIssue {
    pub fn summary(&self) -> &str {
        &self.fields.summary
    }
    pub fn status(&self) -> &str {
        &self.fields.status
    }
    pub fn assignee_name(&self) -> Option<&str> {
        self.fields
            .assignee
            .as_ref()
            .map(|u| u.display_name.as_str())
    }
}

#[derive(Debug, Clone)]
pub struct RemoteComment {
    pub id: String,
    pub author: Option<UserRef>,
    pub created: Option<DateTime<Utc>>,
    pub updated: Option<DateTime<Utc>>,
    pub body: RichText,
}

#[derive(Debug, Clone)]
pub struct RemoteWorklog {
    pub id: String,
    pub author: Option<UserRef>,
    /// When the work started, which is what a worklog is dated by.
    pub started: Option<DateTime<Utc>>,
    pub created: Option<DateTime<Utc>>,
    pub updated: Option<DateTime<Utc>>,
    /// Jira's own rendering of the duration — `2h 30m`.
    pub time_spent: String,
    pub time_spent_seconds: u64,
    pub comment: RichText,
}

/// A worklog on its way to the server.
///
/// `time_spent` goes out as the text it was written as: how long a day is is an
/// instance setting, and sending seconds would silently answer that question.
#[derive(Debug, Clone)]
pub struct WorklogInput {
    pub time_spent: String,
    pub started: Option<DateTime<Utc>>,
    pub comment: RichText,
}

impl WorklogInput {
    /// The request body a worklog write sends, on either deployment.
    pub fn to_body(&self) -> serde_json::Value {
        let mut body = serde_json::json!({ "timeSpent": self.time_spent });
        if let Some(started) = &self.started {
            body["started"] = serde_json::json!(format_worklog_started(started));
        }
        if !self.comment.is_empty() {
            body["comment"] = self.comment.to_field_value();
        }
        body
    }
}

/// The timestamp format Jira's worklog endpoint insists on: milliseconds, and
/// a numeric offset with no colon in it.
pub fn format_worklog_started(dt: &DateTime<Utc>) -> String {
    dt.format("%Y-%m-%dT%H:%M:%S%.3f%z").to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteAttachment {
    pub id: String,
    pub filename: String,
    pub size: u64,
    pub mime: Option<String>,
    pub created: Option<DateTime<Utc>>,
    pub author: Option<String>,
    pub content_url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Transition {
    pub id: String,
    pub name: String,
    pub to_status: String,
    pub to_status_id: Option<String>,
    /// Field ids the transition screen requires.
    pub required_fields: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FieldMeta {
    pub id: String,
    pub name: String,
    pub custom: bool,
    #[serde(default)]
    pub schema_type: Option<String>,
    /// Element type of an array field — `option`, `user`, `string`, … — which
    /// is what decides how one of its values has to be written back.
    #[serde(default)]
    pub items: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChangelogEntry {
    pub id: String,
    pub author: Option<String>,
    pub created: Option<DateTime<Utc>>,
    pub items: Vec<ChangelogItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChangelogItem {
    pub field: String,
    pub from: Option<String>,
    pub to: Option<String>,
}

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamps_normalize_to_utc() {
        let cloud = parse_timestamp("2026-08-16T14:03:00.000+0200").unwrap();
        assert_eq!(format_timestamp(&cloud), "2026-08-16T12:03:00Z");

        let rfc = parse_timestamp("2026-08-16T14:03:00+02:00").unwrap();
        assert_eq!(cloud, rfc);

        let zulu = parse_timestamp("2026-08-16T12:03:00Z").unwrap();
        assert_eq!(cloud, zulu);

        assert!(parse_timestamp("not a date").is_none());
    }

    /// The instant is unchanged; only the clock it is read off is.
    #[test]
    fn a_local_rendering_names_the_same_instant() {
        let t = parse_timestamp("2026-08-16T12:03:00Z").unwrap();
        assert_eq!(parse_timestamp(&format_local_iso(&t)), Some(t));
        // Display drops the zone marker, so it is the reader's own clock or
        // nothing — never UTC wearing a local face.
        assert!(!format_local(&t).ends_with('Z'));
        assert_eq!(format_local(&t).len(), "2026-08-16 12:03:00".len());
    }

    /// Whatever the form prints, someone can type back.
    #[test]
    fn a_time_typed_without_a_zone_is_read_as_the_readers_own() {
        let t = parse_timestamp("2026-08-16T12:03:00Z").unwrap();
        let shown = format_local(&t);
        assert_eq!(parse_local_timestamp(&shown), Some(t));

        // The same reading without seconds, which is how people write times.
        let to_the_minute = shown.rsplit_once(':').map(|(head, _)| head).unwrap();
        assert_eq!(parse_local_timestamp(to_the_minute), Some(t));
    }

    /// An explicit zone is never second-guessed.
    #[test]
    fn a_time_that_names_its_zone_keeps_it() {
        for text in [
            "2026-08-16T12:03:00Z",
            "2026-08-16T14:03:00+02:00",
            "2026-08-16T14:03:00.000+0200",
        ] {
            assert_eq!(
                parse_local_timestamp(text),
                parse_timestamp(text),
                "{text} should parse the same either way"
            );
        }
        assert!(parse_local_timestamp("not a date").is_none());
    }

    #[test]
    fn date_only_values_parse_as_midnight_utc() {
        let d = parse_timestamp("2026-08-16").unwrap();
        assert_eq!(format_timestamp(&d), "2026-08-16T00:00:00Z");
    }

    #[test]
    fn a_worklog_start_time_carries_milliseconds_and_a_bare_offset() {
        let dt = parse_timestamp("2026-08-15T09:00:00Z").unwrap();
        assert_eq!(format_worklog_started(&dt), "2026-08-15T09:00:00.000+0000");
    }

    #[test]
    fn jql_literal_has_minute_precision() {
        let dt = parse_timestamp("2026-08-16T12:03:45Z").unwrap();
        assert_eq!(jql_timestamp(&dt), "2026-08-16 12:03");
    }

    #[test]
    fn rich_text_storage_round_trips() {
        let adf = RichText::Adf(serde_json::json!({"type":"doc","content":[]}));
        let back = RichText::from_storage(Markup::Adf, &adf.to_storage());
        assert_eq!(adf, back);

        let wiki = RichText::Wiki("h1. Title".into());
        assert_eq!(
            RichText::from_storage(Markup::Wiki, &wiki.to_storage()),
            wiki
        );

        assert_eq!(RichText::from_storage(Markup::Adf, ""), RichText::Empty);
    }

    #[test]
    fn emptiness_is_markup_aware() {
        assert!(RichText::Empty.is_empty());
        assert!(RichText::Wiki("   ".into()).is_empty());
        assert!(RichText::Adf(serde_json::json!({"type":"doc","content":[]})).is_empty());
        assert!(
            !RichText::Adf(serde_json::json!({"type":"doc","content":[{"type":"paragraph"}]}))
                .is_empty()
        );
    }
}
