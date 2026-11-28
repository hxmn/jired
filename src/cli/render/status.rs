//! How `status`'s report reads on a terminal.

use crate::json::Out;
use crate::sync::status::*;

impl StatusReport {
    pub fn render(&self, out: Out) {
        use crate::style::{self, BOLD, DIM};

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

        render_new_issues(out, &self.new_issues, width);
        render_dirty(out, &self.dirty, width);
        render_staged(out, &self.staged_transitions, width);
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
        render_conflicts(out, &self.conflicts, width);
        render_pending(out, &self.pending_ops, width);
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
            out.say(format!("\n{CLEAN}"));
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
}

impl WorkspaceState {
    /// The same sections and closing line `status` prints, without its header.
    pub fn render(&self, out: Out) {
        let width = self
            .new_issues
            .iter()
            .map(|n| n.key.len())
            .chain(self.dirty.iter().map(|d| d.key.len()))
            .chain(self.staged_transitions.iter().map(|t| t.key.len()))
            .chain(self.conflicts.iter().map(|c| c.issue_key.len()))
            .chain(self.pending_ops.iter().map(|o| o.issue_key.len()))
            .max()
            .unwrap_or(10)
            .clamp(8, 30);
        render_new_issues(out, &self.new_issues, width);
        render_dirty(out, &self.dirty, width);
        render_staged(out, &self.staged_transitions, width);
        render_conflicts(out, &self.conflicts, width);
        render_pending(out, &self.pending_ops, width);
        if self.clean {
            out.say(format!("\n{CLEAN}"));
        }
    }
}

fn render_new_issues(out: Out, new_issues: &[NewIssue], width: usize) {
    use crate::style::{self, BOLD, CYAN};
    if !new_issues.is_empty() {
        out.say(format!(
            "\n{}",
            style::out(BOLD, format!("{} new local issue(s):", new_issues.len()))
        ));
        for n in new_issues {
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
}

fn render_dirty(out: Out, dirty: &[DirtyIssue], width: usize) {
    use crate::style::{self, BOLD, CYAN, YELLOW};
    if !dirty.is_empty() {
        out.say(format!(
            "\n{}",
            style::out(BOLD, format!("{} issue(s) with local edits:", dirty.len()))
        ));
        for d in dirty {
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
}

fn render_staged(out: Out, staged: &[StagedTransition], width: usize) {
    use crate::style::{self, BOLD, CYAN};
    if !staged.is_empty() {
        out.say(format!(
            "\n{}",
            style::out(BOLD, format!("{} staged transition(s):", staged.len()))
        ));
        for t in staged {
            out.say(format!(
                "  {} {} → {} ({})",
                style::out(CYAN, format!("{:<width$}", t.key)),
                t.from.as_deref().unwrap_or("?"),
                t.to,
                t.origin
            ));
        }
        // How to take one back, not only how to send it: a move nobody
        // meant is the case where somebody reads this list most closely.
        let moved = staged.iter().any(|t| t.origin == "board-move");
        let edited = staged.iter().any(|t| t.origin == "frontmatter");
        out.hint("use \"jired push\" to send them");
        match (edited, moved) {
            (true, true) => out.hint(
                "to drop one, set `status:` back in the file or move the link back; \
                 \"jired doctor --regroup\" puts every card back",
            ),
            (true, false) => out.hint("to drop one, set `status:` back in the file"),
            (false, _) => out.hint(
                "to drop one, move the link back; \"jired doctor --regroup\" puts every card back",
            ),
        }
    }
}

fn render_conflicts(out: Out, conflicts: &[crate::store::state::ConflictRow], width: usize) {
    use crate::style::{self, RED};
    if !conflicts.is_empty() {
        out.say(format!(
            "\n{}",
            style::out(RED, format!("{} recorded conflict(s):", conflicts.len()))
        ));
        for c in conflicts {
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
}

fn render_pending(out: Out, pending_ops: &[crate::store::state::PendingOp], width: usize) {
    use crate::style::{self, YELLOW};
    if !pending_ops.is_empty() {
        out.say(format!(
            "\n{}",
            style::out(
                YELLOW,
                format!("{} unfinished push operation(s):", pending_ops.len())
            )
        ));
        for op in pending_ops {
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
}
