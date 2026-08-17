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
    Transition { to_status: String },
    CreateComment(RichText),
    UpdateComment { id: String, body: RichText },
    DeleteComment { id: String },
}

pub fn run(ctx: &mut Context, args: &PushArgs) -> Result<PushReport> {
    let engine = Engine::open(ctx)?;
    let client = engine.client(ctx)?;
    let markup = engine.markup();
    let fields = engine.field_map()?;
    let base_url = engine.base_url().to_string();
    let files = engine.projector.scan()?;
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
    };

    for row in engine.state.issues()? {
        if !wanted.is_empty() && !wanted.contains(&row.key) {
            continue;
        }
        let (Some(base), Some(file)) = (&row.base, files.get(&row.key)) else {
            continue;
        };
        let Some(hash) = engine.projector.hash_of(&file.relative) else {
            continue;
        };
        if hash == base.md_hash {
            continue;
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

        let ops = plan(&base_doc, &working, markup, &raw, &engine, args)?;
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

/// Build the ordered operation list for one issue (decision D23).
fn plan(
    base: &IssueDoc,
    working: &IssueDoc,
    markup: Markup,
    raw: &[crate::convert::ast::RawBlock],
    engine: &Engine,
    args: &PushArgs,
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
    for (id, value) in &wf.custom {
        if bf.custom.get(id) != Some(value) {
            fields.insert(id.clone(), value.clone());
        }
    }

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

    // Status is a transition, not a field, so it is planned after the fields
    // that its screen may require.
    if bf.status != wf.status {
        ops.push(Op {
            kind: "transition",
            summary: format!("{} → {}", bf.status, wf.status),
            request: json!({"method": "POST", "path": "/issue/{key}/transitions",
                            "body": {"to": wf.status}}),
            action: Action::Transition {
                to_status: wf.status.clone(),
            },
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
        };
        assert_eq!(ok.exit_code(), ExitCode::Success);

        let stale = PushReport {
            dry_run: false,
            results: vec![],
            stale: vec!["PROJ-1".into()],
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
