//! `jired edit` [M4.7] — change an issue in a form, and the shared plumbing the
//! other single-issue commands use.
//!
//! `comment`, `transition` and `assign` are conveniences, not a second way to
//! reach the server: each stages its change in the working copy so it shows up
//! in `status` and `diff` and goes out through the normal `push` path. `--push`
//! only runs that same path immediately. `edit` is the same thing with a form
//! in front of it rather than a flag.

use crate::cli::cmd::pick::Listing;
use crate::cli::cmd::push::PushArgs;
use crate::cli::context::Context;
use crate::cli::GlobalArgs;
use crate::doc::IssueDoc;
use crate::error::{ExitCode, JiredError, Result};
use crate::json::Out;
use crate::sync::push::PushReport;
use crate::sync::Engine;
use crate::tui::catalog::Catalog;
use crate::tui::edit::{EditScreen, Edits};
use clap::Args;

#[derive(Args, Debug, Clone, Default)]
pub struct EditArgs {
    /// Issue to edit, by key or file name; omit to search for one
    pub issue: Option<String>,
    /// Send the change to Jira straight away
    #[arg(long)]
    pub push: bool,
}

#[derive(serde::Serialize)]
struct EditReport {
    key: String,
    file: String,
    /// Frontmatter fields the edit moved; empty when nothing was changed.
    changed: Vec<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pushed: Option<PushReport>,
}

pub fn run(g: &GlobalArgs, args: &EditArgs, out: Out) -> Result<ExitCode> {
    let mut ctx = Context::open(g, out)?;
    ctx.lock()?;

    let engine = Engine::open(&mut ctx)?;
    let wanted = match &args.issue {
        Some(issue) => issue.clone(),
        // Nothing named: let them find it. Typing a key from memory is the
        // thing this command was hardest to use without.
        None => crate::cli::cmd::pick::choose(&mut ctx, "Edit which issue?", Listing::WithFiles)?,
    };
    let (key, file) = locate(&engine, &wanted)?;
    crate::cli::cmd::pick::mark_viewed(&engine.state, &key);
    let doc = engine.projector.read(&file)?;
    let before = Edits::of(&doc);

    // Three rungs, in the order the terminal can support them: the form, the
    // user's own editor on the file, and — with neither — exit 7. There is no
    // flags-only path here on purpose: `edit` exists to be interactive, and a
    // script has `comment`, `transition`, `assign` and the file itself.
    let (relative, after) = if crate::tui::available() {
        let catalog = Catalog::from_rows(
            &engine.state.issues()?,
            ctx.resolver.get_quiet("workspace.project")?.as_deref(),
        )
        .with_sprints(&engine.state.sprints()?);
        let mut screen = EditScreen::new(&key, &catalog, &before);
        match crate::tui::run(&mut screen)? {
            Some(edits) => (write_back(&engine, doc, &file, &edits)?, edits),
            None => {
                out.say("no changes");
                out.emit(
                    "edit",
                    &EditReport {
                        key,
                        file,
                        changed: Vec::new(),
                        pushed: None,
                    },
                );
                return Ok(ExitCode::Success);
            }
        }
    } else if ctx.resolver.interactive() {
        // A terminal that cannot do full-screen still has an editor on it, and
        // the file *is* the issue — nothing is lost by handing it over.
        external_editor(&engine, &file, out)?;
        let doc = engine.projector.read(&file)?;
        (file.clone(), Edits::of(&doc))
    } else {
        return Err(
            JiredError::missing_value("`jired edit` needs a terminal to draw on")
                .with_detail("issue", key)
                .with_hint(
                    "edit the file directly, or use `jired comment`, `transition`, `assign`",
                ),
        );
    };

    let changed = changed_fields(&before, &after);
    if changed.is_empty() {
        out.say("no changes");
    } else {
        out.say(format!("{key}: {} — {relative}", changed.join(", ")));
    }

    let pushed = if args.push && !changed.is_empty() {
        Some(push_one(&mut ctx, &key, false)?)
    } else {
        None
    };
    if pushed.is_none() && !changed.is_empty() {
        out.say("run `jired push` to send it");
    }

    out.emit(
        "edit",
        &EditReport {
            key,
            file: relative,
            changed,
            pushed,
        },
    );
    Ok(ExitCode::Success)
}

