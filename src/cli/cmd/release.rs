//! `jired release` and `jired releases` — a project's versions, from the one
//! being planned to the one that shipped.
//!
//! A version is what `fix_versions:` and `affects_versions:` name, and until
//! these existed it was the one thing in a sprint's set-up that still meant
//! opening the web UI. They are `jired sprint`'s neighbours and work the same
//! way: a version has no file to stage it in, so every write here goes to the
//! server there and then, and `-n/--dry-run` prints the call instead. The group
//! is `release` because `jired version` already says which binary this is.
//!
//! Names are resolved against the server's own listing rather than the cache:
//! every command here talks to the server anyway, and one request that is
//! always current beats a cache that has to be fetched before it can be
//! trusted. What comes back is written into the cache on the way through,
//! which is what lets `fix_versions:` name a version made a moment ago with no
//! fetch in between — and what push checks those names against.
//!
//! `--issues` on `create`, and `--move-unfixed-to` on `ship`, are the opposite:
//! an issue's versions *are* a file field, so they stage edits to the files and
//! go out through the normal push, the way `sprint create --issues` and
//! `sprint close --move-incomplete-to` do.

use crate::api::{JiraApi, Version, VersionInput, VersionUpdate};
use crate::cli::cmd::edit::{push_these, report_push};
use crate::cli::cmd::sprint::{api_client, unfinished_where, Incomplete};
use crate::cli::cmd::stage::{self, Staged};
use crate::cli::context::Context;
use crate::cli::GlobalArgs;
use crate::error::{ExitCode, JiredError, Result};
use crate::json::Out;
use crate::style::{self, CYAN, DIM};
use chrono::NaiveDate;
use clap::{Args, Subcommand};

#[derive(Subcommand, Debug)]
pub enum ReleaseCmd {
    /// Create a version in the project
    Create(CreateArgs),
    /// Change a version's name, description or dates
    Edit(EditArgs),
    /// Mark a version released, optionally moving its unfinished issues on
    Ship(ShipArgs),
    /// Archive a version, which hides it from Jira's pickers
    Archive(ArchiveArgs),
}

#[derive(Args, Debug, Clone, Default)]
pub struct ReleasesArgs {
    /// Include released and archived versions
    #[arg(long)]
    pub all: bool,
}

#[derive(Args, Debug, Clone, Default)]
pub struct CreateArgs {
    /// What the version is called: `0.5.0`
    #[arg(value_name = "NAME")]
    pub name: String,

    /// When work on it starts: `2026-10-15`
    #[arg(long, value_name = "DATE")]
    pub start: Option<String>,

    /// When it is due out: `2026-10-29`
    #[arg(long, value_name = "DATE")]
    pub release_date: Option<String>,

    /// What the version is for
    #[arg(long, value_name = "TEXT")]
    pub description: Option<String>,

    /// Issues to add it to as a fix version once it exists
    #[arg(long, value_name = "ISSUES", value_delimiter = ',')]
    pub issues: Vec<String>,

    /// With --issues, send the edits instead of leaving them staged
    #[arg(long, requires = "issues")]
    pub push: bool,

    /// Show what would be done without creating or sending anything
    #[arg(long, short = 'n')]
    pub dry_run: bool,
}

#[derive(Args, Debug, Clone, Default)]
pub struct EditArgs {
    /// The version to change: a name, or an id
    #[arg(value_name = "VERSION")]
    pub version: String,

    /// Rename it
    #[arg(long, value_name = "TEXT")]
    pub name: Option<String>,

    /// What the version is for; `--description ''` clears it
    #[arg(long, value_name = "TEXT")]
    pub description: Option<String>,

    /// When work on it starts
    #[arg(long, value_name = "DATE")]
    pub start: Option<String>,

    /// When it is due out
    #[arg(long, value_name = "DATE")]
    pub release_date: Option<String>,

    /// Show the API call without sending it
    #[arg(long, short = 'n')]
    pub dry_run: bool,
}

