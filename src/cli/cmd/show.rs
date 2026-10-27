//! `jired show` — one issue, as the workspace knows it.
//!
//! An unset field is not written into a `.md` file, so the file cannot answer
//! "is this empty, or does this instance not have that field?". The answer was
//! `sqlite3 .jired/state.db` and `json_extract` over `remote_raw` — an agent
//! reverse-engineering a private schema nobody promised to keep, which is a
//! habit worth heading off rather than a one-off.
//!
//! `--raw` is the server's own payload, which is where that question is
//! actually answered; the default is the issue as jired models it, which is
//! what every other command acts on.

use crate::cli::cmd::lookup;
use crate::cli::context::Context;
use crate::cli::GlobalArgs;
use crate::error::{ExitCode, Result};
use crate::json::Out;
use crate::style::{self, BOLD, CYAN, DIM};
use crate::sync::Engine;
use clap::Args;

/// A custom value as one line: the simple form where there is one, and the
/// JSON where there is not — which is exactly what the file shows.
fn plain(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Array(items) => items.iter().map(plain).collect::<Vec<_>>().join(", "),
        other => other.to_string(),
    }
}

#[derive(Args, Debug, Clone, Default)]
pub struct ShowArgs {
    /// Issue to show: a key, or the path to its file
    #[arg(value_name = "ISSUE")]
    pub issue: String,

    /// The server's own payload, including the fields jired does not model
    #[arg(long)]
    pub raw: bool,

    /// Print only the issue file's path, relative to the workspace
    ///
    /// File names carry a zero-padded number — `TSK-0710. Summary.md` — so the
    /// name cannot be guessed from the key, and `ls TSK-710*` finds nothing.
    #[arg(long, conflicts_with = "raw")]
    pub path: bool,
}

pub fn run(g: &GlobalArgs, args: &ShowArgs, out: Out) -> Result<ExitCode> {
    let mut ctx = Context::open(g, out)?;
    let engine = Engine::open(&mut ctx)?;
    let key = crate::project::naming::key_from_input(&args.issue)?;
    let file = engine.projector.locate(&key)?.map(|f| f.relative);

    if args.path {
        let Some(file) = file else {
            // Asked of the snapshot first, so an issue the workspace has never
            // heard of says so rather than claiming it merely lacks a file.
            lookup::raw(&engine, &key)?;
            return Err(crate::error::JiredError::usage(format!(
                "{key} has no file in this workspace"
            ))
            .with_detail("issue", key.clone())
            .with_hint(format!("`jired pull {key}` writes one")));
        };
        // The bare path even on a terminal: it is what `$(jired show K --path)`
        // and `xargs` want, and there is nothing else to say.
        out.say(&file);
        out.emit("show", &serde_json::json!({ "key": key, "file": file }));
        return Ok(ExitCode::Success);
    }

    if args.raw {
        let raw = lookup::raw(&engine, &key)?;
        out.say(serde_json::to_string_pretty(&raw)?);
        out.emit(
            "show",
            &serde_json::json!({ "key": key, "file": file, "raw": raw }),
        );
        return Ok(ExitCode::Success);
    }

    let (doc, source) = lookup::doc(&engine, &key)?;
    let fm = &doc.frontmatter;

    let field = |name: &str, value: &str| {
        if value.is_empty() {
            return;
        }
        out.say(format!(
            "{}  {value}",
            style::out(DIM, format!("{name:>14}"))
        ));
    };
    out.say(format!(
        "{} {}",
        style::out(CYAN, &fm.key),
        style::out(BOLD, &fm.summary)
    ));
    field("type", &fm.issue_type);
    out.say(format!(
        "{}  {}",
        style::out(DIM, format!("{:>14}", "status")),
        style::status(&fm.status, "")
    ));
    field("priority", fm.priority.as_deref().unwrap_or(""));
    field("assignee", fm.assignee.as_deref().unwrap_or(""));
    field("reporter", fm.reporter.as_deref().unwrap_or(""));
    field("labels", &fm.labels.join(", "));
    field("components", &fm.components.join(", "));
    field("sprint", &fm.sprint.join(" → "));
    field("parent", fm.parent.as_deref().unwrap_or(""));
    field("epic", fm.epic.as_deref().unwrap_or(""));
    field("due", fm.due_date.as_deref().unwrap_or(""));
    field(
        "story points",
        &fm.story_points.map(|p| p.to_string()).unwrap_or_default(),
    );
    for (id, value) in &fm.custom {
        field(id, &plain(value));
    }
    // Where it was read from, because "the file" and "the snapshot" can say
    // different things and which one this is decides what an edit would touch.
    out.say(style::out(
        DIM,
        match source {
            lookup::Source::File => "  (from the working copy)",
            lookup::Source::Snapshot => "  (from the snapshot; this issue has no file)",
        },
    ));
    if !args.raw {
        out.hint("`--raw` for the server's own payload, fields jired does not model included");
    }

    out.emit(
        "show",
        &serde_json::json!({
            "key": fm.key,
            "file": file,
            "source": source,
            "frontmatter": fm,
            "description": doc.description,
        }),
    );
    Ok(ExitCode::Success)
}
