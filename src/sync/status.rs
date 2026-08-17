//! `jired status` [M2.10] — the one-screen answer to "where do I stand".
//!
//! Entirely offline: it reports the workspace against the last fetch, and says
//! how old that fetch is rather than silently going to the network.

use crate::cli::cmd::status::StatusArgs;
use crate::cli::context::Context;
use crate::error::Result;
use crate::json::Out;
use crate::sync::Engine;
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct StatusReport {
    pub workspace: WorkspaceStatus,
    pub dirty: Vec<DirtyIssue>,
    pub new_issues: Vec<NewIssue>,
    pub behind: Vec<String>,
    pub conflicts: Vec<crate::store::state::ConflictRow>,
    pub staged_transitions: Vec<StagedTransition>,
    pub pending_ops: Vec<crate::store::state::PendingOp>,
    pub warnings: Vec<Warning>,
}

/// Something in a file that will be ignored unless the user fixes it.
#[derive(Debug, Clone, Serialize)]
pub struct Warning {
    pub key: String,
    pub file: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct WorkspaceStatus {
    pub root: String,
    pub project: Option<String>,
    pub base_url: Option<String>,
    pub deployment: Option<String>,
    pub issues: usize,
    pub last_fetch_at: Option<String>,
    pub last_fetch_age_seconds: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DirtyIssue {
    pub key: String,
    pub file: String,
    pub fields: Vec<String>,
    pub lossy_touched: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct NewIssue {
    pub key: String,
    pub file: String,
    pub summary: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct StagedTransition {
    pub key: String,
    pub from: Option<String>,
    pub to: String,
    pub origin: &'static str,
}

impl StatusReport {
    pub fn render(&self, out: Out) {
        out.say(format!(
            "workspace {} ({} issues){}",
            self.workspace.root,
            self.workspace.issues,
            self.workspace
                .project
                .as_ref()
                .map(|p| format!(", project {p}"))
                .unwrap_or_default()
        ));
        match (
            &self.workspace.last_fetch_at,
            self.workspace.last_fetch_age_seconds,
        ) {
            (Some(at), Some(age)) => out.say(format!("last fetch {at} ({})", humanize_age(age))),
            _ => out.say("never fetched — run `jired fetch`"),
        }

        if !self.new_issues.is_empty() {
            out.say(format!("\n{} new local issue(s):", self.new_issues.len()));
            for n in &self.new_issues {
                out.say(format!("  {}  {}", n.key, n.file));
            }
        }
        if !self.dirty.is_empty() {
            out.say(format!("\n{} issue(s) with local edits:", self.dirty.len()));
            for d in &self.dirty {
                out.say(format!(
                    "  {:<14} [{}]{}",
                    d.key,
                    d.fields.join(", "),
                    if d.lossy_touched {
                        "  (read-only region edited)"
                    } else {
                        ""
                    }
                ));
            }
        }
        if !self.staged_transitions.is_empty() {
            out.say(format!(
                "\n{} staged transition(s):",
                self.staged_transitions.len()
            ));
            for t in &self.staged_transitions {
                out.say(format!(
                    "  {:<14} {} → {} ({})",
                    t.key,
                    t.from.as_deref().unwrap_or("?"),
                    t.to,
                    t.origin
                ));
            }
        }
        if !self.behind.is_empty() {
            out.say(format!(
                "\n{} issue(s) changed on the server since the last pull:",
                self.behind.len()
            ));
            out.say(format!("  {}", self.behind.join(", ")));
        }
        if !self.conflicts.is_empty() {
            out.say(format!("\n{} recorded conflict(s):", self.conflicts.len()));
            for c in &self.conflicts {
                out.say(format!(
                    "  {:<14} {}{}",
                    c.issue_key,
                    c.field,
                    c.resolved
                        .as_ref()
                        .map(|r| format!("  (resolved: {r})"))
                        .unwrap_or_default()
                ));
            }
            out.say("resolve with `jired resolve`");
        }
        if !self.pending_ops.is_empty() {
            out.say(format!(
                "\n{} unfinished push operation(s) — run `jired push --resume`:",
                self.pending_ops.len()
            ));
            for op in &self.pending_ops {
                out.say(format!(
                    "  {:<14} {} ({}){}",
                    op.issue_key,
                    op.kind,
                    op.status,
                    op.error
                        .as_ref()
                        .map(|e| format!(": {e}"))
                        .unwrap_or_default()
                ));
            }
        }
        for w in &self.warnings {
            out.warn(format!("{}: {}", w.file, w.message));
        }
        if self.is_clean() {
            out.say("\nnothing to push, no conflicts");
        }
    }

    pub fn is_clean(&self) -> bool {
        self.dirty.is_empty()
            && self.new_issues.is_empty()
            && self.conflicts.is_empty()
            && self.pending_ops.is_empty()
            && self.staged_transitions.is_empty()
    }
}

pub fn run(ctx: &mut Context, _args: &StatusArgs) -> Result<StatusReport> {
    let engine = Engine::open(ctx)?;
    let files = engine.projector.scan()?;
    let field_map = engine.field_map()?;
    let rows = engine.state.issues()?;

    let mut dirty = Vec::new();
    let mut new_issues = Vec::new();
    let mut behind = Vec::new();
    let mut staged = Vec::new();
    let mut warnings = Vec::new();

    for row in &rows {
        if row.key.starts_with("NEW-") {
            if let Some(f) = files.get(&row.key) {
                new_issues.push(NewIssue {
                    key: row.key.clone(),
                    file: f.relative.clone(),
                    summary: engine
                        .projector
                        .read(&f.relative)
                        .ok()
                        .map(|d| d.frontmatter.summary),
                });
            }
            continue;
        }
        if row.is_behind() && row.base.is_some() {
            behind.push(row.key.clone());
        }

        let (Some(base), Some(file)) = (&row.base, files.get(&row.key)) else {
            continue;
        };
        if let Ok(text) = engine.projector.read_raw(&file.relative) {
            for heading in crate::doc::misplaced_comment_headings(&text) {
                warnings.push(Warning {
                    key: row.key.clone(),
                    file: file.relative.clone(),
                    message: format!(
                        "`{heading}` is not under `## Comments`, so it will be ignored on push"
                    ),
                });
            }
        }

        let Some(hash) = engine.projector.hash_of(&file.relative) else {
            continue;
        };
        let staged_uploads = !crate::sync::attachments::pending_uploads(
            &engine.state,
            &row.key,
            &engine.projector.attachment_dir(&file.relative),
        )?
        .is_empty();
        if hash == base.md_hash && !staged_uploads {
            continue;
        }

        let working = match engine.projector.read(&file.relative) {
            Ok(d) => d,
            Err(e) => {
                out_warn(ctx, &format!("{}: {}", file.relative, e.message));
                continue;
            }
        };
        let base_doc = crate::sync::render::render_issue(
            &crate::api::normalize::issue_from_json(&base.raw, engine.markup(), &field_map)?,
            &crate::sync::render::RenderContext {
                markup: engine.markup(),
                base_url: engine.base_url(),
            },
        )?;

        let mut fields = Vec::new();
        for name in crate::doc::frontmatter::EDITABLE {
            if base_doc.doc.frontmatter.field(name) != working.frontmatter.field(name) {
                fields.push((*name).to_string());
            }
        }
        if base_doc.doc.description != working.description {
            fields.push("description".into());
        }
        if base_doc.doc.checklist != working.checklist {
            fields.push("checklist".into());
        }
        if working.new_comments().count() > 0 {
            fields.push("comments".into());
        }

        if working.frontmatter.status != base_doc.doc.frontmatter.status {
            staged.push(StagedTransition {
                key: row.key.clone(),
                from: Some(base_doc.doc.frontmatter.status.clone()),
                to: working.frontmatter.status.clone(),
                origin: "frontmatter",
            });
        }

        if staged_uploads {
            fields.push("attachments".into());
        }

        let raw = engine.state.raw_blocks(&row.key, "description")?;
        let lossy_touched = !crate::convert::edited_regions(&working.description, &raw).is_empty();

        if !fields.is_empty() || lossy_touched {
            dirty.push(DirtyIssue {
                key: row.key.clone(),
                file: file.relative.clone(),
                fields,
                lossy_touched,
            });
        }
    }

    let last_fetch = engine.state.last_fetch_at()?;
    let workspace = WorkspaceStatus {
        root: ctx.workspace.root().display().to_string(),
        project: ctx.resolver.get_value("workspace.project")?,
        base_url: Some(engine.base_url().to_string()),
        deployment: Some(engine.account.deployment.as_str().to_string()),
        issues: rows.len(),
        last_fetch_at: last_fetch.as_ref().map(crate::api::model::format_timestamp),
        last_fetch_age_seconds: last_fetch.map(|t| (chrono::Utc::now() - t).num_seconds().max(0)),
    };

    Ok(StatusReport {
        workspace,
        dirty,
        new_issues,
        behind,
        conflicts: engine.state.conflicts()?,
        staged_transitions: staged,
        pending_ops: engine.state.pending_ops()?,
        warnings,
    })
}

fn out_warn(ctx: &Context, message: &str) {
    ctx.out.warn(message);
}

pub fn humanize_age(seconds: i64) -> String {
    match seconds {
        s if s < 90 => format!("{s}s ago"),
        s if s < 5400 => format!("{}m ago", s / 60),
        s if s < 172_800 => format!("{}h ago", s / 3600),
        s => format!("{}d ago", s / 86_400),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn age_is_reported_in_a_readable_unit() {
        assert_eq!(humanize_age(5), "5s ago");
        assert_eq!(humanize_age(300), "5m ago");
        assert_eq!(humanize_age(7200), "2h ago");
        assert_eq!(humanize_age(300_000), "3d ago");
    }

    #[test]
    fn a_workspace_with_nothing_outstanding_is_clean() {
        let report = StatusReport {
            workspace: WorkspaceStatus {
                root: "/tmp".into(),
                project: None,
                base_url: None,
                deployment: None,
                issues: 0,
                last_fetch_at: None,
                last_fetch_age_seconds: None,
            },
            dirty: vec![],
            new_issues: vec![],
            behind: vec!["PROJ-1".into()],
            conflicts: vec![],
            staged_transitions: vec![],
            pending_ops: vec![],
            warnings: vec![],
        };
        // Being behind the server is not "unclean": a pull fixes it and there
        // is nothing of the user's to lose.
        assert!(report.is_clean());
    }
}