#[derive(Args, Debug, Clone, Default)]
pub struct ShipArgs {
    /// The version to release: a name, or an id
    #[arg(value_name = "VERSION")]
    pub version: String,

    /// The day it went out; defaults to its planned date, or today
    #[arg(long, value_name = "DATE")]
    pub date: Option<String>,

    /// Move the issues that are not done to this version before releasing
    #[arg(long = "move-unfixed-to", value_name = "VERSION")]
    pub move_unfixed_to: Option<String>,

    /// Show the API calls without sending them
    #[arg(long, short = 'n')]
    pub dry_run: bool,
}

#[derive(Args, Debug, Clone, Default)]
pub struct ArchiveArgs {
    /// The version to archive: a name, or an id
    #[arg(value_name = "VERSION")]
    pub version: String,

    /// Show the API call without sending it
    #[arg(long, short = 'n')]
    pub dry_run: bool,
}

pub fn run(g: &GlobalArgs, cmd: &ReleaseCmd, out: Out) -> Result<ExitCode> {
    match cmd {
        ReleaseCmd::Create(args) => create(g, args, out),
        ReleaseCmd::Edit(args) => edit(g, args, out),
        ReleaseCmd::Ship(args) => ship(g, args, out),
        ReleaseCmd::Archive(args) => archive(g, args, out),
    }
}

// ---- releases ------------------------------------------------------------

pub fn run_list(g: &GlobalArgs, args: &ReleasesArgs, out: Out) -> Result<ExitCode> {
    let mut ctx = Context::open(g, out)?;
    let client = api_client(&mut ctx)?;
    let project = project(&mut ctx)?;
    let versions = listing(&ctx, client.as_ref(), &project, out)?;

    // Released ones are hidden the way `sprints` hides closed sprints: a
    // project that ships every fortnight has more history than plans, and the
    // plans are what the list is opened for.
    let shown: Vec<&Version> = versions
        .iter()
        .filter(|v| args.all || (!v.released && !v.archived))
        .collect();
    if shown.is_empty() {
        out.say(if versions.is_empty() {
            format!("{project} has no versions")
        } else {
            format!("{project} has no unreleased versions")
        });
    }
    let width = crate::cli::cmd::list::column(shown.iter().map(|v| v.name.len()), 24);
    for v in &shown {
        out.say(version_row(v, width));
    }
    if shown.len() < versions.len() {
        out.hint(format!(
            "{} released or archived version(s) not shown — --all includes them",
            versions.len() - shown.len()
        ));
    }
    out.emit(
        "releases",
        &serde_json::json!({ "project": project, "versions": versions }),
    );
    Ok(ExitCode::Success)
}

/// One version as a row: its id, its state, its name, and its dates.
///
/// The same reading as a sprint's — not started, running, finished — so an
/// unreleased version is in the colour of work still open and a released one
/// in the colour of work done.
fn version_row(v: &Version, width: usize) -> String {
    let category = match v.state() {
        "unreleased" => "new",
        "released" => "done",
        _ => "",
    };
    let dates = match (&v.start_date, &v.release_date) {
        (None, None) => String::new(),
        (start, release) => format!(
            "{} → {}",
            start.as_ref().map(day).unwrap_or_default(),
            release.as_ref().map(day).unwrap_or_default()
        )
        .trim()
        .to_string(),
    };
    format!(
        "{} {} {:<width$} {}",
        style::out(CYAN, format!("{:<8}", v.id)),
        style::status(&format!("{:<10}", v.state()), category),
        v.name,
        style::out(DIM, dates)
    )
    .trim_end()
    .to_string()
}

fn day(d: &NaiveDate) -> String {
    crate::api::model::format_day(d)
}

// ---- create --------------------------------------------------------------

