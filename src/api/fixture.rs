//! Recorded-fixture client [M1.12].
//!
//! Replays a directory of scrubbed API responses so the whole CLI can be
//! exercised end to end without a server. Writes are not sent anywhere: they
//! are journaled (so tests can assert the *exact* request bodies, which is how
//! byte-identical lossy round-tripping is verified) and applied to an in-memory
//! copy of the fixture so a later read reflects them.
//!
//! Set `JIRED_FIXTURES=<dir>` to activate it for any command.

use crate::api::client::{JiraApi, SearchPage, SearchQuery};
use crate::api::model::*;
use crate::api::normalize::{self, FieldMap};
use crate::api::{Deployment, Markup};
use crate::error::{JiredError, Result};

/// Every fixture failure points back at the directory that produced it.
///
/// `JIRED_FIXTURES` is a supported escape hatch, not test-only plumbing: it is
/// how the end-to-end suite runs offline and how a bug can be reproduced from a
/// recording. Its errors have to be as legible as any other.
const FIXTURE_HINT: &str = "the fixture directory in JIRED_FIXTURES is incomplete; \
                            re-record it with `jired-record`, or unset the variable \
                            to talk to a real server";
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// One recorded write, in the order it was attempted.
#[derive(Debug, Clone, serde::Serialize)]
pub struct RecordedCall {
    pub method: String,
    pub path: String,
    pub body: Value,
}

pub struct FixtureClient {
    dir: PathBuf,
    deployment: Deployment,
    field_map: FieldMap,
    /// Issue payloads by key, mutated by recorded writes.
    issues: Mutex<BTreeMap<String, Value>>,
    calls: Mutex<Vec<RecordedCall>>,
    /// Scripted failures: `"PUT /rest/api/3/issue/PROJ-1" -> error`.
    faults: BTreeMap<String, FixtureFault>,
    next_id: Mutex<u64>,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct FixtureFault {
    pub kind: String,
    pub message: String,
}

impl FixtureClient {
    pub fn open(dir: &Path, deployment: Deployment) -> Result<Self> {
        if !dir.is_dir() {
            return Err(JiredError::usage(format!(
                "fixture directory {} does not exist",
                dir.display()
            )));
        }
        let fields: Vec<FieldMeta> = read_json(dir, "fields.json")
            .map(|v| serde_json::from_value(v).unwrap_or_default())
            .unwrap_or_default();

        let mut issues = BTreeMap::new();
        for value in read_json(dir, "issues.json")
            .and_then(|v| v.as_array().cloned())
            .unwrap_or_default()
        {
            if let Some(key) = value.get("key").and_then(Value::as_str) {
                issues.insert(key.to_string(), value.clone());
            }
        }
        let issue_dir = dir.join("issue");
        if issue_dir.is_dir() {
            for entry in std::fs::read_dir(&issue_dir)?.flatten() {
                let text = std::fs::read_to_string(entry.path())?;
                let value: Value = serde_json::from_str(&text)
                    .map_err(|e| JiredError::usage(format!("{}: {e}", entry.path().display())))?;
                if let Some(key) = value.get("key").and_then(Value::as_str) {
                    issues.insert(key.to_string(), value);
                }
            }
        }

        let faults = read_json(dir, "faults.json")
            .and_then(|v| serde_json::from_value(v).ok())
            .unwrap_or_default();

        Ok(Self {
            dir: dir.to_path_buf(),
            deployment,
            field_map: FieldMap::from_meta(&fields),
            issues: Mutex::new(issues),
            calls: Mutex::new(Vec::new()),
            faults,
            next_id: Mutex::new(90000),
        })
    }

    pub fn calls(&self) -> Vec<RecordedCall> {
        self.calls.lock().expect("fixture lock").clone()
    }

    fn record(&self, method: &str, path: &str, body: Value) -> Result<()> {
        self.calls.lock().expect("fixture lock").push(RecordedCall {
            method: method.to_string(),
            path: path.to_string(),
            body,
        });
        // Journaling to disk lets an end-to-end test inspect the calls a
        // separate `jired` process made.
        if let Ok(journal) = std::env::var("JIRED_FIXTURE_JOURNAL") {
            let line = serde_json::to_string(
                self.calls
                    .lock()
                    .expect("fixture lock")
                    .last()
                    .expect("just pushed"),
            )?;
            use std::io::Write;
            let mut f = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(journal)?;
            writeln!(f, "{line}")?;
        }
        self.check_fault(method, path)
    }

