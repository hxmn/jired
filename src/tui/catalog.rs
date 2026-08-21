//! What the workspace already knows, offered as suggestions [M4.7].
//!
//! Both forms — the one that files an issue and the one that edits it — need
//! the same thing: the types this project actually uses, its statuses,
//! priorities, labels, components, people, and its issues as parent candidates.
//! All of it comes from snapshots already on disk, so a form never waits on a
//! network call to draw a menu.
//!
//! The snapshots are read with plain lookups rather than the normalizer: this
//! only needs a handful of well-known fields whose shape is the same on both
//! deployments, and a workspace of ten thousand issues should not pay for a
//! full parse to populate a list.

use crate::store::state::IssueRow;
use std::collections::HashMap;

/// Every list is ordered by how often the workspace uses the value, so the
/// first option is usually the right one.
#[derive(Debug, Clone, Default)]
pub struct Catalog {
    pub projects: Vec<String>,
    pub types: Vec<String>,
    pub statuses: Vec<String>,
    pub priorities: Vec<String>,
    pub labels: Vec<String>,
    pub components: Vec<String>,
    pub people: Vec<KnownUser>,
    pub issues: Vec<KnownIssue>,
}

#[derive(Debug, Clone)]
pub struct KnownIssue {
    pub key: String,
    pub summary: String,
    pub issue_type: String,
    pub status: String,
}

/// Someone this workspace has seen, in the form the file spells them.
#[derive(Debug, Clone)]
pub struct KnownUser {
    /// `@[Ilya A](5b10a2)` — what goes into the frontmatter.
    pub mention: String,
    pub display_name: String,
}

impl Catalog {
    /// Build the catalog from the issues the workspace has pulled.
    ///
    /// `project_hint` is the workspace's own project, which belongs first
    /// whether or not it has the most issues in the snapshot.
    pub fn from_rows(rows: &[IssueRow], project_hint: Option<&str>) -> Self {
        let mut types = Tally::default();
        let mut statuses = Tally::default();
        let mut priorities = Tally::default();
        let mut labels = Tally::default();
        let mut components = Tally::default();
        let mut projects = Tally::default();
        let mut people = Tally::default();
        let mut names: HashMap<String, String> = HashMap::new();
        let mut issues = Vec::new();

        for row in rows {
            // A `NEW-*` issue has no server key yet, so it can be neither a
            // parent nor evidence of what a project uses.
            if row.key.starts_with("NEW-") {
                continue;
            }
            if let Some((project, _)) = row.key.rsplit_once('-') {
                projects.add(project);
            }
            let Some(fields) = row
                .remote
                .as_ref()
                .map(|r| &r.raw)
                .or(row.base.as_ref().map(|b| &b.raw))
                .and_then(|raw| raw.get("fields"))
            else {
                continue;
            };

            let issue_type = text(fields.pointer("/issuetype/name"));
            let status = text(fields.pointer("/status/name"));
            types.add(&issue_type);
            statuses.add(&status);
            priorities.add(&text(fields.pointer("/priority/name")));
            for label in array(fields.get("labels")) {
                labels.add(&text(Some(label)));
            }
            for component in array(fields.get("components")) {
                components.add(&text(component.get("name")));
            }
            for who in ["assignee", "reporter"] {
                if let Some(user) = crate::api::normalize::user_ref(fields.get(who)) {
                    let mention = format!("@[{}]({})", user.display_name, user.id);
                    names.insert(mention.clone(), user.display_name);
                    people.add(&mention);
                }
            }
            issues.push((
                row.remote
                    .as_ref()
                    .map(|r| r.updated)
                    .or(row.base.as_ref().map(|b| b.updated)),
                KnownIssue {
                    key: row.key.clone(),
                    summary: text(fields.get("summary")),
                    issue_type,
                    status,
                },
            ));
        }

        // Most recently touched first: the issue someone is filing a subtask
        // under is usually the one they were just looking at.
        issues.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.key.cmp(&b.1.key)));

        let mut projects = projects.ranked();
        if let Some(hint) = project_hint.map(str::to_uppercase) {
            projects.retain(|p| *p != hint);
            projects.insert(0, hint);
        }

        Self {
            projects,
            types: types.ranked(),
            statuses: statuses.ranked(),
            priorities: priorities.ranked(),
            labels: labels.ranked(),
            components: components.ranked(),
            people: people
                .ranked()
                .into_iter()
                .map(|mention| KnownUser {
                    display_name: names.get(&mention).cloned().unwrap_or_default(),
                    mention,
                })
                .collect(),
            issues: issues.into_iter().map(|(_, issue)| issue).collect(),
        }
    }
}

/// Counts of values seen, ordered by how common they are.
#[derive(Default)]
struct Tally(HashMap<String, usize>);

impl Tally {
    fn add(&mut self, value: &str) {
        if !value.is_empty() {
            *self.0.entry(value.to_string()).or_default() += 1;
        }
    }

    /// Most common first, ties broken alphabetically so the order is stable
    /// from one run to the next.
    fn ranked(self) -> Vec<String> {
        let mut all: Vec<(String, usize)> = self.0.into_iter().collect();
        all.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        all.into_iter().map(|(value, _)| value).collect()
    }
}

fn text(value: Option<&serde_json::Value>) -> String {
    value
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string()
}

