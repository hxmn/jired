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
    pub names: BTreeMap<String, String>,
}

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
                _ => {}
            }
            map.names.insert(f.id.clone(), f.name.clone());
        }
        map
    }

    fn is_well_known(&self, id: &str) -> bool {
        [&self.story_points, &self.epic_link, &self.sprint]
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
            .and_then(sprint_name),
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
        subtasks: subtasks(f.get("subtasks")),
        custom: custom_fields(f, map),
    };

    Ok(RemoteIssue {
        key,
        id,
        updated,
        description,
        fields,
        comments: comments_from(f.get("comment"), markup),
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

pub fn comments_from(v: Option<&Value>, markup: Markup) -> Vec<RemoteComment> {
    let list = match v {
        Some(Value::Object(o)) => o.get("comments").and_then(Value::as_array).cloned(),
        Some(Value::Array(a)) => Some(a.clone()),
        _ => None,
    };
    list.unwrap_or_default()
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
        .collect()
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

/// Data Center returns sprints as `...Sprint@1[id=5,name=Sprint 42,...]`.
fn sprint_name(v: &Value) -> Option<String> {
    let last = match v {
        Value::Array(a) => a.last()?,
        other => other,
    };
    if let Some(name) = last.get("name").and_then(Value::as_str) {
        return Some(name.to_string());
    }
    let s = last.as_str()?;
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
        .map(|a| {
            a.iter()
                .filter_map(|s| {
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
                })
                .collect()
        })
        .unwrap_or_default()
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
        ])
    }

    fn cloud_issue() -> Value {
        json!({
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
        })
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
        assert_eq!(issue.fields.sprint.as_deref(), Some("Sprint 42"));
        assert_eq!(issue.fields.fix_versions, ["2.4.0"]);
        assert_eq!(issue.comments.len(), 1);
        assert_eq!(issue.attachments[0].filename, "trace.har");
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

    #[test]
    fn subtask_done_state_comes_from_the_status_category() {
        let issue = issue_from_json(&cloud_issue(), Markup::Adf, &field_map()).unwrap();
        assert_eq!(issue.fields.subtasks.len(), 2);
        assert!(!issue.fields.subtasks[0].done);
        assert!(issue.fields.subtasks[1].done);
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
        assert_eq!(sprint_name(&v).as_deref(), Some("Sprint 42"));
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