    fn check_fault(&self, method: &str, path: &str) -> Result<()> {
        let Some(fault) = self.faults.get(&format!("{method} {path}")) else {
            return Ok(());
        };
        Err(match fault.kind.as_str() {
            "auth" => JiredError::auth(fault.message.clone()),
            "network" => JiredError::network(fault.message.clone()),
            "conflict" => JiredError::conflict(fault.message.clone()),
            _ => JiredError::generic(fault.message.clone()),
        })
    }

    fn markup(&self) -> Markup {
        self.deployment.markup()
    }

    fn issue_value(&self, key: &str) -> Result<Value> {
        self.issues
            .lock()
            .expect("fixture lock")
            .get(key)
            .cloned()
            .ok_or_else(|| {
                JiredError::usage(format!("fixture has no issue {key}"))
                    .with_hint(FIXTURE_HINT)
                    .with_detail("status", 404)
            })
    }

    fn bump_updated(&self, key: &str) {
        {
            let mut issues = self.issues.lock().expect("fixture lock");
            if let Some(issue) = issues.get_mut(key) {
                // A real server advances `updated` on every write; the fixture
                // must too, or the optimistic-concurrency check would never be
                // exercised.
                let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, false);
                issue["fields"]["updated"] = json!(now);
            }
        }
        self.persist();
    }

    /// Write the mutated issues back to disk.
    ///
    /// Every `jired` invocation is a separate process with a fresh client, so
    /// without this the "server" would forget each write as soon as the command
    /// that made it exited — and a multi-command test would silently assert
    /// against stale state.
    fn persist(&self) {
        let issues = self.issues.lock().expect("fixture lock");
        let all: Vec<&Value> = issues.values().collect();
        if let Ok(text) = serde_json::to_string_pretty(&all) {
            let _ = std::fs::write(self.dir.join("issues.json"), text);
        }
    }

    fn next_id(&self) -> String {
        let mut n = self.next_id.lock().expect("fixture lock");
        *n += 1;
        n.to_string()
    }
}

/// Which custom field this fixture's issues keep their sprint in.
fn sprint_field(issue: &Value) -> Option<String> {
    issue
        .get("fields")?
        .as_object()?
        .iter()
        .find(|(k, v)| {
            k.starts_with("customfield_")
                && v.as_array()
                    .is_some_and(|a| a.first().is_some_and(|first| first.get("state").is_some()))
        })
        .map(|(k, _)| k.clone())
}

fn read_json(dir: &Path, name: &str) -> Option<Value> {
    let text = std::fs::read_to_string(dir.join(name)).ok()?;
    serde_json::from_str(&text).ok()
}

#[async_trait]
impl JiraApi for FixtureClient {
    fn deployment(&self) -> Deployment {
        self.deployment
    }

    fn base_url(&self) -> &str {
        "https://fixture.invalid"
    }

    async fn server_info(&self) -> Result<ServerInfo> {
        let v = read_json(&self.dir, "serverInfo.json").unwrap_or(json!({}));
        // Report the recorded deploymentType, exactly as a real server would;
        // echoing back the assumption would make `init`'s detection untestable.
        let deployment = v
            .get("deploymentType")
            .and_then(Value::as_str)
            .and_then(|d| match d.to_ascii_lowercase().as_str() {
                "cloud" => Some(Deployment::Cloud),
                "server" | "datacenter" | "data center" => Some(Deployment::DataCenter),
                _ => None,
            });
        Ok(ServerInfo {
            deployment: Some(deployment.unwrap_or(self.deployment)),
            version: v.get("version").and_then(Value::as_str).map(str::to_string),
            title: v
                .get("serverTitle")
                .and_then(Value::as_str)
                .map(str::to_string),
        })
    }

