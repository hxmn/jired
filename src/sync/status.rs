//! `jired status` [M2.10] — the one-screen answer to "where do I stand".
//!
//! Entirely offline: it reports the workspace against the last fetch, and says
//! how old that fetch is rather than silently going to the network.

use crate::cli::cmd::status::StatusArgs;
use crate::cli::context::Context;
use crate::error::Result;
use crate::json::Out;
use crate::sync::Engine;
use chrono::{DateTime, Utc};
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
    /// Generated instructions written by a different jired than this one.
    pub stale_agent_docs: Vec<StaleAgentDoc>,
    pub warnings: Vec<Warning>,
    /// Issues with no local changes; only collected under `--all`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub unchanged: Vec<UnchangedIssue>,
}

#[derive(Debug, Clone, Serialize)]
pub struct UnchangedIssue {
    pub key: String,
    pub file: String,
    pub status: String,
    pub summary: String,
}

/// A `CLAUDE.md` or `AGENTS.md` whose generated block is not this build's.
///
/// Worth a line of its own because a stale one is worse than none: it reads as
/// authoritative, and it is what an agent plans against before it runs
/// anything.
#[derive(Debug, Clone, Serialize)]
pub struct StaleAgentDoc {
    pub file: String,
    /// The version stamped in the file; `null` for a block written before jired
    /// started stamping one.
    pub generated_by: Option<String>,
    /// The version running now.
    pub expected: String,
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
    /// The same instant as `last_fetch_at`, kept so the rendered line can be
    /// shown in the reader's timezone while the JSON field stays UTC.
    #[serde(skip)]
    pub last_fetch: Option<DateTime<Utc>>,
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
        use crate::style::{self, BOLD, CYAN, DIM, RED, YELLOW};

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
            &self.workspace.last_fetch,
            self.workspace.last_fetch_age_seconds,
        ) {
            (Some(at), Some(age)) => out.say(style::out(
                DIM,
                format!(
                    "last fetch {} ({})",
                    crate::api::model::format_local(at),
                    humanize_age(age)
                ),
            )),
            _ => {
                out.say("never fetched");
                out.hint("use \"jired pull\" to fetch and write issue files");
            }
        }

        // One width for every key column, computed from the data: a fixed 14
        // wraps as soon as a project key is long.
        let width = self.key_width();

        if !self.new_issues.is_empty() {
            out.say(format!(
                "\n{}",
                style::out(
                    BOLD,
                    format!("{} new local issue(s):", self.new_issues.len())
                )
            ));
            for n in &self.new_issues {
                // Padded before painting: escape codes count towards a format
                // width and would knock every following column out of line.
                out.say(format!(
                    "  {}  {}",
                    style::out(CYAN, format!("{:<width$}", n.key)),
                    n.file
                ));
            }
            out.hint("use \"jired push\" to create them in Jira");
        }
        if !self.dirty.is_empty() {
            out.say(format!(
                "\n{}",
                style::out(
                    BOLD,
                    format!("{} issue(s) with local edits:", self.dirty.len())
                )
            ));
            for d in &self.dirty {
                out.say(format!(
                    "  {} [{}]{}",
                    style::out(CYAN, format!("{:<width$}", d.key)),
                    d.fields.join(", "),
                    if d.lossy_touched {
                        style::out(YELLOW, "  (read-only region edited)")
                    } else {
                        String::new()
                    }
                ));
            }
            out.hint("use \"jired diff\" to review, \"jired push\" to send");
        }
        if !self.staged_transitions.is_empty() {
            out.say(format!(
                "\n{}",
                style::out(
                    BOLD,
                    format!("{} staged transition(s):", self.staged_transitions.len())
                )
            ));
            for t in &self.staged_transitions {
                out.say(format!(
                    "  {} {} → {} ({})",
                    style::out(CYAN, format!("{:<width$}", t.key)),
                    t.from.as_deref().unwrap_or("?"),
                    t.to,
                    t.origin
                ));
            }
        }
        if !self.behind.is_empty() {
            out.say(format!(
                "\n{}",
                style::out(
                    BOLD,
                    format!(
                        "{} issue(s) changed on the server since the last pull:",
                        self.behind.len()
                    )
                )
            ));
            out.say(format!("  {}", self.behind.join(", ")));
            out.hint("use \"jired pull\" to update them");
        }
        if !self.conflicts.is_empty() {
            out.say(format!(
                "\n{}",
                style::out(
                    RED,
                    format!("{} recorded conflict(s):", self.conflicts.len())
                )
            ));
            for c in &self.conflicts {
                out.say(format!(
                    "  {:<width$} {}{}",
                    c.issue_key,
                    c.field,
                    c.resolved
                        .as_ref()
                        .map(|r| format!("  (resolved: {r})"))
                        .unwrap_or_default()
                ));
            }
            out.hint("use \"jired resolve --ours|--theirs\", or edit and \"jired resolve --done\"");
        }
        if !self.pending_ops.is_empty() {
            out.say(format!(
                "\n{}",
                style::out(
                    YELLOW,
                    format!("{} unfinished push operation(s):", self.pending_ops.len())
                )
            ));
            for op in &self.pending_ops {
                out.say(format!(
                    "  {:<width$} {} ({}){}",
                    op.issue_key,
                    op.kind,
                    op.status,
                    op.error
                        .as_ref()
                        .map(|e| format!(": {e}"))
                        .unwrap_or_default()
                ));
            }
            out.hint("use \"jired push --resume\" to finish them");
        }
        if !self.unchanged.is_empty() {
            out.say(format!(
                "\n{}",
                style::out(
                    BOLD,
                    format!("{} unchanged issue(s):", self.unchanged.len())
                )
            ));
            for u in &self.unchanged {
                out.say(format!(
                    "  {:<width$} {:<12} {}",
                    u.key, u.status, u.summary
                ));
            }
        }
        for w in &self.warnings {
            out.warn(format!("{}: {}", w.file, w.message));
        }
        // After the warnings and before the all-clear: it is not a reason to
        // stop, but an agent reading instructions from an older jired is
        // planning against a binary that is not here any more.
        for d in &self.stale_agent_docs {
            out.warn(format!(
                "{} was written by jired {}, and this is {}",
                d.file,
                d.generated_by.as_deref().unwrap_or("an earlier version"),
                d.expected
            ));
        }
        if !self.stale_agent_docs.is_empty() {
            out.hint("use \"jired init-agents\" to rewrite them, or \"jired doctor\"");
        }
        if self.is_clean() {
            out.say("\nnothing to push, no conflicts");
            if self.behind.is_empty() && self.workspace.last_fetch_at.is_some() {
                out.hint("use \"jired pull\" to check the server for changes");
            }
        }
    }

    fn key_width(&self) -> usize {
        let widest = self
            .new_issues
            .iter()
            .map(|n| n.key.len())
            .chain(self.dirty.iter().map(|d| d.key.len()))
            .chain(self.staged_transitions.iter().map(|t| t.key.len()))
            .chain(self.conflicts.iter().map(|c| c.issue_key.len()))
            .chain(self.pending_ops.iter().map(|o| o.issue_key.len()))
            .chain(self.unchanged.iter().map(|u| u.key.len()))
            .max()
            .unwrap_or(10);
        widest.clamp(8, 30)
    }

    pub fn is_clean(&self) -> bool {
        self.dirty.is_empty()
            && self.new_issues.is_empty()
            && self.conflicts.is_empty()
            && self.pending_ops.is_empty()
            && self.staged_transitions.is_empty()
    }
}

