//! Recorded-fixture client [M1.12].
//!
//! Replays a directory of scrubbed API responses so the whole CLI can be
//! exercised end to end without a server. Writes are not sent anywhere: they
//! are journaled (so tests can assert the *exact* request bodies, which is how
//! byte-identical lossy round-tripping is verified) and applied to an in-memory
//! copy of the fixture so a later read reflects them.
//!
//! Set `JIRED_FIXTURES=<dir>` to activate it for any command.

use crate::api::client::{ApiSpec, JiraApi, SearchPage, SearchQuery, SpecModule, UserScope};
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
    /// Writes the "server" accepts and does not act on: `"PUT /issue/DC-77"`.
    ///
    /// The failure `faults.json` cannot express, and the one that cost a user
    /// their edit: Jira answers 204 to a payload it has no intention of
    /// applying — a sub-task's `parent` on Data Center, a sub-task in a board
    /// move — and nothing in the response says so.
    ignored: std::collections::BTreeSet<String>,
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
        let ignored = read_json(dir, "ignored.json")
            .and_then(|v| serde_json::from_value(v).ok())
            .unwrap_or_default();

        Ok(Self {
            dir: dir.to_path_buf(),
            deployment,
            field_map: FieldMap::from_meta(&fields),
            issues: Mutex::new(issues),
            calls: Mutex::new(Vec::new()),
            faults,
            ignored,
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

    /// Whether this write is one the "server" acts on, or one it only answers.
    ///
    /// The call is still recorded either way — what was sent is exactly what a
    /// test asserting this behaviour needs to see.
    fn applies(&self, method: &str, path: &str) -> bool {
        !self.ignored.contains(&format!("{method} {path}"))
    }

    fn markup(&self) -> Markup {
        self.deployment.markup()
    }

    /// Ask for the rest of anything the payload paged, as the real client does.
    ///
    /// This client stands in for [`crate::api::rest::Rest`], and completion is
    /// something a client does rather than something a server sends — so
    /// without it here, no end-to-end test could ever observe it. A recording
    /// poses the case by giving an issue a `comment.total` larger than the
    /// comments in it, and a `comments-{key}.json` holding the rest.
    fn complete(&self, issue: &mut RemoteIssue) {
        let markup = self.markup();
        if crate::api::comments_are_paged(issue) {
            crate::api::set_comments(issue, self.comment_values(&issue.key), markup);
        }
        if crate::api::worklogs_are_paged(issue) {
            crate::api::set_worklogs(issue, self.worklog_values(&issue.key), markup);
        }
    }

    /// The recorded comment listing for an issue, or the issue's own.
    fn comment_values(&self, key: &str) -> Vec<Value> {
        if let Some(v) = read_json(&self.dir, &format!("comments-{key}.json")) {
            return values_of(&v, "comments");
        }
        self.issue_value(key)
            .ok()
            .map(|i| {
                values_of(
                    i.pointer("/fields/comment").unwrap_or(&Value::Null),
                    "comments",
                )
            })
            .unwrap_or_default()
    }

    /// The recorded worklog listing for an issue, or the issue's own.
    fn worklog_values(&self, key: &str) -> Vec<Value> {
        if let Some(v) = read_json(&self.dir, &format!("worklogs-{key}.json")) {
            return values_of(&v, "worklogs");
        }
        self.issue_value(key)
            .ok()
            .map(|i| {
                values_of(
                    i.pointer("/fields/worklog").unwrap_or(&Value::Null),
                    "worklogs",
                )
            })
            .unwrap_or_default()
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

    /// Add or remove the caller from an issue's watchers.
    ///
    /// The count moves with the flag and only when the flag moves, so a test
    /// that watches an issue twice sees one watcher rather than two — which is
    /// what a real server does, watchers being a set.
    fn set_watching(&self, key: &str, user_id: &str, watching: bool) -> Result<()> {
        let path = format!("/issue/{key}/watchers");
        let method = if watching { "POST" } else { "DELETE" };
        self.record(method, &path, json!(user_id))?;
        if !self.applies(method, &path) {
            return Ok(());
        }
        {
            let mut issues = self.issues.lock().expect("fixture lock");
            let Some(issue) = issues.get_mut(key) else {
                return Err(JiredError::generic(format!("{key} is not on this server")));
            };
            let watches = issue["fields"]["watches"].clone();
            let was = watches.get("isWatching").and_then(Value::as_bool) == Some(true);
            let count = watches
                .get("watchCount")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            let count = match (was, watching) {
                (false, true) => count + 1,
                (true, false) => count.saturating_sub(1),
                _ => count,
            };
            issue["fields"]["watches"] = json!({
                "isWatching": watching,
                "watchCount": count,
            });
        }
        // Deliberately no `bump_updated`: watching an issue is not an edit to
        // it, and a real server does not move `updated` for one.
        self.persist();
        Ok(())
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

    /// Which of Jira's three buckets a transition's target status falls in,
    /// declared beside the transition in the fixture's `transitions.json`.
    ///
    /// Read off the raw file rather than through [`Transition`], which carries
    /// no category: the model has no use for one, and a fixture does.
    fn status_category(&self, transition_id: &str) -> String {
        read_json(&self.dir, "transitions.json")
            .and_then(|v| v.as_array().cloned())
            .unwrap_or_default()
            .iter()
            .find(|t| t.get("id").and_then(Value::as_str) == Some(transition_id))
            .and_then(|t| t.get("to_status_category").and_then(Value::as_str))
            .unwrap_or("indeterminate")
            .to_string()
    }

    /// The sprint pool as it stands, including anything this run has written.
    fn stored_sprints(&self) -> Vec<Sprint> {
        read_json(&self.dir, "sprints.json")
            .and_then(|v| serde_json::from_value(v).ok())
            .unwrap_or_default()
    }

    /// Put the pool back on disk, so the next `jired` process in the same test
    /// sees what this one did rather than the recording it started from.
    fn save_sprints(&self, sprints: &[Sprint]) {
        if let Ok(text) = serde_json::to_string_pretty(sprints) {
            let _ = std::fs::write(self.dir.join("sprints.json"), text);
        }
    }
}

/// Which custom field this fixture's issues keep their sprint in.
///
/// Recognized by shape, the way `normalize` recognizes it, because `/field`
/// recordings do not always name it.
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

    fn spec_modules(&self) -> Vec<SpecModule> {
        crate::api::spec_modules(self.deployment)
    }

    /// A recording describes whichever modules were captured beside it, as
    /// `spec-<module>.<ext>`. One that was not replays the answer a server that
    /// does not serve it gives, so both halves of the command are exercised
    /// from the same fixture.
    async fn api_spec(&self, module: &SpecModule) -> Result<Option<ApiSpec>> {
        let file = self
            .dir
            .join(format!("spec-{}.{}", module.name, module.extension));
        let Ok(bytes) = std::fs::read(&file) else {
            return Ok(None);
        };
        Ok(Some(ApiSpec {
            module: module.clone(),
            media_type: "application/vnd.sun.wadl+xml".to_string(),
            bytes,
        }))
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

    async fn board_project(&self, board_id: &str) -> Result<Option<String>> {
        let located =
            read_json(&self.dir, &format!("board-{board_id}-config.json")).and_then(|v| {
                v.get("location")
                    .and_then(|l| l.get("projectKey").or_else(|| l.get("key")))
                    .and_then(Value::as_str)
                    .map(str::to_string)
            });
        if located.is_some() {
            return Ok(located);
        }
        // The Data Center shape: no `location` in the configuration, and the
        // answer on the board's project listing instead. Read through the same
        // function the real client uses, so a fixture cannot pass on a reading
        // the server would not give.
        Ok(
            read_json(&self.dir, &format!("board-{board_id}-projects.json"))
                .as_ref()
                .and_then(crate::api::rest::single_project),
        )
    }

    async fn sprints(&self, board_id: &str) -> Result<Vec<Sprint>> {
        // Boards that share a sprint pool answer identically, which is what the
        // single `sprints.json` stands for; a board with a pool of its own gets
        // a file of its own.
        Ok(read_json(&self.dir, &format!("sprints-{board_id}.json"))
            .or_else(|| read_json(&self.dir, "sprints.json"))
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
        self.save_sprints(&all);
        Ok(sprint)
    }

    async fn sprint(&self, id: &str) -> Result<Sprint> {
        self.stored_sprints()
            .into_iter()
            .find(|s| s.id == id)
            .ok_or_else(|| {
                JiredError::usage(format!("no sprint with id {id}")).with_hint(FIXTURE_HINT)
            })
    }

    async fn update_sprint(&self, id: &str, update: &SprintUpdate) -> Result<Sprint> {
        let path = format!("/rest/agile/1.0/sprint/{id}");
        self.record("POST", &path, update.to_body())?;

        let mut all = self.stored_sprints();
        let found = all.iter_mut().find(|s| s.id == id).ok_or_else(|| {
            JiredError::usage(format!("no sprint with id {id}")).with_hint(FIXTURE_HINT)
        })?;
        if !self.applies("POST", &path) {
            return Ok(found.clone());
        }
        // Merged, not replaced, exactly as the endpoint behind it merges: a
        // fixture that overwrote the unmentioned fields would let a caller that
        // forgot to send one pass here and lose it against a real server.
        if let Some(name) = &update.name {
            found.name = name.clone();
        }
        if let Some(goal) = &update.goal {
            found.goal = Some(goal.clone()).filter(|g| !g.is_empty());
        }
        if update.start.is_some() {
            found.start_date = update.start;
        }
        if update.end.is_some() {
            found.end_date = update.end;
        }
        if let Some(state) = &update.state {
            found.state = state.clone();
        }
        let updated = found.clone();
        self.save_sprints(&all);
        Ok(updated)
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

        let mut parsed = page
            .iter()
            .map(|v| normalize::issue_from_json(v, self.markup(), &self.field_map))
            .collect::<Result<Vec<_>>>()?;
        for issue in &mut parsed {
            self.complete(issue);
        }

        Ok(SearchPage {
            issues: parsed,
            start_at: query.start_at,
            total,
            next_page_token: None,
        })
    }

    async fn issue(&self, key: &str) -> Result<RemoteIssue> {
        let v = self.issue_value(key)?;
        let mut issue = normalize::issue_from_json(&v, self.markup(), &self.field_map)?;
        self.complete(&mut issue);
        Ok(issue)
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
        let path = format!("/issue/{key}");
        self.record("PUT", &path, json!({ "fields": fields }))?;
        // A server that takes the payload and does nothing leaves `updated`
        // alone too, which is how the reported case was spotted from outside.
        if !self.applies("PUT", &path) {
            return Ok(());
        }
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

    /// A recorded edit screen, or nothing.
    ///
    /// Absent means the server described none, which is how a recording that
    /// predates screen checks keeps working: unknown is not "no fields", and
    /// nothing is refused on the strength of a file that is not there.
    async fn edit_meta(&self, key: &str) -> Result<Option<ScreenMeta>> {
        Ok(read_json(&self.dir, &format!("editmeta-{key}.json"))
            .map(|v| normalize::screen_from_json(&v)))
    }

    /// A recorded create screen, keyed by project and the type's name.
    ///
    /// By name rather than by id because that is what a file carries and what a
    /// test writes; the real client turns one into the other with a call this
    /// stands in for.
    async fn create_meta(&self, project: &str, issue_type: &str) -> Result<Option<ScreenMeta>> {
        Ok(read_json(
            &self.dir,
            &format!("createmeta-{project}-{issue_type}.json"),
        )
        .map(|v| normalize::screen_from_json(&v)))
    }

    async fn link_types(&self) -> Result<Vec<IssueLinkType>> {
        Ok(read_json(&self.dir, "linkTypes.json")
            .and_then(|v| serde_json::from_value(v).ok())
            .unwrap_or_default())
    }

    /// Applied to both ends, the way the server stores one record and shows it
    /// twice: the inward issue gains an `outwardIssue` entry and vice versa,
    /// so a re-read of either renders the link from its own side.
    async fn create_link(&self, type_name: &str, inward: &str, outward: &str) -> Result<()> {
        self.record(
            "POST",
            "/issueLink",
            json!({
                "type": { "name": type_name },
                "inwardIssue": { "key": inward },
                "outwardIssue": { "key": outward },
            }),
        )?;
        let link_type = self
            .link_types()
            .await?
            .into_iter()
            .find(|t| t.name.eq_ignore_ascii_case(type_name))
            .ok_or_else(|| {
                JiredError::usage(format!("fixture has no link type `{type_name}`"))
                    .with_hint(FIXTURE_HINT)
            })?;
        let id = self.next_id();
        let type_json = json!({
            "id": link_type.id, "name": link_type.name,
            "inward": link_type.inward, "outward": link_type.outward,
        });
        {
            let mut issues = self.issues.lock().expect("fixture lock");
            for (own_key, role, other_key) in [
                (inward, "outwardIssue", outward),
                (outward, "inwardIssue", inward),
            ] {
                // An end outside the recording — a link to an issue the
                // fixture never held — is simply not reflected, as a pull
                // that never fetches it would never see it either.
                let Some(issue) = issues.get_mut(own_key) else {
                    continue;
                };
                let entry = json!({
                    "id": id, "type": type_json,
                    role: { "key": other_key },
                });
                match issue["fields"]["issuelinks"].as_array_mut() {
                    Some(list) => list.push(entry),
                    None => issue["fields"]["issuelinks"] = json!([entry]),
                }
            }
        }
        self.bump_updated(inward);
        self.bump_updated(outward);
        Ok(())
    }

    async fn delete_link(&self, link_id: &str) -> Result<()> {
        self.record("DELETE", &format!("/issueLink/{link_id}"), Value::Null)?;
        let mut touched = Vec::new();
        {
            let mut issues = self.issues.lock().expect("fixture lock");
            for (key, issue) in issues.iter_mut() {
                if let Some(list) = issue["fields"]["issuelinks"].as_array_mut() {
                    let before = list.len();
                    list.retain(|l| l.get("id").and_then(Value::as_str) != Some(link_id));
                    if list.len() != before {
                        touched.push(key.clone());
                    }
                }
            }
        }
        for key in touched {
            self.bump_updated(&key);
        }
        Ok(())
    }

    /// The dedicated assignee route, recorded under its own path.
    ///
    /// Its own recording is the point: a test can tell an assignee that was
    /// sent through the route from one that was smuggled into a field payload,
    /// and `ignored.json` can make this "server" answer it and do nothing,
    /// which is the failure the real one is capable of.
    async fn assign(&self, key: &str, user_id: Option<&str>) -> Result<()> {
        let path = format!("/issue/{key}/assignee");
        let body = crate::api::assignee_body(self.deployment, user_id);
        self.record("PUT", &path, body)?;
        if !self.applies("PUT", &path) {
            return Ok(());
        }
        {
            let mut issues = self.issues.lock().expect("fixture lock");
            if let Some(issue) = issues.get_mut(key) {
                issue["fields"]["assignee"] = match user_id {
                    Some(id) => {
                        // The display name a recorded directory gives this user,
                        // so the re-read renders the name a file would show.
                        let users: Vec<UserRef> = read_json(&self.dir, "users.json")
                            .and_then(|v| serde_json::from_value(v).ok())
                            .unwrap_or_default();
                        let display = users
                            .iter()
                            .find(|u| u.id == id)
                            .map(|u| u.display_name.clone())
                            .unwrap_or_else(|| id.to_string());
                        let field = match self.deployment {
                            Deployment::Cloud => "accountId",
                            Deployment::DataCenter => "name",
                        };
                        json!({ field: id, "displayName": display })
                    }
                    None => Value::Null,
                };
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
                    // A real server never returns a status without its category,
                    // and the category is what jired reads for "is this
                    // finished" — `list --category`, `sprint close`. Leaving it
                    // off would let a transitioned issue pass here in a shape no
                    // server sends, and only those two commands would notice.
                    "statusCategory": { "key": self.status_category(&t.id) },
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
        if !self.applies("POST", &path) {
            return Ok(());
        }

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
            // An issue that has never been in a sprint has no such field to
            // recognize, and a real server would still put one there. Borrowing
            // the id from an issue that does have one is what lets a move onto
            // an untouched issue be seen afterwards — without it the "server"
            // accepted every such move and recorded none of them, which is
            // precisely the silence this fixture now exists to catch.
            let fallback = issues.values().find_map(sprint_field);
            for key in keys {
                let Some(issue) = issues.get_mut(key) else {
                    continue;
                };
                let field = sprint_field(issue).or_else(|| fallback.clone());
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

    /// Recorded under the path the wizard's own step posts to, so a test can
    /// tell a move that was sent from one that was only planned — and so
    /// `ignored.json` can make this "server" answer it and do nothing, which
    /// is the failure the real one is capable of.
    async fn move_subtask_parent(&self, key: &str, parent: &str) -> Result<()> {
        let path = "/secure/MoveSubTaskParent.jspa";
        self.record("POST", path, json!({ "id": key, "parentIssue": parent }))?;
        if !self.applies("POST", path) {
            return Ok(());
        }
        {
            let mut issues = self.issues.lock().expect("fixture lock");
            let Some(issue) = issues.get_mut(key) else {
                return Err(JiredError::usage(format!("fixture has no issue {key}"))
                    .with_hint(FIXTURE_HINT)
                    .with_detail("status", 404));
            };
            // Keep the shape a real payload has: the id and summary of the
            // parent travel with the key, and the renderer reads all three.
            issue["fields"]["parent"] = json!({ "key": parent });
        }
        self.bump_updated(key);
        Ok(())
    }

    async fn worklogs(&self, key: &str) -> Result<Vec<RemoteWorklog>> {
        let values = self.worklog_values(key);
        Ok(normalize::worklogs_from(Some(&json!(values)), self.markup()).0)
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

    /// A recorded comment listing if there is one, else the issue's own.
    ///
    /// `comments-{key}.json` is how a fixture poses the case this call exists
    /// for: an issue payload whose `comment.total` exceeds the comments in it.
    async fn comments(&self, key: &str) -> Result<Vec<RemoteComment>> {
        let values = self.comment_values(key);
        Ok(normalize::comments_from(Some(&json!(values)), self.markup()).0)
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
            .0
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
            .0
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

    /// The users a scope permits, from the narrowest recording that exists.
    ///
    /// `assignable-{key}.json`, then `assignable-{project}.json`, then
    /// `users.json` — the same falling-back the sprints listing does. A fixture
    /// that records only `users.json` therefore answers every scope with it,
    /// which is what keeps a recording that predates the assignable search
    /// working; a test that cares about the difference writes the narrower file.
    async fn search_users(&self, query: &str, scope: UserScope<'_>) -> Result<Vec<UserRef>> {
        let mut names: Vec<String> = Vec::new();
        match scope {
            UserScope::Any => {}
            UserScope::Issue(key) => {
                names.push(format!("assignable-{key}.json"));
                if let Some((project, _)) = key.rsplit_once('-') {
                    names.push(format!("assignable-{project}.json"));
                }
            }
            UserScope::Project(project) => names.push(format!("assignable-{project}.json")),
        }
        names.push("users.json".to_string());

        let users: Vec<UserRef> = names
            .iter()
            .find_map(|name| read_json(&self.dir, name))
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

    /// Watching is recorded on the issue itself, in the shape a real payload
    /// carries it: `fields.watches.isWatching` is what `jired watch` reads back
    /// and what the `watcher = currentUser()` clause below filters on.
    async fn watch(&self, key: &str, user_id: &str) -> Result<()> {
        self.set_watching(key, user_id, true)
    }

    async fn unwatch(&self, key: &str, user_id: &str) -> Result<()> {
        self.set_watching(key, user_id, false)
    }
}

/// The rows of a listing, which a fixture may write as the server's wrapper
/// object or as a bare array.
fn values_of(v: &Value, field: &str) -> Vec<Value> {
    match v {
        Value::Object(o) => o
            .get(field)
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default(),
        Value::Array(a) => a.clone(),
        _ => Vec::new(),
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

    // `watcher = currentUser()` is how `jired watch` asks which issues it
    // already watches; the fixture answers from the flag its own watch call set.
    if lower.contains("watcher = currentuser()") {
        let watching = issue
            .pointer("/fields/watches/isWatching")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if !watching {
            return false;
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
