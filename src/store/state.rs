//! `.jired/state.db` — base snapshot, remote-tracking snapshot, push journal.
//!
//! Everything here except [`pending_op`](StateStore::pending_ops) is a cache:
//! deleting the file and re-running `jired fetch` must always recover, which is
//! why nothing irreplaceable is stored in it (decision D17).

use crate::api::model::{parse_timestamp, RemoteIssue};
use crate::api::{Markup, RichText};
use crate::convert::ast::RawBlock;
use crate::error::{JiredError, Result};
use crate::store::{migrate, now_rfc3339, open_db, Migration};
use crate::workspace::Workspace;
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension};
use std::collections::BTreeMap;

const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        sql: r#"
CREATE TABLE workspace (key TEXT PRIMARY KEY, value TEXT);

CREATE TABLE issue (
  key             TEXT PRIMARY KEY,
  id              TEXT NOT NULL,
  base_raw        TEXT,
  base_updated    TEXT,
  base_desc       TEXT,
  base_md_hash    TEXT,
  remote_raw      TEXT,
  remote_updated  TEXT,
  remote_fetched_at TEXT,
  file_path       TEXT,
  parent_key      TEXT,
  deleted_remote  INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX issue_remote_updated ON issue(remote_updated);
CREATE INDEX issue_parent ON issue(parent_key);

CREATE TABLE comment (
  id            TEXT NOT NULL,
  issue_key     TEXT NOT NULL,
  author        TEXT,
  author_id     TEXT,
  created       TEXT,
  base_updated  TEXT,
  base_body     TEXT,
  remote_updated TEXT,
  remote_body   TEXT,
  deleted_remote INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (issue_key, id)
);

CREATE TABLE attachment (
  id          TEXT NOT NULL,
  issue_key   TEXT NOT NULL,
  filename    TEXT NOT NULL,
  size        INTEGER NOT NULL DEFAULT 0,
  mime        TEXT,
  sha256      TEXT,
  author      TEXT,
  created     TEXT,
  content_url TEXT,
  downloaded  INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (issue_key, id)
);

CREATE TABLE raw_block (
  id         TEXT NOT NULL,
  issue_key  TEXT NOT NULL,
  container  TEXT NOT NULL,
  ordinal    INTEGER NOT NULL,
  node_json  TEXT NOT NULL,
  approx_md  TEXT NOT NULL,
  label      TEXT NOT NULL DEFAULT '',
  PRIMARY KEY (issue_key, container, id)
);

CREATE TABLE pending_op (
  id         INTEGER PRIMARY KEY,
  issue_key  TEXT NOT NULL,
  kind       TEXT NOT NULL,
  payload    TEXT NOT NULL,
  status     TEXT NOT NULL DEFAULT 'pending'
             CHECK (status IN ('pending','done','failed')),
  error      TEXT,
  created_at TEXT NOT NULL
);
CREATE INDEX pending_op_issue ON pending_op(issue_key, status);

CREATE TABLE conflict (
  id         INTEGER PRIMARY KEY,
  issue_key  TEXT NOT NULL,
  field      TEXT NOT NULL,
  base_val   TEXT,
  ours_val   TEXT,
  theirs_val TEXT,
  resolved   TEXT CHECK (resolved IN ('ours','theirs','manual'))
);
CREATE UNIQUE INDEX conflict_issue_field ON conflict(issue_key, field);

CREATE TABLE sprint (id TEXT PRIMARY KEY, board_id TEXT, name TEXT, state TEXT, raw TEXT);
CREATE TABLE board (id TEXT PRIMARY KEY, name TEXT, type TEXT, columns_json TEXT);
CREATE TABLE field_meta (field_id TEXT PRIMARY KEY, name TEXT, schema_json TEXT, well_known TEXT);
CREATE TABLE transition_cache (
  issue_key TEXT NOT NULL, tid TEXT NOT NULL, name TEXT, to_status TEXT,
  required_fields TEXT, fetched_at TEXT, PRIMARY KEY (issue_key, tid)
);
"#,
    },
    Migration {
        version: 2,
        sql: r#"
CREATE TABLE changelog (
  issue_key  TEXT NOT NULL,
  entry_id   TEXT NOT NULL,
  author     TEXT,
  created    TEXT,
  items_json TEXT NOT NULL,
  PRIMARY KEY (issue_key, entry_id)
);
CREATE INDEX changelog_issue ON changelog(issue_key, created);
"#,
    },
];

/// The last state jired successfully synced to a file.
#[derive(Debug, Clone)]
pub struct BaseSnapshot {
    pub raw: serde_json::Value,
    pub updated: DateTime<Utc>,
    pub description: String,
    pub md_hash: String,
}

/// The last state seen on the server.
#[derive(Debug, Clone)]
pub struct RemoteSnapshot {
    pub raw: serde_json::Value,
    pub updated: DateTime<Utc>,
    pub fetched_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone)]
pub struct IssueRow {
    pub key: String,
    pub id: String,
    pub base: Option<BaseSnapshot>,
    pub remote: Option<RemoteSnapshot>,
    pub file_path: Option<String>,
    pub parent_key: Option<String>,
    pub deleted_remote: bool,
}

