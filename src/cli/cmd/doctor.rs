//! `jired doctor` — check a workspace over and repair what can be repaired.
//!
//! Two things age badly in a workspace: files a human moved or renamed, and
//! conventions jired itself changed between versions — zero-padded issue
//! numbers in filenames, the section headings inside a file, the name of a
//! setting. Rather than making every command defensive about all of them, the
//! repairs live here, run on demand, and say exactly what they did.
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
use crate::style::{self, BOLD, DIM, GREEN, RED, YELLOW};
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

    #[command(flatten)]
    pub conn: crate::cli::ConnectionArgs,
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

    /// The colour the mark is read in: green for settled, yellow for something
    /// still to do, red for something doctor cannot do itself.
    fn color(self) -> &'static str {
        match self {
            Status::Ok => crate::style::DIM,
            Status::Fixed => crate::style::GREEN,
            Status::WouldFix => crate::style::YELLOW,
            Status::Problem => crate::style::RED,
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
    /// What a grouping rebuild did, when this is the groups check.
    #[serde(skip_serializing_if = "Option::is_none")]
    regrouped: Option<crate::sync::group::GroupReport>,
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
    let mut ctx = Context::open_with(g, &args.conn, out)?;

    // Always survey first. Doctor renames files, rewrites config and moves
    // credentials; someone typing `jired doctor` to find out what is wrong
    // deserves to see the list before any of that happens.
    let survey = DoctorArgs {
        dry_run: true,
        ..args.clone()
    };
    let preview = collect(&mut ctx, &survey)?;
    let repairable = preview
        .iter()
        .filter(|c| c.status == Status::WouldFix)
        .count();

    // Only a person gets asked; a script or `--yes` proceeds, and then the
    // preview would just be the same list printed twice.
    let must_ask = ctx.resolver.interactive() && !ctx.yes;
    let apply = !args.dry_run
        && repairable > 0
        && (!must_ask || {
            render(&preview, out);
            out.say("");
            crate::cli::cmd::prompt::confirm(
                &format!("apply {repairable} repair(s)?"),
                false,
                true,
            )?
        });

    let checks = if apply {
        ctx.lock()?;
        collect(&mut ctx, args)?
    } else {
        if !args.dry_run && repairable > 0 {
            out.say("no changes made");
            out.emit(
                "doctor",
                &DoctorReport {
                    dry_run: true,
                    fixed: 0,
                    pending: repairable,
                    problems: preview
                        .iter()
                        .filter(|c| c.status == Status::Problem)
                        .count(),
                    checks: preview,
                },
            );
            return Ok(ExitCode::Success);
        }
        preview
    };

    render(&checks, out);
    finish(checks, args.dry_run && !apply, out)
}

/// Run every check, repairing as it goes unless `args.dry_run`.
fn collect(ctx: &mut Context, args: &DoctorArgs) -> Result<Vec<Check>> {
    // Section repairs rewrite files where they stand, so they run before
    // anything moves them.
    let sections = check_sections(ctx, args)?;
    let custom_fields = check_custom_fields(ctx, args)?;
    // The filename pass decides where every issue ends up, so the database
    // check works from its result rather than from what is on disk right now —
    // otherwise a dry run would report the two repairs undoing each other.
    let (filenames, final_paths) = check_filenames(ctx, args)?;
    // Grouping folders hold links to issue files by name, so a rename leaves
    // them dangling; repairing the names without repairing the links would be
    // half a repair.
    let renamed = !filenames.changes.is_empty();
    let mut checks = vec![
        check_layout(ctx, args)?,
        check_config(ctx, args)?,
        check_credentials(ctx, args)?,
        sections,
        custom_fields,
        filenames,
        check_state_paths(ctx, args, &final_paths)?,
        check_gitignore(ctx, args)?,
        check_agents(ctx, args)?,
    ];
    if args.regroup || renamed {
        checks.push(check_groups(ctx, args)?);
    }
    Ok(checks)
}

fn render(checks: &[Check], out: Out) {
    for c in checks {
        // Padded before painting: escape codes count towards a format width.
        out.say(format!(
            "{} {} {}",
            style::out(c.status.color(), format!("{:<5}", c.status.mark())),
            style::out(BOLD, format!("{:<12}", c.name)),
            c.detail
        ));
        for change in &c.changes {
            out.say(format!(
                "            {} {} {}",
                style::out(RED, &change.from),
                style::out(DIM, "→"),
                style::out(GREEN, &change.to)
            ));
        }
    }
}

