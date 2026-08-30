//! `jired push` [M2.8, M2.9] — apply the working-copy delta to Jira.
//!
//! Two invariants shape this module:
//!
//! * base advances only for an issue whose every operation the server
//!   confirmed, so `state.db` can never claim a push that did not happen;
//! * a lossy region is rebuilt from its stored original, never from the file,
//!   so untouched read-only content goes back byte-identical.

use crate::api::{JiraApi, Markup, RemoteIssue, RichText, WorklogInput};
use crate::cli::cmd::push::PushArgs;
use crate::cli::context::Context;
use crate::convert;
use crate::doc::{CommentId, IssueDoc, WorklogEntry, WorklogId};
use crate::error::{ExitCode, JiredError, Result};
use crate::json::Out;
use crate::sync::render::{render_issue, RenderContext};
use crate::sync::screen::Screen;
use crate::sync::Engine;
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::BTreeMap;

/// The outcome in one line, before the array anybody would otherwise read to
/// work it out. A caller checking whether a push went through should not have
/// to walk every operation of every issue to find out.
#[derive(Debug, Clone, Default, Serialize)]
pub struct PushSummary {
    pub ok: usize,
    pub failed: usize,
    /// Issues the server had moved on from; they were not sent.
    pub stale: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PushReport {
    /// First, so it is the first thing read.
    pub summary: PushSummary,
    pub dry_run: bool,
    pub results: Vec<IssueResult>,
    pub stale: Vec<String>,
    /// Rebuilt when a push moved an issue somewhere a grouping folder shows.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub groups: Option<crate::sync::group::GroupReport>,
    /// Things push deliberately did not do, and why.
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct IssueResult {
    pub key: String,
    pub ok: bool,
    pub new_key: Option<String>,
    pub error: Option<String>,
    pub error_kind: Option<String>,
    /// What to do about `error`; dropping it left the user with a bare failure.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
    pub ops: Vec<OpReport>,
}

impl IssueResult {
    /// A failure for one issue, carrying everything the error knew.
    ///
    /// Built rather than propagated: one issue that cannot be pushed must not
    /// discard the report of the issues that already went out.
    fn failed(key: &str, e: &JiredError) -> Self {
        Self {
            key: key.to_string(),
            ok: false,
            new_key: None,
            error: Some(e.message.clone()),
            error_kind: Some(e.kind.as_str().to_string()),
            hint: e.hint.clone(),
            ops: vec![],
        }
    }

    fn ok(key: &str, ops: Vec<OpReport>) -> Self {
        Self {
            key: key.to_string(),
            ok: true,
            new_key: None,
            error: None,
            error_kind: None,
            hint: None,
            ops,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct OpReport {
    pub kind: &'static str,
    pub status: &'static str,
    pub summary: Option<String>,
    pub request: Option<Value>,
    pub error: Option<String>,
}

impl PushReport {
    pub fn exit_code(&self) -> ExitCode {
        if !self.stale.is_empty() {
            return ExitCode::Conflict;
        }
        let kinds: std::collections::BTreeSet<&str> = self
            .results
            .iter()
            .filter(|r| !r.ok)
            .filter_map(|r| r.error_kind.as_deref())
            .collect();
        match kinds.len() {
            0 => ExitCode::Success,
            // One kind of failure gets its own exit code: an auth problem and a
            // lossy block are different situations, and collapsing both to 1
            // defeats the exit-code contract agents rely on.
            1 => match *kinds.iter().next().expect("checked") {
                "auth" => ExitCode::Auth,
                "network" => ExitCode::Network,
                "lossy" => ExitCode::LossyBlocked,
                "usage" => ExitCode::Usage,
                "conflict" => ExitCode::Conflict,
                "locked" => ExitCode::Locked,
                "corrupt_state" => ExitCode::CorruptState,
                "unsupported_server" => ExitCode::UnsupportedServer,
                "missing_value" => ExitCode::MissingValue,
                _ => ExitCode::Generic,
            },
            // Mixed causes have no single right code.
            _ => ExitCode::Generic,
        }
    }

    /// The lines `--dry-run` and the pre-push confirmation share.
    pub fn plan_lines(&self, include_requests: bool) -> Vec<String> {
        use crate::style::{self, CYAN, DIM, GREEN, RED};
        let mut lines = Vec::new();
        for r in &self.results {
            let head = match (&r.new_key, r.ok) {
                (Some(k), _) => format!("{} → {k}", r.key),
                (None, true) => r.key.clone(),
                (None, false) => format!("{} (failed)", r.key),
            };
            lines.push(format!("── {}", style::out(CYAN, head)));
            for op in &r.ops {
                let mark = match op.status {
                    "planned" => String::new(),
                    "done" => style::out(GREEN, "  ✓"),
                    "skipped" => style::out(DIM, "  (skipped)"),
                    _ => style::out(RED, "  ✗"),
                };
                lines.push(format!(
                    "   {:<20} {}{}",
                    op.kind,
                    op.summary.as_deref().unwrap_or(""),
                    mark
                ));
                if include_requests {
                    if let Some(req) = &op.request {
                        lines.push(format!("     {}", style::out(DIM, compact(req))));
                    }
                }
                if let Some(e) = &op.error {
                    lines.push(format!("     {} {e}", style::out(RED, "error:")));
                }
            }
            if let Some(e) = &r.error {
                lines.push(format!("   {} {e}", style::out(RED, "error:")));
            }
            // The hint is the actionable half of a failure; the report used to
            // keep only the message and throw this away.
            if let Some(h) = &r.hint {
                lines.push(format!("   {} {h}", style::out(DIM, "hint:")));
            }
        }
        lines
    }

    /// Total operations across every issue, for the confirmation prompt.
    pub fn op_count(&self) -> usize {
        self.results.iter().map(|r| r.ops.len()).sum()
    }

    pub fn render(&self, out: Out) {
        if self.results.is_empty() {
            out.say("nothing to push");
            out.hint("use \"jired status\" to see what jired thinks has changed");
            for w in &self.warnings {
                out.warn(w);
            }
            return;
        }
        for line in self.plan_lines(self.dry_run) {
            out.say(line);
        }
        if !self.stale.is_empty() {
            out.say(format!(
                "\n{} issue(s) changed on the server since your last sync: {}",
                self.stale.len(),
                self.stale.join(", ")
            ));
            out.hint("use \"jired pull\" first, or \"jired push --force\" to overwrite");
        }
        for w in &self.warnings {
            out.warn(w);
        }
        if self.dry_run {
            out.say("\ndry run — nothing was sent");
            return;
        }

        // A count is the one thing a partial failure has to say plainly.
        let failed = self.results.iter().filter(|r| !r.ok).count();
        let total = self.results.len();
        out.say(match failed {
            0 => format!("\npushed {total} issue(s)"),
            n if n == total => format!("\n{n} issue(s) failed, none pushed"),
            n => format!("\npushed {} of {total} issue(s), {n} failed", total - n),
        });
        if failed > 0 {
            out.hint("fix the causes above, then \"jired push --resume\"");
        }
    }
}

/// Truncate a request body for display.
///
/// Counting characters rather than bytes: a summary with an accent in it is the
/// common case, and `&s[..240]` would panic on the byte boundary.
fn compact(v: &Value) -> String {
    let s = v.to_string();
    if s.chars().count() > 240 {
        format!("{}…", s.chars().take(240).collect::<String>())
    } else {
        s
    }
}

/// One planned API call.
struct Op {
    kind: &'static str,
    summary: String,
    request: Value,
    action: Action,
}

enum Action {
    UpdateFields(Value),
    UpdateDescription(RichText),
    Transition {
        to_status: String,
    },
    CreateComment(RichText),
    UpdateComment {
        id: String,
        body: RichText,
    },
    DeleteComment {
        id: String,
    },
    CreateWorklog(WorklogInput),
    UpdateWorklog {
        id: String,
        worklog: WorklogInput,
    },
    DeleteWorklog {
        id: String,
    },
    UploadAttachment {
        filename: String,
        bytes: Vec<u8>,
    },
    /// Move the issue into a sprint, or out of every sprint for the backlog.
    MoveSprint {
        /// The sprint's id, or `None` for the backlog.
        sprint: Option<String>,
        /// The sprint's name, or `None` for the backlog: what the re-read after
        /// the push has to show for the move to have happened. Ids do not
        /// survive into a rendered issue — names are what the file carries — so
        /// the check is made on the name.
        to: Option<String>,
        /// Where it was. A move to the backlog is checked against this rather
        /// than against an empty list: Jira keeps the sprints an issue was in
        /// when they closed, so "in no sprint at all" is not what leaving one
        /// looks like.
        from: Option<String>,
    },
    /// Put a sub-task under a different parent. Its own operation because Data
    /// Center has no field update that does it; see
    /// [`crate::api::datacenter::wizard::move_subtask_parent`].
    /// Set or clear the assignee, through the route Jira gives it.
    Assign {
        /// The user id, or `None` for nobody. Kept so the re-read can be asked
        /// whether the server actually did it.
        to: Option<String>,
    },
    MoveParent {
        /// The parent it must be under when the issue is read back.
        to: String,
    },
    /// Transition a *different* issue: ticking a subtask's box under `## Children`.
    TransitionOther {
        key: String,
        to_status: String,
    },
    /// Link this issue to another. The type's registered name, because that is
    /// what the create call takes; which side this issue is on, because the
    /// call does not know whose file the line was written in.
    CreateLink {
        type_name: String,
        other_key: String,
        /// Whether this issue is the link's *inward* issue — which, in Jira's
        /// backwards convention, is the side that bears the type's *outward*
        /// description: `blocks OTHER` puts this issue inward.
        self_is_inward: bool,
    },
    /// Unlink, by the link's own id out of the base snapshot — the only place
    /// it exists, since the file spells a link as a direction and a key.
    DeleteLink {
        id: String,
    },
}

pub fn run(ctx: &mut Context, args: &PushArgs) -> Result<PushReport> {
    let engine = Engine::open(ctx)?;
    let client = engine.client(ctx)?;
    let markup = engine.markup();
    let fields = engine.field_map()?;
    // Custom fields need their schema, not just their name: it is what says
    // whether a value goes out bare, wrapped in `{"value": …}`, or as a list.
    let field_meta = engine.state.field_meta_by_id()?;
    // Sprint names come from the board cache; moving between sprints needs the
    // id behind the name the file carries.
    let sprints = engine.state.sprints()?;
    let base_url = engine.base_url().to_string();
    let files = engine.projector.scan()?;
    let max_upload = ctx
        .resolver
        .get_u64("attachments.max_upload")?
        .unwrap_or(50 * 1024 * 1024);
    let done_transition = ctx.resolver.get_value("children.done_transition")?;
    let mut link_types = LinkTypes {
        client: client.as_ref(),
        cached: None,
    };
    let mut wanted: Vec<String> = args
        .issues
        .iter()
        .map(|k| crate::project::naming::key_from_input(k))
        .collect();

    // `--resume` narrows the run to issues an interrupted push left unfinished.
    // Replaying is safe because base was never advanced for them: the working
    // copy still holds the edits, so re-planning reproduces the same work, and
    // the stale journal rows are cleared first so they cannot accumulate.
    if args.resume {
        let unfinished: Vec<String> = engine
            .state
            .pending_ops()?
            .into_iter()
            .map(|op| op.issue_key)
            .collect();
        if unfinished.is_empty() {
            return Ok(PushReport {
                summary: PushSummary::default(),
                dry_run: args.dry_run,
                results: Vec::new(),
                stale: Vec::new(),
                groups: None,
                warnings: Vec::new(),
            });
        }
        for key in &unfinished {
            if !args.dry_run {
                engine.state.clear_ops(key)?;
            }
            if !wanted.contains(key) {
                wanted.push(key.clone());
            }
        }
    }

    let mut report = PushReport {
        summary: PushSummary::default(),
        dry_run: args.dry_run,
        results: Vec::new(),
        stale: Vec::new(),
        groups: None,
        warnings: Vec::new(),
    };

    // Locally created issues go first: an existing issue may name one as its
    // parent or epic, and that reference has to resolve to a real key.
    let mut rekeyed: Vec<(String, String)> = Vec::new();
    for row in engine.state.issues()? {
        if !row.key.starts_with("NEW-") {
            continue;
        }
        if !wanted.is_empty() && !wanted.contains(&row.key) {
            continue;
        }
        let Some(file) = files.get(&row.key) else {
            continue;
        };
        match create_issue(
            ctx,
            &engine,
            client.as_ref(),
            &row.key,
            &file.relative,
            args,
            &mut link_types,
        ) {
            Ok(result) => {
                if let Some(new_key) = &result.new_key {
                    rekeyed.push((row.key.clone(), new_key.clone()));
                }
                report.results.push(result);
            }
            Err(e) => report.results.push(IssueResult::failed(&row.key, &e)),
        }
    }

    // Re-scan: creating an issue renames its file, so the old paths are stale.
    let files = if rekeyed.is_empty() {
        files
    } else {
        engine.projector.scan()?
    };
    for (from, to) in &rekeyed {
        if let Some(pos) = wanted.iter().position(|k| k == from) {
            wanted[pos] = to.clone();
        }
    }

    // A card dragged between board columns stages a transition (decision D4).
    let (moves, move_warnings) =
        crate::sync::group::detect_moves(&engine, engine.projector.root())?;
    let board_moves: std::collections::BTreeMap<String, String> =
        moves.into_iter().map(|m| (m.key, m.to_status)).collect();
    report.warnings.extend(move_warnings);

    for row in engine.state.issues()? {
        if row.key.starts_with("NEW-") {
            continue;
        }
        if !wanted.is_empty() && !wanted.contains(&row.key) {
            continue;
        }
        let (Some(base), Some(file)) = (&row.base, files.get(&row.key)) else {
            continue;
        };
        let Some(hash) = engine.projector.hash_of(&file.relative) else {
            continue;
        };
        // A staged attachment does not change the `.md` file, so the hash alone
        // would make the issue look clean and the upload would never be sent.
        let attachment_dir = engine.projector.attachment_dir(&file.relative);
        let has_uploads =
            !crate::sync::attachments::pending_uploads(&engine.state, &row.key, &attachment_dir)?
                .is_empty();
        let moved = board_moves.get(&row.key);
        if hash == base.md_hash && !has_uploads && moved.is_none() {
            continue;
        }

        match engine.projector.read_raw(&file.relative) {
            Ok(text) => {
                for heading in crate::doc::misplaced_comment_headings(&text) {
                    ctx.out.warn(format!(
                        "{}: `{heading}` is not under `## Comments` and will not be pushed",
                        file.relative
                    ));
                }
            }
            Err(e) => {
                report.results.push(IssueResult::failed(&row.key, &e));
                continue;
            }
        }

        // Everything from here to the API call can fail for *this* issue. Each
        // failure becomes a result and the loop goes on: `commands.md` promises
        // that one bad issue does not stop the others, and a `?` here would
        // throw away the report of everything already pushed.
        let prepared = (|| -> Result<Vec<Op>> {
            let mut working = engine.projector.read(&file.relative)?;
            let base_issue = crate::api::normalize::issue_from_json(&base.raw, markup, &fields)?;
            let base_doc = render_issue(
                &base_issue,
                &RenderContext {
                    markup,
                    base_url: &base_url,
                    ignored_custom: &fields.ignored,
                },
            )?
            .doc;

            // A read-only region that was edited must stop this issue before any
            // call is made, or the original markup would be silently discarded.
            //
            // The description is not the only place one can live: a comment or
            // a worklog note carries its own, and an edit inside one of those
            // went out as a success having sent the original text back, because
            // the payload is rebuilt from the store either way.
            let touched = edited_regions(&engine.state, &row.key, &base_doc, &working)?;
            let edited: Vec<String> = touched.iter().map(|(id, _)| id.clone()).collect();
            let labels: Vec<String> = touched.into_iter().map(|(_, label)| label).collect();

            if !edited.is_empty() {
                if !args.allow_lossy {
                    return Err(JiredError::lossy(format!(
                        "{}: a read-only region was edited and cannot be converted back",
                        row.key
                    ))
                    .with_detail("issue", row.key.clone())
                    .with_detail("regions", json!(edited))
                    .with_detail("node_types", json!(labels))
                    .with_hint(
                        "restore the region, or pass --allow-lossy to replace it with plain \
                         Markdown",
                    ));
                }
                // `--allow-lossy` says the region goes as plain Markdown, so the
                // markers come out and the file's own text is what is sent.
                // Left in, they would still resolve against the store and the
                // edit the flag was reached for would be dropped.
                unwrap_edited_regions(&mut working, &edited);
            }

            let raw = engine.state.raw_blocks(&row.key, "description")?;
            plan(
                &base_doc,
                &working,
                &base.raw,
                &field_meta,
                &fields.ignored,
                fields.story_points.as_deref(),
                &sprints,
                markup,
                &raw,
                &engine,
                args,
                &attachment_dir,
                max_upload,
                done_transition.as_deref(),
                moved.map(String::as_str),
                &mut link_types,
                &mut report.warnings,
            )
        })();

        let ops = match prepared {
            Ok(ops) => ops,
            Err(e) => {
                report.results.push(IssueResult::failed(&row.key, &e));
                continue;
            }
        };
        if ops.is_empty() {
            continue;
        }

        // What is about to go out in a `PUT /issue` — the field payload, and
        // the description, which travels in one too. Asked of the screen before
        // it is sent, so a field the screen does not carry is named here rather
        // than in a 400 that arrives after everything else in the push.
        let edited: Vec<&str> = ops
            .iter()
            .flat_map(|op| match &op.action {
                Action::UpdateFields(payload) => payload
                    .as_object()
                    .map(|o| o.keys().map(String::as_str).collect::<Vec<_>>())
                    .unwrap_or_default(),
                Action::UpdateDescription(_) => vec!["description"],
                _ => Vec::new(),
            })
            .collect();
        if !edited.is_empty() {
            let meta = crate::api::block_on(client.edit_meta(&row.key));
            if let Err(e) = check_screen(&engine, &row.key, Screen::Edit, meta, edited, &field_meta)
            {
                report.results.push(IssueResult::failed(&row.key, &e));
                continue;
            }
        }

        if args.dry_run {
            report.results.push(IssueResult::ok(
                &row.key,
                ops.iter()
                    .map(|o| OpReport {
                        kind: o.kind,
                        status: "planned",
                        summary: Some(o.summary.clone()),
                        request: Some(o.request.clone()),
                        error: None,
                    })
                    .collect(),
            ));
            continue;
        }

        match execute(
            ctx,
            &engine,
            client.as_ref(),
            &row.key,
            &base.updated,
            ops,
            args,
            &file.relative,
            markup,
            &base_url,
        ) {
            Ok(result) => {
                if result.error_kind.as_deref() == Some("conflict") {
                    report.stale.push(row.key.clone());
                }
                report.results.push(result);
            }
            Err(e) => report.results.push(IssueResult::failed(&row.key, &e)),
        }
    }

    report.results.sort_by(|a, b| a.key.cmp(&b.key));

    // Before the regroup below, so an issue that only went out on the retry
    // still moves the folders it belongs in. `engine` owns its handles rather
    // than borrowing `ctx`, so the retry can open its own alongside it.
    if args.refresh_stale && !args.dry_run && !report.stale.is_empty() {
        retry_stale(ctx, &mut report, args)?;
    }

    report.summary = PushSummary {
        ok: report.results.iter().filter(|r| r.ok).count(),
        failed: report.results.iter().filter(|r| !r.ok).count(),
        stale: report.stale.clone(),
    };

    // Work an earlier push left unfinished, for an issue this push did not
    // touch: `failed: 0` above is true of what was attempted now, and a caller
    // reading only this report would believe the queue is empty while
    // `--resume` still has something to retry. An issue in `results` already
    // tells its own story.
    for op in engine.state.pending_ops()? {
        if report.results.iter().any(|r| r.key == op.issue_key) {
            continue;
        }
        report.warnings.push(format!(
            "{}: a {} operation from an earlier push is still unfinished{} — \
             `jired push --resume` retries it",
            op.issue_key,
            op.kind,
            op.error
                .as_ref()
                .map(|e| format!(" ({e})"))
                .unwrap_or_default(),
        ));
    }

    // Grouping folders are views over base, and a push has just moved base: the
    // sprint an issue is in, the column it sits in, the parent it hangs from.
    // Rebuilding them here is what makes `sprints/<the sprint just created>`
    // appear from the push that filled it, rather than only from the next pull
    // that happens to write a file — which, after a push, is usually none.
    if !args.dry_run
        && report
            .results
            .iter()
            .any(|r| r.ops.iter().any(|o| o.status == "done" && regroups(o.kind)))
    {
        let options = crate::sync::group::GroupOptions::from_context(ctx)?;
        match crate::sync::group::regroup(&engine, engine.projector.root(), &options) {
            Ok(groups) => {
                report.warnings.extend(groups.warnings.iter().cloned());
                report.groups = Some(groups);
            }
            Err(e) => report.warnings.push(format!(
                "cannot rebuild grouping folders: {} — `jired doctor --regroup` retries it",
                e.message
            )),
        }
    }
    Ok(report)
}

/// Every read-only region the user edited, by id and by what it is on the
/// server, across every container the issue carries.
///
/// `push` refuses the issue over this and `status` reports it, and the two have
/// to answer the same question or the file is clean right up until the push
/// that fails.
///
/// The description is checked always; a comment or a worklog only when its text
/// changed. A note nobody touched cannot have had its regions edited, and
/// checking it anyway would let some later difference in how it renders block a
/// push that has nothing to do with it.
pub fn edited_regions(
    state: &crate::store::state::StateStore,
    key: &str,
    base: &IssueDoc,
    working: &IssueDoc,
) -> Result<Vec<(String, String)>> {
    let mut containers: Vec<(String, &str)> =
        vec![("description".into(), working.description.as_str())];
    for c in &working.comments {
        if let CommentId::Existing(id) = &c.id {
            if base.comment(id).is_some_and(|b| b.body != c.body) {
                containers.push((format!("comment:{id}"), c.body.as_str()));
            }
        }
    }
    for w in &working.worklogs {
        if let WorklogId::Existing(id) = &w.id {
            if base.worklog(id).is_some_and(|b| b.body != w.body) {
                containers.push((format!("worklog:{id}"), w.body.as_str()));
            }
        }
    }

    let mut out = Vec::new();
    for (container, text) in containers {
        let raw = state.raw_blocks(key, &container)?;
        for id in convert::edited_regions(text, &raw) {
            let label = raw
                .iter()
                .find(|b| b.id == id)
                .map(|b| b.label.clone())
                .unwrap_or_default();
            out.push((id, label));
        }
    }
    Ok(out)
}

/// Take the markers of the edited regions out of every text the issue carries,
/// so what the file says is what gets converted.
///
/// Applied everywhere rather than only where each id was found: an id that is
/// not in a text is not in it, and looking is cheaper than tracking which
/// container each one came from.
fn unwrap_edited_regions(working: &mut IssueDoc, edited: &[String]) {
    working.description = convert::markdown::unwrap_regions(&working.description, edited);
    for c in &mut working.comments {
        c.body = convert::markdown::unwrap_regions(&c.body, edited);
    }
    for w in &mut working.worklogs {
        w.body = convert::markdown::unwrap_regions(&w.body, edited);
    }
}

/// Whether an applied op leaves the grouping folders describing where the issue
/// used to be. Everything else a push sends — a comment, a worklog, an upload —
/// changes the issue without changing where it is filed, and rebuilding the
/// tree for those would only risk discarding a card someone has dragged since.
fn regroups(kind: &str) -> bool {
    matches!(
        kind,
        "create_issue" | "move_sprint" | "transition" | "update_fields" | "rename_file"
    )
}

/// Create a locally authored `NEW-*` issue on the server.
///
/// On success the temporary key is replaced everywhere — state rows, the file
/// name and the attachment folder — so nothing keeps referring to a key that
/// never existed on the server.
/// Refuse the fields a screen does not carry, if the server described one.
///
/// Data Center only, for now. Cloud has both endpoints and both clients
/// implement them, but nothing in the tree has a recorded Cloud screen to check
/// the reading against — and a Cloud sub-task's re-parenting is an ordinary
/// `parent` field edit, so a company-managed screen that omitted `parent` while
/// the edit worked would turn a push that succeeds today into a refusal. A
/// false refusal is worse than the 400 it pre-empts. Lifting the gate is the
/// first line of this function.
fn check_screen(
    engine: &Engine,
    key: &str,
    screen: Screen,
    meta: Result<Option<crate::api::ScreenMeta>>,
    sent: Vec<&str>,
    field_meta: &BTreeMap<String, crate::api::FieldMeta>,
) -> Result<()> {
    if engine.account.deployment != crate::api::Deployment::DataCenter {
        return Ok(());
    }
    let meta = match meta {
        Ok(Some(meta)) => meta,
        // A server that describes no screen: unknown is not "no fields".
        Ok(None) => return Ok(()),
        // And a metadata call that failed is not a reason to fail the push —
        // the server's own answer to the write is still the backstop, and a
        // lookup must never turn a working push into a failing one.
        Err(e) => {
            tracing::warn!("{key}: cannot read the {screen:?} screen: {}", e.message);
            return Ok(());
        }
    };
    let names: BTreeMap<String, String> = field_meta
        .iter()
        .map(|(id, f)| (id.clone(), f.name.clone()))
        .collect();
    let refused = crate::sync::screen::refused_by_screen(&meta, sent.into_iter(), &names);
    match refused.is_empty() {
        true => Ok(()),
        false => Err(crate::sync::screen::refusal(key, screen, &refused)),
    }
}

fn create_issue(
    ctx: &mut Context,
    engine: &Engine,
    client: &dyn JiraApi,
    temp_key: &str,
    file: &str,
    args: &PushArgs,
    link_types: &mut LinkTypes<'_>,
) -> Result<IssueResult> {
    let doc = engine.projector.read(file)?;
    let project = engine
        .state
        .new_issue_project(temp_key)?
        .or_else(|| ctx.resolver.get_quiet("workspace.project").ok().flatten())
        .ok_or_else(|| {
            JiredError::missing_value(format!("{temp_key} has no project"))
                .with_hint("set workspace.project, or recreate the issue with --project")
        })?;

    let markup = engine.markup();
    let mut fields = serde_json::Map::new();
    fields.insert("project".into(), json!({ "key": project }));
    fields.insert("summary".into(), json!(doc.frontmatter.summary));
    fields.insert(
        "issuetype".into(),
        json!({ "name": if doc.frontmatter.issue_type.is_empty() {
            "Task".to_string()
        } else {
            doc.frontmatter.issue_type.clone()
        }}),
    );
    if !doc.description.trim().is_empty() {
        let rich = convert::from_markdown(&doc.description, markup, &[]);
        fields.insert("description".into(), rich.to_field_value());
    }

    // Everything else the file already says, through the same mapping an update
    // uses. Building this list by hand is what lost `assignee`: it was handled
    // for updates and simply absent here, so a create sent four fields, dropped
    // the rest, and reported success.
    let field_map = engine.field_map()?;
    for name in crate::doc::frontmatter::EDITABLE {
        // Summary and type are already in, and the create call decides the
        // project rather than the file.
        if matches!(*name, "summary" | "type") {
            continue;
        }
        match issue_field(
            name,
            &doc.frontmatter,
            engine.account.deployment,
            field_map.story_points.as_deref(),
        ) {
            Mapped::Send(id, value) if worth_creating(&value) => {
                fields.insert(id, value);
            }
            Mapped::Send(_, _) | Mapped::Elsewhere => {}
            // Only worth saying when the file actually set the thing.
            Mapped::Refused(why) => {
                if doc
                    .frontmatter
                    .field(name)
                    .is_some_and(|v| worth_creating(&v))
                {
                    ctx.out
                        .warn(format!("{temp_key}: {why}; the value was not sent"));
                }
            }
        }
    }

    // A locally created issue may already carry custom fields; there is no
    // previous value to learn the shape from, so the schema decides.
    let field_meta = engine.state.field_meta_by_id()?;
    // Sprint names come from the board cache; moving between sprints needs the
    // id behind the name the file carries.
    let sprints = engine.state.sprints()?;
    for (id, value) in &doc.frontmatter.custom {
        let payload =
            crate::api::custom::expand(value, None, field_meta.get(id), engine.account.deployment);
        if !payload.is_null() {
            fields.insert(id.clone(), payload);
        }
    }

    let payload = Value::Object(fields);
    let request = json!({"method": "POST", "path": "/issue", "body": {"fields": payload}});

    // Before the create rather than after its 400: a field the create screen
    // does not carry is refused here, naming the field, where `--dry-run` shows
    // it and nothing has been sent.
    let issue_type = payload
        .get("issuetype")
        .and_then(|t| t.get("name"))
        .and_then(Value::as_str)
        .unwrap_or("Task")
        .to_string();
    check_screen(
        engine,
        temp_key,
        Screen::Create,
        crate::api::block_on(client.create_meta(&project, &issue_type)),
        payload
            .as_object()
            .map(|o| {
                o.keys()
                    // The two that choose the screen rather than sit on it.
                    .filter(|k| k.as_str() != "project" && k.as_str() != "issuetype")
                    .map(String::as_str)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default(),
        &field_meta,
    )?;

    // What the create call cannot carry, and so happens after it. Planned here
    // rather than after the write so `--dry-run` can show them: a plan that
    // listed only `create_issue` was how a move or a transition went unnoticed
    // until the file came back without it.
    let (follow_ups, follow_up_warnings) = follow_ups_after_create(&doc, &sprints, link_types)?;
    for w in &follow_up_warnings {
        ctx.out.warn(w);
    }

    if args.dry_run {
        let mut ops = vec![OpReport {
            kind: "create_issue",
            status: "planned",
            summary: Some(format!("{project}: {}", doc.frontmatter.summary)),
            request: Some(request),
            error: None,
        }];
        ops.extend(follow_ups.iter().map(|f| OpReport {
            kind: f.kind,
            status: "planned",
            summary: Some(f.summary.clone()),
            request: Some(f.request.clone()),
            error: None,
        }));
        return Ok(IssueResult {
            key: temp_key.to_string(),
            ok: true,
            new_key: None,
            error: None,
            error_kind: None,
            hint: None,
            ops,
        });
    }

    let op_ids = engine
        .state
        .journal_ops(temp_key, &[("create_issue".into(), request.to_string())])?;

    let (new_key, new_id) = match crate::api::block_on(client.create_issue(payload)) {
        Ok(created) => created,
        Err(e) => {
            engine
                .state
                .mark_op(op_ids[0], "failed", Some(&e.message))?;
            return Ok(IssueResult {
                key: temp_key.to_string(),
                ok: false,
                new_key: None,
                error: Some(e.message.clone()),
                error_kind: Some(e.kind.as_str().to_string()),
                hint: e.hint.clone(),
                ops: vec![OpReport {
                    kind: "create_issue",
                    status: "failed",
                    summary: None,
                    request: None,
                    error: Some(e.message.clone()),
                }],
            });
        }
    };
    engine.state.mark_op(op_ids[0], "done", None)?;

    engine.state.rekey_issue(temp_key, &new_key, &new_id)?;
    engine.state.forget_new_issue_project(temp_key)?;

    // The operations the create call could not carry, now that there is a key
    // to aim them at. One that fails is worth saying out loud and reporting as
    // a failed op, but not worth failing an issue the server has already
    // created — and each is reported, so a `--dry-run` plan and what actually
    // happened line up op for op.
    let mut reports = vec![OpReport {
        kind: "create_issue",
        status: "done",
        summary: Some(format!("{project}: {}", doc.frontmatter.summary)),
        request: None,
        error: None,
    }];
    for f in &follow_ups {
        match perform(client, &new_key, &f.action) {
            Ok(()) => reports.push(OpReport {
                kind: f.kind,
                status: "done",
                summary: Some(f.summary.clone()),
                request: None,
                error: None,
            }),
            Err(e) => {
                ctx.out.warn(format!(
                    "{new_key} was created, but {}: {}",
                    f.summary, e.message
                ));
                reports.push(OpReport {
                    kind: f.kind,
                    status: "failed",
                    summary: Some(f.summary.clone()),
                    request: Some(f.request.clone()),
                    error: Some(e.message.clone()),
                });
            }
        }
    }

    let confirmed = crate::api::block_on(client.issue(&new_key))?;

    // The same silence a follow-up can meet: a sub-task created into a sprint
    // gets a 204 from the board and stays wherever its parent is. Unlike an
    // update there is nothing to protect — the file is being rebuilt around a
    // key it did not have, and the sprint it comes back with is the truth about
    // the issue — so this reports rather than refuses, which is all a create
    // that did happen can honestly do.
    for (f, report) in follow_ups.iter().zip(reports.iter_mut().skip(1)) {
        if report.status != "done" {
            continue;
        }
        let Some(why) = unapplied(&f.action, &confirmed) else {
            continue;
        };
        ctx.out
            .warn(format!("{new_key} was created, but {}: {why}", f.summary));
        report.status = "failed";
        report.request = Some(f.request.clone());
        report.error = Some(why);
    }

    let base_url = engine.base_url().to_string();
    rewrite_after_push(engine, &confirmed, file, markup, &base_url)?;
    engine.state.clear_ops(&new_key)?;

    Ok(IssueResult {
        key: temp_key.to_string(),
        ok: true,
        new_key: Some(new_key),
        error: None,
        error_kind: None,
        hint: None,
        ops: reports,
    })
}

/// Check a worklog's duration before it can cost an API call.
///
/// A duration Jira would reject fails the whole push otherwise, taking the
/// field edits planned alongside it down as well; reporting it here leaves the
/// rest of the issue to go out and the entry in the file to be corrected.
fn validate_duration<'a>(
    w: &'a WorklogEntry,
    key: &str,
    warnings: &mut Vec<String>,
) -> Option<&'a str> {
    if crate::doc::parse_duration(&w.time_spent).is_some() {
        return Some(&w.time_spent);
    }
    warnings.push(format!(
        "{key}: `{}` is not a length of time, so that worklog was not sent — \
         write it as `2h 30m`, in weeks, days, hours or minutes",
        w.time_spent
    ));
    None
}

fn worklog_input(
    w: &WorklogEntry,
    time_spent: &str,
    markup: Markup,
    raw: &[crate::convert::ast::RawBlock],
) -> WorklogInput {
    WorklogInput {
        time_spent: time_spent.to_string(),
        // An entry with no start time is one written just now; leaving it out
        // lets the server date it rather than guessing a time here.
        started: crate::api::model::parse_local_timestamp(&w.started),
        comment: if w.body.trim().is_empty() {
            RichText::Empty
        } else {
            convert::from_markdown(&w.body, markup, raw)
        },
    }
}

fn summarize_worklog(w: &WorklogEntry) -> String {
    match first_line(&w.body) {
        note if note.is_empty() => w.time_spent.clone(),
        note => format!("{} — {note}", w.time_spent),
    }
}

/// The sprint id to move into, `None` for the backlog, or the names this
/// workspace does know when the one asked for is not among them.
fn sprint_target(
    wanted: Option<&str>,
    sprints: &[crate::store::state::SprintRow],
) -> std::result::Result<Option<String>, Vec<String>> {
    let Some(name) = wanted.map(str::trim).filter(|n| !n.is_empty()) else {
        return Ok(None);
    };
    // An open sprint wins over a closed one of the same name: sprint names
    // repeat, and nobody means the finished one.
    let mut candidates: Vec<&crate::store::state::SprintRow> = sprints
        .iter()
        .filter(|s| s.name.eq_ignore_ascii_case(name))
        .collect();
    candidates.sort_by_key(|s| match s.state.as_str() {
        "active" => 0,
        "future" => 1,
        _ => 2,
    });
    match candidates.first() {
        Some(sprint) => Ok(Some(sprint.id.clone())),
        None => Err(sprints
            .iter()
            .filter(|s| s.state != "closed")
            .map(|s| s.name.clone())
            .collect()),
    }
}

/// Plan the custom-field half of an update.
///
/// Comparison is done on the simplified form of both sides, so a file written
/// before jired started collapsing these values does not read as an edit; and
/// what goes out is rebuilt from the shape the server last sent.
#[allow(clippy::too_many_arguments)]
fn plan_custom_fields(
    bf: &crate::doc::Frontmatter,
    wf: &crate::doc::Frontmatter,
    base_raw: &Value,
    field_meta: &std::collections::BTreeMap<String, crate::api::FieldMeta>,
    ignored: &std::collections::BTreeSet<String>,
    engine: &Engine,
    fields: &mut serde_json::Map<String, Value>,
    warnings: &mut Vec<String>,
) {
    let deployment = engine.account.deployment;
    let raw_of = |id: &str| base_raw.get("fields").and_then(|f| f.get(id));

    for (id, value) in &wf.custom {
        // An ignored field is missing from base, so every push would otherwise
        // read the file's leftover copy as a new value and send it back.
        if ignored.contains(id) {
            warnings.push(format!(
                "{}: `{id}` is in ignored_custom_fields and was not pushed — \
                 run `jired doctor` to take it out of the file",
                bf.key
            ));
            continue;
        }
        let before = bf.custom.get(id).map(crate::api::custom::simplify);
        if before.as_ref() == Some(&crate::api::custom::simplify(value)) {
            continue;
        }
        fields.insert(
            id.clone(),
            crate::api::custom::expand(value, raw_of(id), field_meta.get(id), deployment),
        );
    }

    // Deleting the entry is how a file says "clear this field"; leaving the
    // server value in place would make the file lie about the issue.
    for id in bf.custom.keys() {
        if !wf.custom.contains_key(id) && !ignored.contains(id) {
            fields.insert(id.clone(), Value::Null);
        }
    }
}

/// Re-fetch the issues refused as stale and push again the ones that still
/// merge cleanly.
///
/// "Still merges" is the right test for "the staleness does not touch what I am
/// sending", and it is a test that already exists: a three-way merge conflicts
/// exactly when the incoming change and the local one touch the same field. A
/// worklog somebody logged while you were working has nothing to do with the
/// labels you are sending, and dropping the whole issue over it is stricter
/// than it needs to be — but a priority they changed under a priority you
/// changed is a real conflict and stays refused.
///
/// The recovery this replaces was three commands and a full pull of every issue
/// in the workspace to send one label.
fn retry_stale(ctx: &mut Context, report: &mut PushReport, args: &PushArgs) -> Result<()> {
    let stale = report.stale.clone();
    ctx.out.say(format!(
        "re-fetching {} issue(s) the server had moved on from",
        stale.len()
    ));

    let pull_args = crate::cli::cmd::pull::PullArgs {
        // Abort, deliberately: a conflicting issue must be left exactly as a
        // plain push would leave it, not merged with markers behind the user's
        // back in the middle of a push they did not ask to be interactive.
        strategy: crate::cli::cmd::pull::Strategy::Abort,
        ..Default::default()
    };
    let pulled = match crate::sync::pull::run_scoped(ctx, &pull_args, Some(&stale)) {
        Ok(pulled) => pulled,
        // A failed re-fetch leaves every stale issue stale, which is where they
        // already were: worth saying, not worth failing the push that worked.
        Err(e) => {
            report.warnings.push(format!(
                "could not re-fetch the stale issues: {}",
                e.message
            ));
            return Ok(());
        }
    };

    let conflicted: std::collections::BTreeSet<&str> =
        pulled.conflicts.iter().map(|c| c.key.as_str()).collect();
    let retryable: Vec<String> = stale
        .iter()
        .filter(|k| !conflicted.contains(k.as_str()))
        .cloned()
        .collect();
    for key in &stale {
        if conflicted.contains(key.as_str()) {
            report.warnings.push(format!(
                "{key}: the incoming change touches a field you changed, so it was not retried"
            ));
        }
    }
    if retryable.is_empty() {
        return Ok(());
    }

    // `refresh_stale` off, so a second round of staleness reports rather than
    // looping: an issue that goes stale twice in one push is a signal, not
    // something to keep chasing.
    let retry = PushArgs {
        issues: retryable.clone(),
        refresh_stale: false,
        ..args.clone()
    };
    let second = run(ctx, &retry)?;

    // The retried issues replace their stale results; everything else stands.
    let retried: std::collections::BTreeSet<&str> =
        second.results.iter().map(|r| r.key.as_str()).collect();
    report.results.retain(|r| !retried.contains(r.key.as_str()));
    report.results.extend(second.results);
    report.results.sort_by(|a, b| a.key.cmp(&b.key));
    report.stale = second.stale;
    report.warnings.extend(second.warnings);
    if second.groups.is_some() {
        report.groups = second.groups;
    }
    Ok(())
}

/// An operation the create call cannot carry, to run once the issue has a key.
struct FollowUp {
    kind: &'static str,
    summary: String,
    request: Value,
    action: Action,
}

/// What has to happen after a create, because the create call cannot do it.
///
/// The sprint is a board move — the Sprint field is read-only on most instances
/// — and a status the workflow does not start in is a transition. Both are
/// worked out before the write rather than after it, so `--dry-run` can show
/// them: a plan listing `create_issue` alone was how the rest of a file went
/// unnoticed until it came back missing.
fn follow_ups_after_create(
    doc: &IssueDoc,
    sprints: &[crate::store::state::SprintRow],
    link_types: &mut LinkTypes<'_>,
) -> Result<(Vec<FollowUp>, Vec<String>)> {
    let mut out = Vec::new();
    let mut warnings = Vec::new();

    if let Some(name) = doc.frontmatter.sprint.last() {
        match sprint_target(Some(name), sprints) {
            Ok(target) => {
                let path = match &target {
                    Some(id) => format!("/rest/agile/1.0/sprint/{id}/issue"),
                    None => "/rest/agile/1.0/backlog/issue".to_string(),
                };
                out.push(FollowUp {
                    kind: "move_sprint",
                    summary: format!("the backlog → {name}"),
                    request: json!({"method": "POST", "path": path,
                                    "body": {"issues": ["{key}"]}}),
                    action: Action::MoveSprint {
                        // Not `name` itself: a blank `sprint:` entry resolves to
                        // the backlog, and the check has to expect what the call
                        // is actually making happen.
                        to: target.as_ref().map(|_| name.clone()),
                        sprint: target,
                        from: None,
                    },
                });
            }
            Err(_) => warnings.push(format!(
                "`{name}` is not a sprint this workspace knows; the new issue was not moved into it"
            )),
        }
    }

    // An empty status is the usual case — `create` writes one — and means
    // "wherever the workflow starts".
    let status = doc.frontmatter.status.trim();
    if !status.is_empty() {
        out.push(FollowUp {
            kind: "transition",
            summary: format!("→ {status}"),
            request: json!({"method": "POST", "path": "/issue/{key}/transitions",
                            "body": {"to": status}}),
            action: Action::Transition {
                to_status: status.to_string(),
            },
        });
    }

    // The create call has no field for links, so each `links:` line is a call
    // of its own, aimed at the key the create hands back. Resolved here,
    // before anything is sent, so an unknown direction refuses the issue with
    // the edit still on screen rather than after the create has happened.
    for (direction, other) in doc.frontmatter.link_pairs() {
        let (type_name, self_is_inward) =
            resolved_link(&doc.frontmatter.key, &direction, link_types.get()?)?;
        let (inward, outward) = link_ends("{key}", &other, self_is_inward);
        out.push(FollowUp {
            kind: "create_link",
            summary: format!("{direction} {other}"),
            request: json!({"method": "POST", "path": "/issueLink",
                            "body": {"type": {"name": type_name},
                                     "inwardIssue": {"key": inward},
                                     "outwardIssue": {"key": outward}}}),
            action: Action::CreateLink {
                type_name,
                other_key: other,
                self_is_inward,
            },
        });
    }
    Ok((out, warnings))
}

/// The instance's link types, fetched once per push and only when a `links:`
/// line actually changed. Most pushes carry none, and a request on every one
/// would be paid by everybody for a field few touch.
struct LinkTypes<'a> {
    client: &'a dyn JiraApi,
    cached: Option<Vec<crate::api::IssueLinkType>>,
}

impl LinkTypes<'_> {
    fn get(&mut self) -> Result<&[crate::api::IssueLinkType]> {
        if self.cached.is_none() {
            self.cached = Some(crate::api::block_on(self.client.link_types())?);
        }
        Ok(self.cached.as_deref().expect("just filled"))
    }
}

/// Resolve the direction a `links:` line spells — `is blocked by` — into the
/// type's registered name and which side this issue is on, or refuse with the
/// directions this Jira does offer (exit 2, at plan time, so `--dry-run` shows
/// it and nothing has been sent).
fn resolved_link(
    key: &str,
    direction: &str,
    types: &[crate::api::IssueLinkType],
) -> Result<(String, bool)> {
    // The outward reading first; when both match the type is symmetric —
    // `relates to` both ways — and the sides are interchangeable anyway.
    for t in types {
        if t.outward.eq_ignore_ascii_case(direction) {
            return Ok((t.name.clone(), true));
        }
    }
    for t in types {
        if t.inward.eq_ignore_ascii_case(direction) {
            return Ok((t.name.clone(), false));
        }
    }
    let mut known: Vec<String> = types
        .iter()
        .flat_map(|t| [t.outward.clone(), t.inward.clone()])
        .collect();
    known.sort();
    known.dedup();
    Err(JiredError::usage(format!(
        "{key}: `{direction}` is not a link direction this Jira knows"
    ))
    .with_detail("issue", key.to_string())
    .with_detail("direction", direction.to_string())
    .with_detail("known", json!(known))
    .with_hint(match known.is_empty() {
        true => "the server offered no link types; check the credential can read them".to_string(),
        false => format!("one of: {}", known.join(", ")),
    }))
}

/// The `(inwardIssue, outwardIssue)` of the create call, given which side this
/// issue is on. `{key}` stands for an issue not yet created.
fn link_ends(self_key: &str, other: &str, self_is_inward: bool) -> (String, String) {
    match self_is_inward {
        true => (self_key.to_string(), other.to_string()),
        false => (other.to_string(), self_key.to_string()),
    }
}

/// The id of the link the base snapshot shows as `(direction, key)`.
///
/// Read the way `normalize` reads the pair — the entry's `outwardIssue` under
/// the type's outward name, else its `inwardIssue` under the inward one — so
/// what was rendered into the file is exactly what is found here.
fn link_id_in(base_raw: &Value, direction: &str, other_key: &str) -> Option<String> {
    base_raw
        .pointer("/fields/issuelinks")?
        .as_array()?
        .iter()
        .find_map(|l| {
            let t = l.get("type")?;
            let (desc, other) = match l.get("outwardIssue") {
                Some(o) => (t.get("outward")?.as_str()?, o.get("key")?.as_str()?),
                None => {
                    let i = l.get("inwardIssue")?;
                    (t.get("inward")?.as_str()?, i.get("key")?.as_str()?)
                }
            };
            if desc != direction || other != other_key {
                return None;
            }
            // Jira is inconsistent about whether ids are numbers or strings.
            match l.get("id")? {
                Value::String(s) => Some(s.clone()),
                Value::Number(n) => Some(n.to_string()),
                _ => None,
            }
        })
}

/// Plan the `links:` half of an update: a create per added line, a delete per
/// removed one.
///
/// A removed link is deleted without the opt-in a comment or a worklog takes:
/// those are unrecoverable content someone else may have written, while a link
/// is exactly the pair on the line that was deleted, and an ordinary revert
/// puts it back.
fn plan_links(
    bf: &crate::doc::Frontmatter,
    wf: &crate::doc::Frontmatter,
    base_raw: &Value,
    link_types: &mut LinkTypes<'_>,
    ops: &mut Vec<Op>,
    warnings: &mut Vec<String>,
) -> Result<()> {
    let before = bf.link_pairs();
    let after = wf.link_pairs();
    if before == after {
        return Ok(());
    }

    for (direction, other) in after.iter().filter(|p| !before.contains(p)) {
        let (type_name, self_is_inward) = resolved_link(&bf.key, direction, link_types.get()?)?;
        let (inward, outward) = link_ends(&bf.key, other, self_is_inward);
        ops.push(Op {
            kind: "create_link",
            summary: format!("{direction} {other}"),
            request: json!({"method": "POST", "path": "/issueLink",
                            "body": {"type": {"name": type_name},
                                     "inwardIssue": {"key": inward},
                                     "outwardIssue": {"key": outward}}}),
            action: Action::CreateLink {
                type_name,
                other_key: other.clone(),
                self_is_inward,
            },
        });
    }

    for (direction, other) in before.iter().filter(|p| !after.contains(p)) {
        match link_id_in(base_raw, direction, other) {
            Some(id) => ops.push(Op {
                kind: "delete_link",
                summary: format!("{direction} {other}"),
                request: json!({"method": "DELETE", "path": format!("/issueLink/{id}")}),
                action: Action::DeleteLink { id },
            }),
            // A snapshot too old to carry the id: nothing to aim a DELETE at,
            // and guessing would delete somebody's other link.
            None => warnings.push(format!(
                "{}: the last-synced snapshot carries no id for `{direction}: {other}`, so \
                 that link was not deleted — run `jired pull`, then remove it again",
                bf.key
            )),
        }
    }
    Ok(())
}

/// What an editable frontmatter field becomes on the wire.
pub enum Mapped {
    /// The Jira field id, and the value for the frontmatter as it now stands.
    Send(String, Value),
    /// Not a field update: another operation carries this one — a transition, a
    /// board move, a request of its own.
    Elsewhere,
    /// Nothing can send it, and this is why. Said out loud rather than dropped.
    Refused(String),
}

/// The Jira field an editable frontmatter field maps to.
///
/// One mapping, read by both paths: `plan` sends the fields that differ from
/// base, `create_issue` sends the ones a new file arrives with. Keeping two
/// lists is what let `assignee` reach an update and never a create — a field
/// handled in one was simply absent from the other, and nothing said so, which
/// is the failure mode a silent success is made of.
///
/// Every name in [`frontmatter::EDITABLE`] is answered here; the test below
/// proves it, so a field added to the format cannot quietly go nowhere.
pub fn issue_field(
    name: &str,
    fm: &crate::doc::Frontmatter,
    deployment: crate::api::Deployment,
    story_points_field: Option<&str>,
) -> Mapped {
    let named = |values: &[String]| {
        json!(values
            .iter()
            .map(|v| json!({ "name": v }))
            .collect::<Vec<_>>())
    };
    match name {
        "summary" => Mapped::Send("summary".into(), json!(fm.summary)),
        "labels" => Mapped::Send("labels".into(), json!(fm.labels)),
        "components" => Mapped::Send("components".into(), named(&fm.components)),
        "fix_versions" => Mapped::Send("fixVersions".into(), named(&fm.fix_versions)),
        "affects_versions" => Mapped::Send("versions".into(), named(&fm.affects_versions)),
        "type" => Mapped::Send("issuetype".into(), json!({ "name": fm.issue_type })),
        "due_date" => Mapped::Send("duedate".into(), json!(fm.due_date)),
        "priority" => Mapped::Send(
            "priority".into(),
            fm.priority
                .as_ref()
                .map(|p| json!({ "name": p }))
                .unwrap_or(Value::Null),
        ),
        "parent" => Mapped::Send(
            "parent".into(),
            fm.parent
                .as_ref()
                .map(|p| json!({ "key": p }))
                .unwrap_or(Value::Null),
        ),
        "assignee" => {
            let id = fm
                .assignee
                .as_deref()
                .and_then(crate::doc::frontmatter::parse_user);
            Mapped::Send(
                "assignee".into(),
                crate::api::user_field_value(deployment, id.as_deref()),
            )
        }
        // Story points live in a custom field whose id differs per instance,
        // which is why it is set through the field map rather than by name.
        "story_points" => match story_points_field {
            Some(id) => Mapped::Send(id.to_string(), json!(fm.story_points)),
            None => Mapped::Refused("this instance has no Story Points field".to_string()),
        },
        // Carried by an operation of their own further down.
        "status" | "sprint" | "custom" | "links" => Mapped::Elsewhere,
        // Known to the format, not yet to push. Warned about rather than
        // dropped: `diff` calls it sendable, and a push that reported clean
        // success without it would be lying about what it did.
        "epic" => Mapped::Refused(
            "jired cannot send `epic` yet; set the epic in Jira and pull".to_string(),
        ),
        other => Mapped::Refused(format!("`{other}` is not a field jired knows how to send")),
    }
}

/// Whether a mapped value is worth putting in a *create* payload.
///
/// An update sends `null` and `[]` to clear a field; a create has nothing to
/// clear, and sending empties would only ask the server to unset what it never
/// set — and can be refused outright on a field the create screen does not
/// offer.
fn worth_creating(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Array(a) => !a.is_empty(),
        Value::String(s) => !s.is_empty(),
        Value::Object(o) => !o.is_empty(),
        _ => true,
    }
}

/// Build the ordered operation list for one issue (decision D23).
#[allow(clippy::too_many_arguments)]
fn plan(
    base: &IssueDoc,
    working: &IssueDoc,
    // The snapshot the base document was rendered from: the only place the
    // original shape of each custom field still exists.
    base_raw: &Value,
    field_meta: &std::collections::BTreeMap<String, crate::api::FieldMeta>,
    ignored: &std::collections::BTreeSet<String>,
    story_points_field: Option<&str>,
    sprints: &[crate::store::state::SprintRow],
    markup: Markup,
    raw: &[crate::convert::ast::RawBlock],
    engine: &Engine,
    args: &PushArgs,
    attachment_dir: &std::path::Path,
    max_upload: u64,
    done_transition: Option<&str>,
    board_move: Option<&str>,
    link_types: &mut LinkTypes<'_>,
    warnings: &mut Vec<String>,
) -> Result<Vec<Op>> {
    let mut ops = Vec::new();
    let mut fields = serde_json::Map::new();

    let bf = &base.frontmatter;
    let wf = &working.frontmatter;

    // Every editable field, compared the way `diff` compares it and mapped the
    // way `create` maps it. A field jired cannot send says so here rather than
    // going quietly missing from a push that then reports success.
    for name in crate::doc::frontmatter::EDITABLE {
        if bf.field(name) == wf.field(name) {
            continue;
        }
        match issue_field(name, wf, engine.account.deployment, story_points_field) {
            Mapped::Send(id, value) => {
                fields.insert(id, value);
            }
            Mapped::Elsewhere => {}
            Mapped::Refused(why) => {
                warnings.push(format!("{}: {why}; the change was not sent", bf.key))
            }
        }
    }
    // Jira re-parents a sub-task through its Move operation, and on Data Center
    // that is a web action rather than a REST endpoint: `PUT /issue` takes the
    // payload, answers success and leaves the parent exactly where it was. The
    // change leaves the field payload here and becomes a call of its own, so
    // what goes out is the operation that works rather than the one that is
    // quietly discarded.
    // Assigning is not editing. Jira gives the assignee a route of its own
    // because it is its own permission, and an instance whose edit screen does
    // not carry `assignee` — a normal configuration — refuses the field update
    // while accepting the route. Taken out here on both deployments: push plans
    // one operation for both, and the client knows which spelling the body
    // wants.
    let assign = fields.remove("assignee").map(|payload| {
        payload
            .get("accountId")
            .or_else(|| payload.get("name"))
            .and_then(Value::as_str)
            .map(str::to_string)
    });

    let mut move_parent = None;
    if is_subtask(base_raw) && engine.account.deployment == crate::api::Deployment::DataCenter {
        if let Some(payload) = fields.remove("parent") {
            let to = subtask_parent(&bf.key, &payload)?;
            web_session(&bf.key, engine.account.auth_kind)?;
            move_parent = Some(to);
        }
    }

    plan_custom_fields(
        bf,
        wf,
        base_raw,
        field_meta,
        ignored,
        engine,
        &mut fields,
        warnings,
    );

    if !fields.is_empty() {
        let payload = Value::Object(fields);
        ops.push(Op {
            kind: "update_fields",
            summary: field_names(&payload),
            request: json!({"method": "PUT", "path": "/issue/{key}", "body": {"fields": payload}}),
            action: Action::UpdateFields(payload),
        });
    }

    if let Some(to) = assign {
        let body = crate::api::assignee_body(engine.account.deployment, to.as_deref());
        ops.push(Op {
            kind: "assign",
            summary: match &to {
                Some(id) => id.clone(),
                None => "nobody".to_string(),
            },
            request: json!({"method": "PUT", "path": "/issue/{key}/assignee", "body": body}),
            action: Action::Assign { to },
        });
    }

    // After the field update rather than before it: the wizard walks away from
    // the REST API and is the one operation here that can leave a step
    // half-finished, so everything an ordinary call can do is already done when
    // it starts.
    if let Some(to) = move_parent {
        ops.push(Op {
            kind: "move_parent",
            summary: match &bf.parent {
                Some(from) => format!("{from} → {to}"),
                None => to.clone(),
            },
            request: json!({"method": "POST", "path": "/secure/MoveSubTaskParent.jspa",
                            "body": {"id": "{id}", "parentIssue": to}}),
            action: Action::MoveParent { to },
        });
    }

    if base.description != working.description {
        let rich = convert::from_markdown(&working.description, markup, raw);
        ops.push(Op {
            kind: "update_description",
            summary: format!("{} bytes", working.description.len()),
            request: json!({"method": "PUT", "path": "/issue/{key}",
                            "body": {"fields": {"description": rich.to_field_value()}}}),
            action: Action::UpdateDescription(rich),
        });
    }

    // A sprint is not a field on most instances — the Sprint custom field is
    // read-only — so moving between sprints is its own board call.
    //
    // The list is a history: the sprints an issue has been through, the last of
    // them being where it is now. A board move can only change that last one,
    // which is the only part of the list a person can act on anyway.
    if bf.sprint != wf.sprint {
        let (before, after) = (bf.sprint.last(), wf.sprint.last());
        if !crate::doc::frontmatter::sprint_history_only(&bf.sprint, &wf.sprint) {
            // A sub-task is not a board item: it takes its sprint from its
            // parent, and the board call for one is accepted and ignored. There
            // is no request that could carry this, so nothing is planned and the
            // issue stops here — reverting the line from the server afterwards
            // would throw the edit away with nothing but a success to show.
            if is_subtask(base_raw) {
                return Err(JiredError::usage(format!(
                    "{}: a sub-task takes its sprint from its parent and cannot be \
                     moved into one of its own",
                    bf.key
                ))
                .with_detail("issue", bf.key.clone())
                .with_detail("parent", json!(bf.parent))
                .with_detail("sprint", json!(after))
                .with_hint(match &bf.parent {
                    Some(parent) => format!("move {parent} instead"),
                    None => "move the parent issue instead".to_string(),
                }));
            }
            match sprint_target(after.map(String::as_str), sprints) {
                Ok(target) => {
                    let (path, label) = match &target {
                        Some(id) => (
                            format!("/rest/agile/1.0/sprint/{id}/issue"),
                            after.cloned().unwrap_or_default(),
                        ),
                        None => (
                            "/rest/agile/1.0/backlog/issue".to_string(),
                            "the backlog".to_string(),
                        ),
                    };
                    ops.push(Op {
                        kind: "move_sprint",
                        summary: format!(
                            "{} → {label}",
                            before.map(String::as_str).unwrap_or("the backlog")
                        ),
                        request: json!({"method": "POST", "path": path,
                                        "body": {"issues": ["{key}"]}}),
                        action: Action::MoveSprint {
                            to: target.as_ref().map(|_| label),
                            sprint: target,
                            from: before.cloned(),
                        },
                    });
                }
                Err(known) => warnings.push(format!(
                    "`{}` is not a sprint this workspace knows{}; the change was not sent",
                    after.map(String::as_str).unwrap_or(""),
                    if known.is_empty() {
                        String::new()
                    } else {
                        format!(" (it has {})", known.join(", "))
                    }
                )),
            }
        } else {
            // Only the earlier entries moved. Which sprints an issue has been
            // through is the server's record of what happened, and no API lets
            // it be rewritten.
            warnings.push(format!(
                "{}: sprint history cannot be changed; only the sprint it is in now",
                bf.key
            ));
        }
    }

    // A board move and a frontmatter edit are the same intent expressed two
    // ways; the file wins, since it is the more explicit of the two.
    let target_status = if bf.status != wf.status {
        Some(wf.status.clone())
    } else {
        board_move.map(str::to_string)
    };

    // Status is a transition, not a field, so it is planned after the fields
    // that its screen may require.
    if let Some(to) = target_status {
        ops.push(Op {
            kind: "transition",
            summary: format!("{} → {to}", bf.status),
            request: json!({"method": "POST", "path": "/issue/{key}/transitions",
                            "body": {"to": to}}),
            action: Action::Transition { to_status: to },
        });
    }

    plan_links(bf, wf, base_raw, link_types, &mut ops, warnings)?;

    for c in &working.comments {
        match &c.id {
            CommentId::New => {
                if c.body.trim().is_empty() {
                    continue;
                }
                let rich = convert::from_markdown(&c.body, markup, &[]);
                ops.push(Op {
                    kind: "create_comment",
                    summary: first_line(&c.body),
                    request: json!({"method": "POST", "path": "/issue/{key}/comment",
                                    "body": {"body": rich.to_field_value()}}),
                    action: Action::CreateComment(rich),
                });
            }
            CommentId::Existing(id) => {
                let Some(before) = base.comment(id) else {
                    continue;
                };
                if before.body == c.body {
                    continue;
                }
                let container = format!("comment:{id}");
                let craw = engine.state.raw_blocks(&base.frontmatter.key, &container)?;
                let rich = convert::from_markdown(&c.body, markup, &craw);
                ops.push(Op {
                    kind: "update_comment",
                    summary: format!("c-{id}"),
                    request: json!({"method": "PUT", "path": format!("/issue/{{key}}/comment/{id}"),
                                    "body": {"body": rich.to_field_value()}}),
                    action: Action::UpdateComment {
                        id: id.clone(),
                        body: rich,
                    },
                });
            }
        }
    }

    for w in &working.worklogs {
        let duration = validate_duration(w, &base.frontmatter.key, warnings);
        match (&w.id, duration) {
            (_, None) => continue,
            (WorklogId::New, Some(time_spent)) => {
                let input = worklog_input(w, time_spent, markup, &[]);
                ops.push(Op {
                    kind: "create_worklog",
                    summary: summarize_worklog(w),
                    request: json!({"method": "POST", "path": "/issue/{key}/worklog",
                                    "body": input.to_body()}),
                    action: Action::CreateWorklog(input),
                });
            }
            (WorklogId::Existing(id), Some(time_spent)) => {
                let Some(before) = base.worklog(id) else {
                    continue;
                };
                if (&before.time_spent, &before.started, &before.body)
                    == (&w.time_spent, &w.started, &w.body)
                {
                    continue;
                }
                let container = format!("worklog:{id}");
                let wraw = engine.state.raw_blocks(&base.frontmatter.key, &container)?;
                let input = worklog_input(w, time_spent, markup, &wraw);
                ops.push(Op {
                    kind: "update_worklog",
                    summary: format!("w-{id} {}", summarize_worklog(w)),
                    request: json!({"method": "PUT",
                                    "path": format!("/issue/{{key}}/worklog/{id}"),
                                    "body": input.to_body()}),
                    action: Action::UpdateWorklog {
                        id: id.clone(),
                        worklog: input,
                    },
                });
            }
        }
    }

    for w in &base.worklogs {
        let WorklogId::Existing(id) = &w.id else {
            continue;
        };
        if working.worklog(id).is_some() {
            continue;
        }
        if !args.allow_worklog_delete {
            // Deleting time somebody booked is not recoverable from the working
            // copy, so it takes the same explicit opt-in a comment does.
            warnings.push(format!(
                "{}: worklog w-{id} was removed from the file; pass \
                 --allow-worklog-delete to delete it on the server",
                base.frontmatter.key
            ));
            continue;
        }
        ops.push(Op {
            kind: "delete_worklog",
            summary: format!("w-{id} ({})", w.time_spent),
            request: json!({"method": "DELETE",
                            "path": format!("/issue/{{key}}/worklog/{id}")}),
            action: Action::DeleteWorklog { id: id.clone() },
        });
    }

    // The checklist plugin's list is rendered read-only: jired can show what
    // the plugin holds but has no supported way to write it back, and inventing
    // one would corrupt the field for everyone else looking at the issue.
    if working.checklist != base.checklist {
        warnings.push(format!(
            "the Checklist section of {} is read-only, so the edit was not pushed; \
             the next pull restores what the server holds",
            base.frontmatter.key
        ));
    }

    // A ticked child box means "this subtask is done". Acting on it is off
    // until `children.done_transition` is configured (decision D12): silently
    // transitioning an issue because a box was ticked is too surprising to be
    // the default.
    for item in &working.children {
        let Some(key) = &item.key else {
            continue;
        };
        let was_checked = base
            .children
            .iter()
            .find(|b| b.key.as_deref() == Some(key.as_str()))
            .map(|b| b.checked);
        if was_checked != Some(false) || !item.checked {
            continue;
        }
        let Some(to_status) = done_transition else {
            warnings.push(format!(
                "{key} is ticked in {} but children.done_transition is not set, \
                 so nothing was transitioned",
                base.frontmatter.key
            ));
            continue;
        };
        ops.push(Op {
            kind: "transition",
            summary: format!("{key} → {to_status} (children)"),
            request: json!({
                "method": "POST",
                "path": format!("/issue/{key}/transitions"),
                "body": {"to": to_status}
            }),
            action: Action::TransitionOther {
                key: key.clone(),
                to_status: to_status.to_string(),
            },
        });
    }

    // Files the user dropped into the attachment folder. Planned last so an
    // upload cannot be what fails an otherwise-good field edit.
    for path in crate::sync::attachments::pending_uploads(
        &engine.state,
        &base.frontmatter.key,
        attachment_dir,
    )? {
        let filename = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
            .to_string();
        let bytes = crate::sync::attachments::check_upload_size(&path, max_upload)?;
        ops.push(Op {
            kind: "upload_attachment",
            summary: format!(
                "{filename} ({})",
                crate::sync::render::human_size(bytes.len() as u64)
            ),
            request: json!({
                "method": "POST",
                "path": "/issue/{key}/attachments",
                "body": {"filename": filename, "size": bytes.len()}
            }),
            action: Action::UploadAttachment {
                filename: filename.clone(),
                bytes,
            },
        });
    }

    for c in &base.comments {
        let CommentId::Existing(id) = &c.id else {
            continue;
        };
        if working.comment(id).is_some() {
            continue;
        }
        if !args.allow_comment_delete {
            // Silently deleting on the server is the one mistake that cannot be
            // undone from the working copy, so it needs an explicit opt-in.
            //
            // Reported as a push warning rather than a log line: at the default
            // verbosity `tracing::warn!` is easy to miss, and the JSON report
            // never carried it at all.
            warnings.push(format!(
                "{}: comment c-{id} was removed from the file but left on the \
                 server; pass --allow-comment-delete to delete it too",
                base.frontmatter.key
            ));
            continue;
        }
        ops.push(Op {
            kind: "delete_comment",
            summary: format!("c-{id}"),
            request: json!({"method": "DELETE", "path": format!("/issue/{{key}}/comment/{id}")}),
            action: Action::DeleteComment { id: id.clone() },
        });
    }

    Ok(ops)
}

/// The key a sub-task's new `parent:` names, or why it cannot be one.
///
/// Two things Jira's Move wizard will not do, refused here rather than in the
/// HTML it would answer with. A sub-task has to have a parent — the operation
/// that leaves it without one is a conversion to a standard issue, which is a
/// different wizard — and it cannot cross projects, because its key would have
/// to change with it ([JRASERVER-15259]).
///
/// [JRASERVER-15259]: https://jira.atlassian.com/browse/JRASERVER-15259
fn subtask_parent(key: &str, payload: &Value) -> Result<String> {
    let to = payload.get("key").and_then(Value::as_str).unwrap_or("");
    if to.is_empty() {
        return Err(JiredError::usage(format!(
            "{key}: a sub-task cannot be left without a parent"
        ))
        .with_detail("issue", key.to_string())
        .with_hint(
            "put `parent:` back, or convert the sub-task to a standard issue in \
             Jira and pull",
        ));
    }
    let project = |k: &str| k.rsplit_once('-').map(|(p, _)| p.to_uppercase());
    if project(to) != project(key) {
        return Err(JiredError::usage(format!(
            "{key}: a sub-task cannot be moved to a parent in another project"
        ))
        .with_detail("issue", key.to_string())
        .with_detail("parent", to.to_string())
        .with_hint("move the parent issue's project in Jira, which moves its sub-tasks with it"));
    }
    Ok(to.to_string())
}

/// Refuse the move when the credential cannot open a web session.
///
/// Data Center's Move wizard is a servlet page, and Seraph authenticates those
/// from basic auth or a session cookie — never from a bearer token. A personal
/// access token that reads and writes issues all day is redirected to
/// `SessionTimeoutMessage.jspa` the moment it asks for one, and Jira serves
/// that page as HTTP 200: nothing in the reply says the credential was
/// refused. 0.4.0 walked into it and only the re-read afterwards caught it,
/// which is a push that fails after doing everything else in the plan.
///
/// So it is decided here, where a `--dry-run` can show it. Exit 2 rather than
/// 10: this is a credential the user chose and can change, not a server that
/// cannot do the thing.
fn web_session(key: &str, auth: crate::api::AuthKind) -> Result<()> {
    if auth.opens_web_session() {
        return Ok(());
    }
    Err(JiredError::usage(format!(
        "{key}: a sub-task's parent is changed through a Jira web page, which \
         a {} credential cannot open a session for",
        auth.as_str()
    ))
    .with_detail("issue", key.to_string())
    .with_detail("auth_kind", auth.as_str())
    .with_hint(
        "re-authenticate with `jired init --force --auth basic` if the instance \
         allows it, or move the sub-task in Jira and run `jired pull`",
    ))
}

/// Whether the issue's type is one of Jira's sub-tasks.
///
/// Read off the flag Jira itself goes by rather than off the type's name:
/// `Sub-task` is only the default spelling, and an instance is free to add types
/// of its own — the one this was reported against is called `Analytics
/// sub-task`. A snapshot too old to carry the flag answers "no", which leaves
/// the change to be caught by [`unapplied`] after the fact instead.
fn is_subtask(raw: &Value) -> bool {
    raw.pointer("/fields/issuetype/subtask")
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

/// Why an operation the server said it had applied is not in what the server now
/// holds, or `None` when it is.
///
/// A 2xx is not evidence. Jira takes a payload it has no intention of acting on
/// and says nothing about it: `PUT /issue` with a sub-task's `parent` on Data
/// Center, `POST …/sprint/{id}/issue` with a sub-task in the list. The push
/// already re-reads the issue to rebuild the file from it, so asking that same
/// answer whether the change is in it costs nothing.
///
/// Only what can be compared without guessing is checked. A field's payload and
/// the server's rendering of it are different shapes — `{"name": "High"}` comes
/// back as an object with an id, a self link and an icon — and a comparison that
/// had to guess would invent failures, which is this same silence pointing the
/// other way. `parent` and the sprint are the two whose answer is unambiguous,
/// and the two Jira is known to swallow.
fn unapplied(action: &Action, confirmed: &RemoteIssue) -> Option<String> {
    let same = |a: Option<&str>, b: Option<&str>| match (a, b) {
        (Some(a), Some(b)) => a.eq_ignore_ascii_case(b),
        (None, None) => true,
        _ => false,
    };
    match action {
        Action::UpdateFields(payload) => {
            let wanted = payload.get("parent")?.get("key").and_then(Value::as_str);
            let now = confirmed.fields.parent.as_deref();
            (wanted != now).then(|| match (wanted, now) {
                (Some(wanted), Some(now)) => format!("the parent is still {now}, not {wanted}"),
                (Some(wanted), None) => format!("the parent is still unset, not {wanted}"),
                (None, Some(now)) => format!("the parent is still {now}"),
                (None, None) => unreachable!("equal parents are not a difference"),
            })
        }
        // Checked like the parent, and for the same reason: assigning can be
        // refused for a permission the account turns out not to have, and Jira
        // does not always say so in the status line.
        Action::Assign { to } => {
            let now = confirmed.fields.assignee.as_ref().map(|u| u.id.as_str());
            (!same(now, to.as_deref())).then(|| match (now, to) {
                (Some(now), Some(to)) => format!("the assignee is still {now}, not {to}"),
                (Some(now), None) => format!("the assignee is still {now}, not nobody"),
                (None, Some(to)) => format!("the assignee is still unset, not {to}"),
                (None, None) => unreachable!("equal assignees are not a difference"),
            })
        }
        // The wizard answers in HTML, and a page that renders is not a move
        // that happened — an upgrade that moves the form, a permission the
        // account turns out not to have, a step that quietly re-renders. This
        // is the only evidence there is.
        Action::MoveParent { to } => {
            let now = confirmed.fields.parent.as_deref();
            (!same(now, Some(to))).then(|| match now {
                Some(now) => format!("the parent is still {now}, not {to}"),
                None => format!("the parent is still unset, not {to}"),
            })
        }
        // Read through the workspace's field map, so an instance whose `/field`
        // does not name a Sprint field would report every move as unapplied.
        // That workspace cannot render `sprint:` at all and is already broken
        // for sprints; a loud failure that leaves the edit alone is the right
        // side to be wrong on.
        Action::MoveSprint { to, from, .. } => {
            let now = confirmed.fields.sprint.last().map(String::as_str);
            match to {
                Some(to) if !same(now, Some(to)) => Some(match now {
                    Some(now) => format!("the issue is still in {now}, not {to}"),
                    None => format!("the issue is in no sprint, not {to}"),
                }),
                // Out of every sprint: Jira keeps the ones that closed while the
                // issue was in them, so what proves the move is that the one it
                // was in is no longer the one it is in.
                None if same(now, from.as_deref()) && from.is_some() => Some(format!(
                    "the issue is still in {}",
                    from.as_deref().unwrap_or_default()
                )),
                _ => None,
            }
        }
        _ => None,
    }
}

/// Whether the server has moved on from the snapshot this push was planned
/// against.
///
/// The two values do not arrive at the same precision. Jira reports `updated`
/// to the millisecond, but the snapshot in `state.db` keeps it in jired's own
/// timestamp format, which stops at the second — so an issue last written at
/// `08:42:10.123` is remembered as `08:42:10.000`, and a plain `live > base`
/// calls it stale against its own unchanged self, for ever, on every issue
/// whose `updated` has a non-zero millisecond. `pull` cannot clear that: it
/// compares snapshot to snapshot and correctly finds nothing to do.
///
/// Comparing whole seconds is what makes the two comparable. It costs the
/// ability to notice a change landing inside the same second as the snapshot,
/// which is a narrower window than the one between this check and the write it
/// guards — already the residual risk this check accepts (decision D8).
fn moved_on(live: chrono::DateTime<chrono::Utc>, base: chrono::DateTime<chrono::Utc>) -> bool {
    use chrono::Timelike;
    live.with_nanosecond(0).unwrap_or(live) > base
}

#[allow(clippy::too_many_arguments)]
fn execute(
    _ctx: &mut Context,
    engine: &Engine,
    client: &dyn JiraApi,
    key: &str,
    base_updated: &chrono::DateTime<chrono::Utc>,
    ops: Vec<Op>,
    args: &PushArgs,
    file: &str,
    markup: Markup,
    base_url: &str,
) -> Result<IssueResult> {
    let journal: Vec<(String, String)> = ops
        .iter()
        .map(|o| (o.kind.to_string(), o.request.to_string()))
        .collect();
    // This plan supersedes whatever journal a failed attempt left: base never
    // advanced for a failure, so the same work was re-planned from the same
    // working copy, and keeping the old rows gave every retry a growing pile
    // of identical `failed` entries for `status` to report.
    engine.state.clear_ops(key)?;
    let op_ids = engine.state.journal_ops(key, &journal)?;

    // Optimistic concurrency: Jira offers no compare-and-set, so the window
    // between this read and the write below is the residual risk (decision D8).
    if !args.force {
        let live = crate::api::block_on(client.issue_updated(key))?;
        if moved_on(live, *base_updated) {
            for id in &op_ids {
                engine.state.mark_op(*id, "failed", Some("stale"))?;
            }
            return Ok(IssueResult {
                key: key.to_string(),
                ok: false,
                new_key: None,
                // Spelled with the offset, not as bare local time: this is a
                // value to check against `jired.base_updated` in the file,
                // which is UTC, so it has to name one instant rather than
                // leave the reader subtracting.
                error: Some(format!(
                    "the server changed at {} but this workspace last synced {}",
                    crate::api::model::format_local_iso(&live),
                    crate::api::model::format_local_iso(base_updated)
                )),
                error_kind: Some("conflict".into()),
                hint: Some(
                    "run `jired pull` to take the server's changes, or `jired push --force`".into(),
                ),
                ops: ops
                    .iter()
                    .map(|o| OpReport {
                        kind: o.kind,
                        status: "skipped",
                        summary: Some(o.summary.clone()),
                        request: None,
                        error: None,
                    })
                    .collect(),
            });
        }
    }

    let mut reports = Vec::new();
    let mut failure: Option<JiredError> = None;

    for (op, op_id) in ops.iter().zip(op_ids.iter()) {
        if failure.is_some() {
            engine.state.mark_op(*op_id, "pending", None)?;
            reports.push(OpReport {
                kind: op.kind,
                status: "skipped",
                summary: Some(op.summary.clone()),
                request: None,
                error: None,
            });
            continue;
        }
        match perform(client, key, &op.action) {
            Ok(()) => {
                engine.state.mark_op(*op_id, "done", None)?;
                reports.push(OpReport {
                    kind: op.kind,
                    status: "done",
                    summary: Some(op.summary.clone()),
                    request: None,
                    error: None,
                });
            }
            Err(e) => {
                engine.state.mark_op(*op_id, "failed", Some(&e.message))?;
                reports.push(OpReport {
                    kind: op.kind,
                    status: "failed",
                    summary: Some(op.summary.clone()),
                    // Carried on a failure, where a plain push otherwise left
                    // nothing to say what had been sent: the error names the
                    // status the server answered with, and this names the call
                    // it answered.
                    request: Some(op.request.clone()),
                    error: Some(e.message.clone()),
                });
                failure = Some(e);
            }
        }
    }

    if let Some(e) = failure {
        // Base stays where it was: the issue is still dirty, and `status` and
        // `push --resume` will both say so.
        return Ok(IssueResult {
            key: key.to_string(),
            ok: false,
            new_key: None,
            error: Some(e.message.clone()),
            error_kind: Some(e.kind.as_str().to_string()),
            hint: e.hint.clone(),
            ops: reports,
        });
    }

    // Re-read what the server now holds and make that the new base.
    let confirmed = crate::api::block_on(client.issue(key))?;

    // Before the file is rewritten from it, not after: the rewrite is what would
    // replace the user's edit with the server's answer, and an operation the
    // server only pretended to apply must not be what triggers it. Base stays
    // where it was, the edit stays in the file, and `push --resume` still has
    // something to resume.
    let mut swallowed = Vec::new();
    for ((op, op_id), report) in ops.iter().zip(op_ids.iter()).zip(reports.iter_mut()) {
        let Some(why) = unapplied(&op.action, &confirmed) else {
            continue;
        };
        engine.state.mark_op(*op_id, "failed", Some(&why))?;
        report.status = "failed";
        report.request = Some(op.request.clone());
        report.error = Some(why.clone());
        swallowed.push(why);
    }
    if !swallowed.is_empty() {
        return Ok(IssueResult {
            key: key.to_string(),
            ok: false,
            new_key: None,
            error: Some(format!(
                "the server accepted the change and did not apply it: {}",
                swallowed.join("; ")
            )),
            error_kind: Some("unsupported_server".into()),
            hint: Some(
                "the edit is still in the file and nothing was overwritten; \
                 make the change in Jira and run `jired pull`"
                    .into(),
            ),
            ops: reports,
        });
    }

    rewrite_after_push(engine, &confirmed, file, markup, base_url)?;
    engine.state.clear_ops(key)?;

    Ok(IssueResult {
        key: key.to_string(),
        ok: true,
        new_key: None,
        error: None,
        error_kind: None,
        hint: None,
        ops: reports,
    })
}

fn perform(client: &dyn JiraApi, key: &str, action: &Action) -> Result<()> {
    crate::api::block_on(async {
        match action {
            Action::UpdateFields(fields) => client.update_fields(key, fields.clone()).await,
            Action::UpdateDescription(rich) => {
                client
                    .update_fields(key, json!({ "description": rich.to_field_value() }))
                    .await
            }
            Action::TransitionOther {
                key: other,
                to_status,
            } => {
                let available = client.transitions(other).await?;
                let target = available
                    .iter()
                    .find(|t| t.to_status.eq_ignore_ascii_case(to_status))
                    .or_else(|| {
                        available
                            .iter()
                            .find(|t| t.name.eq_ignore_ascii_case(to_status))
                    })
                    .ok_or_else(|| {
                        JiredError::usage(format!(
                            "{other} cannot move to `{to_status}` from its current status"
                        ))
                        .with_detail("issue", other.clone())
                        .with_detail(
                            "available",
                            json!(available.iter().map(|t| &t.to_status).collect::<Vec<_>>()),
                        )
                        .with_hint("check children.done_transition against the workflow")
                    })?;
                client.do_transition(other, &target.id, None).await
            }
            Action::Transition { to_status } => {
                let available = client.transitions(key).await?;
                let target = available
                    .iter()
                    .find(|t| t.to_status.eq_ignore_ascii_case(to_status))
                    .or_else(|| {
                        available
                            .iter()
                            .find(|t| t.name.eq_ignore_ascii_case(to_status))
                    })
                    .ok_or_else(|| {
                        JiredError::usage(format!(
                            "no transition from the current status leads to `{to_status}`"
                        ))
                        .with_detail("issue", key.to_string())
                        .with_detail(
                            "available",
                            json!(available.iter().map(|t| &t.to_status).collect::<Vec<_>>()),
                        )
                    })?;
                if !target.required_fields.is_empty() {
                    return Err(JiredError::usage(format!(
                        "the transition to `{to_status}` requires fields jired cannot supply"
                    ))
                    .with_detail("issue", key.to_string())
                    .with_detail("required", json!(target.required_fields))
                    .with_hint("run `jired transition` with --field key=value"));
                }
                client.do_transition(key, &target.id, None).await
            }
            Action::MoveSprint { sprint, .. } => {
                client
                    .move_to_sprint(sprint.as_deref(), &[key.to_string()])
                    .await
            }
            Action::Assign { to } => client.assign(key, to.as_deref()).await,
            Action::MoveParent { to } => client.move_subtask_parent(key, to).await,
            Action::CreateLink {
                type_name,
                other_key,
                self_is_inward,
            } => {
                let (inward, outward) = link_ends(key, other_key, *self_is_inward);
                client.create_link(type_name, &inward, &outward).await
            }
            Action::DeleteLink { id } => client.delete_link(id).await,
            Action::CreateComment(body) => client.add_comment(key, body).await.map(|_| ()),
            Action::UpdateComment { id, body } => {
                client.update_comment(key, id, body).await.map(|_| ())
            }
            Action::DeleteComment { id } => client.delete_comment(key, id).await,
            Action::CreateWorklog(worklog) => client.add_worklog(key, worklog).await.map(|_| ()),
            Action::UpdateWorklog { id, worklog } => {
                client.update_worklog(key, id, worklog).await.map(|_| ())
            }
            Action::DeleteWorklog { id } => client.delete_worklog(key, id).await,
            Action::UploadAttachment { filename, bytes } => client
                .upload_attachment(key, filename, bytes.clone())
                .await
                .map(|_| ()),
        }
    })
}

/// Rewrite the file from the server's confirmed state and advance base.
fn rewrite_after_push(
    engine: &Engine,
    issue: &RemoteIssue,
    file: &str,
    markup: Markup,
    base_url: &str,
) -> Result<()> {
    // The issue just came back from the client, whose field map knows nothing
    // about this workspace's config, so the ignore list is applied here.
    let ignored = engine.field_map()?.ignored;
    let rendered = render_issue(
        issue,
        &RenderContext {
            markup,
            base_url,
            ignored_custom: &ignored,
        },
    )?;
    let mut doc = rendered.doc;
    doc.frontmatter.jired.content_hash = String::new();
    let provisional = doc.render()?;
    doc.frontmatter.jired.content_hash = crate::store::state::content_hash(&provisional);

    let outcome = engine.projector.write(&doc, Some(file))?;
    engine
        .state
        .replace_raw_blocks(&issue.key, "description", &rendered.description_raw)?;
    for (container, blocks) in &rendered.body_raw {
        engine
            .state
            .replace_raw_blocks(&issue.key, container, blocks)?;
    }
    engine.state.set_base_from_remote(
        issue,
        &rendered.description_storage,
        &outcome.content_hash,
        &outcome.relative,
    )
}

fn field_names(payload: &Value) -> String {
    payload
        .as_object()
        .map(|o| o.keys().cloned().collect::<Vec<_>>().join(", "))
        .unwrap_or_default()
}

fn first_line(text: &str) -> String {
    let line = text.lines().next().unwrap_or("").trim();
    if line.chars().count() > 60 {
        format!("{}…", line.chars().take(59).collect::<String>())
    } else {
        line.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(text: &str) -> chrono::DateTime<chrono::Utc> {
        crate::api::model::parse_timestamp(text).expect("a timestamp")
    }

    /// The drift this mapping exists to prevent: `assignee` was handled for an
    /// update and absent from a create, so the value went nowhere and the push
    /// reported success. Every editable field must be answered here — sent,
    /// carried by another operation, or refused out loud.
    #[test]
    fn every_editable_field_is_answered_by_the_mapping() {
        let fm = crate::doc::Frontmatter::default();
        for name in crate::doc::frontmatter::EDITABLE {
            let mapped = issue_field(name, &fm, crate::api::Deployment::Cloud, Some("cf_1"));
            if let Mapped::Refused(why) = &mapped {
                // A refusal is a promise to say something useful about it.
                assert!(
                    why.contains(name) || why.contains("Story Points"),
                    "`{name}` is refused without naming itself: {why}"
                );
            }
            // The catch-all arm means a field nobody mapped; it names the field
            // rather than describing it, which is how this test tells them apart.
            assert!(
                !matches!(&mapped, Mapped::Refused(why) if why.ends_with("is not a field jired knows how to send")),
                "`{name}` fell through to the catch-all: add it to `issue_field`"
            );
        }
    }

    /// The snapshot keeps `updated` to the second and the server reports it to
    /// the millisecond, so the same instant reaches this check spelled two
    /// ways. Reading that as "the server changed" refused every push against an
    /// untouched issue, and no amount of pulling cleared it.
    #[test]
    fn an_issue_the_server_has_not_touched_is_not_stale() {
        // The reported case: the snapshot's own timestamp, to the second.
        let base = at("2026-08-28T06:42:10Z");
        assert!(!moved_on(base, base), "equal timestamps are not a change");
        assert!(
            !moved_on(at("2026-08-28T06:42:10.123Z"), base),
            "the same second, reported with the milliseconds the snapshot dropped"
        );
        assert!(
            !moved_on(at("2026-08-28T06:42:10.999Z"), base),
            "still the same second"
        );
        // A snapshot that did keep its milliseconds must not read as behind the
        // truncation of its own value.
        let exact = at("2026-08-28T06:42:10.123Z");
        assert!(!moved_on(exact, exact));
        assert!(!moved_on(at("2026-08-28T06:42:09.500Z"), exact));
    }

    #[test]
    fn a_server_write_after_the_snapshot_is_still_caught() {
        let base = at("2026-08-28T06:42:10Z");
        assert!(moved_on(at("2026-08-28T06:42:11Z"), base));
        assert!(moved_on(at("2026-08-28T06:42:11.001Z"), base));
        assert!(moved_on(at("2026-08-28T09:15:00.480Z"), base));
        // Across the second boundary, which is the tightest a snapshot holding
        // milliseconds can still be told apart from a later write.
        assert!(moved_on(
            at("2026-08-28T06:42:11.001Z"),
            at("2026-08-28T06:42:10.999Z")
        ));
    }

    /// Jira's own flag, not the type's name: an instance names its sub-task
    /// types whatever it likes — the one this was reported against is called
    /// `Analytics sub-task` — and a snapshot too old to carry the flag has to
    /// fall through to the check that runs after the write instead.
    #[test]
    fn a_sub_task_is_recognized_by_the_flag_and_not_by_its_name() {
        assert!(is_subtask(
            &json!({"fields": {"issuetype": {"name": "Analytics sub-task", "subtask": true}}})
        ));
        assert!(!is_subtask(
            &json!({"fields": {"issuetype": {"name": "Task", "subtask": false}}})
        ));
        // Named like one, and not one.
        assert!(!is_subtask(
            &json!({"fields": {"issuetype": {"name": "Sub-task"}}})
        ));
        assert!(!is_subtask(&json!({})));
    }

    fn confirmed(parent: Option<&str>, sprint: &[&str]) -> RemoteIssue {
        RemoteIssue {
            key: "PROJ-142".into(),
            id: "10023".into(),
            updated: at("2026-08-28T06:42:10Z"),
            description: RichText::Empty,
            fields: crate::api::IssueFields {
                parent: parent.map(str::to_string),
                sprint: sprint.iter().map(|s| s.to_string()).collect(),
                ..Default::default()
            },
            comments: vec![],
            comment_total: 0,
            worklogs: vec![],
            worklog_total: 0,
            attachments: vec![],
            raw: json!({}),
        }
    }

    /// The reported case, from the side the push can see it from: both calls
    /// answered success and neither changed anything.
    #[test]
    fn a_change_the_server_did_not_make_is_not_applied() {
        let parent = Action::UpdateFields(json!({"parent": {"key": "TSK-301"}}));
        let unchanged = confirmed(Some("TSK-203"), &["TSK Спринт 6"]);
        assert!(unapplied(&parent, &unchanged)
            .expect("the parent did not move")
            .contains("TSK-203"));
        assert!(unapplied(&parent, &confirmed(Some("TSK-301"), &[])).is_none());

        let move_sprint = Action::MoveSprint {
            sprint: Some("9".into()),
            to: Some("TSK Спринт 7".into()),
            from: Some("TSK Спринт 6".into()),
        };
        assert!(unapplied(&move_sprint, &unchanged).is_some());
        assert!(
            unapplied(
                &move_sprint,
                &confirmed(None, &["TSK Спринт 6", "TSK Спринт 7"])
            )
            .is_none(),
            "the sprint it rolled over from is still in the list"
        );
    }

    /// The wizard answers in HTML: a page rendered without complaint is not
    /// evidence, so the move is judged by where the issue then is.
    #[test]
    fn a_re_parenting_is_believed_only_when_the_parent_moved() {
        let moved = Action::MoveParent {
            to: "TSK-301".into(),
        };
        assert!(unapplied(&moved, &confirmed(Some("TSK-301"), &[])).is_none());
        assert!(unapplied(&moved, &confirmed(Some("tsk-301"), &[])).is_none());
        assert!(unapplied(&moved, &confirmed(Some("TSK-203"), &[]))
            .expect("the parent did not move")
            .contains("TSK-203"));
        assert!(unapplied(&moved, &confirmed(None, &[]))
            .expect("the parent did not move")
            .contains("still unset"));
    }

    /// The two moves Jira's wizard has no page for, refused while the edit is
    /// still on screen rather than as the HTML it would answer with.
    #[test]
    fn a_sub_task_needs_a_parent_and_cannot_leave_its_project() {
        assert_eq!(
            subtask_parent("DC-79", &json!({"key": "DC-78"})).unwrap(),
            "DC-78"
        );
        let none = subtask_parent("DC-79", &Value::Null).unwrap_err();
        assert!(
            none.message.contains("without a parent"),
            "{}",
            none.message
        );

        let across = subtask_parent("DC-79", &json!({"key": "OPS-12"})).unwrap_err();
        assert!(
            across.message.contains("another project"),
            "{}",
            across.message
        );
    }

    /// Clearing a field is a change like any other, and "still set" is how the
    /// server says it did not make it.
    #[test]
    fn clearing_a_parent_is_verified_too() {
        let clear = Action::UpdateFields(json!({ "parent": Value::Null }));
        assert!(unapplied(&clear, &confirmed(Some("PROJ-1"), &[])).is_some());
        assert!(unapplied(&clear, &confirmed(None, &[])).is_none());
    }

    /// Jira keeps the sprints an issue was in when they closed, so an issue in
    /// no sprint at all is not what leaving one looks like — what proves the
    /// move is that the sprint it was in is no longer the one it is in.
    #[test]
    fn a_move_to_the_backlog_is_checked_against_where_it_was() {
        let backlog = |from: Option<&str>| Action::MoveSprint {
            sprint: None,
            to: None,
            from: from.map(str::to_string),
        };
        assert!(unapplied(
            &backlog(Some("Sprint 42")),
            &confirmed(None, &["Sprint 42"])
        )
        .is_some());
        assert!(unapplied(&backlog(Some("Sprint 42")), &confirmed(None, &[])).is_none());
        assert!(
            unapplied(
                &backlog(Some("Sprint 42")),
                &confirmed(None, &["Sprint 41"])
            )
            .is_none(),
            "a closed sprint it never left is not the sprint it was moved out of"
        );
    }

    /// A field whose payload and whose rendering are different shapes is not
    /// checked at all: guessing at `{"name": "High"}` against the object with an
    /// id, a self link and an icon that comes back would invent failures, which
    /// is the same silence pointing the other way.
    #[test]
    fn only_what_can_be_compared_without_guessing_is_checked() {
        let priority = Action::UpdateFields(json!({"priority": {"name": "High"}}));
        assert!(unapplied(&priority, &confirmed(None, &[])).is_none());
        assert!(unapplied(
            &Action::Transition {
                to_status: "Done".into()
            },
            &confirmed(None, &[])
        )
        .is_none());
    }

    fn link_types() -> Vec<crate::api::IssueLinkType> {
        let t = |name: &str, inward: &str, outward: &str| crate::api::IssueLinkType {
            id: "1".into(),
            name: name.into(),
            inward: inward.into(),
            outward: outward.into(),
        };
        vec![
            t("Blocks", "is blocked by", "blocks"),
            t("Relates", "relates to", "relates to"),
        ]
    }

    /// The reported case, spelled from the file's side: `TSK-303 is blocked by
    /// TSK-266` must put the *blocker* at the inward end, because Jira's inward
    /// issue is the one that bears the type's outward description.
    #[test]
    fn a_link_direction_resolves_to_the_sides_jira_expects() {
        let types = link_types();

        let (name, self_is_inward) = resolved_link("TSK-303", "is blocked by", &types).unwrap();
        assert_eq!(name, "Blocks");
        assert!(!self_is_inward);
        assert_eq!(
            link_ends("TSK-303", "TSK-266", self_is_inward),
            ("TSK-266".to_string(), "TSK-303".to_string())
        );

        let (name, self_is_inward) = resolved_link("TSK-303", "Blocks", &types).unwrap();
        assert_eq!(name, "Blocks");
        assert!(self_is_inward, "`blocks` reads outward from this issue");
        assert_eq!(
            link_ends("TSK-303", "TSK-310", self_is_inward),
            ("TSK-303".to_string(), "TSK-310".to_string())
        );

        // A symmetric type matches both ways; either side is the same link.
        assert!(resolved_link("TSK-303", "relates to", &types).is_ok());
    }

    /// The refusal is exit 2 at plan time, and it names what the server does
    /// offer — the file's spelling is per instance, not a fixed vocabulary.
    #[test]
    fn an_unknown_link_direction_is_refused_naming_the_known_ones() {
        let err = resolved_link("TSK-303", "depends on", &link_types()).unwrap_err();
        assert_eq!(err.exit_code().as_i32(), 2);
        assert!(err.message.contains("depends on"), "{}", err.message);
        let hint = err.hint.as_deref().unwrap();
        assert!(hint.contains("is blocked by"), "{hint}");
        assert!(hint.contains("blocks"), "{hint}");
    }

    /// A deletion aims at the link's own id, which only the base snapshot
    /// holds; the pair is matched exactly the way the file was rendered.
    #[test]
    fn a_removed_link_is_found_in_the_snapshot_by_direction_and_key() {
        let raw = json!({"fields": {"issuelinks": [
            {"id": "10500",
             "type": {"name": "Blocks", "inward": "is blocked by", "outward": "blocks"},
             "outwardIssue": {"key": "PROJ-150"}},
            {"id": 10501,
             "type": {"name": "Blocks", "inward": "is blocked by", "outward": "blocks"},
             "inwardIssue": {"key": "PROJ-99"}}
        ]}});
        assert_eq!(
            link_id_in(&raw, "blocks", "PROJ-150").as_deref(),
            Some("10500")
        );
        // A numeric id still reads, and the inward side matches its own name.
        assert_eq!(
            link_id_in(&raw, "is blocked by", "PROJ-99").as_deref(),
            Some("10501")
        );
        // The same key under the wrong direction is a different link.
        assert_eq!(link_id_in(&raw, "is blocked by", "PROJ-150"), None);
        assert_eq!(link_id_in(&json!({}), "blocks", "PROJ-150"), None);
    }

    #[test]
    fn exit_code_reflects_the_worst_outcome() {
        let ok = PushReport {
            summary: PushSummary::default(),
            dry_run: false,
            results: vec![],
            stale: vec![],
            groups: None,
            warnings: vec![],
        };
        assert_eq!(ok.exit_code(), ExitCode::Success);

        let stale = PushReport {
            summary: PushSummary::default(),
            dry_run: false,
            results: vec![],
            stale: vec!["PROJ-1".into()],
            groups: None,
            warnings: vec![],
        };
        assert_eq!(stale.exit_code(), ExitCode::Conflict);

        let failed = PushReport {
            summary: PushSummary::default(),
            dry_run: false,
            results: vec![IssueResult::failed("PROJ-1", &JiredError::generic("boom"))],
            stale: vec![],
            groups: None,
            warnings: vec![],
        };
        assert_eq!(failed.exit_code(), ExitCode::Generic);

        // A single kind of failure keeps its own exit code, so a script can
        // tell "the token expired" from "the conversion was lossy".
        let auth = PushReport {
            summary: PushSummary::default(),
            dry_run: false,
            results: vec![IssueResult::failed("PROJ-1", &JiredError::auth("no"))],
            stale: vec![],
            groups: None,
            warnings: vec![],
        };
        assert_eq!(auth.exit_code(), ExitCode::Auth);

        // Mixed causes have no single right answer, so they stay generic.
        let mixed = PushReport {
            summary: PushSummary::default(),
            dry_run: false,
            results: vec![
                IssueResult::failed("PROJ-1", &JiredError::auth("no")),
                IssueResult::failed("PROJ-2", &JiredError::lossy("no")),
            ],
            stale: vec![],
            groups: None,
            warnings: vec![],
        };
        assert_eq!(mixed.exit_code(), ExitCode::Generic);
    }

    /// A summary with an accent used to panic the whole command: `&s[..240]`
    /// slices bytes, and 240 need not be a character boundary.
    #[test]
    fn compact_never_splits_a_multibyte_character() {
        let long: String = "é".repeat(400);
        let text = compact(&json!({ "summary": long }));
        assert!(text.ends_with('…'));
    }

    #[test]
    fn field_summary_lists_what_changes() {
        let payload = json!({"summary": "x", "labels": ["a"]});
        let names = field_names(&payload);
        assert!(names.contains("summary"));
        assert!(names.contains("labels"));
    }

    #[test]
    fn comment_summaries_are_truncated() {
        assert_eq!(first_line("short"), "short");
        assert_eq!(first_line("a\nb"), "a");
        let long = "x".repeat(100);
        let s = first_line(&long);
        assert_eq!(s.chars().count(), 60);
        assert!(s.ends_with('…'));
    }
}