impl IssueRow {
    /// True when the server has moved on since the working copy was written.
    pub fn is_behind(&self) -> bool {
        match (&self.base, &self.remote) {
            (Some(b), Some(r)) => r.updated > b.updated,
            (None, Some(_)) => true,
            _ => false,
        }
    }

    /// An issue fetched but never written to a file yet.
    pub fn is_new_locally(&self) -> bool {
        self.base.is_none()
    }
}

#[derive(Debug, Clone)]
pub struct CommentRow {
    pub id: String,
    pub issue_key: String,
    pub author: Option<String>,
    pub author_id: Option<String>,
    pub created: Option<DateTime<Utc>>,
    pub base_updated: Option<DateTime<Utc>>,
    pub base_body: Option<String>,
    pub remote_updated: Option<DateTime<Utc>>,
    pub remote_body: Option<String>,
    pub deleted_remote: bool,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct PendingOp {
    pub id: i64,
    pub issue_key: String,
    pub kind: String,
    pub payload: String,
    pub status: String,
    pub error: Option<String>,
}

/// A board and the column-to-status mapping the grouping projector needs.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct BoardRow {
    pub id: String,
    pub name: String,
    pub board_type: String,
    pub columns: Vec<crate::api::BoardColumn>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct SprintRow {
    pub id: String,
    pub board_id: Option<String>,
    pub name: String,
    pub state: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ConflictRow {
    pub id: i64,
    pub issue_key: String,
    pub field: String,
    pub base_val: Option<String>,
    pub ours_val: Option<String>,
    pub theirs_val: Option<String>,
    pub resolved: Option<String>,
}

pub struct StateStore {
    conn: Connection,
}

impl StateStore {
    pub fn open(ws: &Workspace) -> Result<Self> {
        let conn = open_db(&ws.state_db_path())?;
        migrate(&conn, "state.db", MIGRATIONS)?;
        Ok(Self { conn })
    }

    pub fn in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        migrate(&conn, "state.db", MIGRATIONS)?;
        Ok(Self { conn })
    }

    // ---- workspace key/value -------------------------------------------------

    pub fn get_meta(&self, key: &str) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row("SELECT value FROM workspace WHERE key = ?1", [key], |r| {
                r.get(0)
            })
            .optional()?)
    }

    pub fn set_meta(&self, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO workspace (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    pub fn last_fetch_at(&self) -> Result<Option<DateTime<Utc>>> {
        Ok(self
            .get_meta("last_fetch_at")?
            .and_then(|s| parse_timestamp(&s)))
    }

    pub fn set_last_fetch_at(&self, at: &DateTime<Utc>) -> Result<()> {
        self.set_meta("last_fetch_at", &crate::api::model::format_timestamp(at))
    }

    // ---- issues --------------------------------------------------------------

    /// Record a freshly fetched issue as the remote-tracking snapshot.
    ///
    /// Returns true when the snapshot actually changed, which is what `fetch`
    /// counts and what lets `pull` skip untouched issues entirely.
    pub fn upsert_remote(&self, issue: &RemoteIssue) -> Result<bool> {
        let updated = crate::api::model::format_timestamp(&issue.updated);
        let previous: Option<String> = self
            .conn
            .query_row(
                "SELECT remote_updated FROM issue WHERE key = ?1",
                [&issue.key],
                |r| r.get(0),
            )
            .optional()?
            .flatten();

        self.conn.execute(
            "INSERT INTO issue (key, id, remote_raw, remote_updated, remote_fetched_at,
                                parent_key, deleted_remote)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0)
             ON CONFLICT(key) DO UPDATE SET
               id = excluded.id,
               remote_raw = excluded.remote_raw,
               remote_updated = excluded.remote_updated,
               remote_fetched_at = excluded.remote_fetched_at,
               parent_key = excluded.parent_key,
               deleted_remote = 0",
            params![
                issue.key,
                issue.id,
                issue.raw.to_string(),
                updated,
                now_rfc3339(),
                issue.fields.parent,
            ],
        )?;

        self.replace_comments(issue)?;
        self.replace_attachments(issue)?;
        Ok(previous.as_deref() != Some(updated.as_str()))
    }

    /// Advance base to the current remote snapshot after a successful write.
    pub fn advance_base(
        &self,
        key: &str,
        description: &str,
        md_hash: &str,
        file_path: &str,
    ) -> Result<()> {
        let changed = self.conn.execute(
            "UPDATE issue SET base_raw = remote_raw, base_updated = remote_updated,
                              base_desc = ?2, base_md_hash = ?3, file_path = ?4
             WHERE key = ?1",
            params![key, description, md_hash, file_path],
        )?;
        if changed == 0 {
            return Err(JiredError::corrupt(format!(
                "cannot advance base for unknown issue {key}"
            ))
            .with_detail("issue", key)
            .with_hint(crate::store::recovery_hint("state.db")));
        }
        self.conn.execute(
            "UPDATE comment SET base_body = remote_body, base_updated = remote_updated
             WHERE issue_key = ?1",
            [key],
        )?;
        Ok(())
    }

    /// Set base directly from a payload the server just confirmed (push path).
    pub fn set_base_from_remote(
        &self,
        issue: &RemoteIssue,
        description: &str,
        md_hash: &str,
        file_path: &str,
    ) -> Result<()> {
        self.upsert_remote(issue)?;
        self.advance_base(&issue.key, description, md_hash, file_path)
    }

    /// Register a locally created issue that has no server counterpart yet.
    ///
    /// It carries no snapshots at all: `push` recognises the `NEW-` prefix and
    /// creates it rather than diffing it, and `rekey_issue` replaces the
    /// temporary key once the server assigns a real one.
    pub fn record_new_issue(&self, key: &str, project: &str, file_path: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO issue (key, id, file_path) VALUES (?1, '', ?2)
             ON CONFLICT(key) DO UPDATE SET file_path = excluded.file_path",
            params![key, file_path],
        )?;
        self.set_meta(&format!("new_issue_project:{key}"), project)
    }

    /// Project a locally created issue belongs to.
    pub fn new_issue_project(&self, key: &str) -> Result<Option<String>> {
        self.get_meta(&format!("new_issue_project:{key}"))
    }

    pub fn forget_new_issue_project(&self, key: &str) -> Result<()> {
        self.conn.execute(
            "DELETE FROM workspace WHERE key = ?1",
            [format!("new_issue_project:{key}")],
        )?;
        Ok(())
    }

    pub fn issue(&self, key: &str) -> Result<Option<IssueRow>> {
        let mut stmt = self.conn.prepare(ISSUE_SELECT)?;
        let row = stmt
            .query_row([key], |r| Ok(issue_row_from(r)))
            .optional()?;
        row.transpose()
    }

    pub fn issues(&self) -> Result<Vec<IssueRow>> {
        let mut stmt = self
            .conn
            .prepare(&format!("{ISSUE_SELECT_ALL} ORDER BY key"))?;
        let rows = stmt.query_map([], |r| Ok(issue_row_from(r)))?;
        rows.map(|r| r?).collect()
    }

    /// Record a new hash for a file whose text was rewritten without its
    /// meaning changing — a `doctor` repair, not a sync.
    ///
    /// Without this, reformatting a clean file would make every later `pull`
    /// treat it as locally edited.
    pub fn set_base_md_hash(&self, key: &str, hash: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE issue SET base_md_hash = ?2 WHERE key = ?1 AND base_raw IS NOT NULL",
            params![key, hash],
        )?;
        Ok(())
    }

    pub fn set_file_path(&self, key: &str, path: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE issue SET file_path = ?2 WHERE key = ?1",
            params![key, path],
        )?;
        Ok(())
    }

    pub fn mark_deleted_remote(&self, keys: &[String]) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        for key in keys {
            tx.execute("UPDATE issue SET deleted_remote = 1 WHERE key = ?1", [key])?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn forget_issue(&self, key: &str) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        for table in [
            "comment",
            "attachment",
            "raw_block",
            "pending_op",
            "conflict",
        ] {
            tx.execute(&format!("DELETE FROM {table} WHERE issue_key = ?1"), [key])?;
        }
        tx.execute("DELETE FROM issue WHERE key = ?1", [key])?;
        tx.commit()?;
        Ok(())
    }

    /// Replace a temporary `NEW-*` key with the real one the server assigned.
    pub fn rekey_issue(&self, from: &str, to: &str, new_id: &str) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "UPDATE issue SET key = ?2, id = ?3 WHERE key = ?1",
            params![from, to, new_id],
        )?;
        for table in [
            "comment",
            "attachment",
            "raw_block",
            "pending_op",
            "conflict",
        ] {
            tx.execute(
                &format!("UPDATE {table} SET issue_key = ?2 WHERE issue_key = ?1"),
                params![from, to],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    // ---- comments ------------------------------------------------------------

    fn replace_comments(&self, issue: &RemoteIssue) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        // Comments absent from the payload were deleted on the server; keep the
        // row so a merge can tell "deleted remotely" from "never existed".
        tx.execute(
            "UPDATE comment SET deleted_remote = 1 WHERE issue_key = ?1",
            [&issue.key],
        )?;
        for c in &issue.comments {
            tx.execute(
                "INSERT INTO comment (id, issue_key, author, author_id, created,
                                      remote_updated, remote_body, deleted_remote)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 0)
                 ON CONFLICT(issue_key, id) DO UPDATE SET
                   author = excluded.author, author_id = excluded.author_id,
                   remote_updated = excluded.remote_updated,
                   remote_body = excluded.remote_body, deleted_remote = 0",
                params![
                    c.id,
                    issue.key,
                    c.author.as_ref().map(|a| a.display_name.clone()),
                    c.author.as_ref().map(|a| a.id.clone()),
                    c.created.as_ref().map(crate::api::model::format_timestamp),
                    c.updated.as_ref().map(crate::api::model::format_timestamp),
                    c.body.to_storage(),
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn comments(&self, issue_key: &str) -> Result<Vec<CommentRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, issue_key, author, author_id, created, base_updated, base_body,
                    remote_updated, remote_body, deleted_remote
             FROM comment WHERE issue_key = ?1 ORDER BY created, id",
        )?;
        let rows = stmt.query_map([issue_key], |r| {
            Ok(CommentRow {
                id: r.get(0)?,
                issue_key: r.get(1)?,
                author: r.get(2)?,
                author_id: r.get(3)?,
                created: r
                    .get::<_, Option<String>>(4)?
                    .as_deref()
                    .and_then(parse_timestamp),
                base_updated: r
                    .get::<_, Option<String>>(5)?
                    .as_deref()
                    .and_then(parse_timestamp),
                base_body: r.get(6)?,
                remote_updated: r
                    .get::<_, Option<String>>(7)?
                    .as_deref()
                    .and_then(parse_timestamp),
                remote_body: r.get(8)?,
                deleted_remote: r.get::<_, i64>(9)? != 0,
            })
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    pub fn record_comment_base(
        &self,
        issue_key: &str,
        id: &str,
        body: &str,
        updated: Option<&DateTime<Utc>>,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO comment (id, issue_key, base_body, base_updated, remote_body, remote_updated)
             VALUES (?1, ?2, ?3, ?4, ?3, ?4)
             ON CONFLICT(issue_key, id) DO UPDATE SET
               base_body = excluded.base_body, base_updated = excluded.base_updated",
            params![
                id,
                issue_key,
                body,
                updated.map(crate::api::model::format_timestamp)
            ],
        )?;
        Ok(())
    }

    // ---- attachments ---------------------------------------------------------

    fn replace_attachments(&self, issue: &RemoteIssue) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        for a in &issue.attachments {
            tx.execute(
                "INSERT INTO attachment (id, issue_key, filename, size, mime, author,
                                         created, content_url)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                 ON CONFLICT(issue_key, id) DO UPDATE SET
                   filename = excluded.filename, size = excluded.size,
                   mime = excluded.mime, content_url = excluded.content_url",
                params![
                    a.id,
                    issue.key,
                    a.filename,
                    a.size as i64,
                    a.mime,
                    a.author,
                    a.created.as_ref().map(crate::api::model::format_timestamp),
                    a.content_url,
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn attachments(&self, issue_key: &str) -> Result<Vec<crate::api::RemoteAttachment>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, filename, size, mime, created, author, content_url
             FROM attachment WHERE issue_key = ?1 ORDER BY filename, id",
        )?;
        let rows = stmt.query_map([issue_key], |r| {
            Ok(crate::api::RemoteAttachment {
                id: r.get(0)?,
                filename: r.get(1)?,
                size: r.get::<_, i64>(2)? as u64,
                mime: r.get(3)?,
                created: r
                    .get::<_, Option<String>>(4)?
                    .as_deref()
                    .and_then(parse_timestamp),
                author: r.get(5)?,
                content_url: r.get::<_, Option<String>>(6)?.unwrap_or_default(),
            })
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    pub fn mark_attachment_downloaded(
        &self,
        issue_key: &str,
        id: &str,
        sha256: &str,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE attachment SET downloaded = 1, sha256 = ?3 WHERE issue_key = ?1 AND id = ?2",
            params![issue_key, id, sha256],
        )?;
        Ok(())
    }

    // ---- boards and sprints --------------------------------------------------

    pub fn save_boards(&self, boards: &[BoardRow]) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        for b in boards {
            tx.execute(
                "INSERT INTO board (id, name, type, columns_json) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(id) DO UPDATE SET
                   name = excluded.name, type = excluded.type,
                   columns_json = excluded.columns_json",
                params![
                    b.id,
                    b.name,
                    b.board_type,
                    serde_json::to_string(&b.columns)?
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn boards(&self) -> Result<Vec<BoardRow>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, name, type, columns_json FROM board ORDER BY name, id")?;
        let rows = stmt.query_map([], |r| {
            let columns: Option<String> = r.get(3)?;
            Ok(BoardRow {
                id: r.get(0)?,
                name: r.get::<_, Option<String>>(1)?.unwrap_or_default(),
                board_type: r.get::<_, Option<String>>(2)?.unwrap_or_default(),
                columns: columns
                    .and_then(|c| serde_json::from_str(&c).ok())
                    .unwrap_or_default(),
            })
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    pub fn save_sprints(&self, sprints: &[crate::api::Sprint]) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        for s in sprints {
            tx.execute(
                "INSERT INTO sprint (id, board_id, name, state, raw) VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(id) DO UPDATE SET
                   board_id = excluded.board_id, name = excluded.name,
                   state = excluded.state, raw = excluded.raw",
                params![s.id, s.board_id, s.name, s.state, serde_json::to_string(s)?],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn sprints(&self) -> Result<Vec<SprintRow>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, board_id, name, state FROM sprint ORDER BY name, id")?;
        let rows = stmt.query_map([], |r| {
            Ok(SprintRow {
                id: r.get(0)?,
                board_id: r.get(1)?,
                name: r.get::<_, Option<String>>(2)?.unwrap_or_default(),
                state: r.get::<_, Option<String>>(3)?.unwrap_or_default(),
            })
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    // ---- changelog -----------------------------------------------------------

    pub fn save_changelog(
        &self,
        issue_key: &str,
        entries: &[crate::api::ChangelogEntry],
    ) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        for e in entries {
            tx.execute(
                "INSERT INTO changelog (issue_key, entry_id, author, created, items_json)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(issue_key, entry_id) DO UPDATE SET
                   author = excluded.author, created = excluded.created,
                   items_json = excluded.items_json",
                params![
                    issue_key,
                    e.id,
                    e.author,
                    e.created.as_ref().map(crate::api::model::format_timestamp),
                    serde_json::to_string(&e.items)?,
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn changelog(&self, issue_key: &str) -> Result<Vec<crate::api::ChangelogEntry>> {
        let mut stmt = self.conn.prepare(
            "SELECT entry_id, author, created, items_json FROM changelog
             WHERE issue_key = ?1 ORDER BY created DESC, entry_id DESC",
        )?;
        let rows = stmt.query_map([issue_key], |r| {
            let items: String = r.get(3)?;
            Ok(crate::api::ChangelogEntry {
                id: r.get(0)?,
                author: r.get(1)?,
                created: r
                    .get::<_, Option<String>>(2)?
                    .as_deref()
                    .and_then(parse_timestamp),
                items: serde_json::from_str(&items).unwrap_or_default(),
            })
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    // ---- field metadata ------------------------------------------------------

    /// Cache the instance's field definitions.
    ///
    /// Custom-field ids differ per Jira instance, so every code path that
    /// re-parses a stored snapshot needs this map. Without it `story_points`,
    /// `epic` and `sprint` would silently degrade into raw `custom:` entries.
    pub fn save_field_meta(&self, fields: &[crate::api::FieldMeta]) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute("DELETE FROM field_meta", [])?;
        for f in fields {
            // The column has always been called schema_json; it now holds the
            // whole schema rather than only its type. Older rows hold the bare
            // type string and are read back as such.
            let schema = match (&f.schema_type, &f.items) {
                (None, _) => None,
                (Some(t), None) => Some(serde_json::json!({ "type": t }).to_string()),
                (Some(t), Some(i)) => Some(serde_json::json!({"type": t, "items": i}).to_string()),
            };
            tx.execute(
                "INSERT INTO field_meta (field_id, name, schema_json) VALUES (?1, ?2, ?3)",
                params![f.id, f.name, schema],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn field_meta(&self) -> Result<Vec<crate::api::FieldMeta>> {
        let mut stmt = self
            .conn
            .prepare("SELECT field_id, name, schema_json FROM field_meta ORDER BY field_id")?;
        let rows = stmt.query_map([], |r| {
            let id: String = r.get(0)?;
            let (schema_type, items) = parse_schema(r.get::<_, Option<String>>(2)?);
            Ok(crate::api::FieldMeta {
                custom: id.starts_with("customfield_"),
                id,
                name: r.get::<_, Option<String>>(1)?.unwrap_or_default(),
                schema_type,
                items,
            })
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// Cached field definitions by id, for the code that has to know a field's
    /// shape rather than its name.
    pub fn field_meta_by_id(&self) -> Result<BTreeMap<String, crate::api::FieldMeta>> {
        Ok(self
            .field_meta()?
            .into_iter()
            .map(|f| (f.id.clone(), f))
            .collect())
    }

    pub fn field_map(&self) -> Result<crate::api::normalize::FieldMap> {
        Ok(crate::api::normalize::FieldMap::from_meta(
            &self.field_meta()?,
        ))
    }

    // ---- lossy regions -------------------------------------------------------

    pub fn replace_raw_blocks(
        &self,
        issue_key: &str,
        container: &str,
        blocks: &[RawBlock],
    ) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "DELETE FROM raw_block WHERE issue_key = ?1 AND container = ?2",
            params![issue_key, container],
        )?;
        for b in blocks {
            tx.execute(
                "INSERT INTO raw_block (id, issue_key, container, ordinal, node_json, approx_md, label)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![b.id, issue_key, container, b.ordinal as i64, b.node_json, b.approx_md, b.label],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn raw_blocks(&self, issue_key: &str, container: &str) -> Result<Vec<RawBlock>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, ordinal, node_json, approx_md, label FROM raw_block
             WHERE issue_key = ?1 AND container = ?2 ORDER BY ordinal",
        )?;
        let rows = stmt.query_map(params![issue_key, container], |r| {
            Ok(RawBlock {
                id: r.get(0)?,
                ordinal: r.get::<_, i64>(1)? as usize,
                node_json: r.get(2)?,
                approx_md: r.get(3)?,
                label: r.get(4)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    // ---- push journal --------------------------------------------------------

    pub fn journal_ops(&self, issue_key: &str, ops: &[(String, String)]) -> Result<Vec<i64>> {
        let tx = self.conn.unchecked_transaction()?;
        let mut ids = Vec::with_capacity(ops.len());
        for (kind, payload) in ops {
            tx.execute(
                "INSERT INTO pending_op (issue_key, kind, payload, status, created_at)
                 VALUES (?1, ?2, ?3, 'pending', ?4)",
                params![issue_key, kind, payload, now_rfc3339()],
            )?;
            ids.push(tx.last_insert_rowid());
        }
        tx.commit()?;
        Ok(ids)
    }

    pub fn mark_op(&self, id: i64, status: &str, error: Option<&str>) -> Result<()> {
        self.conn.execute(
            "UPDATE pending_op SET status = ?2, error = ?3 WHERE id = ?1",
            params![id, status, error],
        )?;
        Ok(())
    }

    pub fn pending_ops(&self) -> Result<Vec<PendingOp>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, issue_key, kind, payload, status, error FROM pending_op
             WHERE status != 'done' ORDER BY id",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(PendingOp {
                id: r.get(0)?,
                issue_key: r.get(1)?,
                kind: r.get(2)?,
                payload: r.get(3)?,
                status: r.get(4)?,
                error: r.get(5)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    pub fn clear_ops(&self, issue_key: &str) -> Result<()> {
        self.conn
            .execute("DELETE FROM pending_op WHERE issue_key = ?1", [issue_key])?;
        Ok(())
    }

    // ---- conflicts -----------------------------------------------------------

    pub fn record_conflict(
        &self,
        issue_key: &str,
        field: &str,
        base: Option<&str>,
        ours: Option<&str>,
        theirs: Option<&str>,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO conflict (issue_key, field, base_val, ours_val, theirs_val)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(issue_key, field) DO UPDATE SET
               base_val = excluded.base_val, ours_val = excluded.ours_val,
               theirs_val = excluded.theirs_val, resolved = NULL",
            params![issue_key, field, base, ours, theirs],
        )?;
        Ok(())
    }

    pub fn conflicts(&self) -> Result<Vec<ConflictRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, issue_key, field, base_val, ours_val, theirs_val, resolved
             FROM conflict ORDER BY issue_key, field",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(ConflictRow {
                id: r.get(0)?,
                issue_key: r.get(1)?,
                field: r.get(2)?,
                base_val: r.get(3)?,
                ours_val: r.get(4)?,
                theirs_val: r.get(5)?,
                resolved: r.get(6)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// Remember an issue's transitions so `jired transition` still knows the
    /// workflow when the network is unreachable.
    pub fn cache_transitions(
        &self,
        issue_key: &str,
        transitions: &[crate::api::Transition],
    ) -> Result<()> {
        self.conn.execute(
            "DELETE FROM transition_cache WHERE issue_key = ?1",
            [issue_key],
        )?;
        let now = crate::api::model::format_timestamp(&chrono::Utc::now());
        for t in transitions {
            self.conn.execute(
                "INSERT OR REPLACE INTO transition_cache
                 (issue_key, tid, name, to_status, required_fields, fetched_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                rusqlite::params![
                    issue_key,
                    t.id,
                    t.name,
                    t.to_status,
                    serde_json::to_string(&t.required_fields)?,
                    now,
                ],
            )?;
        }
        Ok(())
    }

    /// The last known transitions for an issue, and when they were fetched.
    pub fn cached_transitions(
        &self,
        issue_key: &str,
    ) -> Result<Option<(Vec<crate::api::Transition>, String)>> {
        let mut stmt = self.conn.prepare(
            "SELECT tid, name, to_status, required_fields, fetched_at
             FROM transition_cache WHERE issue_key = ?1 ORDER BY tid",
        )?;
        let rows = stmt.query_map([issue_key], |r| {
            let required: String = r.get(3).unwrap_or_default();
            Ok((
                crate::api::Transition {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    to_status: r.get(2)?,
                    to_status_id: None,
                    required_fields: serde_json::from_str(&required).unwrap_or_default(),
                },
                r.get::<_, Option<String>>(4)?.unwrap_or_default(),
            ))
        })?;
        let collected: Vec<(crate::api::Transition, String)> =
            rows.collect::<std::result::Result<Vec<_>, _>>()?;
        if collected.is_empty() {
            return Ok(None);
        }
        let fetched = collected[0].1.clone();
        Ok(Some((
            collected.into_iter().map(|(t, _)| t).collect(),
            fetched,
        )))
    }

    pub fn clear_conflicts(&self, issue_key: &str) -> Result<()> {
        self.conn
            .execute("DELETE FROM conflict WHERE issue_key = ?1", [issue_key])?;
        Ok(())
    }

    /// Description as rich text, for whichever snapshot the caller wants.
    pub fn description_of(&self, raw: &serde_json::Value, markup: Markup) -> RichText {
        crate::api::normalize::rich_text(
            raw.get("fields").and_then(|f| f.get("description")),
            markup,
        )
    }
}

const ISSUE_SELECT: &str = "SELECT key, id, base_raw, base_updated, base_desc, base_md_hash,
     remote_raw, remote_updated, remote_fetched_at, file_path, parent_key, deleted_remote
     FROM issue WHERE key = ?1";

const ISSUE_SELECT_ALL: &str = "SELECT key, id, base_raw, base_updated, base_desc, base_md_hash,
     remote_raw, remote_updated, remote_fetched_at, file_path, parent_key, deleted_remote
     FROM issue";

fn issue_row_from(r: &rusqlite::Row<'_>) -> Result<IssueRow> {
    // Read first: a message about a corrupt row is close to useless without the
    // key of the row it is about.
    let key: String = r.get(0)?;
    let base_raw: Option<String> = r.get(2)?;
    let base_updated: Option<String> = r.get(3)?;
    let remote_raw: Option<String> = r.get(6)?;
    let remote_updated: Option<String> = r.get(7)?;

    let base = match (base_raw, base_updated) {
        (Some(raw), Some(updated)) => Some(BaseSnapshot {
            raw: serde_json::from_str(&raw)?,
            updated: parse_timestamp(&updated).ok_or_else(|| {
                JiredError::corrupt(format!(
                    "{key}: state.db holds an unparseable base timestamp"
                ))
                .with_detail("issue", key.clone())
                .with_hint(crate::store::recovery_hint("state.db"))
            })?,
            description: r.get::<_, Option<String>>(4)?.unwrap_or_default(),
            md_hash: r.get::<_, Option<String>>(5)?.unwrap_or_default(),
        }),
        _ => None,
    };
    let remote = match (remote_raw, remote_updated) {
        (Some(raw), Some(updated)) => Some(RemoteSnapshot {
            raw: serde_json::from_str(&raw)?,
            updated: parse_timestamp(&updated).ok_or_else(|| {
                JiredError::corrupt(format!(
                    "{key}: state.db holds an unparseable remote timestamp"
                ))
                .with_detail("issue", key.clone())
                .with_hint(crate::store::recovery_hint("state.db"))
            })?,
            fetched_at: r
                .get::<_, Option<String>>(8)?
                .as_deref()
                .and_then(parse_timestamp),
        }),
        _ => None,
    };

    Ok(IssueRow {
        key,
        id: r.get(1)?,
        base,
        remote,
        file_path: r.get(9)?,
        parent_key: r.get(10)?,
        deleted_remote: r.get::<_, i64>(11)? != 0,
    })
}

/// Content hash used for change detection (decision D18).
/// Read back what `save_field_meta` wrote, tolerating the bare type string that
/// databases written by earlier versions hold.
fn parse_schema(stored: Option<String>) -> (Option<String>, Option<String>) {
    let Some(text) = stored else {
        return (None, None);
    };
    match serde_json::from_str::<serde_json::Value>(&text) {
        Ok(serde_json::Value::Object(map)) => (
            map.get("type").and_then(|v| v.as_str()).map(str::to_string),
            map.get("items")
                .and_then(|v| v.as_str())
                .map(str::to_string),
        ),
        _ => (Some(text), None),
    }
}

pub fn content_hash(text: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(text.as_bytes());
    format!("sha256:{}", hex::encode(h.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::model::*;
    use serde_json::json;

    fn issue(key: &str, updated: &str) -> RemoteIssue {
        let raw = json!({
            "id": "1", "key": key,
            "fields": {"summary": "s", "updated": updated,
                       "description": {"type": "doc", "content": []}}
        });
        RemoteIssue {
            key: key.into(),
            id: "1".into(),
            updated: parse_timestamp(updated).unwrap(),
            description: RichText::Adf(json!({"type": "doc", "content": []})),
            fields: IssueFields {
                summary: "s".into(),
                ..Default::default()
            },
            comments: vec![],
            worklogs: Vec::new(),
            worklog_total: 0,
            attachments: vec![],
            raw,
        }
    }

    #[test]
    fn upsert_reports_only_real_changes() {
        let s = StateStore::in_memory().unwrap();
        assert!(s
            .upsert_remote(&issue("P-1", "2026-08-01T00:00:00Z"))
            .unwrap());
        assert!(
            !s.upsert_remote(&issue("P-1", "2026-08-01T00:00:00Z"))
                .unwrap(),
            "re-fetching an unchanged issue must not count as a change"
        );
        assert!(s
            .upsert_remote(&issue("P-1", "2026-08-02T00:00:00Z"))
            .unwrap());
    }

    #[test]
    fn a_fetched_issue_has_no_base_until_pull_writes_a_file() {
        let s = StateStore::in_memory().unwrap();
        s.upsert_remote(&issue("P-1", "2026-08-01T00:00:00Z"))
            .unwrap();
        let row = s.issue("P-1").unwrap().unwrap();
        assert!(row.is_new_locally());
        assert!(row.is_behind());

        s.advance_base("P-1", "desc", "sha256:x", "P-1. s.md")
            .unwrap();
        let row = s.issue("P-1").unwrap().unwrap();
        assert!(!row.is_new_locally());
        assert!(!row.is_behind());
        assert_eq!(row.base.unwrap().md_hash, "sha256:x");
        assert_eq!(row.file_path.as_deref(), Some("P-1. s.md"));
    }

    #[test]
    fn base_stays_put_when_a_newer_remote_arrives() {
        let s = StateStore::in_memory().unwrap();
        s.upsert_remote(&issue("P-1", "2026-08-01T00:00:00Z"))
            .unwrap();
        s.advance_base("P-1", "d", "h", "f.md").unwrap();
        s.upsert_remote(&issue("P-1", "2026-08-05T00:00:00Z"))
            .unwrap();

        let row = s.issue("P-1").unwrap().unwrap();
        assert_eq!(
            format_timestamp(&row.base.unwrap().updated),
            "2026-08-01T00:00:00Z"
        );
        assert_eq!(
            format_timestamp(&row.remote.unwrap().updated),
            "2026-08-05T00:00:00Z"
        );
    }

    #[test]
    fn advancing_base_for_an_unknown_issue_is_an_error() {
        let s = StateStore::in_memory().unwrap();
        assert_eq!(
            s.advance_base("NOPE-1", "d", "h", "f")
                .unwrap_err()
                .exit_code()
                .as_i32(),
            9
        );
    }

    #[test]
    fn comments_track_remote_deletion_instead_of_vanishing() {
        let s = StateStore::in_memory().unwrap();
        let mut i = issue("P-1", "2026-08-01T00:00:00Z");
        i.comments.push(RemoteComment {
            id: "c1".into(),
            author: None,
            created: None,
            updated: None,
            body: RichText::Wiki("hi".into()),
        });
        s.upsert_remote(&i).unwrap();
        assert_eq!(s.comments("P-1").unwrap().len(), 1);

        let empty = issue("P-1", "2026-08-02T00:00:00Z");
        s.upsert_remote(&empty).unwrap();
        let comments = s.comments("P-1").unwrap();
        assert_eq!(
            comments.len(),
            1,
            "the row must survive so a merge can see the deletion"
        );
        assert!(comments[0].deleted_remote);
    }

    #[test]
    fn raw_blocks_replace_wholesale_per_container() {
        let s = StateStore::in_memory().unwrap();
        let block = |id: &str| RawBlock {
            id: id.into(),
            ordinal: 0,
            node_json: "{}".into(),
            approx_md: "a".into(),
            label: "panel".into(),
        };
        s.replace_raw_blocks("P-1", "description", &[block("r1")])
            .unwrap();
        s.replace_raw_blocks("P-1", "comment:c1", &[block("r2")])
            .unwrap();
        s.replace_raw_blocks("P-1", "description", &[block("r3")])
            .unwrap();

        let desc = s.raw_blocks("P-1", "description").unwrap();
        assert_eq!(desc.len(), 1);
        assert_eq!(desc[0].id, "r3");
        assert_eq!(s.raw_blocks("P-1", "comment:c1").unwrap().len(), 1);
    }

    #[test]
    fn rekey_moves_every_related_row() {
        let s = StateStore::in_memory().unwrap();
        s.upsert_remote(&issue("NEW-abc123", "2026-08-01T00:00:00Z"))
            .unwrap();
        s.record_conflict("NEW-abc123", "summary", None, None, None)
            .unwrap();
        s.journal_ops("NEW-abc123", &[("create_issue".into(), "{}".into())])
            .unwrap();

        s.rekey_issue("NEW-abc123", "PROJ-201", "10500").unwrap();

        assert!(s.issue("NEW-abc123").unwrap().is_none());
        let row = s.issue("PROJ-201").unwrap().unwrap();
        assert_eq!(row.id, "10500");
        assert_eq!(s.conflicts().unwrap()[0].issue_key, "PROJ-201");
        assert_eq!(s.pending_ops().unwrap()[0].issue_key, "PROJ-201");
    }

    #[test]
    fn journal_records_and_clears_operations() {
        let s = StateStore::in_memory().unwrap();
        let ids = s
            .journal_ops("P-1", &[("update_fields".into(), "{}".into())])
            .unwrap();
        assert_eq!(s.pending_ops().unwrap().len(), 1);
        s.mark_op(ids[0], "done", None).unwrap();
        assert!(
            s.pending_ops().unwrap().is_empty(),
            "done ops are not pending"
        );

        let ids = s
            .journal_ops("P-1", &[("transition".into(), "{}".into())])
            .unwrap();
        s.mark_op(ids[0], "failed", Some("boom")).unwrap();
        let failed = s.pending_ops().unwrap();
        assert_eq!(failed[0].status, "failed");
        assert_eq!(failed[0].error.as_deref(), Some("boom"));
    }

    #[test]
    fn content_hash_is_stable_and_sensitive() {
        assert_eq!(content_hash("a"), content_hash("a"));
        assert_ne!(content_hash("a"), content_hash("b"));
        assert!(content_hash("a").starts_with("sha256:"));
    }

    #[test]
    fn meta_round_trips() {
        let s = StateStore::in_memory().unwrap();
        assert!(s.last_fetch_at().unwrap().is_none());
        let now = parse_timestamp("2026-08-17T10:00:00Z").unwrap();
        s.set_last_fetch_at(&now).unwrap();
        assert_eq!(s.last_fetch_at().unwrap().unwrap(), now);
    }
}