fn create(g: &GlobalArgs, args: &CreateArgs, out: Out) -> Result<ExitCode> {
    let name = checked_name(&args.name)?;
    let start = checked_day(args.start.as_deref(), "--start")?;
    let release = checked_day(args.release_date.as_deref(), "--release-date")?;
    check_order(start, release)?;

    let mut ctx = Context::open(g, out)?;
    ctx.lock()?;
    let client = api_client(&mut ctx)?;
    let project = project(&mut ctx)?;
    let existing = listing(&ctx, client.as_ref(), &project, out)?;

    // Jira refuses a second version of one name in a project, and says so in a
    // 400; saying it here names the one already there, which is usually the
    // version that was wanted.
    if let Some(there) = existing.iter().find(|v| v.name.eq_ignore_ascii_case(&name)) {
        return Err(JiredError::usage(format!(
            "{project} already has a version called `{}`",
            there.name
        ))
        .with_detail("version", there.name.clone())
        .with_detail("version_id", there.id.clone())
        .with_detail("state", there.state())
        .with_hint(format!(
            "change it with `jired release edit \"{}\"` instead",
            there.name
        )));
    }

    // Any version the project already has carries its id, which saves asking
    // for the project; the first version of a project has nobody to ask but
    // the project itself.
    let project_id = match existing.iter().find_map(|v| v.project_id.clone()) {
        Some(id) => id,
        None => crate::api::block_on(client.project(&project))?.id,
    };
    let input = VersionInput {
        name,
        project_id,
        description: args.description.clone(),
        start,
        release,
    };
    let request = serde_json::json!({
        "method": "POST",
        "path": format!("{}/version", api_prefix(client.as_ref())),
        "body": input.to_body(),
    });

    if args.dry_run {
        say_request(&request, out)?;
        // Named, not staged: the version does not exist, so there is nothing
        // for `fix_versions:` to name yet.
        for issue in &args.issues {
            out.say(format!(
                "would add it to the fix versions of {}",
                style::out(CYAN, crate::project::naming::key_from_input(issue)?)
            ));
        }
        out.emit(
            "release-create",
            &serde_json::json!({
                "dry_run": true,
                "project": project,
                "version": serde_json::Value::Null,
                "request": request,
                "issues": [],
            }),
        );
        return Ok(ExitCode::Success);
    }

    let version = crate::api::block_on(client.create_version(&input))?;
    remember(&ctx, &project, &version, out);
    out.say(version_row(&version, version.name.len()));
    out.say(format!("created in {project}"));

    // Making a version is almost always followed by putting issues in it, and
    // those are file edits: they stage, and go out with the next push or
    // straight away with --push.
    let target = version.name.clone();
    let staged = stage::stage(&mut ctx, &args.issues, out, move |doc| {
        Ok(add_version(&mut doc.frontmatter.fix_versions, &target))
    })?;
    let pushed = if staged.is_empty() {
        out.hint(format!(
            "put issues in it with `jired set --field fix_versions --value \"{}\" KEY…`, \
             or edit `fix_versions:` in their files",
            version.name
        ));
        None
    } else {
        let keys = changed_keys(&staged);
        if args.push && !keys.is_empty() {
            let report = push_these(&mut ctx, &keys, false)?;
            report_push(&report, out);
            match report.results.iter().all(|r| r.ok) {
                true => out.say(format!("added it to {} issue(s)", keys.len())),
                false => out.say(format!(
                    "{} issue(s) are staged for it, but the push failed — \
                     fix the cause and run `jired push`",
                    keys.len()
                )),
            }
            Some(report)
        } else {
            out.say(format!("staged it on {} issue(s)", keys.len()));
            if !keys.is_empty() {
                out.hint("run `jired push` to send them");
            }
            None
        }
    };

    let exit = pushed
        .as_ref()
        .map(|r| r.exit_code())
        .unwrap_or(ExitCode::Success);
    out.emit(
        "release-create",
        &serde_json::json!({
            "dry_run": false,
            "project": project,
            "version": version,
            "request": request,
            "issues": staged,
            "push": pushed,
        }),
    );
    Ok(exit)
}

