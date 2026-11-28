//! A parent's list of sub-tasks, kept in line with the sub-tasks themselves.
//!
//! Which list a parent should carry is a rule about how Jira behaves, not about
//! how jired stores it: the store reads the snapshots and writes back what
//! this decides.

use crate::api::normalize::subtask_ref;
use chrono::{DateTime, Utc};
use serde_json::Value;
use std::collections::BTreeMap;

/// One stored issue, as much of it as the rule reads.
pub struct Snapshot {
    pub key: String,
    pub parent: Option<String>,
    pub raw: Value,
    pub fetched_at: Option<DateTime<Utc>>,
}

/// The parents whose sub-task list is out of line with their sub-tasks, each
/// with the snapshot it should have instead.
///
/// Jira does not touch a parent when one of its sub-tasks is created, moved,
/// renamed or resolved, so an incremental fetch never brings the parent
/// back, and the list it carries — which is what `## Children` is rendered
/// from — stays as it was. Each sub-task's own snapshot says where it hangs
/// and what it is called; of the two, whichever was fetched later is
/// believed, so a parent re-read after a sub-task moved away is not undone
/// by the older copy of that sub-task.
///
/// Only the remote snapshot changes.
/// [`IssueRow::is_behind`](crate::store::state::IssueRow::is_behind) compares
/// the lists, so `pull` writes the file and advances base exactly as it would
/// for an edit made on the server — merging into a dirty file, and leaving it
/// clean afterwards. A sub-task the workspace has no copy of is kept as
/// the parent listed it: absent from the store is not absent from Jira.
pub fn reconcile(stored: &[Snapshot]) -> Vec<(String, Value)> {
    let by_key: BTreeMap<&str, &Snapshot> = stored.iter().map(|s| (s.key.as_str(), s)).collect();
    let mut children: BTreeMap<&str, Vec<&Snapshot>> = BTreeMap::new();
    for s in stored {
        // Read off Jira's own flag: in a Cloud team-managed project a story
        // has its epic as `parent` too, and an epic's list holds no stories.
        let is_subtask = s
            .raw
            .pointer("/fields/issuetype/subtask")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if let (Some(parent), true) = (&s.parent, is_subtask) {
            children.entry(parent.as_str()).or_default().push(s);
        }
    }
    let id = |s: &Snapshot| {
        s.raw
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };

    let mut changed = Vec::new();
    for parent in stored {
        // Ties go to the sub-task: both copies were read in the same
        // second, so neither is staler, and the sub-task's own is the one
        // a push has just confirmed.
        let believed = |child: &Snapshot| child.fetched_at >= parent.fetched_at;
        let listed: Vec<Value> = parent
            .raw
            .pointer("/fields/subtasks")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();

        let mut list = Vec::with_capacity(listed.len());
        for entry in &listed {
            let child = entry
                .get("key")
                .and_then(Value::as_str)
                .and_then(|k| by_key.get(k));
            match child {
                Some(child) if believed(child) => {
                    if child.parent.as_deref() != Some(parent.key.as_str()) {
                        continue;
                    }
                    // The entry the server wrote is kept while it says the
                    // same thing, so a list that only differs in which keys
                    // each entry carries does not count as a change.
                    let fresh = subtask_entry(&child.raw);
                    let same = subtask_ref(entry) == subtask_ref(&fresh);
                    list.push(if same { entry.clone() } else { fresh });
                }
                _ => list.push(entry.clone()),
            }
        }

        let mut missing: Vec<&Snapshot> = children
            .get(parent.key.as_str())
            .into_iter()
            .flatten()
            .copied()
            .filter(|c| believed(c))
            .filter(|c| {
                !listed
                    .iter()
                    .any(|e| e.get("key").and_then(Value::as_str) == Some(&c.key))
            })
            .collect();
        // Jira lists sub-tasks in the order they were created.
        missing.sort_by(|a, b| version_order(&id(a), &id(b)));
        list.extend(missing.into_iter().map(|c| subtask_entry(&c.raw)));

        if list == listed {
            continue;
        }
        let mut raw = parent.raw.clone();
        match raw.get_mut("fields").and_then(Value::as_object_mut) {
            Some(fields) => {
                fields.insert("subtasks".into(), Value::Array(list));
            }
            None => continue,
        }
        changed.push((parent.key.clone(), raw));
    }
    changed
}

/// A sub-task as its parent's `subtasks` list carries it, built from the
/// sub-task's own payload: the same four fields Jira puts there.
fn subtask_entry(raw: &Value) -> Value {
    let mut fields = serde_json::Map::new();
    for name in ["summary", "status", "priority", "issuetype"] {
        if let Some(v) = raw.pointer(&format!("/fields/{name}")) {
            fields.insert(name.into(), v.clone());
        }
    }
    let mut entry = serde_json::Map::new();
    for name in ["id", "key", "self"] {
        if let Some(v) = raw.get(name) {
            entry.insert(name.into(), v.clone());
        }
    }
    entry.insert("fields".into(), Value::Object(fields));
    Value::Object(entry)
}

/// Ids are numbers on every Jira, compared as such so `10010` follows `9999`.
pub fn version_order(a: &str, b: &str) -> std::cmp::Ordering {
    match (a.parse::<u64>(), b.parse::<u64>()) {
        (Ok(x), Ok(y)) => x.cmp(&y),
        _ => a.cmp(b),
    }
}
