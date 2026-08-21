//! `jired doctor` — check a workspace over and repair what can be repaired.
//!
//! Two things age badly in a workspace: files a human moved or renamed, and
//! conventions jired itself changed between versions — the zero-padded issue
//! numbers in filenames being the first of those. Rather than making every
//! command defensive about both, the repairs live here, run on demand, and say
//! exactly what they did.
//!
//! Every check is idempotent, so running `doctor` twice is the same as running
//! it once, and `--dry-run` reports without touching anything.

use crate::cli::context::Context;
use crate::cli::GlobalArgs;
use crate::error::{ExitCode, Result};
use crate::json::Out;
use crate::project::naming;
use crate::store::session::SessionStore;
use crate::store::state::StateStore;
use clap::Args;
use serde::Serialize;

#[derive(Args, Debug, Clone, Default)]
pub struct DoctorArgs {
    /// Report problems without changing anything
    #[arg(long)]
    pub dry_run: bool,

    /// Rebuild the grouping folders even when nothing else changed
    #[arg(long)]
    pub regroup: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum Status {
    /// Nothing to do.
    Ok,
    /// Something was wrong and doctor put it right.
    Fixed,
    /// Something is wrong and doctor would put it right without `--dry-run`.
    WouldFix,
    /// Something is wrong that doctor cannot fix on its own.
    Problem,
}

impl Status {
    fn mark(self) -> &'static str {
        match self {
            Status::Ok => "ok  ",
            Status::Fixed => "fixed",
            Status::WouldFix => "todo",
            Status::Problem => "!",
        }
    }
}

#[derive(Debug, Serialize)]
struct Check {
    name: &'static str,
    status: Status,
    detail: String,
    /// Individual repairs, for the checks that make several.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    changes: Vec<Change>,
}

#[derive(Debug, Serialize)]
struct Change {
    from: String,
    to: String,
}

#[derive(Debug, Serialize)]
struct DoctorReport {
    dry_run: bool,
    checks: Vec<Check>,
    fixed: usize,
    pending: usize,
    problems: usize,
}

pub fn run(g: &GlobalArgs, args: &DoctorArgs, out: Out) -> Result<ExitCode> {
    let mut ctx = Context::open(g, out)?;
    if !args.dry_run {
        ctx.lock()?;
    }

    // The filename pass decides where every issue ends up, so the database
    // check works from its result rather than from what is on disk right now —
    // otherwise a dry run would report the two repairs undoing each other.
    let (filenames, final_paths) = check_filenames(&ctx, args)?;
    // Grouping folders hold links to issue files by name, so a rename leaves
    // them dangling; repairing the names without repairing the links would be
    // half a repair.
    let renamed = !filenames.changes.is_empty();
    let mut checks = vec![
        check_layout(&ctx, args)?,
        check_config(&mut ctx, args)?,
        check_credentials(&ctx)?,
        filenames,
        check_state_paths(&ctx, args, &final_paths)?,
        check_gitignore(&ctx, args)?,
        check_agent_docs(&ctx, args)?,
    ];
    if args.regroup || renamed {
        checks.push(check_groups(&mut ctx, args)?);
    }

    for c in &checks {
        out.say(format!(
            "{:<5} {:<12} {}",
            c.status.mark(),
            c.name,
            c.detail
        ));
        for change in &c.changes {
            out.say(format!("            {} → {}", change.from, change.to));
        }
    }

    let fixed = checks.iter().filter(|c| c.status == Status::Fixed).count();
    let pending = checks
        .iter()
        .filter(|c| c.status == Status::WouldFix)
        .count();
    let problems = checks
        .iter()
        .filter(|c| c.status == Status::Problem)
        .count();

    out.say("");
    out.say(match (problems, pending, fixed) {
        (0, 0, 0) => "the workspace is healthy".to_string(),
        (0, 0, n) => format!("repaired {n} thing(s)"),
        (0, n, _) => format!("{n} thing(s) to repair — re-run without --dry-run"),
        (p, _, _) => format!("{p} thing(s) need attention"),
    });

    let report = DoctorReport {
        dry_run: args.dry_run,
        checks,
        fixed,
        pending,
        problems,
    };
    out.emit("doctor", &report);

    Ok(if problems == 0 {
        ExitCode::Success
    } else {
        ExitCode::Generic
    })
}

fn ok(name: &'static str, detail: impl Into<String>) -> Check {
    Check {
        name,
        status: Status::Ok,
        detail: detail.into(),
        changes: Vec::new(),
    }
}

fn repaired(name: &'static str, dry_run: bool, detail: impl Into<String>) -> Check {
    Check {
        name,
        status: if dry_run {
            Status::WouldFix
        } else {
            Status::Fixed
        },
        detail: detail.into(),
        changes: Vec::new(),
    }
}

