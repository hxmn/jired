//! `jired show` — issues, as the workspace knows them.
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
    /// Issues to show: keys, or the paths to their files
    #[arg(value_name = "ISSUE", required = true, num_args = 1..)]
    pub issues: Vec<String>,

    /// The server's own payload, including the fields jired does not model
    #[arg(long)]
    pub raw: bool,

    /// Print only the issue file's path, relative to the workspace
    ///
    /// File names carry a zero-padded number — `TSK-0710. Summary.md` — so the
    /// name cannot be guessed from the key, and `ls TSK-710*` finds nothing.
    #[arg(long, conflicts_with = "raw")]
    pub path: bool,

    /// Print only these parts of each issue, comma-separated: fields,
    /// description, checklist, children, comments, worklogs, attachments
    ///
    /// A section reads as it does in the file, under its `## ` heading, so
    /// the description and comments of several issues are one command rather
    /// than a loop over `--path` and a text filter.
    #[arg(
        long,
        value_delimiter = ',',
        value_name = "SECTIONS",
        conflicts_with_all = ["raw", "path"]
    )]
    pub section: Vec<String>,
}

/// What `--section` accepts besides the file's own section names.
const FIELDS: &str = "fields";

/// The sections asked for, as the file names them, in the file's order.
fn sections(asked: &[String]) -> Result<Vec<&'static str>> {
    let mut wanted = Vec::new();
    for name in asked {
        let name = name.trim();
        let found = std::iter::once(FIELDS)
            .chain(crate::doc::SECTIONS)
            .find(|s| s.eq_ignore_ascii_case(name));
        match found {
            Some(s) => wanted.push(s),
            None => {
                let known: Vec<String> = std::iter::once(FIELDS)
                    .chain(crate::doc::SECTIONS)
                    .map(str::to_ascii_lowercase)
                    .collect();
                return Err(crate::error::JiredError::usage(format!(
                    "no section `{name}` to show"
                ))
                .with_detail("section", name.to_string())
                .with_hint(format!("one of: {}", known.join(", "))));
            }
        }
    }
    // The file's order, whatever order they were asked in, and each once.
    let order = |s: &&str| {
        std::iter::once(FIELDS)
            .chain(crate::doc::SECTIONS)
            .position(|x| x == *s)
    };
    wanted.sort_by_key(order);
    wanted.dedup();
    Ok(wanted)
}

pub fn run(g: &GlobalArgs, args: &ShowArgs, out: Out) -> Result<ExitCode> {
    let mut ctx = Context::open(g, out)?;
    let engine = Engine::open(&mut ctx)?;
    let wanted = sections(&args.section)?;
    let mut reports = Vec::with_capacity(args.issues.len());
    for (i, issue) in args.issues.iter().enumerate() {
        if i > 0 && !args.path {
            out.say("");
        }
        reports.push(show_one(&engine, issue, args, &wanted, out)?);
    }
    // One issue is the document it always was; several are listed under
    // `issues`, each the same document.
    let report = match reports.len() {
        1 => reports.remove(0),
        _ => serde_json::json!({ "issues": reports }),
    };
    out.emit("show", &report);
    Ok(ExitCode::Success)
}

fn show_one(
    engine: &Engine,
    issue: &str,
    args: &ShowArgs,
    wanted: &[&str],
    out: Out,
) -> Result<serde_json::Value> {
    let key = crate::project::naming::key_from_input(issue)?;
    let file = engine.projector.locate(&key)?.map(|f| f.relative);

    if args.path {
        let Some(file) = file else {
            // Asked of the snapshot first, so an issue the workspace has never
            // heard of says so rather than claiming it merely lacks a file.
            lookup::raw(engine, &key)?;
            return Err(crate::error::JiredError::usage(format!(
                "{key} has no file in this workspace"
            ))
            .with_detail("issue", key.clone())
            .with_hint(format!("`jired pull {key}` writes one")));
        };
        // The bare path even on a terminal: it is what `$(jired show K --path)`
        // and `xargs` want, and there is nothing else to say.
        out.say(&file);
        return Ok(serde_json::json!({ "key": key, "file": file }));
    }

    if args.raw {
        let raw = lookup::raw(engine, &key)?;
        out.say(serde_json::to_string_pretty(&raw)?);
        return Ok(serde_json::json!({ "key": key, "file": file, "raw": raw }));
    }

    let (doc, source) = lookup::doc(engine, &key)?;
    let fm = &doc.frontmatter;

    out.say(format!(
        "{} {}",
        style::out(CYAN, &fm.key),
        style::out(BOLD, &fm.summary)
    ));
    if wanted.is_empty() || wanted.contains(&FIELDS) {
        print_fields(fm, out);
    }
    let mut texts = serde_json::Map::new();
    for name in wanted.iter().filter(|n| **n != FIELDS) {
        let body = doc.section_body(name).unwrap_or_default();
        out.say(format!("\n## {name}\n"));
        if !body.trim().is_empty() {
            out.say(body.trim_end());
        }
        texts.insert(name.to_ascii_lowercase(), body.trim_end().into());
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
    if wanted.is_empty() && args.issues.len() == 1 {
        out.hint("`--raw` for the server's own payload, fields jired does not model included");
    }

    let mut report = serde_json::json!({
        "key": fm.key,
        "file": file,
        "source": source,
        "frontmatter": fm,
        "description": doc.description,
    });
    if !wanted.is_empty() {
        report["sections"] = serde_json::Value::Object(texts);
    }
    Ok(report)
}

fn print_fields(fm: &crate::doc::Frontmatter, out: Out) {
    let field = |name: &str, value: &str| {
        if value.is_empty() {
            return;
        }
        out.say(format!(
            "{}  {value}",
            style::out(DIM, format!("{name:>14}"))
        ));
    };
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
}