/// Add `name` to an issue's versions, unless it is there already. Appended,
/// because the list is in the order the issue gained them.
fn add_version(versions: &mut Vec<String>, name: &str) -> bool {
    if versions.iter().any(|v| v.eq_ignore_ascii_case(name)) {
        return false;
    }
    versions.push(name.to_string());
    true
}

/// Swap one version for another in an issue's list, keeping its place.
///
/// An issue can be in several fix versions — a fix that also goes into a
/// maintenance branch — so only the one being released is moved, and the
/// target is not added twice when the issue already names it.
fn move_version(versions: &mut Vec<String>, from: &str, to: &str) -> bool {
    let Some(at) = versions.iter().position(|v| v.eq_ignore_ascii_case(from)) else {
        return false;
    };
    if versions.iter().any(|v| v.eq_ignore_ascii_case(to)) {
        versions.remove(at);
    } else {
        versions[at] = to.to_string();
    }
    true
}

// ---- edit, archive -------------------------------------------------------

fn edit(g: &GlobalArgs, args: &EditArgs, out: Out) -> Result<ExitCode> {
    let update = VersionUpdate {
        name: match args.name.as_deref().map(str::trim) {
            Some("") => {
                return Err(JiredError::usage("a version cannot be renamed to nothing")
                    .with_hint("pass the new name — `jired release edit 0.5.0 --name 0.5.1`"))
            }
            other => other.map(str::to_string),
        },
        // `--description ''` is how a description comes back off, so it is
        // sent as given rather than filtered for emptiness.
        description: args.description.clone(),
        start: checked_day(args.start.as_deref(), "--start")?,
        release: checked_day(args.release_date.as_deref(), "--release-date")?,
        ..Default::default()
    };
    // Checked before anything is opened: no flags is a mistake about the
    // command, and a listing fetched to find that out is a wasted trip.
    if update.is_empty() {
        return Err(JiredError::missing_value("nothing to change").with_hint(
            "pass --name, --description, --start or --release-date — \
             `jired release edit 0.5.0 --release-date 2026-10-29`",
        ));
    }

    let mut ctx = Context::open(g, out)?;
    ctx.lock()?;
    let client = api_client(&mut ctx)?;
    let project = project(&mut ctx)?;
    let versions = listing(&ctx, client.as_ref(), &project, out)?;
    let version = pick(&project, &versions, &args.version)?;
    if let Some(new) = &update.name {
        if let Some(clash) = versions
            .iter()
            .find(|v| v.id != version.id && v.name.eq_ignore_ascii_case(new))
        {
            return Err(JiredError::usage(format!(
                "{project} already has a version called `{}`",
                clash.name
            ))
            .with_detail("version_id", clash.id.clone()));
        }
    }
    check_order(
        update.start.or(version.start_date),
        update.release.or(version.release_date),
    )?;

    let renamed = update.name.is_some();
    let exit = send(
        &ctx,
        client.as_ref(),
        &project,
        &version,
        update,
        args.dry_run,
        out,
    );
    if renamed && !args.dry_run && exit.is_ok() {
        // Issue files spell a version by name, and renaming it does not touch
        // the issues, so an ordinary fetch — which asks for what changed — does
        // not see it either.
        out.hint(
            "run `jired fetch --full`, then `jired pull`, to bring `fix_versions:` \
             in the issue files up to the new name",
        );
    }
    exit
}

fn archive(g: &GlobalArgs, args: &ArchiveArgs, out: Out) -> Result<ExitCode> {
    let mut ctx = Context::open(g, out)?;
    ctx.lock()?;
    let client = api_client(&mut ctx)?;
    let project = project(&mut ctx)?;
    let versions = listing(&ctx, client.as_ref(), &project, out)?;
    let version = pick(&project, &versions, &args.version)?;
    if version.archived {
        return Err(already(&version, "is already archived"));
    }
    send(
        &ctx,
        client.as_ref(),
        &project,
        &version,
        VersionUpdate {
            archived: Some(true),
            ..Default::default()
        },
        args.dry_run,
        out,
    )
}

