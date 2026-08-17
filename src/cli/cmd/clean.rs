//! `jired clean` [M3.9] — remove files whose issues left the workspace scope.

use crate::cli::cmd::prompt::confirm;
use crate::cli::context::Context;
use crate::cli::GlobalArgs;
use crate::error::{ExitCode, JiredError, Result};
use crate::json::Out;
use crate::sync::Engine;
use clap::Args;
use serde::Serialize;

#[derive(Args, Debug, Clone)]
pub struct CleanArgs {
    /// Rebuild every grouping folder from the state database
    #[arg(long)]
    pub regroup: bool,
    /// Report what would be removed without touching anything
    #[arg(long)]
    pub dry_run: bool,
    /// Remove orphaned files even when they have unpushed edits
    #[arg(long)]
    pub force: bool,
}

#[derive(Debug, Serialize)]
struct Orphan {
    key: String,
    file: String,
    reason: &'static str,
    dirty: bool,
}

pub fn run(g: &GlobalArgs, args: &CleanArgs, out: Out) -> Result<ExitCode> {
    let mut ctx = Context::open(g, out)?;
    ctx.lock()?;
    let engine = Engine::open(&ctx)?;

    if args.regroup {
        // Grouping folders are views: throwing them away and rebuilding from
        // the database is always safe, and is the recovery path when a link
        // ends up dangling or a group was renamed by hand.
        let options = crate::sync::group::GroupOptions::from_context(&mut ctx)?;
        let report = crate::sync::group::regroup(&engine, engine.projector.root(), &options)?;
        out.say(format!(
            "rebuilt {} link(s) across {} parent(s), {} sprint(s), {} board(s)",
            report.links, report.parents, report.sprints, report.boards
        ));
        for w in &report.warnings {
            out.warn(w);
        }
        out.emit("clean", &serde_json::json!({ "regrouped": report }));
        return Ok(ExitCode::Success);
    }

    let files = engine.projector.scan()?;
    let rows = engine.state.issues()?;
    let known: std::collections::HashMap<&str, &crate::store::state::IssueRow> =
        rows.iter().map(|r| (r.key.as_str(), r)).collect();

    let mut orphans = Vec::new();
    for (key, file) in &files {
        let (reason, row) = match known.get(key.as_str()) {
            // A locally created issue is not an orphan; it is unpushed work.
            _ if key.starts_with("NEW-") => continue,
            None => ("not tracked", None),
            Some(row) if row.deleted_remote => ("gone from the server", Some(*row)),
            Some(_) => continue,
        };

        // "Dirty" here means the file holds edits the server has never seen, so
        // deleting it would lose them for good.
        let dirty = match row.and_then(|r| r.base.as_ref()) {
            Some(base) => engine
                .projector
                .hash_of(&file.relative)
                .is_some_and(|h| h != base.md_hash),
            None => true,
        };
        orphans.push(Orphan {
            key: key.clone(),
            file: file.relative.clone(),
            reason,
            dirty,
        });
    }
    orphans.sort_by(|a, b| a.key.cmp(&b.key));

    if orphans.is_empty() {
        out.say("nothing to clean");
        out.emit("clean", &serde_json::json!({"removed": [], "kept": []}));
        return Ok(ExitCode::Success);
    }

    for o in &orphans {
        out.say(format!(
            "  {:<14} {}  ({}){}",
            o.key,
            o.file,
            o.reason,
            if o.dirty { ", has unpushed edits" } else { "" }
        ));
    }

    if args.dry_run {
        out.say(format!("{} file(s) would be removed", orphans.len()));
        out.emit(
            "clean",
            &serde_json::json!({"dry_run": true, "removed": [], "kept": orphans}),
        );
        return Ok(ExitCode::Success);
    }

    let (removable, kept): (Vec<&Orphan>, Vec<&Orphan>) =
        orphans.iter().partition(|o| args.force || !o.dirty);

    if !removable.is_empty()
        && !confirm(
            &format!("remove {} orphaned file(s)?", removable.len()),
            ctx.yes,
            ctx.resolver.interactive(),
        )?
    {
        out.say("cancelled");
        return Ok(ExitCode::Success);
    }

    let mut removed = Vec::new();
    for o in removable {
        engine.projector.remove(&o.file)?;
        engine.state.forget_issue(&o.key)?;
        removed.push(o);
        out.say(format!("removed {}", o.file));
    }

    if !kept.is_empty() {
        out.say(format!(
            "kept {} file(s) with unpushed edits — push them, or re-run with --force",
            kept.len()
        ));
    }
    out.emit(
        "clean",
        &serde_json::json!({"dry_run": false, "removed": removed, "kept": kept}),
    );

    // Leaving work behind is not a failure, but it is not a clean sweep either.
    Ok(if kept.is_empty() {
        ExitCode::Success
    } else {
        ExitCode::Generic
    })
}

/// Exposed for the error path when a workspace has no state at all.
pub fn require_workspace(ctx: &Context) -> Result<()> {
    if ctx.workspace.is_initialized() {
        return Ok(());
    }
    Err(JiredError::usage("not a jired workspace").with_hint("run `jired init`"))
}
