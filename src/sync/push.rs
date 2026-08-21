//! `jired push` [M2.8, M2.9] — apply the working-copy delta to Jira.
//!
//! Two invariants shape this module:
//!
//! * base advances only for an issue whose every operation the server
//!   confirmed, so `state.db` can never claim a push that did not happen;
//! * a lossy region is rebuilt from its stored original, never from the file,
//!   so untouched read-only content goes back byte-identical.

use crate::api::{JiraApi, Markup, RemoteIssue, RichText};
use crate::cli::cmd::push::PushArgs;
use crate::cli::context::Context;
use crate::convert;
use crate::doc::{CommentId, IssueDoc};
use crate::error::{ExitCode, JiredError, Result};
use crate::json::Out;
use crate::sync::render::{render_issue, RenderContext};
use crate::sync::Engine;
use serde::Serialize;
use serde_json::{json, Value};

#[derive(Debug, Clone, Serialize)]
pub struct PushReport {
    pub dry_run: bool,
    pub results: Vec<IssueResult>,
    pub stale: Vec<String>,
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
    pub ops: Vec<OpReport>,
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
        if self.results.iter().any(|r| !r.ok) {
            return ExitCode::Generic;
        }
        ExitCode::Success
    }

    pub fn render(&self, out: Out) {
        if self.results.is_empty() {
            out.say("nothing to push");
            for w in &self.warnings {
                out.warn(w);
            }
            return;
        }
        for r in &self.results {
            let head = match (&r.new_key, r.ok) {
                (Some(k), _) => format!("{} → {k}", r.key),
                (None, true) => r.key.clone(),
                (None, false) => format!("{} (failed)", r.key),
            };
            out.say(format!("── {head}"));
            for op in &r.ops {
                out.say(format!(
                    "   {:<20} {}{}",
                    op.kind,
                    op.summary.as_deref().unwrap_or(""),
                    match op.status {
                        "planned" => "",
                        "done" => "  ✓",
                        "skipped" => "  (skipped)",
                        _ => "  ✗",
                    }
                ));
                if self.dry_run {
                    if let Some(req) = &op.request {
                        out.say(format!("     {}", compact(req)));
                    }
                }
                if let Some(e) = &op.error {
                    out.say(format!("     error: {e}"));
                }
            }
            if let Some(e) = &r.error {
                out.say(format!("   error: {e}"));
            }
        }
        if !self.stale.is_empty() {
            out.say(format!(
                "\n{} issue(s) changed on the server since your last sync: {}",
                self.stale.len(),
                self.stale.join(", ")
            ));
            out.say("run `jired pull` first, or push with --force to overwrite");
        }
        for w in &self.warnings {
            out.warn(w);
        }
        if self.dry_run {
            out.say("\ndry run — nothing was sent");
        }
    }
}