    async fn myself(&self) -> Result<Myself> {
        let v = read_json(&self.dir, "myself.json").ok_or_else(|| {
            JiredError::auth("fixture has no myself.json").with_hint(FIXTURE_HINT)
        })?;
        Ok(Myself {
            account_id: v
                .get("accountId")
                .or_else(|| v.get("name"))
                .and_then(Value::as_str)
                .unwrap_or("fixture-user")
                .to_string(),
            display_name: v
                .get("displayName")
                .and_then(Value::as_str)
                .map(str::to_string),
            email: v
                .get("emailAddress")
                .and_then(Value::as_str)
                .map(str::to_string),
            time_zone: v
                .get("timeZone")
                .and_then(Value::as_str)
                .map(str::to_string),
        })
    }

    async fn projects(&self) -> Result<Vec<Project>> {
        Ok(read_json(&self.dir, "projects.json")
            .and_then(|v| serde_json::from_value(v).ok())
            .unwrap_or_default())
    }

    async fn fields(&self) -> Result<Vec<FieldMeta>> {
        Ok(read_json(&self.dir, "fields.json")
            .and_then(|v| serde_json::from_value(v).ok())
            .unwrap_or_default())
    }

    async fn boards(&self, _project: Option<&str>) -> Result<Vec<Board>> {
        Ok(read_json(&self.dir, "boards.json")
            .and_then(|v| serde_json::from_value(v).ok())
            .unwrap_or_default())
    }

    async fn board_columns(&self, board_id: &str) -> Result<Vec<BoardColumn>> {
        Ok(
            read_json(&self.dir, &format!("board-{board_id}-columns.json"))
                .and_then(|v| serde_json::from_value(v).ok())
                .unwrap_or_default(),
        )
    }

    async fn sprints(&self, _board_id: &str) -> Result<Vec<Sprint>> {
        Ok(read_json(&self.dir, "sprints.json")
            .and_then(|v| serde_json::from_value(v).ok())
            .unwrap_or_default())
    }

    async fn create_sprint(&self, input: &SprintInput) -> Result<Sprint> {
        self.record("POST", "/rest/agile/1.0/sprint", input.to_body())?;
        let sprint = Sprint {
            id: self.next_id(),
            name: input.name.clone(),
            // Jira decides this, and a sprint nobody has started yet is future.
            state: "future".to_string(),
            board_id: Some(input.board_id.clone()),
            start_date: input.start,
            end_date: input.end,
            goal: input.goal.clone(),
        };
        // Written back to the fixture directory, so the next process to run —
        // a `jired sprints` in the same test — sees the sprint that was just
        // created rather than the recording it started from.
        let mut all: Vec<Sprint> = read_json(&self.dir, "sprints.json")
            .and_then(|v| serde_json::from_value(v).ok())
            .unwrap_or_default();
        all.push(sprint.clone());
        if let Ok(text) = serde_json::to_string_pretty(&all) {
            let _ = std::fs::write(self.dir.join("sprints.json"), text);
        }
        Ok(sprint)
    }

    async fn search(&self, query: &SearchQuery) -> Result<SearchPage> {
        self.check_fault("POST", "/search")?;
        let issues = self.issues.lock().expect("fixture lock").clone();
        let mut matching: Vec<Value> = issues
            .values()
            .filter(|v| jql_matches(&query.jql, v))
            .cloned()
            .collect();
        // Deterministic ordering: fixtures must not depend on map iteration.
        matching.sort_by_key(|v| {
            v.get("key")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string()
        });

        let total = matching.len();
        let page: Vec<Value> = matching
            .into_iter()
            .skip(query.start_at)
            .take(query.max_results)
            .collect();

        let parsed = page
            .iter()
            .map(|v| normalize::issue_from_json(v, self.markup(), &self.field_map))
            .collect::<Result<Vec<_>>>()?;

        Ok(SearchPage {
            issues: parsed,
            start_at: query.start_at,
            total,
            next_page_token: None,
        })
    }

    async fn issue(&self, key: &str) -> Result<RemoteIssue> {
        let v = self.issue_value(key)?;
        normalize::issue_from_json(&v, self.markup(), &self.field_map)
    }

