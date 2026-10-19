//! Turning raw Jira payloads into [`RemoteIssue`] [M1.6].
//!
//! Both deployments go through here. The differences it absorbs: ADF objects vs
//! wiki strings, `accountId` vs `name`, custom-field ids that differ per
//! instance, and Data Center's stringified sprint values.

use crate::api::model::*;
use crate::api::Markup;
use crate::error::{JiredError, Result};
use serde_json::Value;
use std::collections::BTreeMap;

/// Custom-field ids discovered from `/rest/api/*/field`.
///
/// Ids are instance-specific, so nothing may be hard-coded; the well-known
/// fields are matched by display name and everything else round-trips by id.
#[derive(Debug, Clone, Default)]
pub struct FieldMap {
    pub story_points: Option<String>,
    pub epic_link: Option<String>,
    pub sprint: Option<String>,
    /// The checklist plugin's own field, and the text mirror of it that the
    /// plugin keeps alongside — either one is enough to render the checklist,
    /// and the text one is the only one that can be written back.
    pub checklist: Option<String>,
    pub checklist_text: Option<String>,
    pub checklist_progress: Option<String>,
    pub names: BTreeMap<String, String>,
    /// Field ids the workspace has asked to keep out of issue files.
    pub ignored: std::collections::BTreeSet<String>,
}

/// Display names the checklist plugin is known to give its two fields.
///
/// Spelled out rather than matched loosely: an instance is free to have a
/// `Checklist due date` of its own, and writing the plugin's syntax into it
/// would be worse than not finding the plugin at all. `checklist.field` is the
/// way out for a name no list can predict.
const CHECKLIST_NAMES: [&str; 6] = [
    "checklist",
    "checklists",
    "checklist text",
    "smart checklist",
    "smart checklists",
    "smart checklist text",
];

impl FieldMap {
    pub fn from_meta(fields: &[FieldMeta]) -> Self {
        let mut map = FieldMap::default();
        for f in fields {
            let name = f.name.trim().to_ascii_lowercase();
            match name.as_str() {
                // Company-managed and team-managed projects name this differently.
                "story points" | "story point estimate" => {
                    map.story_points.get_or_insert(f.id.clone());
                }
                "epic link" => {
                    map.epic_link.get_or_insert(f.id.clone());
                }
                "sprint" => {
                    map.sprint.get_or_insert(f.id.clone());
                }
                "checklist progress" | "smart checklist progress" => {
                    map.checklist_progress.get_or_insert(f.id.clone());
                }
                // Which of the plugin's two fields this is cannot be read off
                // the name: one instance calls them `Checklist` and `Checklist
                // Text`, another `Smart Checklist` and `Checklists`. The schema
                // does say — the text mirror is a `string`, the plugin's own
                // field is `any` — and getting it wrong means writing the
                // syntax into a field that does not read it.
                name if CHECKLIST_NAMES.contains(&name) => {
                    match f.schema_type.as_deref() {
                        Some("string") => map.checklist_text.get_or_insert(f.id.clone()),
                        _ => map.checklist.get_or_insert(f.id.clone()),
                    };
                }
                _ => {}
            }
            map.names.insert(f.id.clone(), f.name.clone());
        }
        map
    }

    /// Note which fields `ignored_custom_fields` names.
    ///
    /// Entries are ids or display names, because an id is unmemorable and a
    /// name is what the person reading the issue actually sees. Nothing is
    /// dropped from the database by this — only from the file — so removing an
    /// entry brings the field back on the next write.
    pub fn ignore(&mut self, entries: &[String]) {
        for entry in entries {
            let entry = entry.trim();
            if entry.is_empty() {
                continue;
            }
            self.ignored.extend(
                self.names
                    .iter()
                    .filter(|(id, name)| {
                        id.as_str() == entry || name.trim().eq_ignore_ascii_case(entry)
                    })
                    .map(|(id, _)| id.clone()),
            );
            // An id the instance never reported is still worth honouring: the
            // field metadata may simply be older than the config.
            if entry.starts_with("customfield_") {
                self.ignored.insert(entry.to_string());
            }
        }
    }

    pub fn is_ignored(&self, id: &str) -> bool {
        self.ignored.contains(id)
    }