/// Send one change to a version, or print the call it would have been.
fn send(
    ctx: &Context,
    client: &dyn JiraApi,
    project: &str,
    version: &Version,
    update: VersionUpdate,
    dry_run: bool,
    out: Out,
) -> Result<ExitCode> {
    let request = update_request(client, version, &update);
    if dry_run {
        say_request(&request, out)?;
        out.emit(
            "release-update",
            &serde_json::json!({
                "dry_run": true,
                "project": project,
                "version": serde_json::Value::Null,
                "request": request,
            }),
        );
        return Ok(ExitCode::Success);
    }
    let updated = crate::api::block_on(client.update_version(&version.id, &update))?;
    remember(ctx, project, &updated, out);
    out.say(version_row(&updated, updated.name.len()));
    if let Some(text) = updated
        .description
        .as_deref()
        .filter(|d| !d.trim().is_empty())
    {
        out.say(format!("description: {text}"));
    }
    out.emit(
        "release-update",
        &serde_json::json!({
            "dry_run": false,
            "project": project,
            "version": updated,
            "request": request,
        }),
    );
    Ok(ExitCode::Success)
}

// ---- ship ----------------------------------------------------------------

fn ship(g: &GlobalArgs, args: &ShipArgs, out: Out) -> Result<ExitCode> {
    let date = checked_day(args.date.as_deref(), "--date")?;

    let mut ctx = Context::open(g, out)?;
    ctx.lock()?;
    let client = api_client(&mut ctx)?;
    let project = project(&mut ctx)?;
    let versions = listing(&ctx, client.as_ref(), &project, out)?;
    let version = pick(&project, &versions, &args.version)?;
    if version.archived {
        return Err(already(&version, "is archived").with_hint(
            "an archived version is out of every picker; release it in Jira if you mean to",
        ));
    }
    if version.released {
        return Err(already(&version, "is already released"));
    }

    let carry_to = args
        .move_unfixed_to
        .as_deref()
        .map(|wanted| carry_target(&project, &versions, wanted, &version))
        .transpose()?;

    // Read off the working copy, not the snapshot: these are the files a
    // carry-over edits, so the set that moves is the set that is listed.
    let unfinished = unfinished_where(&mut ctx, |fm| {
        fm.fix_versions
            .iter()
            .any(|v| v.eq_ignore_ascii_case(&version.name))
    })?;
    let keys: Vec<String> = unfinished.iter().map(|i| i.key.clone()).collect();

    // The day it went out: the one given, else the one it was planned for, else
    // today — which is what Jira's own release dialog offers. Only sent when
    // it differs from what the version already holds.
    let release_date = date.or(version.release_date).unwrap_or_else(today);
    let update = VersionUpdate {
        released: Some(true),
        release: (Some(release_date) != version.release_date).then_some(release_date),
        ..Default::default()
    };
    let request = update_request(client.as_ref(), &version, &update);
    // Said before anything moves or is released, in both paths: which issues a
    // release ships unfinished is the thing worth reading, and reading it
    // afterwards is reading it too late.
    report_unfinished(&unfinished, carry_to.as_ref(), out);

    let doc = |released: bool,
               shipped: Option<&Version>,
               issues: &[Staged],
               push: Option<&crate::sync::push::PushReport>| {
        serde_json::json!({
            "dry_run": args.dry_run,
            "released": released,
            "project": project,
            "version": shipped,
            "request": request,
            "unfinished": unfinished,
            "moved_to": carry_to.as_ref().map(brief),
            "issues": issues,
            "push": push,
        })
    };

    if args.dry_run {
        if let (Some(target), false) = (&carry_to, keys.is_empty()) {
            out.say(format!(
                "would move {} from {} to {} and push",
                keys.join(", "),
                version.name,
                target.name
            ));
        }
        say_request(&request, out)?;
        out.emit("release-ship", &doc(false, None, &[], None));
        return Ok(ExitCode::Success);
    }

    // Moved first, and the release abandoned if the move fails: the issues
    // are what the release is about, and a version that shipped with work
    // still pointing at it is the state this flag exists to prevent.
    let mut moved: Vec<Staged> = Vec::new();
    let mut pushed = None;
    if let (Some(target), false) = (&carry_to, keys.is_empty()) {
        let (from, to) = (version.name.clone(), target.name.clone());
        moved = stage::stage(&mut ctx, &keys, out, move |doc| {
            Ok(move_version(&mut doc.frontmatter.fix_versions, &from, &to))
        })?;
        let changed = changed_keys(&moved);
        if !changed.is_empty() {
            let report = push_these(&mut ctx, &changed, false)?;
            report_push(&report, out);
            if !report.results.iter().all(|r| r.ok) {
                out.say(format!(
                    "{} was left unreleased — moving its issues did not go through",
                    version.name
                ));
                out.hint("fix the cause, run `jired push`, then ship it again");
                out.emit("release-ship", &doc(false, None, &moved, Some(&report)));
                return Ok(report.exit_code());
            }
            out.say(format!(
                "moved {} issue(s) to {}",
                changed.len(),
                target.name
            ));
            pushed = Some(report);
        }
    }

    let shipped = crate::api::block_on(client.update_version(&version.id, &update))?;
    remember(&ctx, &project, &shipped, out);
    out.say(version_row(&shipped, shipped.name.len()));
    out.emit(
        "release-ship",
        &doc(true, Some(&shipped), &moved, pushed.as_ref()),
    );
    Ok(ExitCode::Success)
}