/// Find the issue's file, or say what to do about it not being here.
///
/// A key, a file name and a path all work: tab-completion produces the last of
/// those, and an issue whose summary is hard to type is exactly the one someone
/// completes rather than types.
fn locate(engine: &Engine, input: &str) -> Result<(String, String)> {
    match engine.projector.locate(input)? {
        Some(found) => Ok((found.key, found.relative)),
        None => Err(
            JiredError::usage(format!("`{input}` is not in this workspace"))
                .with_detail("issue", input.to_string())
                .with_hint("run `jired pull` first, or check the key"),
        ),
    }
}

/// Apply the form's result and write the file, renaming it when the summary
/// changed so the name keeps describing the issue.
fn write_back(engine: &Engine, mut doc: IssueDoc, file: &str, edits: &Edits) -> Result<String> {
    edits.apply(&mut doc);
    Ok(engine.projector.write(&doc, Some(file))?.relative)
}

/// Hand the file to `$VISUAL`/`$EDITOR`, then check it still parses.
fn external_editor(engine: &Engine, file: &str, out: Out) -> Result<()> {
    let editor = std::env::var("VISUAL")
        .or_else(|_| std::env::var("EDITOR"))
        .unwrap_or_else(|_| "vi".to_string());
    let path = engine.projector.path_of(file);
    out.say(format!("opening {file} in {editor}"));

    // The editor owns the terminal until it exits; splitting the command lets
    // `EDITOR="code -w"` work as well as a bare name.
    let mut parts = editor.split_whitespace();
    let program = parts.next().unwrap_or("vi");
    let status = std::process::Command::new(program)
        .args(parts)
        .arg(&path)
        .status()
        .map_err(|e| {
            JiredError::generic(format!("cannot run {editor}: {e}"))
                .with_hint("set $EDITOR to something on your PATH")
        })?;
    if !status.success() {
        return Err(JiredError::generic(format!(
            "{editor} exited with {status}"
        )));
    }
    // Parsing it now turns a stray edit into a message here rather than a
    // confusing failure in the next command.
    engine.projector.read(file)?;
    Ok(())
}

/// Which fields the edit moved, in the order the file lists them.
fn changed_fields(before: &Edits, after: &Edits) -> Vec<&'static str> {
    let mut changed = Vec::new();
    for (name, moved) in [
        ("summary", before.summary != after.summary),
        ("status", before.status != after.status),
        ("type", before.issue_type != after.issue_type),
        ("priority", before.priority != after.priority),
        ("assignee", before.assignee != after.assignee),
        ("labels", before.labels != after.labels),
        ("components", before.components != after.components),
        ("sprint", before.sprint != after.sprint),
        ("story_points", before.story_points != after.story_points),
        ("parent", before.parent != after.parent),
        ("description", before.description != after.description),
    ] {
        if moved {
            changed.push(name);
        }
    }
    changed
}

/// Locate an issue's file, apply `mutate`, and write it back.
///
/// Returns the file's path relative to the workspace root.
pub fn edit_issue(
    ctx: &mut Context,
    key: &str,
    mutate: impl FnOnce(&mut IssueDoc) -> Result<()>,
) -> Result<String> {
    let engine = Engine::open(ctx)?;
    let found = engine.projector.locate(key)?.ok_or_else(|| {
        JiredError::usage(format!("`{key}` is not in this workspace"))
            .with_detail("issue", key.to_string())
            .with_hint("run `jired pull` first, or check the key")
    })?;

    // Every staging command comes through here, so this is the one place that
    // has to know that going to an issue counts as having looked at it.
    crate::cli::cmd::pick::mark_viewed(&engine.state, &found.key);

    let mut doc = engine.projector.read(&found.relative)?;
    mutate(&mut doc)?;
    // Pass the current path as `previous` so a summary change renames rather
    // than leaving two files behind.
    let outcome = engine.projector.write(&doc, Some(&found.relative))?;
    Ok(outcome.relative)
}