fn compact(v: &Value) -> String {
    let s = v.to_string();
    if s.len() > 240 {
        format!("{}…", &s[..240])
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
    UploadAttachment {
        filename: String,
        bytes: Vec<u8>,
    },
    /// Transition a *different* issue: ticking a subtask's checklist box.
    TransitionOther {
        key: String,
        to_status: String,
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
    let base_url = engine.base_url().to_string();
    let files = engine.projector.scan()?;
    let max_upload = ctx
        .resolver
        .get_u64("attachments.max_upload")?
        .unwrap_or(50 * 1024 * 1024);
    let done_transition = ctx.resolver.get_value("checklist.done_transition")?;
    let mut wanted: Vec<String> = args.issues.iter().map(|k| k.to_uppercase()).collect();

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
                dry_run: args.dry_run,
                results: Vec::new(),
                stale: Vec::new(),
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
        dry_run: args.dry_run,
        results: Vec::new(),
        stale: Vec::new(),
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
        ) {
            Ok(result) => {
                if let Some(new_key) = &result.new_key {
                    rekeyed.push((row.key.clone(), new_key.clone()));
                }
                report.results.push(result);
            }
            Err(e) => report.results.push(IssueResult {
                key: row.key.clone(),
                ok: false,
                new_key: None,
                error: Some(e.message.clone()),
                error_kind: Some(e.kind.as_str().to_string()),
                ops: vec![],
            }),
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

        for heading in
            crate::doc::misplaced_comment_headings(&engine.projector.read_raw(&file.relative)?)
        {
            ctx.out.warn(format!(
                "{}: `{heading}` is not under `## Comments` and will not be pushed",
                file.relative
            ));
        }

        let working = engine.projector.read(&file.relative)?;
        let base_issue = crate::api::normalize::issue_from_json(&base.raw, markup, &fields)?;
        let base_doc = render_issue(
            &base_issue,
            &RenderContext {
                markup,
                base_url: &base_url,
            },
        )?
        .doc;

        // A read-only region that was edited must stop this issue before any
        // call is made, or the original markup would be silently discarded.
        let raw = engine.state.raw_blocks(&row.key, "description")?;
        let edited = convert::edited_regions(&working.description, &raw);
        if !edited.is_empty() && !args.allow_lossy {
            let labels: Vec<&str> = raw
                .iter()
                .filter(|b| edited.contains(&b.id))
                .map(|b| b.label.as_str())
                .collect();
            return Err(JiredError::lossy(format!(
                "{}: a read-only region was edited and cannot be converted back",
                row.key
            ))
            .with_detail("issue", row.key.clone())
            .with_detail("regions", json!(edited))
            .with_detail("node_types", json!(labels))
            .with_hint(
                "restore the region, or pass --allow-lossy to replace it with plain Markdown",
            ));
        }

        let ops = plan(
            &base_doc,
            &working,
            &base.raw,
            &field_meta,
            markup,
            &raw,
            &engine,
            args,
            &attachment_dir,
            max_upload,
            done_transition.as_deref(),
            moved.map(String::as_str),
            &mut report.warnings,
        )?;
        if ops.is_empty() {
            continue;
        }

        if args.dry_run {
            report.results.push(IssueResult {
                key: row.key.clone(),
                ok: true,
                new_key: None,
                error: None,
                error_kind: None,
                ops: ops
                    .iter()
                    .map(|o| OpReport {
                        kind: o.kind,
                        status: "planned",
                        summary: Some(o.summary.clone()),
                        request: Some(o.request.clone()),
                        error: None,
                    })
                    .collect(),
            });
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
            Err(e) => report.results.push(IssueResult {
                key: row.key.clone(),
                ok: false,
                new_key: None,
                error: Some(e.message.clone()),
                error_kind: Some(e.kind.as_str().to_string()),
                ops: vec![],
            }),
        }
    }

    report.results.sort_by(|a, b| a.key.cmp(&b.key));
    Ok(report)
}

/// Create a locally authored `NEW-*` issue on the server.
///
/// On success the temporary key is replaced everywhere — state rows, the file
/// name and the attachment folder — so nothing keeps referring to a key that
/// never existed on the server.
fn create_issue(
    ctx: &mut Context,
    engine: &Engine,
    client: &dyn JiraApi,
    temp_key: &str,
    file: &str,
    args: &PushArgs,
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
    if !doc.frontmatter.labels.is_empty() {
        fields.insert("labels".into(), json!(doc.frontmatter.labels));
    }
    if let Some(parent) = &doc.frontmatter.parent {
        fields.insert("parent".into(), json!({ "key": parent }));
    }
    if let Some(priority) = &doc.frontmatter.priority {
        fields.insert("priority".into(), json!({ "name": priority }));
    }
    // A locally created issue may already carry custom fields; there is no
    // previous value to learn the shape from, so the schema decides.
    let field_meta = engine.state.field_meta_by_id()?;
    for (id, value) in &doc.frontmatter.custom {
        let payload =
            crate::api::custom::expand(value, None, field_meta.get(id), engine.account.deployment);
        if !payload.is_null() {
            fields.insert(id.clone(), payload);
        }
    }

    let payload = Value::Object(fields);
    let request = json!({"method": "POST", "path": "/issue", "body": {"fields": payload}});

    if args.dry_run {
        return Ok(IssueResult {
            key: temp_key.to_string(),
            ok: true,
            new_key: None,
            error: None,
            error_kind: None,
            ops: vec![OpReport {
                kind: "create_issue",
                status: "planned",
                summary: Some(format!("{project}: {}", doc.frontmatter.summary)),
                request: Some(request),
                error: None,
            }],
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

    let confirmed = crate::api::block_on(client.issue(&new_key))?;
    let base_url = engine.base_url().to_string();
    rewrite_after_push(engine, &confirmed, file, markup, &base_url)?;
    engine.state.clear_ops(&new_key)?;

    Ok(IssueResult {
        key: temp_key.to_string(),
        ok: true,
        new_key: Some(new_key),
        error: None,
        error_kind: None,
        ops: vec![OpReport {
            kind: "create_issue",
            status: "done",
            summary: Some(format!("{project}: {}", doc.frontmatter.summary)),
            request: None,
            error: None,
        }],
    })
}

/// Plan the custom-field half of an update.
///
/// Comparison is done on the simplified form of both sides, so a file written
/// before jired started collapsing these values does not read as an edit; and
/// what goes out is rebuilt from the shape the server last sent.
fn plan_custom_fields(
    bf: &crate::doc::Frontmatter,
    wf: &crate::doc::Frontmatter,
    base_raw: &Value,
    field_meta: &std::collections::BTreeMap<String, crate::api::FieldMeta>,
    engine: &Engine,
    fields: &mut serde_json::Map<String, Value>,
) {
    let deployment = engine.account.deployment;
    let raw_of = |id: &str| base_raw.get("fields").and_then(|f| f.get(id));

    for (id, value) in &wf.custom {
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
        if !wf.custom.contains_key(id) {
            fields.insert(id.clone(), Value::Null);
        }
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
    markup: Markup,
    raw: &[crate::convert::ast::RawBlock],
    engine: &Engine,
    args: &PushArgs,
    attachment_dir: &std::path::Path,
    max_upload: u64,
    done_transition: Option<&str>,
    board_move: Option<&str>,
    warnings: &mut Vec<String>,
) -> Result<Vec<Op>> {
    let mut ops = Vec::new();
    let mut fields = serde_json::Map::new();

    let bf = &base.frontmatter;
    let wf = &working.frontmatter;

    if bf.summary != wf.summary {
        fields.insert("summary".into(), json!(wf.summary));
    }
    if bf.labels != wf.labels {
        fields.insert("labels".into(), json!(wf.labels));
    }
    if bf.components != wf.components {
        fields.insert(
            "components".into(),
            json!(wf
                .components
                .iter()
                .map(|c| json!({"name": c}))
                .collect::<Vec<_>>()),
        );
    }
    if bf.priority != wf.priority {
        fields.insert(
            "priority".into(),
            wf.priority
                .as_ref()
                .map(|p| json!({ "name": p }))
                .unwrap_or(Value::Null),
        );
    }
    if bf.issue_type != wf.issue_type {
        fields.insert("issuetype".into(), json!({ "name": wf.issue_type }));
    }
    if bf.assignee != wf.assignee {
        let id = wf
            .assignee
            .as_deref()
            .and_then(crate::doc::frontmatter::parse_user);
        fields.insert(
            "assignee".into(),
            crate::api::user_field_value(engine.account.deployment, id.as_deref()),
        );
    }
    if bf.fix_versions != wf.fix_versions {
        fields.insert(
            "fixVersions".into(),
            json!(wf
                .fix_versions
                .iter()
                .map(|v| json!({"name": v}))
                .collect::<Vec<_>>()),
        );
    }
    if bf.affects_versions != wf.affects_versions {
        fields.insert(
            "versions".into(),
            json!(wf
                .affects_versions
                .iter()
                .map(|v| json!({"name": v}))
                .collect::<Vec<_>>()),
        );
    }
    if bf.parent != wf.parent {
        fields.insert(
            "parent".into(),
            wf.parent
                .as_ref()
                .map(|p| json!({ "key": p }))
                .unwrap_or(Value::Null),
        );
    }
    plan_custom_fields(bf, wf, base_raw, field_meta, engine, &mut fields);

    if !fields.is_empty() {
        let payload = Value::Object(fields);
        ops.push(Op {
            kind: "update_fields",
            summary: field_names(&payload),
            request: json!({"method": "PUT", "path": "/issue/{key}", "body": {"fields": payload}}),
            action: Action::UpdateFields(payload),
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

    // A ticked checklist box means "this subtask is done". Acting on it is off
    // until `checklist.done_transition` is configured (decision D12): silently
    // transitioning an issue because a box was ticked is too surprising to be
    // the default.
    for item in &working.checklist {
        let Some(key) = &item.key else {
            continue;
        };
        let was_checked = base
            .checklist
            .iter()
            .find(|b| b.key.as_deref() == Some(key.as_str()))
            .map(|b| b.checked);
        if was_checked != Some(false) || !item.checked {
            continue;
        }
        let Some(to_status) = done_transition else {
            warnings.push(format!(
                "{key} is ticked in {} but checklist.done_transition is not set, \
                 so nothing was transitioned",
                base.frontmatter.key
            ));
            continue;
        };
        ops.push(Op {
            kind: "transition",
            summary: format!("{key} → {to_status} (checklist)"),
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
            tracing::warn!(
                "{}: comment c-{id} was removed from the file; pass \
                 --allow-comment-delete to delete it on the server",
                base.frontmatter.key
            );
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
    let op_ids = engine.state.journal_ops(key, &journal)?;

    // Optimistic concurrency: Jira offers no compare-and-set, so the window
    // between this read and the write below is the residual risk (decision D8).
    if !args.force {
        let live = crate::api::block_on(client.issue_updated(key))?;
        if live > *base_updated {
            for id in &op_ids {
                engine.state.mark_op(*id, "failed", Some("stale"))?;
            }
            return Ok(IssueResult {
                key: key.to_string(),
                ok: false,
                new_key: None,
                error: Some(format!(
                    "the server changed at {} but this workspace last synced {}",
                    crate::api::model::format_timestamp(&live),
                    crate::api::model::format_timestamp(base_updated)
                )),
                error_kind: Some("conflict".into()),
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
                    request: None,
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
            ops: reports,
        });
    }

    // Re-read what the server now holds and make that the new base.
    let confirmed = crate::api::block_on(client.issue(key))?;
    rewrite_after_push(engine, &confirmed, file, markup, base_url)?;
    engine.state.clear_ops(key)?;

    Ok(IssueResult {
        key: key.to_string(),
        ok: true,
        new_key: None,
        error: None,
        error_kind: None,
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
                        .with_hint("check checklist.done_transition against the workflow")
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
            Action::CreateComment(body) => client.add_comment(key, body).await.map(|_| ()),
            Action::UpdateComment { id, body } => {
                client.update_comment(key, id, body).await.map(|_| ())
            }
            Action::DeleteComment { id } => client.delete_comment(key, id).await,
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
    let rendered = render_issue(issue, &RenderContext { markup, base_url })?;
    let mut doc = rendered.doc;
    doc.frontmatter.jired.content_hash = String::new();
    let provisional = doc.render()?;
    doc.frontmatter.jired.content_hash = crate::store::state::content_hash(&provisional);

    let outcome = engine.projector.write(&doc, Some(file))?;
    engine
        .state
        .replace_raw_blocks(&issue.key, "description", &rendered.description_raw)?;
    for (container, blocks) in &rendered.comment_raw {
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

    #[test]
    fn exit_code_reflects_the_worst_outcome() {
        let ok = PushReport {
            dry_run: false,
            results: vec![],
            stale: vec![],
            warnings: vec![],
        };
        assert_eq!(ok.exit_code(), ExitCode::Success);

        let stale = PushReport {
            dry_run: false,
            results: vec![],
            stale: vec!["PROJ-1".into()],
            warnings: vec![],
        };
        assert_eq!(stale.exit_code(), ExitCode::Conflict);

        let failed = PushReport {
            dry_run: false,
            results: vec![IssueResult {
                key: "PROJ-1".into(),
                ok: false,
                new_key: None,
                error: Some("boom".into()),
                error_kind: Some("generic".into()),
                ops: vec![],
            }],
            stale: vec![],
            warnings: vec![],
        };
        assert_eq!(failed.exit_code(), ExitCode::Generic);
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
