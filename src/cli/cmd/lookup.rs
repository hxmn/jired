//! Reading one issue out of the workspace, from wherever it is known.
//!
//! An issue can be a file, a snapshot row, or both, and a command that only
//! looks at files answers "I don't have it" about an issue the workspace has
//! fetched. `jired show` and `jired create --like` both need the same answer, so
//! the order of preference is decided once: the file when there is one, because
//! that is what a following edit would change, and the snapshot when there is
//! not, because knowing less than the database does is not a useful kind of
//! honesty.

use crate::api::normalize;
use crate::error::{JiredError, Result};
use crate::store::state::IssueRow;
use crate::sync::render::{render_issue, RenderContext};
use crate::sync::Engine;
use serde_json::Value;

/// Where an issue's values came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    /// The `.md` file in the working copy, local edits and all.
    File,
    /// The last snapshot fetched from the server; there is no file.
    Snapshot,
}

/// The issue as this workspace knows it, and which of the two it read.
pub fn doc(engine: &Engine, key: &str) -> Result<(crate::doc::IssueDoc, Source)> {
    if let Some(found) = engine.projector.locate(key)? {
        let doc = engine.projector.read(&found.relative)?;
        return Ok((doc, Source::File));
    }

    let row = row(engine, key)?;
    // Prefer the remote snapshot over base: it is the newer of the two, and a
    // reader asking about an issue with no file wants what the server last
    // said, not what it said at the last sync.
    let raw = row
        .remote
        .as_ref()
        .map(|r| &r.raw)
        .or(row.base.as_ref().map(|b| &b.raw))
        .ok_or_else(|| unknown(key))?;
    let fields = engine.field_map()?;
    let issue = normalize::issue_from_json(raw, engine.markup(), &fields)?;
    let rendered = render_issue(
        &issue,
        &RenderContext {
            markup: engine.markup(),
            base_url: engine.base_url(),
            ignored_custom: &fields.ignored,
        },
    )?;
    Ok((rendered.doc, Source::Snapshot))
}

/// The server's own payload for an issue, exactly as it was last fetched.
///
/// The answer to "is this field empty, or does this instance not have it?",
/// which the file cannot give: an unset field is not written to it at all. The
/// alternative was `sqlite3 .jired/state.db` and `json_extract` over a column
/// nobody promised to keep.
pub fn raw(engine: &Engine, key: &str) -> Result<Value> {
    let row = row(engine, key)?;
    row.remote
        .as_ref()
        .map(|r| r.raw.clone())
        .or(row.base.as_ref().map(|b| b.raw.clone()))
        .ok_or_else(|| {
            JiredError::usage(format!("{key} has no snapshot to show"))
                .with_hint("run `jired fetch` to get one")
        })
}

fn row(engine: &Engine, key: &str) -> Result<IssueRow> {
    engine
        .state
        .issues()?
        .into_iter()
        .find(|r| r.key.eq_ignore_ascii_case(key))
        .ok_or_else(|| unknown(key))
}

fn unknown(key: &str) -> JiredError {
    JiredError::usage(format!("`{key}` is not an issue this workspace knows"))
        .with_detail("issue", key.to_string())
        .with_hint("run `jired pull` first, or check the key")
}