pub fn run(ctx: &mut Context, args: &StatusArgs) -> Result<StatusReport> {
    let engine = Engine::open(ctx)?;
    let files = engine.projector.scan()?;
    let field_map = engine.field_map()?;
    let rows = engine.state.issues()?;

    let mut dirty = Vec::new();
    let mut new_issues = Vec::new();
    let mut behind = Vec::new();
    let mut staged = Vec::new();
    let mut warnings = Vec::new();
    let mut unchanged = Vec::new();

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
            if args.all {
                if let Ok(doc) = engine.projector.read(&file.relative) {
                    unchanged.push(UnchangedIssue {
                        key: row.key.clone(),
                        file: file.relative.clone(),
                        status: doc.frontmatter.status.clone(),
                        summary: doc.frontmatter.summary.clone(),
                    });
                }
            }
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
                ignored_custom: &field_map.ignored,
            },
        )?;

        let mut fields = Vec::new();
        for name in crate::doc::frontmatter::EDITABLE {
            if base_doc.doc.frontmatter.field(name) != working.frontmatter.field(name) {
                // Sprint history is the server's record; a file that differs
                // only there has nothing to push, and calling it dirty would
                // leave it dirty for good.
                if *name == "sprint"
                    && crate::doc::frontmatter::sprint_history_only(
                        &base_doc.doc.frontmatter.sprint,
                        &working.frontmatter.sprint,
                    )
                {
                    continue;
                }
                fields.push((*name).to_string());
            }
        }
        if base_doc.doc.description != working.description {
            fields.push("description".into());
        }
        if base_doc.doc.checklist != working.checklist {
            fields.push("checklist".into());
        }
        if base_doc.doc.children != working.children {
            fields.push("children".into());
        }
        // An edit to a comment already on the server is unpushed work too, and
        // `push` plans an `update_comment` for it — reporting only the new ones
        // left `status` calling a file clean that the next push would send.
        if working.new_comments().count() > 0 || base_doc.doc.comments != working.comments {
            fields.push("comments".into());
        }
        // Staged time and edits to time already booked are both unpushed work.
        if working.new_worklogs().count() > 0 || base_doc.doc.worklogs != working.worklogs {
            fields.push("worklogs".into());
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

        // The same question `push` asks, asked the same way: a region edited in
        // a comment blocks the push too, and reporting only the description's
        // would leave the file looking pushable.
        let lossy_touched =
            !crate::sync::push::edited_regions(&engine.state, &row.key, &base_doc.doc, &working)?
                .is_empty();

        if !fields.is_empty() || lossy_touched {
            dirty.push(DirtyIssue {
                key: row.key.clone(),
                file: file.relative.clone(),
                fields,
                lossy_touched,
            });
        }
    }

    // A card dragged between columns is a status change the `.md` file knows
    // nothing about, so it can only be found by looking at the board folders.
    let (moves, move_warnings) =
        crate::sync::group::detect_moves(&engine, engine.projector.root())?;
    for m in moves {
        if staged.iter().any(|s| s.key == m.key) {
            continue;
        }
        staged.push(StagedTransition {
            key: m.key,
            from: Some(m.from_status),
            to: m.to_status,
            origin: "board-move",
        });
    }

    for message in move_warnings {
        warnings.push(Warning {
            key: String::new(),
            file: String::new(),
            message,
        });
    }

    let last_fetch = engine.state.last_fetch_at()?;
    let workspace = WorkspaceStatus {
        root: ctx.workspace.root().display().to_string(),
        project: ctx.resolver.get_quiet("workspace.project")?,
        base_url: Some(engine.base_url().to_string()),
        deployment: Some(engine.account.deployment.as_str().to_string()),
        issues: rows.len(),
        last_fetch_at: last_fetch.as_ref().map(crate::api::model::format_timestamp),
        last_fetch_age_seconds: last_fetch.map(|t| (chrono::Utc::now() - t).num_seconds().max(0)),
        last_fetch,
    };

    Ok(StatusReport {
        workspace,
        dirty,
        new_issues,
        behind,
        conflicts: engine.state.conflicts()?,
        staged_transitions: staged,
        pending_ops: engine.state.pending_ops()?,
        stale_agent_docs: stale_agent_docs(ctx.workspace.root()),
        warnings,
        unchanged,
    })
}