    async fn issue_updated(&self, key: &str) -> Result<DateTime<Utc>> {
        let v = self.issue_value(key)?;
        v.get("fields")
            .and_then(|f| f.get("updated"))
            .and_then(Value::as_str)
            .and_then(parse_timestamp)
            .ok_or_else(|| {
                JiredError::usage(format!("fixture issue {key} has no `updated` field"))
                    .with_hint(FIXTURE_HINT)
            })
    }

    async fn changelog(&self, key: &str) -> Result<Vec<ChangelogEntry>> {
        Ok(read_json(&self.dir, &format!("changelog-{key}.json"))
            .and_then(|v| serde_json::from_value(v).ok())
            .unwrap_or_default())
    }

    async fn transitions(&self, key: &str) -> Result<Vec<Transition>> {
        Ok(read_json(&self.dir, &format!("transitions-{key}.json"))
            .or_else(|| read_json(&self.dir, "transitions.json"))
            .and_then(|v| serde_json::from_value(v).ok())
            .unwrap_or_default())
    }

    async fn update_fields(&self, key: &str, fields: Value) -> Result<()> {
        self.record("PUT", &format!("/issue/{key}"), json!({ "fields": fields }))?;
        {
            let mut issues = self.issues.lock().expect("fixture lock");
            if let Some(issue) = issues.get_mut(key) {
                if let Some(obj) = fields.as_object() {
                    for (k, v) in obj {
                        issue["fields"][k] = v.clone();
                    }
                }
            }
        }
        self.bump_updated(key);
        Ok(())
    }