/// Push just this issue, reusing the normal push path.
pub fn push_one(ctx: &mut Context, key: &str, dry_run: bool) -> Result<PushReport> {
    push_these(ctx, std::slice::from_ref(&key.to_string()), dry_run)
}

/// The same, for a command that staged a change across several issues.
///
/// One call rather than one per issue: the report is then the whole outcome,
/// and an issue that fails does not hide the ones that went out.
pub fn push_these(ctx: &mut Context, keys: &[String], dry_run: bool) -> Result<PushReport> {
    let args = PushArgs {
        issues: keys
            .iter()
            .map(|k| crate::project::naming::key_from_input(k))
            .collect(),
        dry_run,
        ..Default::default()
    };
    crate::sync::push::run(ctx, &args)
}

/// Report the outcome of an inline `--push`.
///
/// Without this the user is told only that "the push failed": the report holds
/// the server's actual complaint, and printing it is the difference between a
/// dead end and something they can act on.
pub fn report_push(report: &PushReport, out: Out) {
    if report.dry_run || report.results.iter().any(|r| !r.ok) || !report.stale.is_empty() {
        report.render(out);
    }
}

/// Where a body comes from: the flag, a pipe, or the user's editor.
pub fn message_or_stdin(message: &Option<String>, what: &str) -> Result<String> {
    if let Some(m) = message {
        return Ok(m.clone());
    }
    if !crate::cli::context::is_interactive() {
        let mut buf = String::new();
        std::io::Read::read_to_string(&mut std::io::stdin(), &mut buf)?;
        if buf.trim().is_empty() {
            return Err(JiredError::missing_value(format!("no {what} given"))
                .with_hint("pass -m \"text\", or pipe the text on stdin"));
        }
        return Ok(buf.trim_end().to_string());
    }
    edit_in_editor(what, "")
}

/// Open `$VISUAL`/`$EDITOR` on a scratch file and return what was written.
///
/// git's convention throughout: comment lines are stripped, and an empty result
/// means "I changed my mind", which is an abort rather than an empty comment.
pub fn edit_in_editor(what: &str, initial: &str) -> Result<String> {
    let editor = std::env::var("VISUAL")
        .or_else(|_| std::env::var("EDITOR"))
        .map_err(|_| {
            JiredError::missing_value(format!("no {what} given"))
                .with_hint("pass -m \"text\", pipe it on stdin, or set $EDITOR")
        })?;

    let dir = std::env::temp_dir();
    let path = dir.join(format!(
        "jired-{}-{}.md",
        what.replace(' ', "-"),
        std::process::id()
    ));
    let template = format!(
        "{initial}\n\n\
         # Write the {what} above. Lines starting with `#` are ignored.\n\
         # Saving an empty file aborts.\n"
    );
    std::fs::write(&path, template).map_err(crate::error::io_ctx("write", &path))?;

    // The editor owns the terminal while it runs; anything jired printed to
    // stdout stays put because the editor draws on the tty directly.
    let status = std::process::Command::new("sh")
        .arg("-c")
        .arg(format!("{editor} \"$1\"",))
        .arg("sh")
        .arg(&path)
        .status()
        .map_err(|e| {
            JiredError::generic(format!("cannot run $EDITOR ({editor})")).with_source(e)
        })?;
    if !status.success() {
        let _ = std::fs::remove_file(&path);
        return Err(JiredError::generic(format!(
            "$EDITOR ({editor}) exited with an error"
        )));
    }

    let text = std::fs::read_to_string(&path).map_err(crate::error::io_ctx("read", &path))?;
    let _ = std::fs::remove_file(&path);

    let body: String = text
        .lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n");
    if body.trim().is_empty() {
        return Err(JiredError::missing_value(format!(
            "empty {what} — nothing was staged"
        )));
    }
    Ok(body.trim_end().to_string())
}