fn problem(name: &'static str, detail: impl Into<String>) -> Check {
    Check {
        name,
        status: Status::Problem,
        detail: detail.into(),
        changes: Vec::new(),
    }
}

/// `.jired/` exists, has its subdirectories, and is not world-readable.
fn check_layout(ctx: &Context, args: &DoctorArgs) -> Result<Check> {
    let ws = &ctx.workspace;
    let missing: Vec<&str> = [
        (ws.state_dir(), ".jired"),
        (ws.logs_dir(), ".jired/logs"),
        (ws.templates_dir(), ".jired/templates"),
    ]
    .into_iter()
    .filter(|(path, _)| !path.is_dir())
    .map(|(_, name)| name)
    .collect();

    let loose = insecure_mode(ws);
    if missing.is_empty() && !loose {
        return Ok(ok("layout", ".jired/ is complete and private"));
    }
    if args.dry_run {
        return Ok(repaired("layout", true, describe_layout(&missing, loose)));
    }
    ws.create()?;
    let session = ws.session_db_path();
    if session.is_file() {
        crate::workspace::restrict_file_mode(&session)?;
    }
    Ok(repaired("layout", false, describe_layout(&missing, loose)))
}

fn describe_layout(missing: &[&str], loose: bool) -> String {
    let mut parts = Vec::new();
    if !missing.is_empty() {
        parts.push(format!("recreating {}", missing.join(", ")));
    }
    if loose {
        parts.push("tightening permissions on the credential store".to_string());
    }
    parts.join("; ")
}

#[cfg(unix)]
fn insecure_mode(ws: &crate::workspace::Workspace) -> bool {
    use std::os::unix::fs::PermissionsExt;
    let path = ws.session_db_path();
    match std::fs::metadata(&path) {
        Ok(meta) => meta.permissions().mode() & 0o077 != 0,
        Err(_) => false,
    }
}

#[cfg(not(unix))]
fn insecure_mode(_ws: &crate::workspace::Workspace) -> bool {
    false
}

/// The config file names every setting, so nothing has to be guessed later.
fn check_config(ctx: &mut Context, args: &DoctorArgs) -> Result<Check> {
    let path = ctx.workspace.config_path();
    let mut cfg = ctx.local_config()?;
    if cfg.get("server.url").is_none() {
        return Ok(problem(
            "config",
            format!("{} has no server.url — run `jired init`", path.display()),
        ));
    }

    let missing: Vec<&(&str, &str)> = crate::config::DEFAULTS
        .iter()
        .filter(|(key, _)| cfg.get(key).is_none())
        .collect();
    if missing.is_empty() {
        return Ok(ok("config", "every setting is spelled out"));
    }
    let detail = format!(
        "writing defaults for {}",
        missing
            .iter()
            .map(|(k, _)| *k)
            .collect::<Vec<_>>()
            .join(", ")
    );
    if args.dry_run {
        return Ok(repaired("config", true, detail));
    }
    for (key, value) in missing {
        cfg.set_documented(key, value)?;
    }
    cfg.save_to(&path)?;
    Ok(repaired("config", false, detail))
}

/// There is an account, and its token can still be read back.
fn check_credentials(ctx: &Context) -> Result<Check> {
    let session = SessionStore::open(&ctx.workspace)?;
    let Some(account) = session.account()? else {
        return Ok(problem(
            "credentials",
            "no account is stored — run `jired init`",
        ));
    };
    match account.token() {
        Ok(_) => Ok(ok(
            "credentials",
            format!(
                "{} ({}, {})",
                account.identity.as_deref().unwrap_or("account"),
                account.deployment.as_str(),
                account.secret_storage()
            ),
        )),
        Err(e) => Ok(problem(
            "credentials",
            format!(
                "the stored token cannot be read ({}) — run `jired init --force`",
                e.message
            ),
        )),
    }
}