    /// Point the checklist at the field `checklist.field` names.
    ///
    /// By id or by display name, the way `ignored_custom_fields` accepts both:
    /// the plugin's field ids differ per instance, and an instance that names
    /// them something no list here predicts would otherwise have no checklist
    /// at all. The named field becomes the text one — the writable half — so
    /// this is also how a workspace says which field a push may edit.
    pub fn set_checklist_text(&mut self, entry: &str) {
        let entry = entry.trim();
        if entry.is_empty() {
            return;
        }
        let by_name = self
            .names
            .iter()
            .find(|(id, name)| id.as_str() == entry || name.trim().eq_ignore_ascii_case(entry))
            .map(|(id, _)| id.clone());
        // An id the instance never reported is still honoured, for the same
        // reason `ignore` honours one: the field metadata may be older than the
        // config, and a field jired cannot see is one it also cannot refuse.
        if let Some(id) =
            by_name.or_else(|| entry.starts_with("customfield_").then(|| entry.to_string()))
        {
            self.checklist_text = Some(id);
        }
    }

    /// True when this field has a home of its own in the file — `story_points`,
    /// `sprint`, `epic`, the checklist section — so it never belongs under
    /// `custom:`, whatever an older jired wrote there.
    pub fn is_well_known(&self, id: &str) -> bool {
        [
            &self.story_points,
            &self.epic_link,
            &self.sprint,
            &self.checklist,
            &self.checklist_text,
            &self.checklist_progress,
        ]
        .iter()
        .any(|f| f.as_deref() == Some(id))
    }
}

pub fn issue_from_json(v: &Value, markup: Markup, map: &FieldMap) -> Result<RemoteIssue> {
    let key = str_at(v, "key")
        .ok_or_else(|| JiredError::generic("issue payload has no key"))?
        .to_string();
    let id = str_at(v, "id").unwrap_or("").to_string();
    let f = v.get("fields").unwrap_or(&Value::Null);

    let updated = f
        .get("updated")
        .and_then(Value::as_str)
        .and_then(parse_timestamp)
        .ok_or_else(|| {
            JiredError::generic(format!("issue {key} has no usable `updated` timestamp"))
        })?;

    let description = rich_text(f.get("description"), markup);

    let fields = IssueFields {
        summary: f
            .get("summary")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        issue_type: nested_name(f, "issuetype").unwrap_or_default(),
        status: nested_name(f, "status").unwrap_or_default(),
        status_id: f
            .get("status")
            .and_then(|s| s.get("id"))
            .and_then(Value::as_str)
            .map(str::to_string),
        priority: nested_name(f, "priority"),
        labels: string_array(f.get("labels")),
        components: named_array(f.get("components")),
        assignee: user_ref(f.get("assignee")),
        reporter: user_ref(f.get("reporter")),
        created: f
            .get("created")
            .and_then(Value::as_str)
            .and_then(parse_timestamp),
        updated: Some(updated),
        sprint: map
            .sprint
            .as_ref()
            .and_then(|id| f.get(id))
            .map(sprint_names)
            .unwrap_or_default(),
        parent: f
            .get("parent")
            .and_then(|p| p.get("key"))
            .and_then(Value::as_str)
            .map(str::to_string),
        epic: epic_key(f, map),
        fix_versions: named_array(f.get("fixVersions")),
        affects_versions: named_array(f.get("versions")),
        links: issue_links(f.get("issuelinks")),
        story_points: map
            .story_points
            .as_ref()
            .and_then(|id| f.get(id))
            .and_then(Value::as_f64),
        // A system field, so it is read by name rather than through the map —
        // and read at all, which `custom:` never did: `custom_fields` only
        // collects `customfield_*`, so a due date set on the server reached no
        // file and could be written but never seen.
        due_date: f
            .get("duedate")
            .and_then(Value::as_str)
            .filter(|d| !d.is_empty())
            .map(str::to_string),
        subtasks: subtasks(f.get("subtasks")),
        checklist: checklist(f, map),
        custom: custom_fields(f, map),
    };

    let (worklogs, worklog_total) = worklogs_from(f.get("worklog"), markup);
    let (comments, comment_total) = comments_from(f.get("comment"), markup);

    Ok(RemoteIssue {
        key,
        id,
        updated,
        description,
        fields,

        worklogs,
        comments,
        comment_total,
        worklog_total,
        attachments: attachments_from(f.get("attachment")),
        raw: v.clone(),
    })
}

pub fn rich_text(v: Option<&Value>, markup: Markup) -> RichText {
    match v {
        None | Some(Value::Null) => RichText::Empty,
        Some(Value::String(s)) if s.trim().is_empty() => RichText::Empty,
        Some(Value::String(s)) => RichText::Wiki(s.clone()),
        Some(obj) => {
            if markup == Markup::Adf {
                RichText::Adf(obj.clone())
            } else {
                // A DC instance behind an unexpected proxy could still send an
                // object; keep it rather than dropping the content.
                RichText::Adf(obj.clone())
            }
        }
    }
}