/// Generated instructions in this workspace that another jired wrote.
///
/// Only a file that already carries the block is reported: a workspace that
/// never had one asked for that, and `status` is not the place to sell it.
fn stale_agent_docs(root: &std::path::Path) -> Vec<StaleAgentDoc> {
    use crate::cli::cmd::init_agents::DocState;

    crate::cli::cmd::init_agents::doc_states(root)
        .into_iter()
        .filter_map(|(file, state)| match state {
            DocState::Stale { generated_by } => Some(StaleAgentDoc {
                file: file.to_string(),
                generated_by,
                expected: env!("CARGO_PKG_VERSION").to_string(),
            }),
            DocState::Current | DocState::Missing => None,
        })
        .collect()
}

fn out_warn(ctx: &Context, message: &str) {
    ctx.out.warn(message);
}

/// Issues whose working copy differs from the last sync.
///
/// Used by the commands that are about to overwrite files, so they can name what
/// would be lost before asking.
pub fn dirty_keys(ctx: &mut Context) -> Result<Vec<String>> {
    let report = run(ctx, &StatusArgs::default())?;
    let mut keys: Vec<String> = report.dirty.into_iter().map(|d| d.key).collect();
    keys.sort();
    Ok(keys)
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
                last_fetch: None,
                last_fetch_age_seconds: None,
            },
            dirty: vec![],
            new_issues: vec![],
            behind: vec!["PROJ-1".into()],
            conflicts: vec![],
            staged_transitions: vec![],
            pending_ops: vec![],
            stale_agent_docs: vec![],
            warnings: vec![],
            unchanged: vec![],
        };
        // Being behind the server is not "unclean": a pull fixes it and there
        // is nothing of the user's to lose.
        assert!(report.is_clean());
    }
}
