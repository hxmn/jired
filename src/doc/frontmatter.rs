//! YAML frontmatter of an issue file [M1.11, M2.1].
//!
//! The schema is a published contract (`schemas/frontmatter.json`) that agents
//! validate against, so field names and their editability are fixed here and
//! nowhere else.

use crate::error::{JiredError, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Fields a user may change; a push turns a difference into an API call.
pub const EDITABLE: &[&str] = &[
    "type",
    "status",
    "priority",
    "summary",
    "labels",
    "components",
    "assignee",
    "sprint",
    "parent",
    "epic",
    "fix_versions",
    "affects_versions",
    "due_date",
    "links",
    "story_points",
    "custom",
];

/// Fields that only report server state. Local edits are dropped with a
/// warning on the next write.
pub const READ_ONLY: &[&str] = &["reporter", "created", "updated", "url", "jired"];

/// Fields that identify the issue. Changing one is an error, not a warning:
/// jired would otherwise write one issue's content over another's.
pub const IDENTITY: &[&str] = &["key", "id"];

pub fn is_editable(field: &str) -> bool {
    EDITABLE.contains(&field)
}

/// Whether two `sprint:` lists differ only in the sprints the issue has been
/// through, and not in the one it is in now.
///
/// The field is a history, oldest first, and the board API can only move an
/// issue into a sprint — so the last entry is the whole of what anyone can act
/// on. The earlier entries are the server's record of what happened, which no
/// API rewrites. A difference confined to them is not work to do, which is why
/// `push` refuses to send it and why `status` and `diff` do not count it as a
/// change waiting to go out.
pub fn sprint_history_only(before: &[String], after: &[String]) -> bool {
    before != after && before.last() == after.last()
}

pub fn is_identity(field: &str) -> bool {
    IDENTITY.contains(&field)
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Frontmatter {
    pub key: String,
    pub id: String,
    #[serde(rename = "type", default)]
    pub issue_type: String,
    #[serde(default)]
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<String>,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub labels: Vec<String>,
    #[serde(default)]
    pub components: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assignee: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reporter: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated: Option<String>,
    /// Sprints the issue belongs to, oldest first; empty is the backlog.
    #[serde(
        default,
        with = "sprint_field",
        skip_serializing_if = "Vec::is_empty",
        alias = "sprints"
    )]
    pub sprint: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub epic: Option<String>,
    #[serde(default)]
    pub fix_versions: Vec<String>,
    #[serde(default)]
    pub affects_versions: Vec<String>,
    /// Typed links as single-entry maps: `- blocks: PROJ-150`.
    #[serde(default)]
    pub links: Vec<BTreeMap<String, String>>,
    #[serde(default)]
    pub url: String,
    /// Jira's own `duedate`: a date, no time — `2026-09-14`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub due_date: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub story_points: Option<f64>,
    /// Custom fields with no well-known mapping, kept so they round-trip.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub custom: BTreeMap<String, serde_json::Value>,
    pub jired: SyncMeta,
    /// Anything a user or a future jired added; preserved verbatim.
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct SyncMeta {
    #[serde(default)]
    pub base_updated: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_version: Option<i64>,
    #[serde(default)]
    pub content_hash: String,
    #[serde(default)]
    pub lossy: bool,
}

impl Frontmatter {
    pub fn parse(yaml: &str) -> Result<Self> {
        let fm: Frontmatter = serde_yaml_ng::from_str(yaml).map_err(|e| {
            // serde_yaml reports the line and column within the header, which is
            // the difference between "somewhere in this file" and a cursor
            // position; keep it, and say the offset is into the header.
            let mut err = JiredError::usage(format!("the YAML header is not valid: {e}"))
                .with_hint("the header must contain at least `key` and `id`");
            if let Some(loc) = e.location() {
                err = err
                    .with_detail("line", loc.line())
                    .with_detail("column", loc.column());
            }
            err
        })?;
        if fm.key.is_empty() {
            return Err(JiredError::usage("the frontmatter has no `key`").with_hint(
                "every issue file needs `key: PROJ-142`; `jired pull --force` restores it",
            ));
        }
        Ok(fm)
    }

    pub fn to_yaml(&self) -> Result<String> {
        Ok(serde_yaml_ng::to_string(self)?)
    }

    /// Typed links flattened to a comparable form.
    pub fn link_pairs(&self) -> Vec<(String, String)> {
        let mut pairs: Vec<(String, String)> = self
            .links
            .iter()
            .flat_map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())))
            .collect();
        pairs.sort();
        pairs
    }

    pub fn set_links(&mut self, pairs: &[(String, String)]) {
        self.links = pairs
            .iter()
            .map(|(t, k)| BTreeMap::from([(t.clone(), k.clone())]))
            .collect();
    }

    /// Value of a field by its frontmatter name, for generic comparison.
    pub fn field(&self, name: &str) -> Option<serde_json::Value> {
        let v = match name {
            "type" => serde_json::json!(self.issue_type),
            "status" => serde_json::json!(self.status),
            "priority" => serde_json::json!(self.priority),
            "summary" => serde_json::json!(self.summary),
            "labels" => serde_json::json!(self.labels),
            "components" => serde_json::json!(self.components),
            "assignee" => serde_json::json!(self.assignee),
            "reporter" => serde_json::json!(self.reporter),
            "sprint" => serde_json::json!(self.sprint),
            "parent" => serde_json::json!(self.parent),
            "epic" => serde_json::json!(self.epic),
            "fix_versions" => serde_json::json!(self.fix_versions),
            "affects_versions" => serde_json::json!(self.affects_versions),
            "links" => serde_json::json!(self.link_pairs()),
            "due_date" => serde_json::json!(self.due_date),
            "story_points" => serde_json::json!(self.story_points),
            "custom" => serde_json::json!(self.custom),
            _ => return None,
        };
        Some(v)
    }

    /// Write a field by its frontmatter name, from the text a person would type.
    ///
    /// The inverse of [`Frontmatter::field`], and the reason `jired set` does
    /// not need a flag per field. Reports whether the value actually moved, so
    /// a command can tell "already had that" from "changed it" — the difference
    /// between reporting and staging.
    ///
    /// An empty `value` clears the field, which is what a push sends as `null`.
    /// A list takes its members comma-separated; a value with a comma in it is
    /// not something Jira's own list fields can hold anyway.
    pub fn set_field(&mut self, name: &str, value: &str) -> Result<bool> {
        let before = self.field(name);
        let text = value.trim();
        let some = |t: &str| (!t.is_empty()).then(|| t.to_string());
        let list = |t: &str| -> Vec<String> {
            t.split(',')
                .map(str::trim)
                .filter(|p| !p.is_empty())
                .map(str::to_string)
                .collect()
        };
        match name {
            "summary" => self.summary = text.to_string(),
            "status" => self.status = text.to_string(),
            "type" => self.issue_type = text.to_string(),
            "priority" => self.priority = some(text),
            "assignee" => self.assignee = some(text),
            "parent" => self.parent = some(text).map(|p| p.to_uppercase()),
            "epic" => self.epic = some(text).map(|p| p.to_uppercase()),
            "due_date" => {
                // Jira's date fields are days, not instants, and it refuses
                // anything else — so a mistyped one is caught here rather than
                // at the end of a push.
                if !text.is_empty() && chrono::NaiveDate::parse_from_str(text, "%Y-%m-%d").is_err()
                {
                    return Err(JiredError::usage(format!("`{text}` is not a date"))
                        .with_hint("Jira due dates are days: `2026-09-14`"));
                }
                self.due_date = some(text);
            }
            "labels" => self.labels = list(text),
            "components" => self.components = list(text),
            "fix_versions" => self.fix_versions = list(text),
            "affects_versions" => self.affects_versions = list(text),
            // The list is a history and only its last entry is where the issue
            // is now, so setting it means moving — the same rule
            // `jired sprint add` follows.
            "sprint" => {
                self.sprint.retain(|s| !s.eq_ignore_ascii_case(text));
                if !text.is_empty() {
                    self.sprint.push(text.to_string());
                }
            }
            "story_points" => {
                self.story_points = match some(text) {
                    None => None,
                    Some(n) => Some(n.parse::<f64>().map_err(|_| {
                        JiredError::usage(format!("`{text}` is not a number of story points"))
                    })?),
                }
            }
            // A custom field is addressed by its id, which is what the file
            // shows and what the server wants.
            id if id.starts_with("customfield_") => {
                match some(text) {
                    None => {
                        self.custom.remove(id);
                    }
                    Some(v) => {
                        // A value that reads as JSON is taken as JSON — that is
                        // how a multi-select or a field jired could not
                        // simplify is written in the file, so it has to be
                        // writable the same way.
                        let parsed = serde_json::from_str(&v)
                            .unwrap_or(serde_json::Value::String(v.clone()));
                        self.custom.insert(id.to_string(), parsed);
                    }
                }
                // `field("custom")` compares the whole map, which is the right
                // comparison for one entry inside it too.
                return Ok(before != self.field("custom"));
            }
            "links" => {
                return Err(JiredError::usage(
                    "`links` cannot be set from the command line".to_string(),
                )
                .with_hint("edit `links:` in the file — one `- type: KEY` per line"))
            }
            other => {
                return Err(
                    JiredError::usage(format!("`{other}` is not a field jired can set")).with_hint(
                        format!("one of: {}, or a customfield_* id", EDITABLE.join(", ")),
                    ),
                )
            }
        }
        Ok(before != self.field(name))
    }
}