/// Say which issues a release ships unfinished.
fn report_unfinished(unfinished: &[Incomplete], moved_to: Option<&Version>, out: Out) {
    if unfinished.is_empty() {
        return;
    }
    let keys: Vec<&str> = unfinished.iter().map(|i| i.key.as_str()).collect();
    match moved_to {
        Some(target) => out.say(format!(
            "{} issue(s) are not done and go to {}: {}",
            keys.len(),
            target.name,
            keys.join(", ")
        )),
        None => {
            out.warn(format!(
                "{} issue(s) are not done and stay in the released version: {}",
                keys.len(),
                keys.join(", ")
            ));
            out.hint("--move-unfixed-to \"the next version\" moves them on instead");
        }
    }
}

/// The version a `--move-unfixed-to` names, checked for the ways it can be the
/// wrong one.
fn carry_target(
    project: &str,
    versions: &[Version],
    wanted: &str,
    shipping: &Version,
) -> Result<Version> {
    let target = pick(project, versions, wanted)?;
    if target.id == shipping.id {
        return Err(JiredError::usage(format!(
            "{} cannot move its own issues to itself",
            shipping.name
        ))
        .with_hint("name the version they should go to, or leave the flag off"));
    }
    if target.released || target.archived {
        return Err(already(
            &target,
            &format!("is {}, so nothing can be moved into it", target.state()),
        )
        .with_hint("move them to a version that has not shipped yet"));
    }
    Ok(target)
}

// ---- shared --------------------------------------------------------------

/// The project these commands act on: `--project`, else the workspace's.
fn project(ctx: &mut Context) -> Result<String> {
    let project: Option<String> = ctx.resolver.get_quiet("workspace.project")?;
    project
        .map(|p| p.trim().to_ascii_uppercase())
        .filter(|p| !p.is_empty())
        .ok_or_else(|| {
            JiredError::missing_value("no project to manage versions in")
                .with_hint("pass --project KEY, or set workspace.project")
        })
}