    async fn create_issue(&self, fields: Value) -> Result<(String, String)> {
        self.record("POST", "/issue", json!({ "fields": fields }))?;
        let project = fields
            .get("project")
            .and_then(|p| p.get("key"))
            .and_then(Value::as_str)
            .unwrap_or("NEW")
            .to_string();
        let id = self.next_id();
        let key = format!("{project}-{}", &id[id.len().saturating_sub(3)..]);
        let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, false);
        let mut issue = json!({"id": id, "key": key, "fields": fields});
        issue["fields"]["created"] = json!(now);
        issue["fields"]["updated"] = json!(now);
        issue["fields"]["status"] = json!({"name": "To Do", "id": "1"});
        self.issues
            .lock()
            .expect("fixture lock")
            .insert(key.clone(), issue);
        self.persist();
        Ok((key, id))
    }

    async fn do_transition(
        &self,
        key: &str,
        transition_id: &str,
        fields: Option<Value>,
    ) -> Result<()> {
        self.record(
            "POST",
            &format!("/issue/{key}/transitions"),
            json!({"transition": {"id": transition_id}, "fields": fields}),
        )?;
        let target = self
            .transitions(key)
            .await?
            .into_iter()
            .find(|t| t.id == transition_id);
        if let Some(t) = target {
            let mut issues = self.issues.lock().expect("fixture lock");
            if let Some(issue) = issues.get_mut(key) {
                issue["fields"]["status"] = json!({
                    "name": t.to_status,
                    "id": t.to_status_id.unwrap_or_default(),
                });
            }
        }
        self.bump_updated(key);
        Ok(())
    }

    async fn move_to_sprint(&self, sprint: Option<&str>, keys: &[String]) -> Result<()> {
        let path = match sprint {
            Some(id) => format!("/rest/agile/1.0/sprint/{id}/issue"),
            None => "/rest/agile/1.0/backlog/issue".to_string(),
        };
        self.record("POST", &path, json!({ "issues": keys }))?;

        // Reflect the move the way the server would, so the next fetch sees it.
        let named = sprint.and_then(|id| {
            read_json(&self.dir, "sprints.json")
                .and_then(|v| v.as_array().cloned())
                .unwrap_or_default()
                .into_iter()
                .find(|s| {
                    s.get("id")
                        .map(|i| i.to_string().trim_matches('"').to_string())
                        == Some(id.to_string())
                })
        });
        {
            let mut issues = self.issues.lock().expect("fixture lock");
            for key in keys {
                let Some(issue) = issues.get_mut(key) else {
                    continue;
                };
                let field = sprint_field(issue);
                match (&field, &named) {
                    (Some(field), Some(sprint)) => issue["fields"][field] = json!([sprint]),
                    (Some(field), None) => issue["fields"][field] = Value::Null,
                    (None, _) => {}
                }
            }
        }
        for key in keys {
            self.bump_updated(key);
        }
        Ok(())
    }

    async fn worklogs(&self, key: &str) -> Result<Vec<RemoteWorklog>> {
        let v = self.issue_value(key)?;
        Ok(normalize::worklogs_from(
            v.get("fields").and_then(|f| f.get("worklog")),
            self.markup(),
        )
        .0)
    }

    async fn add_worklog(&self, key: &str, worklog: &WorklogInput) -> Result<RemoteWorklog> {
        self.record("POST", &format!("/issue/{key}/worklog"), worklog.to_body())?;
        let id = self.next_id();
        let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, false);
        let entry = json!({
            "id": id,
            "author": {"accountId": "fixture-user", "displayName": "Fixture User"},
            "started": worklog.started.as_ref().map(format_worklog_started).unwrap_or_else(|| now.clone()),
            "created": now, "updated": now,
            "timeSpent": worklog.time_spent,
            "timeSpentSeconds": crate::doc::parse_duration(&worklog.time_spent).unwrap_or(0),
            "comment": worklog.comment.to_field_value(),
        });
        {
            let mut issues = self.issues.lock().expect("fixture lock");
            if let Some(issue) = issues.get_mut(key) {
                match issue["fields"]["worklog"]["worklogs"].as_array_mut() {
                    Some(list) => {
                        list.push(entry.clone());
                        let n = list.len();
                        issue["fields"]["worklog"]["total"] = json!(n);
                    }
                    None => {
                        issue["fields"]["worklog"] = json!({"startAt": 0, "maxResults": 20, "total": 1,
                                   "worklogs": [entry.clone()]})
                    }
                }
            }
        }
        self.bump_updated(key);
        normalize::one_worklog(&entry, self.markup())
            .ok_or_else(|| JiredError::generic("fixture produced an unreadable worklog"))
    }

    async fn update_worklog(
        &self,
        key: &str,
        id: &str,
        worklog: &WorklogInput,
    ) -> Result<RemoteWorklog> {
        self.record(
            "PUT",
            &format!("/issue/{key}/worklog/{id}"),
            worklog.to_body(),
        )?;
        let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, false);
        let mut updated = json!(null);
        {
            let mut issues = self.issues.lock().expect("fixture lock");
            if let Some(issue) = issues.get_mut(key) {
                if let Some(list) = issue["fields"]["worklog"]["worklogs"].as_array_mut() {
                    for w in list.iter_mut() {
                        if w.get("id").and_then(Value::as_str) != Some(id) {
                            continue;
                        }
                        w["timeSpent"] = json!(worklog.time_spent);
                        w["timeSpentSeconds"] =
                            json!(crate::doc::parse_duration(&worklog.time_spent).unwrap_or(0));
                        w["comment"] = worklog.comment.to_field_value();
                        if let Some(started) = &worklog.started {
                            w["started"] = json!(format_worklog_started(started));
                        }
                        w["updated"] = json!(now);
                        updated = w.clone();
                    }
                }
            }
        }
        if updated.is_null() {
            return Err(JiredError::generic(format!(
                "fixture has no worklog {id} on {key}"
            )));
        }
        self.bump_updated(key);
        normalize::one_worklog(&updated, self.markup())
            .ok_or_else(|| JiredError::generic("fixture produced an unreadable worklog"))
    }

    async fn delete_worklog(&self, key: &str, id: &str) -> Result<()> {
        self.record("DELETE", &format!("/issue/{key}/worklog/{id}"), Value::Null)?;
        {
            let mut issues = self.issues.lock().expect("fixture lock");
            if let Some(issue) = issues.get_mut(key) {
                if let Some(list) = issue["fields"]["worklog"]["worklogs"].as_array_mut() {
                    list.retain(|w| w.get("id").and_then(Value::as_str) != Some(id));
                    let n = list.len();
                    issue["fields"]["worklog"]["total"] = json!(n);
                }
            }
        }
        self.bump_updated(key);
        Ok(())
    }

    async fn add_comment(&self, key: &str, body: &RichText) -> Result<RemoteComment> {
        self.record(
            "POST",
            &format!("/issue/{key}/comment"),
            json!({ "body": body.to_field_value() }),
        )?;
        let id = self.next_id();
        let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, false);
        let comment = json!({
            "id": id,
            "author": {"accountId": "fixture-user", "displayName": "Fixture User"},
            "created": now, "updated": now,
            "body": body.to_field_value(),
        });
        {
            let mut issues = self.issues.lock().expect("fixture lock");
            if let Some(issue) = issues.get_mut(key) {
                let list = issue["fields"]["comment"]["comments"]
                    .as_array_mut()
                    .map(|a| {
                        a.push(comment.clone());
                    });
                if list.is_none() {
                    issue["fields"]["comment"] = json!({"comments": [comment.clone()]});
                }
            }
        }
        self.bump_updated(key);
        normalize::comments_from(Some(&json!([comment])), self.markup())
            .into_iter()
            .next()
            .ok_or_else(|| {
                JiredError::usage("fixture produced an unreadable comment").with_hint(FIXTURE_HINT)
            })
    }

    async fn update_comment(&self, key: &str, id: &str, body: &RichText) -> Result<RemoteComment> {
        self.record(
            "PUT",
            &format!("/issue/{key}/comment/{id}"),
            json!({ "body": body.to_field_value() }),
        )?;
        let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, false);
        let mut updated = json!(null);
        {
            let mut issues = self.issues.lock().expect("fixture lock");
            if let Some(issue) = issues.get_mut(key) {
                if let Some(list) = issue["fields"]["comment"]["comments"].as_array_mut() {
                    for c in list.iter_mut() {
                        if c.get("id").and_then(Value::as_str) == Some(id) {
                            c["body"] = body.to_field_value();
                            c["updated"] = json!(now);
                            updated = c.clone();
                        }
                    }
                }
            }
        }
        if updated.is_null() {
            return Err(JiredError::generic(format!(
                "fixture has no comment {id} on {key}"
            )));
        }
        self.bump_updated(key);
        normalize::comments_from(Some(&json!([updated])), self.markup())
            .into_iter()
            .next()
            .ok_or_else(|| {
                JiredError::usage("fixture produced an unreadable comment").with_hint(FIXTURE_HINT)
            })
    }

    async fn delete_comment(&self, key: &str, id: &str) -> Result<()> {
        self.record("DELETE", &format!("/issue/{key}/comment/{id}"), Value::Null)?;
        let mut issues = self.issues.lock().expect("fixture lock");
        if let Some(issue) = issues.get_mut(key) {
            if let Some(list) = issue["fields"]["comment"]["comments"].as_array_mut() {
                list.retain(|c| c.get("id").and_then(Value::as_str) != Some(id));
            }
        }
        drop(issues);
        self.bump_updated(key);
        Ok(())
    }

    async fn upload_attachment(
        &self,
        key: &str,
        filename: &str,
        bytes: Vec<u8>,
    ) -> Result<RemoteAttachment> {
        self.record(
            "POST",
            &format!("/issue/{key}/attachments"),
            json!({"filename": filename, "size": bytes.len()}),
        )?;
        let attachment = RemoteAttachment {
            id: self.next_id(),
            filename: filename.to_string(),
            size: bytes.len() as u64,
            mime: Some(
                mime_guess::from_path(filename)
                    .first_or_octet_stream()
                    .to_string(),
            ),
            created: Some(chrono::Utc::now()),
            author: Some("Fixture User".into()),
            content_url: format!("fixture://{key}/{filename}"),
        };

        // A real server lists the file on the issue from now on. Without this
        // the next read still shows it missing, and jired would keep offering
        // to upload it.
        {
            let mut issues = self.issues.lock().expect("fixture lock");
            if let Some(issue) = issues.get_mut(key) {
                let entry = json!({
                    "id": attachment.id,
                    "filename": attachment.filename,
                    "size": attachment.size,
                    "mimeType": attachment.mime,
                    "created": attachment.created.map(|c| c.to_rfc3339()),
                    "author": {"displayName": "Fixture User"},
                    "content": attachment.content_url,
                });
                match issue["fields"]["attachment"].as_array_mut() {
                    Some(list) => list.push(entry),
                    None => issue["fields"]["attachment"] = json!([entry]),
                }
            }
        }
        self.bump_updated(key);
        Ok(attachment)
    }

    async fn download_attachment(&self, attachment: &RemoteAttachment) -> Result<Vec<u8>> {
        let path = self.dir.join("attachments").join(&attachment.filename);
        std::fs::read(&path)
            .map_err(|e| JiredError::network(format!("fixture attachment {}: {e}", path.display())))
    }

    async fn search_users(&self, query: &str) -> Result<Vec<UserRef>> {
        let users: Vec<UserRef> = read_json(&self.dir, "users.json")
            .and_then(|v| serde_json::from_value(v).ok())
            .unwrap_or_default();
        let q = query.to_lowercase();
        Ok(users
            .into_iter()
            .filter(|u| {
                u.display_name.to_lowercase().contains(&q) || u.id.to_lowercase().contains(&q)
            })
            .collect())
    }
}