/// `sprint:` holds one name or several.
///
/// An issue that rolls over belongs to the sprint it came from as well as the
/// one it moved to — Jira's own field is a list for exactly that reason. One
/// sprint is still written as a plain string, so a file that has only ever seen
/// one reads exactly as it always did.
mod sprint_field {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(value: &[String], s: S) -> Result<S::Ok, S::Error> {
        match value {
            [one] => s.serialize_str(one),
            many => s.collect_seq(many),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<String>, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum OneOrMany {
            One(String),
            Many(Vec<String>),
        }
        Ok(match Option::<OneOrMany>::deserialize(d)? {
            None => Vec::new(),
            Some(OneOrMany::One(one)) => vec![one],
            Some(OneOrMany::Many(many)) => many,
        })
    }
}

/// Render a mention the way the file format spells it: `@[Name](id)`.
pub fn format_user(user: Option<&crate::api::UserRef>) -> Option<String> {
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

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
key: PROJ-142
id: "10023"
type: Story
status: In Progress
priority: High
summary: Fix login flow
labels:
  - auth
  - backend
components: [web]
assignee: "@[Ilya A](5b10a2)"
reporter: "@[Jane D](5b1af9)"
created: 2026-08-01T09:12:00Z
updated: 2026-08-16T14:03:00Z
sprint: Sprint 42
parent: PROJ-100
epic: PROJ-90
fix_versions: ["2.4.0"]
affects_versions: []
links:
  - blocks: PROJ-150
  - relates to: PROJ-99
url: https://x.atlassian.net/browse/PROJ-142
story_points: 5
custom:
  customfield_10456:
    value: Platform
jired:
  base_updated: 2026-08-16T14:03:00Z
  base_version: 7
  content_hash: "sha256:9f2c"
  lossy: false
"#;

    #[test]
    fn parses_the_documented_shape() {
        let fm = Frontmatter::parse(SAMPLE).unwrap();
        assert_eq!(fm.key, "PROJ-142");
        assert_eq!(fm.id, "10023");
        assert_eq!(fm.issue_type, "Story");
        assert_eq!(fm.labels, ["auth", "backend"]);
        assert_eq!(fm.story_points, Some(5.0));
        assert_eq!(
            fm.link_pairs(),
            vec![
                ("blocks".to_string(), "PROJ-150".to_string()),
                ("relates to".to_string(), "PROJ-99".to_string())
            ]
        );
        assert_eq!(fm.jired.base_version, Some(7));
        assert!(!fm.jired.lossy);
        assert_eq!(
            fm.custom.get("customfield_10456"),
            Some(&serde_json::json!({"value": "Platform"}))
        );
    }

    /// The field is a list, but a file with one sprint has to keep reading the
    /// way it was written; and a file with several has to survive a round trip.
    #[test]
    fn one_sprint_stays_a_string_and_several_stay_a_list() {
        let fm = Frontmatter::parse(SAMPLE).unwrap();
        assert_eq!(fm.sprint, ["Sprint 42"]);
        assert!(
            fm.to_yaml().unwrap().contains("sprint: Sprint 42"),
            "{}",
            fm.to_yaml().unwrap()
        );

        let many = SAMPLE.replace("sprint: Sprint 42", "sprint:\n  - Sprint 41\n  - Sprint 42");
        let fm = Frontmatter::parse(&many).unwrap();
        assert_eq!(fm.sprint, ["Sprint 41", "Sprint 42"]);
        let back = Frontmatter::parse(&fm.to_yaml().unwrap()).unwrap();
        assert_eq!(back.sprint, ["Sprint 41", "Sprint 42"]);

        // No sprint at all is the backlog, and writes nothing.
        let none = SAMPLE.replace("sprint: Sprint 42\n", "");
        let fm = Frontmatter::parse(&none).unwrap();
        assert!(fm.sprint.is_empty());
        assert!(!fm.to_yaml().unwrap().contains("sprint"));
    }

    #[test]
    fn round_trips_through_yaml() {
        let fm = Frontmatter::parse(SAMPLE).unwrap();
        let back = Frontmatter::parse(&fm.to_yaml().unwrap()).unwrap();
        assert_eq!(fm, back);
    }

    #[test]
    fn unknown_keys_survive_a_round_trip() {
        let yaml = format!("{SAMPLE}\nmy_extension: keep-me\n");
        let fm = Frontmatter::parse(&yaml).unwrap();
        assert_eq!(
            fm.extra.get("my_extension"),
            Some(&serde_json::json!("keep-me"))
        );
        assert!(fm.to_yaml().unwrap().contains("keep-me"));
    }

    /// A file in this state was hand-edited, which makes it a usage error
    /// (exit 2) rather than an unexplained internal failure (exit 1).
    #[test]
    fn frontmatter_without_a_key_is_rejected() {
        let err = Frontmatter::parse("id: \"1\"\njired: {}\n").unwrap_err();
        assert_eq!(err.exit_code().as_i32(), 2);
        assert!(err.hint.is_some(), "the user needs to know what to add");
    }

    /// A YAML mistake should point at the line it is on.
    #[test]
    fn a_broken_header_reports_where() {
        let err = Frontmatter::parse("key: PROJ-1\nlabels: [unclosed\n").unwrap_err();
        assert_eq!(err.exit_code().as_i32(), 2);
        assert!(err.details.get("line").is_some(), "{:?}", err.details);
    }

    #[test]
    fn editability_classes_are_disjoint_and_complete() {
        for f in EDITABLE {
            assert!(!READ_ONLY.contains(f) && !IDENTITY.contains(f), "{f}");
        }
        for f in READ_ONLY {
            assert!(!IDENTITY.contains(f), "{f}");
        }
        // Every editable field must be readable generically for merging.
        let fm = Frontmatter::parse(SAMPLE).unwrap();
        for f in EDITABLE {
            assert!(fm.field(f).is_some(), "{f} is not comparable");
        }
    }

    #[test]
    fn mention_syntax_round_trips() {
        let user = crate::api::UserRef {
            id: "5b10a2".into(),
            display_name: "Ilya A".into(),
        };
        let text = format_user(Some(&user)).unwrap();
        assert_eq!(text, "@[Ilya A](5b10a2)");
        assert_eq!(parse_user(&text).as_deref(), Some("5b10a2"));
        // A hand-typed bare id must keep working.
        assert_eq!(parse_user("5b10a2").as_deref(), Some("5b10a2"));
        assert_eq!(parse_user("  "), None);
    }
}