/// The project's versions as the server holds them now, written into the
/// cache on the way through.
///
/// The cache is best-effort, as everywhere: the listing is the answer, and a
/// cache that would not take it is a warning rather than a failed command.
fn listing(ctx: &Context, client: &dyn JiraApi, project: &str, out: Out) -> Result<Vec<Version>> {
    let versions = crate::api::block_on(client.versions(project))?;
    let saved = crate::store::state::StateStore::open(&ctx.workspace)
        .and_then(|state| state.replace_versions(project, &versions));
    if let Err(e) = saved {
        out.warn(format!(
            "the local version cache could not be updated ({e}) — push will check \
             `fix_versions:` against what it held before"
        ));
    }
    Ok(versions)
}

/// The version a name or an id points at, out of a project's listing.
///
/// An id first, because it is unambiguous and is what `--json` hands a script;
/// then the name, without regard to case, because that is how Jira matches it.
fn pick(project: &str, versions: &[Version], wanted: &str) -> Result<Version> {
    let wanted = wanted.trim();
    if let Some(hit) = versions
        .iter()
        .find(|v| v.id == wanted)
        .or_else(|| versions.iter().find(|v| v.name == wanted))
        .or_else(|| {
            versions
                .iter()
                .find(|v| v.name.eq_ignore_ascii_case(wanted))
        })
    {
        return Ok(hit.clone());
    }
    let open: Vec<String> = versions
        .iter()
        .filter(|v| !v.released && !v.archived)
        .map(|v| format!("{} ({})", v.name, v.id))
        .collect();
    Err(
        JiredError::usage(format!("`{wanted}` is not a version of {project}"))
            .with_detail("version", wanted.to_string())
            .with_detail("known", serde_json::json!(open))
            .with_hint(if open.is_empty() {
                format!("`jired releases --all` lists every version {project} has")
            } else {
                format!("one of: {}", open.join(", "))
            }),
    )
}

/// Put a version one of these commands has just written into the cache.
fn remember(ctx: &Context, project: &str, version: &Version, out: Out) {
    let saved = crate::store::state::StateStore::open(&ctx.workspace)
        .and_then(|state| state.save_versions(project, std::slice::from_ref(version)));
    if let Err(e) = saved {
        // The server has the change either way, so this is a stale cache and
        // not a failed command; saying which is the difference between running
        // `fetch` and running the whole thing again.
        out.warn(format!(
            "{}: the change went through, but the local version cache could not be \
             updated ({e}) — run `jired fetch` to refresh it",
            version.name
        ));
    }
}

fn update_request(
    client: &dyn JiraApi,
    version: &Version,
    update: &VersionUpdate,
) -> serde_json::Value {
    serde_json::json!({
        "method": "PUT",
        "path": format!("{}/version/{}", api_prefix(client), version.id),
        "body": update.to_body(),
    })
}

fn api_prefix(client: &dyn JiraApi) -> &'static str {
    client.deployment().api_prefix()
}

fn say_request(request: &serde_json::Value, out: Out) -> Result<()> {
    out.say(format!(
        "{} {}",
        request["method"].as_str().unwrap_or_default(),
        request["path"].as_str().unwrap_or_default()
    ));
    out.say(serde_json::to_string_pretty(&request["body"])?);
    Ok(())
}

fn changed_keys(staged: &[Staged]) -> Vec<String> {
    staged
        .iter()
        .filter(|s| s.changed)
        .map(|s| s.key.clone())
        .collect()
}

/// A version as the two things another command needs to address it by.
fn brief(version: &Version) -> serde_json::Value {
    serde_json::json!({ "id": version.id, "name": version.name })
}

/// "`0.4.0` is already released" — one shape for every state refusal, so the id
/// is always there for whoever is reading the JSON.
fn already(version: &Version, what: &str) -> JiredError {
    JiredError::usage(format!("{} {what}", version.name))
        .with_detail("version", version.name.clone())
        .with_detail("version_id", version.id.clone())
        .with_detail("state", version.state())
}

fn checked_name(name: &str) -> Result<String> {
    let name = name.trim();
    if name.is_empty() {
        return Err(JiredError::missing_value("a version name is required")
            .with_hint("`jired release create 0.5.0`"));
    }
    Ok(name.to_string())
}