/// Evaluate the small JQL subset jired itself generates.
///
/// Deliberately not a JQL engine: it understands `project`, `key in (...)` and
/// `updated >=`, which is exactly what fetch builds, and matches everything else
/// so a hand-written fixture query is never silently empty.
fn jql_matches(jql: &str, issue: &Value) -> bool {
    let key = issue.get("key").and_then(Value::as_str).unwrap_or("");
    let lower = jql.to_lowercase();

    if let Some(rest) = lower.split("project = ").nth(1) {
        let want = rest
            .split_whitespace()
            .next()
            .unwrap_or("")
            .trim_matches(|c| c == '"' || c == '\'');
        let project = key.split('-').next().unwrap_or("");
        if !want.is_empty() && !project.eq_ignore_ascii_case(want) {
            return false;
        }
    }

    if let Some(start) = lower.find("key in (") {
        let rest = &jql[start + "key in (".len()..];
        if let Some(end) = rest.find(')') {
            let wanted: Vec<String> = rest[..end]
                .split(',')
                .map(|k| k.trim().trim_matches('"').to_uppercase())
                .collect();
            if !wanted.iter().any(|w| w == key) {
                return false;
            }
        }
    }

    if let Some(start) = lower.find("updated >= ") {
        let rest = jql[start + "updated >= ".len()..].trim_start();
        let quoted = rest.trim_start_matches('"');
        if let Some(end) = quoted.find('"') {
            if let Some(since) = parse_since(&quoted[..end]) {
                let updated = issue
                    .get("fields")
                    .and_then(|f| f.get("updated"))
                    .and_then(Value::as_str)
                    .and_then(parse_timestamp);
                if let Some(u) = updated {
                    if u < since {
                        return false;
                    }
                }
            }
        }
    }
    true
}

