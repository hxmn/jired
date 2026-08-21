//! `jired edit` [M4.7] — change an issue in a form, and the shared plumbing the
//! other single-issue commands use.
//!
//! `comment`, `transition` and `assign` are conveniences, not a second way to
//! reach the server: each stages its change in the working copy so it shows up
//! in `status` and `diff` and goes out through the normal `push` path. `--push`
//! only runs that same path immediately. `edit` is the same thing with a form
//! in front of it rather than a flag.

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
        None => search(&mut ctx, &engine)?,
    };
    let (key, file) = locate(&engine, &wanted)?;
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
        Some(push_one(&mut ctx, &key)?)
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

/// Ask which issue, with a filtered list of what the workspace holds.
///
/// Only issues that have a file: `edit` works on the working copy, and offering
/// one that was fetched but never written would hand back a key with nothing
/// behind it.
fn search(ctx: &mut Context, engine: &Engine) -> Result<String> {
    if !crate::tui::available() {
        return Err(
            JiredError::missing_value("no issue given, and no terminal to search on")
                .with_hint("name the issue: `jired edit PROJ-142`, or a file name"),
        );
    }

    let files = engine.projector.scan()?;
    let catalog = Catalog::from_rows(
        &engine.state.issues()?,
        ctx.resolver.get_quiet("workspace.project")?.as_deref(),
    );
    // The catalog is ordered most-recently-touched first, which is the order
    // someone looking for "the issue I was just in" wants. Locally created
    // issues are not in it — they have no snapshot — so they come after.
    let mut issues: Vec<crate::tui::catalog::KnownIssue> = catalog
        .issues
        .into_iter()
        .filter(|i| files.contains_key(&i.key))
        .collect();
    for (key, file) in &files {
        if issues.iter().any(|i| &i.key == key) {
            continue;
        }
        let doc = engine.projector.read(&file.relative)?;
        issues.push(crate::tui::catalog::KnownIssue {
            key: key.clone(),
            summary: doc.frontmatter.summary.clone(),
            issue_type: doc.frontmatter.issue_type.clone(),
            status: doc.frontmatter.status.clone(),
        });
    }

    let mut screen = crate::tui::pick::PickScreen::new("Edit which issue?", &issues);
    if screen.is_empty() {
        return Err(JiredError::usage("this workspace has no issue files yet")
            .with_hint("run `jired pull`, or `jired create` to write one"));
    }
    crate::tui::run(&mut screen)?
        .ok_or_else(|| JiredError::usage("no issue chosen").with_hint("nothing was changed"))
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
        issues: vec![crate::project::naming::key_from_input(key)],
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