fn checked_day(text: Option<&str>, flag: &str) -> Result<Option<NaiveDate>> {
    let Some(text) = text else { return Ok(None) };
    crate::api::model::parse_day(text).map(Some).ok_or_else(|| {
        JiredError::usage(format!("`{text}` is not a date"))
            .with_detail("flag", flag.to_string())
            .with_hint("write it as `2026-10-29`: a version's dates are days, with no time")
    })
}

/// Jira refuses a version due out before it starts, with a message that does
/// not say which version.
fn check_order(start: Option<NaiveDate>, release: Option<NaiveDate>) -> Result<()> {
    match (start, release) {
        (Some(start), Some(release)) if release < start => {
            Err(JiredError::usage("the version is due out before it starts")
                .with_detail("start", day(&start))
                .with_detail("release_date", day(&release)))
        }
        _ => Ok(()),
    }
}

fn today() -> NaiveDate {
    chrono::Local::now().date_naive()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(text: &str) -> Option<NaiveDate> {
        crate::api::model::parse_day(text)
    }

    fn version(id: &str, name: &str, released: bool, archived: bool) -> Version {
        Version {
            id: id.to_string(),
            name: name.to_string(),
            project_id: Some("10000".to_string()),
            description: None,
            released,
            archived,
            start_date: None,
            release_date: None,
        }
    }

    /// The list is the issue's own, so adding never duplicates and moving
    /// touches only the version being released.
    #[test]
    fn adding_and_moving_a_version_keep_the_rest_of_the_list() {
        let mut list = vec!["0.4.0".to_string()];
        assert!(add_version(&mut list, "0.5.0"));
        assert!(!add_version(&mut list, "0.5.0"));
        assert_eq!(list, ["0.4.0", "0.5.0"]);

        // A fix that also went into a maintenance line keeps that line.
        let mut list = vec!["0.3.2".to_string(), "0.4.0".to_string()];
        assert!(move_version(&mut list, "0.4.0", "0.5.0"));
        assert_eq!(list, ["0.3.2", "0.5.0"]);

        // Already in the target: the released one goes and nothing doubles.
        let mut list = vec!["0.4.0".to_string(), "0.5.0".to_string()];
        assert!(move_version(&mut list, "0.4.0", "0.5.0"));
        assert_eq!(list, ["0.5.0"]);

        // Not in the version at all is not a change.
        let mut list = vec!["0.3.2".to_string()];
        assert!(!move_version(&mut list, "0.4.0", "0.5.0"));
    }

    #[test]
    fn a_version_is_picked_by_id_then_by_name_and_a_miss_names_the_open_ones() {
        let versions = [
            version("10", "0.4.0", true, false),
            version("11", "0.5.0", false, false),
            version("12", "0.6.0", false, true),
        ];
        assert_eq!(pick("TSK", &versions, "11").unwrap().name, "0.5.0");
        assert_eq!(pick("TSK", &versions, " 0.4.0 ").unwrap().id, "10");

        let err = pick("TSK", &versions, "0.9.0").unwrap_err();
        assert_eq!(err.exit_code().as_i32(), 2);
        // Only what could still be planned into is offered.
        assert_eq!(err.hint.as_deref(), Some("one of: 0.5.0 (11)"));
    }

    #[test]
    fn a_version_due_before_it_starts_is_refused() {
        assert!(check_order(d("2026-10-15"), d("2026-10-29")).is_ok());
        // One day long is a real release: the dates are days, not instants.
        assert!(check_order(d("2026-10-15"), d("2026-10-15")).is_ok());
        assert!(check_order(d("2026-10-29"), d("2026-10-15")).is_err());
        assert!(check_order(None, d("2026-10-15")).is_ok());
        assert!(checked_day(Some("next tuesday"), "--start").is_err());
        assert_eq!(
            checked_day(Some("2026-10-29"), "--start").unwrap(),
            d("2026-10-29")
        );
    }
}