/// The comments a payload carries, and how many the server says there are.
///
/// An issue payload holds only the first page of them — 50 on a stock instance
/// — so the total is what tells a caller it has to ask for the rest. A bare
/// array carries no total and is taken to be all of itself, which is what the
/// single-comment responses a write returns are.
pub fn comments_from(v: Option<&Value>, markup: Markup) -> (Vec<RemoteComment>, usize) {
    let (list, total) = match v {
        Some(Value::Object(o)) => (
            o.get("comments").and_then(Value::as_array).cloned(),
            o.get("total").and_then(Value::as_u64).map(|n| n as usize),
        ),
        Some(Value::Array(a)) => (Some(a.clone()), None),
        _ => (None, None),
    };
    let comments: Vec<RemoteComment> = list
        .unwrap_or_default()
        .iter()
        .filter_map(|c| {
            Some(RemoteComment {
                id: str_at(c, "id")?.to_string(),
                author: user_ref(c.get("author")),
                created: c
                    .get("created")
                    .and_then(Value::as_str)
                    .and_then(parse_timestamp),
                updated: c
                    .get("updated")
                    .and_then(Value::as_str)
                    .and_then(parse_timestamp),
                body: rich_text(c.get("body"), markup),
            })
        })
        .collect();
    let total = total.unwrap_or(comments.len()).max(comments.len());
    (comments, total)
}

/// The worklogs a payload carries, and how many the server says there are.
///
/// An issue payload holds only the first page — 20 on a stock instance — so the
/// total is what tells a caller it has to ask for the rest.
pub fn worklogs_from(v: Option<&Value>, markup: Markup) -> (Vec<RemoteWorklog>, usize) {
    let (list, total) = match v {
        Some(Value::Object(o)) => (
            o.get("worklogs").and_then(Value::as_array).cloned(),
            o.get("total").and_then(Value::as_u64).map(|n| n as usize),
        ),
        Some(Value::Array(a)) => (Some(a.clone()), None),
        _ => (None, None),
    };
    let worklogs: Vec<RemoteWorklog> = list
        .unwrap_or_default()
        .iter()
        .filter_map(|w| one_worklog(w, markup))
        .collect();
    let total = total.unwrap_or(worklogs.len()).max(worklogs.len());
    (worklogs, total)
}

pub fn one_worklog(w: &Value, markup: Markup) -> Option<RemoteWorklog> {
    Some(RemoteWorklog {
        id: str_at(w, "id")?.to_string(),
        author: user_ref(w.get("author")),
        started: w
            .get("started")
            .and_then(Value::as_str)
            .and_then(parse_timestamp),
        created: w
            .get("created")
            .and_then(Value::as_str)
            .and_then(parse_timestamp),
        updated: w
            .get("updated")
            .and_then(Value::as_str)
            .and_then(parse_timestamp),
        // Older Data Center payloads omit the rendered form; the seconds are
        // always there, so the duration is rebuilt from them rather than lost.
        time_spent: str_at(w, "timeSpent")
            .map(str::to_string)
            .unwrap_or_else(|| {
                render_duration(
                    w.get("timeSpentSeconds")
                        .and_then(Value::as_u64)
                        .unwrap_or(0),
                )
            }),
        time_spent_seconds: w
            .get("timeSpentSeconds")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        comment: rich_text(w.get("comment"), markup),
    })
}

/// Seconds as Jira would write them, assuming its default 8-hour day.
fn render_duration(seconds: u64) -> String {
    let mut left = seconds;
    let mut parts = Vec::new();
    for (unit, scale) in [
        ("w", 5 * 8 * 3600u64),
        ("d", 8 * 3600),
        ("h", 3600),
        ("m", 60),
    ] {
        let n = left / scale;
        if n > 0 {
            parts.push(format!("{n}{unit}"));
            left -= n * scale;
        }
    }
    if parts.is_empty() {
        return "0m".to_string();
    }
    parts.join(" ")
}

