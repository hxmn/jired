//! Staging one frontmatter change across several issues.
//!
//! `sprint add`, `label add` and `set` are the same command with a different
//! edit in the middle: write the field, say what was written, and either leave
//! it for `push` or send it now. The shape they share is here so a fourth one
//! is the edit and nothing else — and so "already had that value" means the
//! same thing in all of them, which is *reported and not staged*, because
//! staging a change that does nothing only gives `push` something to explain.

use crate::cli::cmd::edit::{edit_issue, push_these, report_push, Snapshot};
use crate::cli::context::Context;
use crate::doc::IssueDoc;
use crate::error::{ExitCode, JiredError, Result};
use crate::json::Out;
use crate::style::{self, CYAN};
use crate::sync::push::PushReport;

/// One issue's staged edit.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Staged {
    pub key: String,
    pub file: String,
    /// False when the issue already had the value being written.
    pub changed: bool,
    /// False when the edit put the file back to what the server holds — an
    /// `unlink` of a link that was only ever staged — so there is nothing left
    /// for `push` to send.
    pub differs: bool,
    /// The file as it was before this edit, for a dry run to put back.
    #[serde(skip)]
    pub before: Snapshot,
}

/// Apply `edit` to each issue's file, reporting rather than failing on the ones
/// this workspace does not have.
///
/// `edit` returns whether it changed anything. One bad issue does not stop the
/// others — the promise `push` makes — but every one of them failing is the
/// command failing rather than a quiet success.
pub fn stage(
    ctx: &mut Context,
    issues: &[String],
    out: Out,
    edit: impl Fn(&mut IssueDoc) -> Result<bool>,
) -> Result<Vec<Staged>> {
    let mut staged = Vec::new();
    let mut failed = 0;
    for issue in issues {
        let key = crate::project::naming::key_from_input(issue)?;
        let mut changed = false;
        let before = Snapshot::take(ctx, std::slice::from_ref(&key))?;
        let edited = edit_issue(ctx, &key, |doc| {
            changed = edit(doc)?;
            Ok(())
        });
        match edited {
            Ok(file) => {
                let differs = differs_from_base(ctx, &key, &file)?;
                staged.push(Staged {
                    key,
                    file,
                    changed,
                    differs,
                    before,
                })
            }
            Err(e) => {
                failed += 1;
                out.warn(format!("{key}: {}", e.message));
            }
        }
    }
    if failed > 0 && staged.is_empty() {
        return Err(JiredError::usage(format!(
            "none of the {failed} issue(s) named are in this workspace"
        ))
        .with_hint("run `jired pull` first, or check the keys"));
    }
    Ok(staged)
}

/// Whether `file` says something other than what the issue last synced as —
/// the comparison `status` makes, so the two agree on what is staged.
fn differs_from_base(ctx: &mut Context, key: &str, file: &str) -> Result<bool> {
    let engine = crate::sync::Engine::open(ctx)?;
    let base = engine.state.issue(key)?.and_then(|row| row.base);
    Ok(match base {
        Some(base) => engine.projector.hash_of(file).as_deref() != Some(base.md_hash.as_str()),
        // Never synced: a `NEW-*` issue is all edit.
        None => true,
    })
}

/// How a staged change reads on screen.
pub struct Wording<'a> {
    /// What was written, as an object: `Sprint 44`, `the label `regular``.
    pub what: &'a str,
    /// The verb, past tense: `moved`, `labelled`, `set`.
    pub verb: &'a str,
    /// How an issue that already had the value reads: "is already in Sprint 44".
    pub already: &'a str,
}

/// What a finished staging run produces: the exit code, and the document its
/// command emits.
///
/// The caller emits it rather than this function, so the schema name stays a
/// literal at the `out.emit` — which is what `tests/schema_contract.rs` reads
/// the source for, and what a person looking for "where does `jired/label/1`
/// come from" greps for.
pub struct Outcome {
    pub exit: ExitCode,
    pub doc: serde_json::Value,
}

/// Report the staged edits, and send them when asked.
pub fn finish(
    ctx: &mut Context,
    staged: Vec<Staged>,
    wording: &Wording<'_>,
    push: bool,
    dry_run: bool,
    out: Out,
    extra: serde_json::Value,
) -> Result<Outcome> {
    // `--dry-run` alone is a dry run of the push: the plan is what it is for.
    let push = push || dry_run;
    let changed: Vec<&Staged> = staged.iter().filter(|s| s.changed).collect();

    let pushed: Option<PushReport> = if push && !changed.is_empty() {
        let keys: Vec<String> = changed.iter().map(|s| s.key.clone()).collect();
        let report = push_these(ctx, &keys, dry_run);
        if dry_run {
            let mut before = Snapshot::default();
            for s in &staged {
                before.extend(s.before.clone());
            }
            before.restore(ctx)?;
        }
        let report = report?;
        report_push(&report, out);
        Some(report)
    } else {
        None
    };

    for s in &staged {
        if !s.changed {
            out.say(format!("{} {}", style::out(CYAN, &s.key), wording.already));
        }
    }
    let (what, verb) = (wording.what, wording.verb);
    match &pushed {
        _ if changed.is_empty() => {}
        Some(r) if r.dry_run => {
            out.say("dry run — the files are as they were, and nothing was sent")
        }
        Some(r) if r.results.iter().all(|x| x.ok) => {
            out.say(format!("{verb} {} issue(s): {what}", changed.len()))
        }
        Some(_) => out.say(format!(
            "{} issue(s) are staged, but the push failed — \
             fix the cause and run `jired push`",
            changed.len()
        )),
        None => {
            for s in &changed {
                match s.differs {
                    true => out.say(format!(
                        "staged {} → {what} in {}",
                        style::out(CYAN, &s.key),
                        s.file
                    )),
                    false => out.say(format!(
                        "{} is back to what the server has; nothing to push",
                        style::out(CYAN, &s.key)
                    )),
                }
            }
            if changed.iter().any(|s| s.differs) {
                out.hint("run `jired push` to send them");
            }
        }
    }

    // Said in the document as well as on stderr: an agent reading `--json`
    // never sees the hint, took `"pushed": false` for a detail, and believed
    // the issues had moved.
    // Only what is still in the files and still different from the server:
    // a dry run put the files back, and an edit that undid a staged one left
    // nothing to send.
    let waiting: Vec<String> = match &pushed {
        Some(r) if r.dry_run || r.results.iter().all(|x| x.ok) => Vec::new(),
        _ => changed
            .iter()
            .filter(|s| s.differs)
            .map(|s| s.key.clone())
            .collect(),
    };
    let mut doc = serde_json::json!({
        "issues": staged,
        "staged": !waiting.is_empty(),
        "next": (!waiting.is_empty()).then(|| format!("jired push {}", waiting.join(" "))),
        "pushed": pushed.is_some(),
        "push": pushed,
    });
    if let (serde_json::Value::Object(map), serde_json::Value::Object(more)) = (&mut doc, extra) {
        map.extend(more);
    }
    Ok(Outcome {
        exit: match pushed {
            Some(report) => report.exit_code(),
            None => ExitCode::Success,
        },
        doc,
    })
}