fn array(value: Option<&serde_json::Value>) -> &[serde_json::Value] {
    value
        .and_then(|v| v.as_array())
        .map_or(&[], |a| a.as_slice())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::state::{BaseSnapshot, RemoteSnapshot};
    use chrono::{DateTime, Utc};

    fn at(s: &str) -> DateTime<Utc> {
        crate::api::parse_timestamp(s).expect("timestamp")
    }

    fn row(key: &str, updated: &str, fields: serde_json::Value) -> IssueRow {
        IssueRow {
            key: key.to_string(),
            id: "1".into(),
            base: None,
            remote: Some(RemoteSnapshot {
                raw: serde_json::json!({ "fields": fields }),
                updated: at(updated),
                fetched_at: None,
            }),
            file_path: None,
            parent_key: None,
            deleted_remote: false,
        }
    }

    fn fixture() -> Vec<IssueRow> {
        vec![
            row(
                "PROJ-1",
                "2026-08-01T00:00:00Z",
                serde_json::json!({
                    "summary": "Fix login flow",
                    "issuetype": {"name": "Story"},
                    "priority": {"name": "High"},
                    "labels": ["auth", "backend"],
                }),
            ),
            row(
                "PROJ-2",
                "2026-08-09T00:00:00Z",
                serde_json::json!({
                    "summary": "Rotate the IdP certificate",
                    "issuetype": {"name": "Task"},
                    "priority": {"name": "High"},
                    "labels": ["auth"],
                }),
            ),
            row(
                "OPS-3",
                "2026-08-05T00:00:00Z",
                serde_json::json!({
                    "summary": "SSO rollout",
                    "issuetype": {"name": "Task"},
                    "labels": [],
                }),
            ),
        ]
    }

    #[test]
    fn the_catalog_ranks_what_the_workspace_uses_most() {
        let catalog = Catalog::from_rows(&fixture(), None);
        assert_eq!(catalog.types, vec!["Task", "Story"]);
        assert_eq!(catalog.priorities, vec!["High"]);
        assert_eq!(catalog.labels, vec!["auth", "backend"]);
        // Two projects, PROJ ahead of OPS because it has more issues.
        assert_eq!(catalog.projects, vec!["PROJ", "OPS"]);
    }

    #[test]
    fn the_workspaces_own_project_leads_the_list() {
        let catalog = Catalog::from_rows(&fixture(), Some("ops"));
        assert_eq!(catalog.projects, vec!["OPS", "PROJ"]);
        // A project the snapshot has never seen is still offered first.
        let fresh = Catalog::from_rows(&[], Some("NEW1"));
        assert_eq!(fresh.projects, vec!["NEW1"]);
    }

    #[test]
    fn parent_candidates_are_the_issues_touched_most_recently() {
        let catalog = Catalog::from_rows(&fixture(), None);
        let keys: Vec<&str> = catalog.issues.iter().map(|i| i.key.as_str()).collect();
        assert_eq!(keys, vec!["PROJ-2", "OPS-3", "PROJ-1"]);
        assert_eq!(catalog.issues[0].summary, "Rotate the IdP certificate");
    }

    #[test]
    fn locally_created_issues_are_not_offered_as_parents() {
        let mut rows = fixture();
        rows.push(row(
            "NEW-a1b2c3",
            "2026-08-20T00:00:00Z",
            serde_json::json!({"summary": "not on the server yet"}),
        ));
        let catalog = Catalog::from_rows(&rows, None);
        assert!(!catalog.issues.iter().any(|i| i.key.starts_with("NEW-")));
        assert!(!catalog.projects.iter().any(|p| p == "NEW"));
    }

    #[test]
    fn a_snapshot_that_was_never_fetched_still_contributes_what_it_has() {
        // A pulled-then-never-refetched issue only has a base snapshot.
        let mut only_base = row("PROJ-9", "2026-08-01T00:00:00Z", serde_json::json!({}));
        only_base.base = Some(BaseSnapshot {
            raw: serde_json::json!({"fields": {"summary": "From base", "issuetype": {"name": "Bug"}}}),
            updated: at("2026-08-01T00:00:00Z"),
            description: String::new(),
            md_hash: String::new(),
        });
        only_base.remote = None;
        let catalog = Catalog::from_rows(&[only_base], None);
        assert_eq!(catalog.types, vec!["Bug"]);
        assert_eq!(catalog.issues[0].summary, "From base");
    }

    #[test]
    fn a_missing_or_odd_snapshot_never_panics() {
        let mut bare = row("PROJ-1", "2026-08-01T00:00:00Z", serde_json::json!({}));
        bare.remote = None;
        let odd = row(
            "PROJ-2",
            "2026-08-01T00:00:00Z",
            serde_json::json!({"issuetype": "a string, not an object", "labels": "also wrong"}),
        );
        let catalog = Catalog::from_rows(&[bare, odd], None);
        assert!(catalog.types.is_empty());
        assert!(catalog.labels.is_empty());
        assert_eq!(catalog.projects, vec!["PROJ"]);
    }

    /// The edit form needs people and statuses, which come from the same scan.
    #[test]
    fn statuses_components_and_people_come_from_the_same_snapshots() {
        let rows = vec![row(
            "PROJ-1",
            "2026-08-01T00:00:00Z",
            serde_json::json!({
                "summary": "Fix login flow",
                "status": {"name": "In Progress"},
                "components": [{"name": "web"}, {"name": "api"}],
                "assignee": {"accountId": "5b10a2", "displayName": "Ilya A"},
                "reporter": {"accountId": "5b1af9", "displayName": "Jane D"},
            }),
        )];
        let catalog = Catalog::from_rows(&rows, None);
        assert_eq!(catalog.statuses, vec!["In Progress"]);
        assert_eq!(catalog.components, vec!["api", "web"]);
        let mentions: Vec<&str> = catalog.people.iter().map(|p| p.mention.as_str()).collect();
        assert!(mentions.contains(&"@[Ilya A](5b10a2)"), "{mentions:?}");
        assert!(mentions.contains(&"@[Jane D](5b1af9)"), "{mentions:?}");
        assert_eq!(catalog.people[0].display_name, "Ilya A");
    }
}