pub fn attachments_from(v: Option<&Value>) -> Vec<RemoteAttachment> {
    v.and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|x| {
                    Some(RemoteAttachment {
                        id: str_at(x, "id")?.to_string(),
                        filename: str_at(x, "filename")?.to_string(),
                        size: x.get("size").and_then(Value::as_u64).unwrap_or(0),
                        mime: str_at(x, "mimeType").map(str::to_string),
                        created: x
                            .get("created")
                            .and_then(Value::as_str)
                            .and_then(parse_timestamp),
                        author: x
                            .get("author")
                            .and_then(|a| a.get("displayName"))
                            .and_then(Value::as_str)
                            .map(str::to_string),
                        content_url: str_at(x, "content").unwrap_or_default().to_string(),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

fn epic_key(f: &Value, map: &FieldMap) -> Option<String> {
    // Cloud team-managed projects model epics as the parent; company-managed
    // projects and Data Center use the Epic Link custom field.
    if let Some(id) = &map.epic_link {
        if let Some(k) = f.get(id).and_then(Value::as_str) {
            return Some(k.to_string());
        }
    }
    let parent = f.get("parent")?;
    let is_epic = parent
        .get("fields")
        .and_then(|pf| pf.get("issuetype"))
        .and_then(|t| t.get("name"))
        .and_then(Value::as_str)
        .is_some_and(|n| n.eq_ignore_ascii_case("epic"));
    is_epic
        .then(|| {
            parent
                .get("key")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .flatten()
}

/// Every sprint the field names, in the order Jira lists them — oldest first,
/// so the last one is where the issue is now.
fn sprint_names(v: &Value) -> Vec<String> {
    match v {
        Value::Array(a) => a.iter().filter_map(sprint_name).collect(),
        Value::Null => Vec::new(),
        other => sprint_name(other).into_iter().collect(),
    }
}

/// Data Center returns sprints as `...Sprint@1[id=5,name=Sprint 42,...]`.
fn sprint_name(v: &Value) -> Option<String> {
    if let Some(name) = v.get("name").and_then(Value::as_str) {
        return Some(name.to_string());
    }
    let s = v.as_str()?;
    let start = s.find("name=")? + 5;
    let rest = &s[start..];
    let end = rest.find(',').unwrap_or(rest.len());
    let end = rest[..end].find(']').unwrap_or(end);
    Some(rest[..end].to_string())
}

fn issue_links(v: Option<&Value>) -> Vec<IssueLink> {
    let mut out: Vec<IssueLink> = v
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|l| {
                    let t = l.get("type")?;
                    if let Some(out_issue) = l.get("outwardIssue") {
                        return Some(IssueLink {
                            link_type: t.get("outward")?.as_str()?.to_string(),
                            other_key: out_issue.get("key")?.as_str()?.to_string(),
                        });
                    }
                    let in_issue = l.get("inwardIssue")?;
                    Some(IssueLink {
                        link_type: t.get("inward")?.as_str()?.to_string(),
                        other_key: in_issue.get("key")?.as_str()?.to_string(),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    // Deterministic ordering keeps diffs free of server-side reshuffling.
    out.sort_by(|a, b| (&a.link_type, &a.other_key).cmp(&(&b.link_type, &b.other_key)));
    out
}

fn subtasks(v: Option<&Value>) -> Vec<SubtaskRef> {
    v.and_then(Value::as_array)
        .map(|a| a.iter().filter_map(subtask_ref).collect())
        .unwrap_or_default()
}

/// The sub-tasks a stored issue payload lists, as `## Children` renders them.
pub fn subtasks_of(raw: &Value) -> Vec<SubtaskRef> {
    subtasks(raw.pointer("/fields/subtasks"))
}

/// One entry of an issue's `subtasks` list.
pub fn subtask_ref(s: &Value) -> Option<SubtaskRef> {
    let f = s.get("fields")?;
    let status_category = f
        .get("status")
        .and_then(|st| st.get("statusCategory"))
        .and_then(|c| c.get("key"))
        .and_then(Value::as_str)
        .unwrap_or("");
    Some(SubtaskRef {
        key: s.get("key")?.as_str()?.to_string(),
        summary: f
            .get("summary")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        status: nested_name(f, "status").unwrap_or_default(),
        done: status_category == "done",
    })
}

/// The text mirror is read first because it is the half jired writes: reading
/// and writing the same field is what keeps a pushed checklist from coming back
/// as a different rendering of itself. The plugin's own field is the fallback,
/// for the instances that expose only that one — where the section stays
/// read-only, since there is nothing a push could write.
fn checklist(f: &Value, map: &FieldMap) -> Vec<crate::doc::ChecklistEntry> {
    for id in [&map.checklist_text, &map.checklist].into_iter().flatten() {
        let entries = f
            .get(id)
            .map(crate::api::checklist::from_field)
            .unwrap_or_default();
        if !entries.is_empty() {
            return entries;
        }
    }
    Vec::new()
}

fn custom_fields(f: &Value, map: &FieldMap) -> BTreeMap<String, Value> {
    let Some(obj) = f.as_object() else {
        return BTreeMap::new();
    };
    obj.iter()
        .filter(|(k, v)| k.starts_with("customfield_") && !v.is_null() && !map.is_well_known(k))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

/// The fields an editmeta or createmeta payload says a screen carries [M6.9].
///
/// Both endpoints answer with the same `fields` object keyed by field id, which
/// is why one reader serves both.
pub fn screen_from_json(v: &Value) -> ScreenMeta {
    let mut out = ScreenMeta::default();
    let Some(fields) = v.get("fields").and_then(Value::as_object) else {
        return out;
    };
    for (id, f) in fields {
        out.fields.insert(
            id.clone(),
            FieldSchema {
                id: id.clone(),
                // A screen that names no field still permits it; the id is a
                // worse thing to show a person than a name, and better than
                // nothing.
                name: f
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or(id)
                    .to_string(),
                required: f.get("required").and_then(Value::as_bool).unwrap_or(false),
                operations: f
                    .get("operations")
                    .and_then(Value::as_array)
                    .map(|ops| {
                        ops.iter()
                            .filter_map(Value::as_str)
                            .map(str::to_string)
                            .collect()
                    })
                    .unwrap_or_default(),
            },
        );
    }
    out
}

pub fn user_ref(v: Option<&Value>) -> Option<UserRef> {
    let v = v?;
    if v.is_null() {
        return None;
    }
    // Cloud identifies users by accountId; Data Center by name or key.
    let id = ["accountId", "name", "key"]
        .iter()
        .find_map(|k| v.get(*k).and_then(Value::as_str))?
        .to_string();
    let display_name = v
        .get("displayName")
        .and_then(Value::as_str)
        .unwrap_or(&id)
        .to_string();
    Some(UserRef { id, display_name })
}

fn nested_name(v: &Value, key: &str) -> Option<String> {
    v.get(key)?.get("name")?.as_str().map(str::to_string)
}

fn named_array(v: Option<&Value>) -> Vec<String> {
    v.and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|x| x.get("name").and_then(Value::as_str).map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

fn string_array(v: Option<&Value>) -> Vec<String> {
    v.and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

fn str_at<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key)?.as_str()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn field_map() -> FieldMap {
        FieldMap::from_meta(&[
            FieldMeta {
                id: "customfield_10016".into(),
                name: "Story Points".into(),
                custom: true,
                schema_type: Some("number".into()),
                items: None,
            },
            FieldMeta {
                id: "customfield_10014".into(),
                name: "Epic Link".into(),
                custom: true,
                schema_type: Some("any".into()),
                items: None,
            },
            FieldMeta {
                id: "customfield_10020".into(),
                name: "Sprint".into(),
                custom: true,
                schema_type: Some("array".into()),
                items: Some("json".into()),
            },
            FieldMeta {
                id: "customfield_10205".into(),
                name: "Checklist".into(),
                custom: true,
                schema_type: Some("any".into()),
                items: None,
            },
            FieldMeta {
                id: "customfield_10207".into(),
                name: "Checklist Progress".into(),
                custom: true,
                schema_type: Some("string".into()),
                items: None,
            },
        ])
    }

    /// Kept out of `cloud_issue` so that literal stays inside the `json!`
    /// macro's expansion depth.
    fn cloud_worklog_field() -> Value {
        json!({"startAt": 0, "maxResults": 20, "total": 2, "worklogs": [
            {"id": "10500", "author": {"accountId": "5b1af9", "displayName": "Jane D"},
             "started": "2026-08-15T09:00:00.000+0000",
             "created": "2026-08-15T11:30:00.000+0000",
             "updated": "2026-08-15T11:30:00.000+0000",
             "timeSpent": "2h 30m", "timeSpentSeconds": 9000,
             "comment": {"type": "doc", "version": 1, "content": []}},
            {"id": "10501", "author": {"accountId": "5b10a2", "displayName": "Ilya A"},
             "started": "2026-08-16T08:00:00.000+0000",
             "timeSpentSeconds": 3600}
        ]})
    }

    fn cloud_issue() -> Value {
        let mut v = json!({
            "id": "10023",
            "key": "PROJ-142",
            "fields": {
                "summary": "Fix login flow",
                "issuetype": {"name": "Story"},
                "status": {"id": "3", "name": "In Progress"},
                "priority": {"name": "High"},
                "labels": ["auth", "backend"],
                "components": [{"name": "web"}],
                "assignee": {"accountId": "5b10a2", "displayName": "Ilya A"},
                "reporter": {"accountId": "5b1af9", "displayName": "Jane D"},
                "created": "2026-08-01T09:12:00.000+0000",
                "updated": "2026-08-16T14:03:00.000+0200",
                "description": {"type": "doc", "version": 1, "content": []},
                "fixVersions": [{"name": "2.4.0"}],
                "versions": [],
                "customfield_10016": 5.0,
                "customfield_10014": "PROJ-90",
                "customfield_10020": [{"id": 42, "name": "Sprint 42", "state": "active"}],
                "customfield_10456": {"value": "Platform"},
                "customfield_10205": ["Checklist(id=793, issueId=50383, _items=[Item(id=23, checklistId=793, value=Analyse the request, rank=0, status=Status(id=2, rank=1, statusState=UNCHECKED, name=IN PROGRESS, color=BLUE, default=true, global=true, projectIds=[]), quotes=[], assignees=[], mandatory=false, description=null, weight=null)])"],
                "customfield_10207": "0/1",
                "customfield_10999": null,
                "issuelinks": [
                    {"type": {"outward": "blocks", "inward": "is blocked by"},
                     "outwardIssue": {"key": "PROJ-150"}},
                    {"type": {"outward": "relates to", "inward": "relates to"},
                     "inwardIssue": {"key": "PROJ-99"}}
                ],
                "subtasks": [
                    {"key": "PROJ-143", "fields": {"summary": "Add test",
                     "status": {"name": "To Do", "statusCategory": {"key": "new"}}}},
                    {"key": "PROJ-144", "fields": {"summary": "Update runbook",
                     "status": {"name": "Done", "statusCategory": {"key": "done"}}}}
                ],
                "comment": {"comments": [
                    {"id": "10812", "author": {"accountId": "5b1af9", "displayName": "Jane D"},
                     "created": "2026-08-15T10:32:00.000+0000",
                     "updated": "2026-08-15T10:32:00.000+0000",
                     "body": {"type": "doc", "version": 1, "content": []}}
                ]},
                "attachment": [
                    {"id": "5001", "filename": "trace.har", "size": 2200000,
                     "mimeType": "application/json", "created": "2026-08-15T10:00:00.000+0000",
                     "author": {"displayName": "Jane D"},
                     "content": "https://x.atlassian.net/secure/attachment/5001/trace.har"}
                ]
            }
        });
        v["fields"]["worklog"] = cloud_worklog_field();
        v
    }

    #[test]
    fn cloud_issue_normalizes() {
        let issue = issue_from_json(&cloud_issue(), Markup::Adf, &field_map()).unwrap();
        assert_eq!(issue.key, "PROJ-142");
        assert_eq!(issue.id, "10023");
        assert_eq!(format_timestamp(&issue.updated), "2026-08-16T12:03:00Z");
        assert_eq!(issue.fields.summary, "Fix login flow");
        assert_eq!(issue.fields.issue_type, "Story");
        assert_eq!(issue.fields.status, "In Progress");
        assert_eq!(issue.fields.labels, ["auth", "backend"]);
        assert_eq!(issue.fields.components, ["web"]);
        assert_eq!(issue.fields.assignee.unwrap().display_name, "Ilya A");
        assert_eq!(issue.fields.story_points, Some(5.0));
        assert_eq!(issue.fields.epic.as_deref(), Some("PROJ-90"));
        assert_eq!(issue.fields.sprint, ["Sprint 42"]);
        assert_eq!(issue.fields.fix_versions, ["2.4.0"]);
        assert_eq!(issue.comments.len(), 1);
        assert_eq!(issue.attachments[0].filename, "trace.har");
    }

    /// The duration is the server's own text where it sent one, and rebuilt
    /// from the seconds where it did not — never dropped.
    #[test]
    fn worklogs_normalize_with_their_duration_either_way_round() {
        let issue = issue_from_json(&cloud_issue(), Markup::Adf, &field_map()).unwrap();
        assert_eq!(issue.worklog_total, 2);
        assert_eq!(issue.worklogs.len(), 2);
        assert_eq!(issue.worklogs[0].id, "10500");
        assert_eq!(issue.worklogs[0].time_spent, "2h 30m");
        assert_eq!(issue.worklogs[0].time_spent_seconds, 9000);
        assert_eq!(
            issue.worklogs[0].author.as_ref().unwrap().display_name,
            "Jane D"
        );
        assert_eq!(
            format_timestamp(&issue.worklogs[0].started.unwrap()),
            "2026-08-15T09:00:00Z"
        );
        assert_eq!(issue.worklogs[1].time_spent, "1h");
    }

    /// A payload that pages its worklogs says so, and that is what tells the
    /// client to go and fetch the rest rather than show a partial list.
    #[test]
    fn a_paged_worklog_field_reports_the_full_total() {
        let v = json!({"startAt": 0, "maxResults": 20, "total": 45, "worklogs": [
            {"id": "1", "timeSpent": "1h", "timeSpentSeconds": 3600}
        ]});
        let (worklogs, total) = worklogs_from(Some(&v), Markup::Adf);
        assert_eq!(worklogs.len(), 1);
        assert_eq!(total, 45);

        // No worklog field at all is zero of them, not an unknown number.
        assert_eq!(worklogs_from(None, Markup::Adf).1, 0);
    }

    /// The same for comments, which page at 50 on a stock instance: a long
    /// incident is exactly the issue whose discussion a file would truncate.
    #[test]
    fn a_paged_comment_field_reports_the_full_total() {
        let v = json!({"startAt": 0, "maxResults": 50, "total": 120, "comments": [
            {"id": "1", "body": "first"}
        ]});
        let (comments, total) = comments_from(Some(&v), Markup::Wiki);
        assert_eq!(comments.len(), 1);
        assert_eq!(total, 120);

        // A bare array is a write's answer, carrying one comment and no total;
        // it is all of itself.
        let (one, total) = comments_from(Some(&json!([{"id": "9", "body": "hi"}])), Markup::Wiki);
        assert_eq!((one.len(), total), (1, 1));

        assert_eq!(comments_from(None, Markup::Wiki).1, 0);

        // A server that reports fewer than it sent is not believed downward:
        // the total exists to say whether the list is complete, and a total
        // under the count would say a full list needs completing.
        let v = json!({"total": 1, "comments": [{"id": "1"}, {"id": "2"}]});
        assert_eq!(comments_from(Some(&v), Markup::Wiki).1, 2);
    }

    #[test]
    fn seconds_become_the_duration_jira_would_have_written() {
        assert_eq!(render_duration(0), "0m");
        assert_eq!(render_duration(9000), "2h 30m");
        assert_eq!(render_duration(8 * 3600), "1d");
        assert_eq!(render_duration(40 * 3600 + 60), "1w 1m");
    }

    #[test]
    fn links_are_typed_by_direction_and_sorted() {
        let issue = issue_from_json(&cloud_issue(), Markup::Adf, &field_map()).unwrap();
        assert_eq!(
            issue.fields.links,
            vec![
                IssueLink {
                    link_type: "blocks".into(),
                    other_key: "PROJ-150".into()
                },
                IssueLink {
                    link_type: "relates to".into(),
                    other_key: "PROJ-99".into()
                },
            ]
        );
    }

    /// An issue that rolled over belongs to both, and the one it is in now is
    /// the last of them.
    #[test]
    fn every_sprint_the_issue_belongs_to_is_kept_in_order() {
        let v = json!([
            {"id": 41, "name": "Sprint 41", "state": "closed"},
            {"id": 42, "name": "Sprint 42", "state": "active"}
        ]);
        assert_eq!(sprint_names(&v), ["Sprint 41", "Sprint 42"]);

        // Data Center says the same thing in its own stringified way.
        let dc = json!([
            "com.atlassian...Sprint@1[id=41,state=CLOSED,name=Sprint 41,startDate=x]",
            "com.atlassian...Sprint@2[id=42,state=ACTIVE,name=Sprint 42,startDate=y]"
        ]);
        assert_eq!(sprint_names(&dc), ["Sprint 41", "Sprint 42"]);

        assert!(sprint_names(&json!(null)).is_empty());
    }

    #[test]
    fn subtask_done_state_comes_from_the_status_category() {
        let issue = issue_from_json(&cloud_issue(), Markup::Adf, &field_map()).unwrap();
        assert_eq!(issue.fields.subtasks.len(), 2);
        assert!(!issue.fields.subtasks[0].done);
        assert!(issue.fields.subtasks[1].done);
    }

    /// The plugin's fields are found by name, parsed, and kept out of `custom`:
    /// their raw form is a Java debug dump nobody can usefully edit.
    #[test]
    fn a_checklist_field_is_parsed_rather_than_shown_raw() {
        let issue = issue_from_json(&cloud_issue(), Markup::Adf, &field_map()).unwrap();
        assert_eq!(
            issue.fields.checklist,
            vec![crate::doc::ChecklistEntry::Item(
                crate::doc::ChecklistItem {
                    checked: false,
                    status: Some("IN PROGRESS".into()),
                    title: "Analyse the request".into(),
                    description: None,
                }
            )]
        );
        assert!(!issue.fields.custom.contains_key("customfield_10205"));
        assert!(!issue.fields.custom.contains_key("customfield_10207"));
    }

    /// One instance calls the plugin's two fields `Checklist` and `Checklist
    /// Text`, another `Smart Checklist` and `Checklists`. The name does not say
    /// which is which; the schema does, and getting it wrong means writing the
    /// syntax into the field that does not read it.
    #[test]
    fn the_schema_says_which_of_the_plugins_two_fields_is_the_writable_one() {
        let meta = |id: &str, name: &str, schema: &str| FieldMeta {
            id: id.into(),
            name: name.into(),
            custom: true,
            schema_type: Some(schema.into()),
            items: None,
        };
        let map = FieldMap::from_meta(&[
            meta("customfield_10205", "Smart Checklist", "any"),
            meta("customfield_10206", "Checklists", "string"),
            meta("customfield_10207", "Smart Checklist Progress", "string"),
            // Something else entirely, which the plugin's syntax must not be
            // written into however checklist-shaped its name looks.
            meta("customfield_10300", "Checklist due date", "date"),
        ]);
        assert_eq!(map.checklist.as_deref(), Some("customfield_10205"));
        assert_eq!(map.checklist_text.as_deref(), Some("customfield_10206"));
        assert_eq!(map.checklist_progress.as_deref(), Some("customfield_10207"));
        assert!(!map.is_well_known("customfield_10300"));
    }

    /// `checklist.field` is the way out for a name no list can predict.
    #[test]
    fn the_checklist_field_can_be_named_by_id_or_by_name() {
        let mut map = field_map();
        map.set_checklist_text("Checklist Progress");
        assert_eq!(map.checklist_text.as_deref(), Some("customfield_10207"));

        map.set_checklist_text("customfield_19999");
        assert_eq!(map.checklist_text.as_deref(), Some("customfield_19999"));

        // A name the instance does not have leaves the map alone rather than
        // pointing a write at nothing.
        map.set_checklist_text("Nothing Like It");
        assert_eq!(map.checklist_text.as_deref(), Some("customfield_19999"));
    }

    /// An id is unmemorable and a name is what the issue actually shows, so
    /// `ignored_custom_fields` accepts either.
    #[test]
    fn fields_can_be_ignored_by_id_or_by_name() {
        let mut map = field_map();
        map.ignore(&[
            "customfield_10456".into(),
            "checklist progress".into(),
            "customfield_19999".into(),
            "  ".into(),
        ]);
        assert!(map.is_ignored("customfield_10456"));
        // Matched by display name, case-insensitively.
        assert!(map.is_ignored("customfield_10207"));
        // An id this instance never reported: the config may outlive a rename.
        assert!(map.is_ignored("customfield_19999"));
        assert!(!map.is_ignored("customfield_10016"));

        // Ignoring is about the file, not the payload: `custom` still carries
        // the value, and only rendering leaves it out.
        let issue = issue_from_json(&cloud_issue(), Markup::Adf, &map).unwrap();
        assert!(issue.fields.custom.contains_key("customfield_10456"));
    }

    #[test]
    fn unmapped_custom_fields_round_trip_and_nulls_are_dropped() {
        let issue = issue_from_json(&cloud_issue(), Markup::Adf, &field_map()).unwrap();
        assert_eq!(
            issue.fields.custom.get("customfield_10456"),
            Some(&json!({"value": "Platform"}))
        );
        // Well-known fields are modelled, not duplicated into `custom`.
        assert!(!issue.fields.custom.contains_key("customfield_10016"));
        assert!(!issue.fields.custom.contains_key("customfield_10020"));
        // Empty values carry no information and would add diff noise.
        assert!(!issue.fields.custom.contains_key("customfield_10999"));
    }

    #[test]
    fn data_center_stringified_sprint_is_parsed() {
        let v = json!(["com.atlassian.greenhopper.service.sprint.Sprint@6f2a[id=5,rapidViewId=3,state=ACTIVE,name=Sprint 42,startDate=2026-08-01]"]);
        assert_eq!(sprint_names(&v), ["Sprint 42"]);
    }

    #[test]
    fn data_center_issue_uses_wiki_markup_and_usernames() {
        let v = json!({
            "id": "1", "key": "DC-1",
            "fields": {
                "summary": "s",
                "issuetype": {"name": "Bug"},
                "status": {"name": "Open"},
                "updated": "2026-08-16T14:03:00.000+0200",
                "description": "h1. Title\n\nSome *bold* text.",
                "assignee": {"name": "ilya", "displayName": "Ilya A"}
            }
        });
        let issue = issue_from_json(&v, Markup::Wiki, &FieldMap::default()).unwrap();
        assert_eq!(
            issue.description,
            RichText::Wiki("h1. Title\n\nSome *bold* text.".into())
        );
        assert_eq!(issue.fields.assignee.unwrap().id, "ilya");
    }

    #[test]
    fn an_issue_without_updated_is_rejected_rather_than_guessed() {
        let v = json!({"id": "1", "key": "X-1", "fields": {"summary": "s"}});
        assert!(issue_from_json(&v, Markup::Adf, &FieldMap::default()).is_err());
    }

    #[test]
    fn raw_payload_is_preserved_verbatim() {
        let v = cloud_issue();
        let issue = issue_from_json(&v, Markup::Adf, &field_map()).unwrap();
        assert_eq!(issue.raw, v);
    }
}