fn parse_since(s: &str) -> Option<DateTime<Utc>> {
    parse_timestamp(s).or_else(|| {
        chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M")
            .ok()
            .map(|dt| dt.and_utc())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jql_project_filter() {
        let a = json!({"key": "PROJ-1", "fields": {}});
        let b = json!({"key": "OTHER-1", "fields": {}});
        assert!(jql_matches("project = PROJ", &a));
        assert!(!jql_matches("project = PROJ", &b));
        assert!(jql_matches("project = \"PROJ\" ORDER BY key", &a));
    }

    #[test]
    fn jql_key_list_filter() {
        let a = json!({"key": "PROJ-1", "fields": {}});
        assert!(jql_matches("key in (PROJ-1, PROJ-2)", &a));
        assert!(!jql_matches("key in (PROJ-3)", &a));
    }

    #[test]
    fn jql_incremental_filter() {
        let old = json!({"key": "PROJ-1", "fields": {"updated": "2026-01-01T00:00:00.000+0000"}});
        let new = json!({"key": "PROJ-2", "fields": {"updated": "2026-08-16T00:00:00.000+0000"}});
        let q = "project = PROJ AND updated >= \"2026-06-01 00:00\"";
        assert!(!jql_matches(q, &old));
        assert!(jql_matches(q, &new));
    }

    #[test]
    fn unknown_jql_matches_everything_rather_than_nothing() {
        let a = json!({"key": "PROJ-1", "fields": {}});
        assert!(jql_matches("labels = urgent AND cf[123] ~ x", &a));
    }
}
