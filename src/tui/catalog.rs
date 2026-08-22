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
use chrono::{DateTime, Utc};
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
    /// Every sprint the board cache knows: the one running, the ones queued
    /// behind it, then the finished ones, newest first.
    pub sprints: Vec<KnownSprint>,
}

/// A sprint the workspace has seen, and what it is doing now.
#[derive(Debug, Clone)]
pub struct KnownSprint {
    pub name: String,
    /// `active`, `future` or `closed`.
    pub state: String,
}

impl KnownSprint {
    /// How the state reads beside the name in a list.
    pub fn detail(&self) -> &'static str {
        match self.state.as_str() {
            "active" => "running now",
            "future" => "not started",
            _ => "closed",
        }
    }
}

/// An issue the workspace has seen, and everything the picker sorts on.
///
/// Every field past the first four is there to be a column: the picker is the
/// only screen that shows a whole issue at a glance, and "which of these was I
/// in yesterday" is not answerable from a key and a summary. They are cheap —
/// all of it is already in the snapshot the catalog is walking — and empty
/// where the workspace does not know, which the table renders as a dash rather
/// than pretending.
#[derive(Debug, Clone, Default)]
pub struct KnownIssue {
    pub key: String,
    pub summary: String,
    pub issue_type: String,
    pub status: String,
    /// `new`, `indeterminate` or `done` — Jira's own three buckets, which is
    /// what "is this finished" means across differently-named workflows.
    pub status_category: String,
    pub priority: String,
    pub assignee: String,
    pub reporter: String,
    pub created: Option<DateTime<Utc>>,
    pub updated: Option<DateTime<Utc>>,
    /// When this machine last opened it; `None` until it has.
    pub viewed: Option<DateTime<Utc>>,
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
            let priority = text(fields.pointer("/priority/name"));
            types.add(&issue_type);
            statuses.add(&status);
            priorities.add(&priority);
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
            // `updated` off the snapshot rather than out of `fields`: it is the
            // same instant, already parsed, and it is what `is_behind` compares.
            let updated = row
                .remote
                .as_ref()
                .map(|r| r.updated)
                .or(row.base.as_ref().map(|b| b.updated));
            issues.push((
                updated,
                KnownIssue {
                    key: row.key.clone(),
                    summary: text(fields.get("summary")),
                    issue_type,
                    status,
                    status_category: text(fields.pointer("/status/statusCategory/key")),
                    priority,
                    assignee: person(fields.get("assignee")),
                    reporter: person(fields.get("reporter")),
                    created: fields
                        .get("created")
                        .and_then(|v| v.as_str())
                        .and_then(crate::api::model::parse_timestamp),
                    updated,
                    viewed: row.viewed_at,
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
            sprints: Vec::new(),
        }
    }

    /// Add the sprints from the board cache — all of them, closed included.
    ///
    /// `sprint:` is a history as much as a destination, so a finished sprint is
    /// a real answer: it is where the issue has been. The ones an issue can
    /// actually be moved into come first, and every row says which it is, so
    /// the difference is on screen rather than left to be remembered.
    pub fn with_sprints(mut self, sprints: &[crate::store::state::SprintRow]) -> Self {
        let mut all: Vec<&crate::store::state::SprintRow> = sprints.iter().collect();
        // The one running, then the ones queued behind it, then the finished
        // ones newest first — a sprint that just ended is the likeliest of them.
        all.sort_by(|a, b| {
            rank(&a.state)
                .cmp(&rank(&b.state))
                .then_with(|| match (rank(&a.state), rank(&b.state)) {
                    (2, 2) => recency(&b.id).cmp(&recency(&a.id)),
                    _ => std::cmp::Ordering::Equal,
                })
                .then_with(|| a.name.cmp(&b.name))
        });
        // A name can belong to both a closed sprint and an open one; the list
        // holds names, so the higher-ranked of the two is the one to keep.
        let mut seen = std::collections::HashSet::new();
        self.sprints = all
            .into_iter()
            .filter(|s| seen.insert(s.name.to_lowercase()))
            .map(|s| KnownSprint {
                name: s.name.clone(),
                state: s.state.clone(),
            })
            .collect();
        self
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

/// Where a sprint's state puts it in the list.
fn rank(state: &str) -> u8 {
    match state {
        "active" => 0,
        "future" => 1,
        _ => 2,
    }
}

/// Sprint ids climb as sprints are created, so the id orders finished sprints
/// by age. One that is not a number sorts oldest rather than guessing.
fn recency(id: &str) -> u64 {
    id.parse().unwrap_or(0)
}

fn text(value: Option<&serde_json::Value>) -> String {
    value
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string()
}

/// Someone's name, for a column that has to be read rather than pushed.
///
/// The `@[Name](id)` mention spelling is what the file carries and what a form
/// writes back; a table cell wants the name on its own, since the id is neither
/// readable nor something the reader is comparing rows by.
fn person(value: Option<&serde_json::Value>) -> String {
    crate::api::normalize::user_ref(value)
        .map(|u| u.display_name)
        .unwrap_or_default()
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
            viewed_at: None,
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

    fn sprint_row(id: &str, name: &str, state: &str) -> crate::store::state::SprintRow {
        crate::store::state::SprintRow {
            id: id.into(),
            board_id: Some("1".into()),
            name: name.into(),
            state: state.into(),
        }
    }

    /// Closed sprints belong in the list — an issue has been in them — but
    /// behind the ones it can still be moved into.
    #[test]
    fn sprints_are_offered_running_then_queued_then_finished_newest_first() {
        let catalog = Catalog::default().with_sprints(&[
            sprint_row("3", "Sprint 41", "closed"),
            sprint_row("5", "Sprint 43", "future"),
            sprint_row("1", "Sprint 39", "closed"),
            sprint_row("4", "Sprint 42", "active"),
            sprint_row("2", "Sprint 40", "closed"),
        ]);
        let offered: Vec<(&str, &str)> = catalog
            .sprints
            .iter()
            .map(|s| (s.name.as_str(), s.detail()))
            .collect();
        assert_eq!(
            offered,
            vec![
                ("Sprint 42", "running now"),
                ("Sprint 43", "not started"),
                ("Sprint 41", "closed"),
                ("Sprint 40", "closed"),
                ("Sprint 39", "closed"),
            ]
        );
    }

    /// Sprint names repeat across boards and across years. The list holds
    /// names, so one row per name, and the one still running wins it.
    #[test]
    fn a_name_shared_by_a_closed_and_an_open_sprint_is_listed_once() {
        let catalog = Catalog::default().with_sprints(&[
            sprint_row("1", "Hardening", "closed"),
            sprint_row("2", "Hardening", "active"),
        ]);
        let offered: Vec<(&str, &str)> = catalog
            .sprints
            .iter()
            .map(|s| (s.name.as_str(), s.detail()))
            .collect();
        assert_eq!(offered, vec![("Hardening", "running now")]);
    }
}
