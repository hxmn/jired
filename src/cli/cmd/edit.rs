//! Shared plumbing for the commands that edit one issue file [M3].
//!
//! `comment`, `transition` and `assign` are conveniences, not a second way to
//! reach the server: each stages its change in the working copy so it shows up
//! in `status` and `diff` and goes out through the normal `push` path. `--push`
//! only runs that same path immediately.

use crate::cli::cmd::push::PushArgs;
use crate::cli::context::Context;
use crate::doc::IssueDoc;
use crate::error::{JiredError, Result};
use crate::sync::push::PushReport;
use crate::sync::Engine;

/// Locate an issue's file, apply `mutate`, and write it back.
///
/// Returns the file's path relative to the workspace root.
pub fn edit_issue(
    ctx: &mut Context,
    key: &str,
    mutate: impl FnOnce(&mut IssueDoc) -> Result<()>,
) -> Result<String> {
    let engine = Engine::open(ctx)?;
    let key = key.to_uppercase();
    let files = engine.projector.scan()?;

    let found = files.get(&key).ok_or_else(|| {
        JiredError::usage(format!("{key} is not in this workspace"))
            .with_detail("issue", key.clone())
            .with_hint("run `jired pull` first, or check the key")
    })?;

    let mut doc = engine.projector.read(&found.relative)?;
    mutate(&mut doc)?;
    // Pass the current path as `previous` so a summary change renames rather
    // than leaving two files behind.
    let outcome = engine.projector.write(&doc, Some(&found.relative))?;
    Ok(outcome.relative)
}

/// Push just this issue, reusing the normal push path.
pub fn push_one(ctx: &mut Context, key: &str) -> Result<PushReport> {
    let args = PushArgs {
        issues: vec![key.to_uppercase()],
        ..Default::default()
    };
    crate::sync::push::run(ctx, &args)
}

/// Read a value from stdin when the flag was omitted, so `-m` can be piped.
pub fn message_or_stdin(message: &Option<String>, what: &str) -> Result<String> {
    if let Some(m) = message {
        return Ok(m.clone());
    }
    if crate::cli::context::is_interactive() {
        return Err(JiredError::missing_value(format!("no {what} given"))
            .with_hint("pass -m \"text\", or pipe the text on stdin"));
    }
    let mut buf = String::new();
    std::io::Read::read_to_string(&mut std::io::stdin(), &mut buf)?;
    if buf.trim().is_empty() {
        return Err(JiredError::missing_value(format!("no {what} given"))
            .with_hint("pass -m \"text\", or pipe the text on stdin"));
    }
    Ok(buf.trim_end().to_string())
}
