//! Staging one frontmatter change across several issues.
//!
//! `sprint add`, `label add` and `set` are the same command with a different
//! edit in the middle: write the field, say what was written, and either leave
//! it for `push` or send it now. The shape they share is here so a fourth one
//! is the edit and nothing else — and so "already had that value" means the
//! same thing in all of them, which is *reported and not staged*, because
//! staging a change that does nothing only gives `push` something to explain.

use crate::cli::cmd::edit::{edit_issue, push_these, report_push};
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
        let edited = edit_issue(ctx, &key, |doc| {
            changed = edit(doc)?;
            Ok(())
        });
        match edited {
            Ok(file) => staged.push(Staged { key, file, changed }),
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
    let changed: Vec<&Staged> = staged.iter().filter(|s| s.changed).collect();

    let pushed: Option<PushReport> = if push && !changed.is_empty() {
        let keys: Vec<String> = changed.iter().map(|s| s.key.clone()).collect();
        let report = push_these(ctx, &keys, dry_run)?;
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
        Some(r) if r.dry_run => {}
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
                out.say(format!(
                    "staged {} → {what} in {}",
                    style::out(CYAN, &s.key),
                    s.file
                ));
            }
            out.hint("run `jired push` to send them");
        }
    }

    // Said in the document as well as on stderr: an agent reading `--json`
    // never sees the hint, took `"pushed": false` for a detail, and believed
    // the issues had moved.
    let waiting: Vec<String> = match &pushed {
        Some(r) if !r.dry_run && r.results.iter().all(|x| x.ok) => Vec::new(),
        _ => changed.iter().map(|s| s.key.clone()).collect(),
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