fn finish(checks: Vec<Check>, dry_run: bool, out: Out) -> Result<ExitCode> {
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
    let (color, verdict) = match (problems, pending, fixed) {
        (0, 0, 0) => (GREEN, "the workspace is healthy".to_string()),
        (0, 0, n) => (GREEN, format!("repaired {n} thing(s)")),
        (0, n, _) => (
            YELLOW,
            format!("{n} thing(s) to repair — re-run without --dry-run"),
        ),
        (p, _, _) => (RED, format!("{p} thing(s) need attention")),
    };
    out.say(style::out(color, verdict));

    let report = DoctorReport {
        dry_run,
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
        regrouped: None,
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
        regrouped: None,
    }
}

fn problem(name: &'static str, detail: impl Into<String>) -> Check {
    Check {
        name,
        status: Status::Problem,
        detail: detail.into(),
        changes: Vec::new(),
        regrouped: None,
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

    // A setting that changed name is not a setting that stopped mattering: left
    // alone it reads as configured and does nothing.
    let renamed: Vec<(&str, &str, String)> = RENAMED_KEYS
        .iter()
        .filter_map(|(old, new)| Some((*old, *new, cfg.get(old)?)))
        .collect();

    let missing: Vec<&(&str, &str)> = crate::config::DEFAULTS
        .iter()
        .filter(|(key, _)| cfg.get(key).is_none())
        .filter(|(key, _)| !renamed.iter().any(|(_, new, _)| new == key))
        .collect();
    if missing.is_empty() && renamed.is_empty() {
        return Ok(ok("config", "every setting is spelled out"));
    }

    let mut detail = Vec::new();
    if !renamed.is_empty() {
        detail.push(format!(
            "renaming {}",
            renamed
                .iter()
                .map(|(old, new, _)| format!("{old} → {new}"))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if !missing.is_empty() {
        detail.push(format!(
            "writing defaults for {}",
            missing
                .iter()
                .map(|(k, _)| *k)
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    let detail = detail.join("; ");
    if args.dry_run {
        return Ok(repaired("config", true, detail));
    }
    for (old, new, value) in &renamed {
        if cfg.get(new).is_none() {
            cfg.set_documented(new, value)?;
        }
        cfg.unset(old);
    }
    for (key, value) in missing {
        cfg.set_documented(key, value)?;
    }
    cfg.save_to(&path)?;
    Ok(repaired("config", false, detail))
}

/// Settings that changed name between versions, old → new.
const RENAMED_KEYS: [(&str, &str); 1] = [("checklist.done_transition", "children.done_transition")];

/// There is an account, its token can still be read back, and it is kept where
/// `creds.storage` says it should be.
fn check_credentials(ctx: &mut Context, args: &DoctorArgs) -> Result<Check> {
    let session = SessionStore::open(&ctx.workspace)?;
    let Some(account) = session.account()? else {
        return Ok(problem(
            "credentials",
            "no account is stored — run `jired init`",
        ));
    };
    if let Err(e) = account.token() {
        return Ok(problem(
            "credentials",
            format!(
                "the stored token cannot be read ({}) — run `jired init --force`",
                e.message
            ),
        ));
    }

    let here = account.secret_location();
    let wanted = match ctx.resolver.get_quiet("creds.storage")? {
        Some(v) => crate::auth::CredStore::parse(&v)?,
        None => here,
    };
    if wanted == here {
        return Ok(ok(
            "credentials",
            format!(
                "{} in {} ({})",
                account.identity.as_deref().unwrap_or("account"),
                here.describe(),
                account.secret_storage()
            ),
        ));
    }

    // creds.storage changed since the credential was stored: move it rather
    // than making the user re-authenticate for a setting they already answered.
    let detail = format!(
        "moving the credential from {} to {}",
        here.describe(),
        wanted.describe()
    );
    if args.dry_run {
        return Ok(repaired("credentials", true, detail));
    }
    match session.relocate_secret(&account, wanted, args.conn.insecure_store) {
        Ok(()) => Ok(repaired("credentials", false, detail)),
        Err(e) => Ok(problem(
            "credentials",
            format!(
                "cannot move the credential to {}: {}",
                wanted.describe(),
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

/// Every issue file carries the sections this build writes.
///
/// `## Checklist` used to hold the subtasks. They live under `## Children` now,
/// because they are issues of their own, and `## Checklist` belongs to the
/// checklist plugin — so a file written before the split has its subtasks under
/// the wrong heading and no `## Children` at all. Moving them is a repair only
/// doctor can make: `pull` would see a hand-edited file and merge into the
/// confusion rather than out of it.
///
/// `## Worklogs` is newer still, and a file written before it simply lacks the
/// heading; the entries come back from the last snapshot with the checklist.
fn check_sections(ctx: &mut Context, args: &DoctorArgs) -> Result<Check> {
    use crate::doc::{
        ChecklistEntry, ChildItem, SECTION_CHECKLIST, SECTION_CHILDREN, SECTION_WORKLOGS,
    };

    let projector = crate::project::Projector::new(ctx.workspace.root());
    let files = projector.scan()?;
    let state = StateStore::open(&ctx.workspace)?;
    // The checklist and the worklogs are server-owned, so the last snapshot is
    // where their current contents come from. Without a readable account there
    // is none, which costs those two but not the move.
    let engine = crate::sync::Engine::open(ctx).ok();
    let field_map = match &engine {
        Some(engine) => engine.field_map().ok(),
        None => None,
    };

    let mut changes = Vec::new();
    let mut blocked = Vec::new();
    for (key, file) in &files {
        let Ok(text) = projector.read_raw(&file.relative) else {
            continue;
        };
        let headings = crate::doc::section_headings(&text);
        let has = |name: &str| headings.iter().any(|h| h == name);
        if has(SECTION_CHILDREN) && has(SECTION_CHECKLIST) && has(SECTION_WORKLOGS) {
            continue;
        }
        let mut doc = match projector.read(&file.relative) {
            Ok(doc) => doc,
            Err(e) => {
                blocked.push(format!("{}: {}", file.relative, e.message));
                continue;
            }
        };

        // Anything under the old `## Checklist` was a subtask line, key-prefixed
        // or written by hand for push to create.
        let moved = if has(SECTION_CHILDREN) {
            0
        } else {
            doc.children = std::mem::take(&mut doc.checklist)
                .into_iter()
                .filter_map(|entry| match entry {
                    ChecklistEntry::Item(item) => {
                        let (key, title) = crate::doc::split_leading_key(&item.title);
                        Some(ChildItem {
                            checked: item.checked,
                            key,
                            title: title.to_string(),
                        })
                    }
                    ChecklistEntry::Header(_) => None,
                })
                .collect();
            doc.children.len()
        };
        let snapshot = base_snapshot(&state, field_map.as_ref(), engine.as_ref(), key);
        if let Some(issue) = &snapshot {
            doc.checklist = issue.fields.checklist.clone();
            // Only when the file never had the heading: a file that does have
            // it may be carrying time staged but not yet pushed.
            if !has(SECTION_WORKLOGS) {
                doc.worklogs = crate::sync::render::worklog_entries(issue);
            }
        } else {
            doc.checklist = Vec::new();
        }

        let rewritten = doc.render()?;
        if rewritten == text {
            continue;
        }
        if !args.dry_run {
            std::fs::write(projector.path_of(&file.relative), &rewritten)?;
            // A file that was in sync stays in sync: nothing about the issue
            // changed, only the shape of the file holding it.
            if was_in_sync(&state, key, &text)? {
                state.set_base_md_hash(key, &crate::store::state::content_hash(&rewritten))?;
            }
        }
        let mut what = Vec::new();
        if moved > 0 {
            what.push(format!(
                "{moved} subtask(s) moved under `## {SECTION_CHILDREN}`"
            ));
        }
        for (missing, name) in [
            (!has(SECTION_CHECKLIST), SECTION_CHECKLIST),
            (!has(SECTION_WORKLOGS), SECTION_WORKLOGS),
        ] {
            if missing {
                what.push(format!("restoring `## {name}`"));
            }
        }
        changes.push(Change {
            from: file.relative.clone(),
            to: what.join("; "),
        });
    }

    if !blocked.is_empty() {
        let mut check = problem("sections", blocked.join("; "));
        check.changes = changes;
        return Ok(check);
    }
    if changes.is_empty() {
        return Ok(ok(
            "sections",
            format!("{} file(s) have the current sections", files.len()),
        ));
    }
    let mut check = repaired(
        "sections",
        args.dry_run,
        format!(
            "rewriting {} file(s) to the current sections",
            changes.len()
        ),
    );
    check.changes = changes;
    Ok(check)
}

/// Every issue file carries exactly the custom fields this workspace tracks.
///
/// `ignored_custom_fields` decides what gets written, and cannot reach back into
/// files already on disk. Left as they are, those files are worse than untidy:
/// base no longer has the field, so `push` reads the leftover copy as a value
/// someone typed — and after a field is *un*-ignored the file is the one missing
/// it, which reads as "clear this field on the server".
///
/// For a file that is exactly what jired last wrote, `custom:` is derived data
/// and any disagreement with base is jired's own doing, so it is replaced
/// outright — which also clears out fields a later version of jired learned to
/// model elsewhere, such as the checklist plugin's. A file with local edits gets
/// the safe half: an ignored field is removed, because it must not be pushed
/// either way, and nothing is written over an edit that may have been meant.
fn check_custom_fields(ctx: &mut Context, args: &DoctorArgs) -> Result<Check> {
    let projector = crate::project::Projector::new(ctx.workspace.root());
    let files = projector.scan()?;
    let state = StateStore::open(&ctx.workspace)?;
    let Ok(engine) = crate::sync::Engine::open(ctx) else {
        return Ok(ok("custom-fields", "no account yet"));
    };
    let field_map = engine.field_map()?;
    let render_ctx = crate::sync::render::RenderContext {
        markup: engine.markup(),
        base_url: engine.base_url(),
        ignored_custom: &field_map.ignored,
    };

    let mut changes = Vec::new();
    for (key, file) in &files {
        let (Ok(text), Ok(mut doc)) = (
            projector.read_raw(&file.relative),
            projector.read(&file.relative),
        ) else {
            continue;
        };
        let in_sync = was_in_sync(&state, key, &text)?;

        let wanted = if in_sync {
            base_custom(&state, &render_ctx, engine.markup(), &field_map, key)?
        } else {
            let mut kept = doc.frontmatter.custom.clone();
            // An ignored field must not be pushed, and a field that has a home
            // of its own in the file was never a `custom:` entry a person
            // wrote — both are safe to drop even from an edited file.
            kept.retain(|id, _| !field_map.is_ignored(id) && !field_map.is_well_known(id));
            kept
        };
        if wanted == doc.frontmatter.custom {
            continue;
        }

        let dropped: Vec<&String> = doc
            .frontmatter
            .custom
            .keys()
            .filter(|id| !wanted.contains_key(*id))
            .collect();
        let restored: Vec<&String> = wanted
            .keys()
            .filter(|id| !doc.frontmatter.custom.contains_key(*id))
            .collect();
        let mut what = Vec::new();
        if !dropped.is_empty() {
            what.push(format!("dropping {}", join(&dropped)));
        }
        if !restored.is_empty() {
            what.push(format!("restoring {}", join(&restored)));
        }
        if what.is_empty() {
            // Same fields, different values: the file was in sync, so base is
            // what it should have been showing.
            what.push("refreshing the values".to_string());
        }
        doc.frontmatter.custom = wanted;

        let rewritten = doc.render()?;
        if !args.dry_run {
            std::fs::write(projector.path_of(&file.relative), &rewritten)?;
            if in_sync {
                state.set_base_md_hash(key, &crate::store::state::content_hash(&rewritten))?;
            }
        }
        changes.push(Change {
            from: file.relative.clone(),
            to: what.join("; "),
        });
    }

    if changes.is_empty() {
        return Ok(ok(
            "custom-fields",
            match engine.ignored_custom_fields.len() {
                0 => format!("{} file(s) track every custom field", files.len()),
                n => format!(
                    "{} file(s) agree with the {n} ignored field(s)",
                    files.len()
                ),
            },
        ));
    }
    let mut check = repaired(
        "custom-fields",
        args.dry_run,
        format!("correcting the custom fields of {} file(s)", changes.len()),
    );
    check.changes = changes;
    Ok(check)
}

fn join(ids: &[&String]) -> String {
    ids.iter()
        .map(|id| id.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

/// The custom fields the last snapshot renders to, ignore list applied.
fn base_custom(
    state: &StateStore,
    render_ctx: &crate::sync::render::RenderContext<'_>,
    markup: crate::api::Markup,
    field_map: &crate::api::normalize::FieldMap,
    key: &str,
) -> Result<std::collections::BTreeMap<String, serde_json::Value>> {
    let Some(base) = state.issue(key)?.and_then(|row| row.base) else {
        return Ok(Default::default());
    };
    let Ok(issue) = crate::api::normalize::issue_from_json(&base.raw, markup, field_map) else {
        return Ok(Default::default());
    };
    Ok(crate::sync::render::render_issue(&issue, render_ctx)?
        .doc
        .frontmatter
        .custom)
}

/// The issue as the server last showed it, which is where the server-owned
/// sections — the checklist, the worklogs — come back from.
fn base_snapshot(
    state: &StateStore,
    field_map: Option<&crate::api::normalize::FieldMap>,
    engine: Option<&crate::sync::Engine>,
    key: &str,
) -> Option<crate::api::RemoteIssue> {
    let (field_map, engine) = (field_map?, engine?);
    let base = state.issue(key).ok()??.base?;
    crate::api::normalize::issue_from_json(&base.raw, engine.markup(), field_map).ok()
}

/// True when the file on disk is exactly what jired last wrote for this issue.
fn was_in_sync(state: &StateStore, key: &str, text: &str) -> Result<bool> {
    Ok(state
        .issue(key)?
        .and_then(|row| row.base)
        .is_some_and(|base| base.md_hash == crate::store::state::content_hash(text)))
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

/// `CLAUDE.md` and `AGENTS.md` describe the format this build actually writes,
/// and the permission files list the commands this build says only read. Both
/// go stale the same way — by upgrading jired — so both are checked here.
fn check_agents(ctx: &Context, args: &DoctorArgs) -> Result<Check> {
    use crate::cli::cmd::init_agents as agents;

    let docs = agents::render();
    let mut stale = Vec::new();
    for name in ["CLAUDE.md", "AGENTS.md"] {
        let path = ctx.workspace.root().join(name);
        let current = std::fs::read_to_string(&path).unwrap_or_default();
        if !current.contains(docs.trim()) {
            stale.push(name);
            if !args.dry_run {
                agents::write_managed_block(&path, &docs)?;
            }
        }
    }

    let root = ctx.workspace.root();
    let settings = std::fs::read_to_string(root.join(agents::CLAUDE_SETTINGS)).unwrap_or_default();
    let parsed: serde_json::Value =
        serde_json::from_str(&settings).unwrap_or(serde_json::Value::Null);
    if !agents::claude_settings_current(&parsed) {
        stale.push(agents::CLAUDE_SETTINGS);
    }
    let rules = std::fs::read_to_string(root.join(agents::CODEX_RULES)).unwrap_or_default();
    if !rules.contains(agents::codex_rules().trim()) {
        stale.push(agents::CODEX_RULES);
    }
    // One write for both permission files: `write_permissions` is a no-op for
    // whichever of them was already current.
    if !args.dry_run
        && stale
            .iter()
            .any(|s| *s == agents::CLAUDE_SETTINGS || *s == agents::CODEX_RULES)
    {
        agents::write_permissions(root)?;
    }

    if stale.is_empty() {
        return Ok(ok("agents", "instructions and permissions are current"));
    }
    Ok(repaired(
        "agents",
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
    let mut check = repaired(
        "groups",
        false,
        format!(
            "rebuilt {} link(s) across {} parent(s), {} sprint(s), {} board(s)",
            report.links, report.parents, report.sprints, report.boards
        ),
    );
    for w in &report.warnings {
        ctx.out.warn(w);
    }
    // Carried into the JSON: what the rebuild produced — how many links, and in
    // which mode — is the only way a caller can check the outcome.
    check.regrouped = Some(report);
    Ok(check)
}