/// Every issue file carries the name the current conventions give it.
///
/// This is the migration path for the zero-padded issue numbers: an older
/// workspace has `PROJ-9. …md`, the canonical name is now `PROJ-0009. …md`, and
/// the rename takes the attachment folder and the database row with it.
fn check_filenames(
    ctx: &Context,
    args: &DoctorArgs,
) -> Result<(Check, std::collections::BTreeMap<String, String>)> {
    let projector = crate::project::Projector::new(ctx.workspace.root());
    let files = projector.scan()?;
    let state = StateStore::open(&ctx.workspace)?;

    let mut changes = Vec::new();
    let mut blocked = Vec::new();
    let mut final_paths: std::collections::BTreeMap<String, String> = files
        .iter()
        .map(|(key, file)| (key.clone(), file.relative.clone()))
        .collect();
    for (key, file) in &files {
        // The frontmatter is the truth about which issue this is; the name is
        // a rendering of it that may be out of date.
        let doc = match projector.read(&file.relative) {
            Ok(doc) => doc,
            Err(e) => {
                blocked.push(format!("{}: {}", file.relative, e.message));
                continue;
            }
        };
        let canonical = naming::file_name(&doc.frontmatter.key, &doc.frontmatter.summary);
        if canonical == file.relative {
            continue;
        }
        if files.values().any(|f| f.relative == canonical) {
            blocked.push(format!(
                "{} should be {canonical}, but that name is taken",
                file.relative
            ));
            continue;
        }
        if !args.dry_run {
            projector.rename(&file.relative, &canonical)?;
            state.set_file_path(key, &canonical)?;
        }
        final_paths.insert(key.clone(), canonical.clone());
        changes.push(Change {
            from: file.relative.clone(),
            to: canonical,
        });
    }

    if !blocked.is_empty() {
        let mut check = problem("filenames", blocked.join("; "));
        check.changes = changes;
        return Ok((check, final_paths));
    }
    if changes.is_empty() {
        return Ok((
            ok(
                "filenames",
                format!("{} file(s) already named correctly", files.len()),
            ),
            final_paths,
        ));
    }
    let mut check = repaired(
        "filenames",
        args.dry_run,
        format!(
            "renaming {} file(s) to the current convention",
            changes.len()
        ),
    );
    check.changes = changes;
    Ok((check, final_paths))
}

/// The database agrees with the working copy about where each issue lives.
fn check_state_paths(
    ctx: &Context,
    args: &DoctorArgs,
    final_paths: &std::collections::BTreeMap<String, String>,
) -> Result<Check> {
    let state = StateStore::open(&ctx.workspace)?;

    let mut changes = Vec::new();
    for row in state.issues()? {
        let Some(path) = final_paths.get(&row.key) else {
            continue;
        };
        if row.file_path.as_deref() == Some(path.as_str()) {
            continue;
        }
        if !args.dry_run {
            state.set_file_path(&row.key, path)?;
        }
        changes.push(Change {
            from: row.file_path.unwrap_or_else(|| "(unset)".into()),
            to: path.clone(),
        });
    }

    if changes.is_empty() {
        return Ok(ok("state", "every tracked issue points at its file"));
    }
    let mut check = repaired(
        "state",
        args.dry_run,
        format!("repointing {} database row(s)", changes.len()),
    );
    check.changes = changes;
    Ok(check)
}

fn check_gitignore(ctx: &Context, args: &DoctorArgs) -> Result<Check> {
    if args.dry_run {
        // `ensure_gitignore` reports whether it changed anything, but it also
        // writes; in a dry run compare instead of asking.
        let text =
            std::fs::read_to_string(ctx.workspace.root().join(".gitignore")).unwrap_or_default();
        return Ok(if text.contains("/.jired/") {
            ok("gitignore", ".jired/ is ignored")
        } else {
            repaired("gitignore", true, "adding the .jired/ entry")
        });
    }
    Ok(if ctx.workspace.ensure_gitignore()? {
        repaired("gitignore", false, "refreshed the .jired/ entry")
    } else {
        ok("gitignore", ".jired/ is ignored")
    })
}

/// `CLAUDE.md` and `AGENTS.md` describe the format this build actually writes.
fn check_agent_docs(ctx: &Context, args: &DoctorArgs) -> Result<Check> {
    let docs = crate::cli::cmd::agent_docs::render();
    let mut stale = Vec::new();
    for name in ["CLAUDE.md", "AGENTS.md"] {
        let path = ctx.workspace.root().join(name);
        let current = std::fs::read_to_string(&path).unwrap_or_default();
        if !current.contains(docs.trim()) {
            stale.push(name);
            if !args.dry_run {
                crate::cli::cmd::agent_docs::write_managed_block(&path, &docs)?;
            }
        }
    }
    if stale.is_empty() {
        return Ok(ok("agent-docs", "CLAUDE.md and AGENTS.md are current"));
    }
    Ok(repaired(
        "agent-docs",
        args.dry_run,
        format!("refreshing {}", stale.join(", ")),
    ))
}

/// Grouping folders are views; rebuilding them from the database is always safe.
fn check_groups(ctx: &mut Context, args: &DoctorArgs) -> Result<Check> {
    if args.dry_run {
        return Ok(repaired(
            "groups",
            true,
            "rebuilding every grouping folder from the database",
        ));
    }
    let engine = match crate::sync::Engine::open(ctx) {
        Ok(engine) => engine,
        Err(e) => return Ok(problem("groups", e.message.clone())),
    };
    let options = crate::sync::group::GroupOptions::from_context(ctx)?;
    let report = crate::sync::group::regroup(&engine, engine.projector.root(), &options)?;
    Ok(repaired(
        "groups",
        false,
        format!(
            "rebuilt {} link(s) across {} parent(s), {} sprint(s), {} board(s)",
            report.links, report.parents, report.sprints, report.boards
        ),
    ))
}
