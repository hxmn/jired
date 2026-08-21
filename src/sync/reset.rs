//! `jired reset` — put the working copy back to the last synced state.
//!
//! The mirror of `pull --force`, pointing the other way. A forced pull moves a
//! file *forward* to whatever the server holds now; a reset moves it *back* to
//! what jired last wrote there. That earlier state is exactly what `push` diffs
//! against, so a reset issue has nothing left to send — the edits are gone, not
//! stashed.
//!
//! Every file is re-rendered from its base snapshot through the same renderer
//! `pull` uses, so a reset and a fresh pull of an unchanged issue produce the
//! same bytes. Nothing here touches the network: base is already on disk, which
//! is what makes this the recovery path for a file that no longer parses.
//!
//! Two kinds of local work are deliberately *not* dropped, because base cannot
//! bring them back: a staged attachment, whose only copy is the file in the
//! folder, and a locally created issue, which has no earlier state at all.
//! Both are reported rather than silently kept, and `--include-new` opts into
//! deleting the second.

use crate::cli::cmd::reset::ResetArgs;
use crate::cli::context::Context;
use crate::convert::ast::RawBlock;
use crate::doc::IssueDoc;
use crate::error::{JiredError, Result};
use crate::json::Out;
use crate::store::state::content_hash;
use crate::sync::render::{render_issue, RenderContext};
use crate::sync::{attachments, group, Engine};
use serde::Serialize;
use std::collections::BTreeMap;

#[derive(Debug, Clone, Serialize, Default)]
pub struct ResetReport {
    /// True when the run listed the work and did none of it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub dry_run: bool,
    /// Files put back to the last synced state.
    pub restored: Vec<ResetIssue>,
    /// Locally created issues deleted outright: with no earlier state to go
    /// back to, dropping their changes means dropping them.
    pub removed: Vec<ResetIssue>,
    /// Local work this left alone, and why.
    pub kept: Vec<KeptIssue>,
    /// Issues named on the command line that had nothing to drop.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub clean: Vec<String>,
    /// Cards dragged between board columns, put back where base says they go.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub moves_undone: Vec<String>,
    pub conflicts_cleared: usize,
    pub pending_ops_cleared: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub groups: Option<group::GroupReport>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ResetIssue {
    pub key: String,
    pub file: String,
    /// What the working copy was holding, named field by field. Empty when the
    /// file differs in a way no single field describes — or when it no longer
    /// parses, which is one of the better reasons to be running this.
    pub dropped: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct KeptIssue {
    pub key: String,
    pub file: String,
    pub reason: String,
}

impl ResetReport {
    /// Whether the run found anything at all worth reporting.
    pub fn is_empty(&self) -> bool {
        self.restored.is_empty()
            && self.removed.is_empty()
            && self.kept.is_empty()
            && self.clean.is_empty()
            && self.moves_undone.is_empty()
            && self.conflicts_cleared == 0
            && self.pending_ops_cleared == 0
    }

    /// The listing, shown as a preview before the question and again as the
    /// outcome after it. Same lines both times, so what was agreed to and what
    /// happened can be compared without reading two formats.
    pub fn render(&self, out: Out) {
        use crate::style::{self, CYAN, DIM, YELLOW};

        let width = self.key_width();
        for r in &self.restored {
            out.say(format!(
                "  {}  {}",
                style::out(CYAN, format!("{:<width$}", r.key)),
                if r.dropped.is_empty() {
                    r.file.clone()
                } else {
                    format!("[{}]", r.dropped.join(", "))
                }
            ));
        }
        for r in &self.removed {
            out.say(format!(
                "  {}  {}  {}",
                style::out(CYAN, format!("{:<width$}", r.key)),
                r.file,
                style::out(DIM, "(created locally, never pushed)")
            ));
        }
        for key in &self.moves_undone {
            out.say(format!(
                "  {}  {}",
                style::out(CYAN, format!("{:<width$}", key)),
                style::out(DIM, "board move put back")
            ));
        }
        for k in &self.kept {
            out.say(style::out(
                YELLOW,
                format!("  {:<width$}  kept: {}", k.key, k.reason),
            ));
        }
        for key in &self.clean {
            out.say(style::out(
                DIM,
                format!("  {key:<width$}  no local changes"),
            ));
        }
        for w in &self.warnings {
            out.warn(w);
        }
    }

    /// The one line that says what happened, after the work is done.
    pub fn summary(&self) -> String {
        let mut parts = Vec::new();
        if !self.restored.is_empty() {
            parts.push(format!(
                "{} file(s) put back to the last sync",
                self.restored.len()
            ));
        }
        if !self.removed.is_empty() {
            parts.push(format!("{} local issue(s) deleted", self.removed.len()));
        }
        if self.conflicts_cleared > 0 {
            parts.push(format!("{} conflict(s) cleared", self.conflicts_cleared));
        }
        if self.pending_ops_cleared > 0 {
            parts.push(format!(
                "{} unfinished push operation(s) discarded",
                self.pending_ops_cleared
            ));
        }
        if parts.is_empty() {
            return "nothing was changed".to_string();
        }
        parts.join(", ")
    }

    fn key_width(&self) -> usize {
        self.restored
            .iter()
            .chain(&self.removed)
            .map(|r| r.key.len())
            .chain(self.kept.iter().map(|k| k.key.len()))
            .chain(self.moves_undone.iter().map(String::len))
            .chain(self.clean.iter().map(String::len))
            .max()
            .unwrap_or(10)
            .clamp(8, 30)
    }
}

/// One file's worth of work, held between the listing and the confirmation.
struct Restore {
    key: String,
    file: String,
    /// The file as base renders it.
    doc: IssueDoc,
    description_raw: Vec<RawBlock>,
    body_raw: BTreeMap<String, Vec<RawBlock>>,
}

/// Everything the run intends to do, planned before anything is asked.
pub struct Plan {
    pub report: ResetReport,
    restores: Vec<Restore>,
    removals: Vec<(String, String)>,
    /// Every issue this changes. Their recorded conflicts and journal rows go
    /// with the edits, since both describe a local state that will not exist.
    keys: Vec<String>,
    regroup: bool,
}

impl Plan {
    /// How many issues this would change. A `kept` entry is not one of them,
    /// so a run that only has those has nothing to ask about.
    pub fn issues_affected(&self) -> usize {
        self.keys.len()
    }
}

/// Work out what a reset would drop, without dropping any of it.
pub fn plan(ctx: &mut Context, args: &ResetArgs) -> Result<Plan> {
    let engine = Engine::open(ctx)?;
    let wanted: Vec<String> = args
        .issues
        .iter()
        .map(|k| crate::project::naming::key_from_input(k))
        .collect();

    let files = engine.projector.scan()?;
    let rows = engine.state.issues()?;

    // A key nobody here has is a typo, and resetting "everything except the
    // issue you meant" is exactly the outcome worth an error rather than a
    // shrug.
    for key in &wanted {
        if !rows.iter().any(|r| &r.key == key) && !files.contains_key(key) {
            return Err(
                JiredError::usage(format!("`{key}` is not in this workspace"))
                    .with_detail("issue", key.clone())
                    .with_hint("run `jired status` to see what is here"),
            );
        }
    }
    let touches = |key: &str| wanted.is_empty() || wanted.iter().any(|k| k == key);

    let field_map = engine.field_map()?;
    let base_url = engine.base_url().to_string();
    let render_ctx = RenderContext {
        markup: engine.markup(),
        base_url: &base_url,
        ignored_custom: &field_map.ignored,
    };

    let mut report = ResetReport::default();
    let mut restores = Vec::new();
    let mut removals = Vec::new();
    let mut keys: Vec<String> = Vec::new();

    for row in &rows {
        if !touches(&row.key) {
            continue;
        }
        let Some(file) = files.get(&row.key) else {
            continue;
        };

        if row.key.starts_with(crate::doc::TEMP_KEY_PREFIX) {
            if args.include_new {
                keys.push(row.key.clone());
                removals.push((row.key.clone(), file.relative.clone()));
                report.removed.push(ResetIssue {
                    key: row.key.clone(),
                    file: file.relative.clone(),
                    dropped: vec!["the whole issue".into()],
                });
            } else {
                report.kept.push(KeptIssue {
                    key: row.key.clone(),
                    file: file.relative.clone(),
                    reason: "created locally and never pushed — `--include-new` deletes it".into(),
                });
            }
            continue;
        }

        let Some(base) = &row.base else {
            continue;
        };

        // The bytes on disk against the bytes the last sync left there: the
        // same test `push` uses to decide an issue is worth sending, so the two
        // commands cannot disagree about which files hold work.
        let dirty = engine
            .projector
            .hash_of(&file.relative)
            .is_some_and(|hash| hash != base.md_hash);

        let uploads = attachments::pending_uploads(
            &engine.state,
            &row.key,
            &engine.projector.attachment_dir(&file.relative),
        )?;
        if !uploads.is_empty() {
            report.kept.push(KeptIssue {
                key: row.key.clone(),
                file: file.relative.clone(),
                reason: format!(
                    "{} staged attachment upload(s) — the folder holds their only copy",
                    uploads.len()
                ),
            });
        }
        if !dirty {
            continue;
        }

        let base_issue =
            crate::api::normalize::issue_from_json(&base.raw, engine.markup(), &field_map)
                .map_err(|e| {
                    JiredError::corrupt(format!(
                        "{}: stored snapshot is unreadable: {}",
                        row.key, e.message
                    ))
                    .with_hint("delete .jired/state.db and run `jired fetch` to rebuild it")
                })?;
        let rendered = render_issue(&base_issue, &render_ctx)?;

        // Naming what goes needs the working copy parsed, which is precisely
        // what a file somebody broke will not do. That is not a reason to
        // refuse — it is the case reset exists for — so the listing loses its
        // detail and the file is rewritten anyway.
        let dropped = match engine.projector.read(&file.relative) {
            Ok(working) => dropped_fields(&rendered.doc, &working),
            Err(_) => {
                report.warnings.push(format!(
                    "{}: the file no longer parses; it is being rewritten from the last sync",
                    file.relative
                ));
                Vec::new()
            }
        };

        keys.push(row.key.clone());
        report.restored.push(ResetIssue {
            key: row.key.clone(),
            file: file.relative.clone(),
            dropped,
        });
        restores.push(Restore {
            key: row.key.clone(),
            file: file.relative.clone(),
            doc: rendered.doc,
            description_raw: rendered.description_raw,
            body_raw: rendered.body_raw,
        });
    }

    // A card dragged between board columns is a change no `.md` file records,
    // so only the folder tree can reveal it, and only a regroup can undo it.
    let (moves, move_warnings) = group::detect_moves(&engine, engine.projector.root())?;
    report.warnings.extend(move_warnings);
    for m in moves {
        if !touches(&m.key) {
            continue;
        }
        report.moves_undone.push(m.key.clone());
        if !keys.contains(&m.key) {
            keys.push(m.key);
        }
    }

    for c in engine.state.conflicts()? {
        if !touches(&c.issue_key) {
            continue;
        }
        report.conflicts_cleared += 1;
        if !keys.contains(&c.issue_key) {
            keys.push(c.issue_key);
        }
    }
    for op in engine.state.pending_ops()? {
        if !touches(&op.issue_key) {
            continue;
        }
        report.pending_ops_cleared += 1;
        if !keys.contains(&op.issue_key) {
            keys.push(op.issue_key);
        }
    }

    // An issue somebody named that turns out to be clean is worth a line: the
    // alternative is a command that says nothing and leaves them wondering
    // whether it looked.
    for key in &wanted {
        let touched = keys.contains(key) || report.kept.iter().any(|k| &k.key == key);
        if !touched {
            report.clean.push(key.clone());
        }
    }

    let regroup = !restores.is_empty() || !removals.is_empty() || !report.moves_undone.is_empty();
    Ok(Plan {
        report,
        restores,
        removals,
        keys,
        regroup,
    })
}

/// Carry out a plan. Everything destructive lives here and nowhere else.
pub fn apply(ctx: &mut Context, plan: Plan) -> Result<ResetReport> {
    let Plan {
        mut report,
        restores,
        removals,
        keys,
        regroup,
    } = plan;
    let engine = Engine::open(ctx)?;

    for r in restores {
        let mut doc = r.doc;
        // The hash in the frontmatter has to describe the file that contains
        // it, so it goes in two passes: render, hash, rewrite. The same order
        // `pull` uses — anything else and the restored file would not be
        // byte-identical to the one a pull writes.
        doc.frontmatter.jired.content_hash = String::new();
        let provisional = doc.render()?;
        doc.frontmatter.jired.content_hash = content_hash(&provisional);

        let outcome = engine.projector.write(&doc, Some(&r.file))?;
        engine
            .state
            .replace_raw_blocks(&r.key, "description", &r.description_raw)?;
        for (container, blocks) in &r.body_raw {
            engine.state.replace_raw_blocks(&r.key, container, blocks)?;
        }
        // Base itself does not move — that is the whole point of the command —
        // but an edited summary renamed the file, and the recorded hash has to
        // describe what is on disk now or `status` would go on calling the
        // issue dirty against a file that is already back.
        engine.state.set_file_path(&r.key, &outcome.relative)?;
        engine
            .state
            .set_base_md_hash(&r.key, &outcome.content_hash)?;
    }

    for (key, file) in &removals {
        engine.projector.remove(file)?;
        engine.state.forget_issue(key)?;
        engine.state.forget_new_issue_project(key)?;
    }

    // A recorded conflict describes a disagreement between a local edit and the
    // server's copy. The local edit is gone, so the disagreement is too. The
    // same goes for an interrupted push's journal: it lists work planned from a
    // file that no longer says any of it.
    for key in &keys {
        engine.state.clear_conflicts(key)?;
        engine.state.clear_ops(key)?;
    }

    // Grouping folders are views over what was just written, so they are
    // rebuilt last — and only when the working copy actually moved.
    if regroup {
        let options = group::GroupOptions::from_context(ctx)?;
        match group::regroup(&engine, engine.projector.root(), &options) {
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

/// What the working copy holds that base does not, named field by field.
///
/// Only used for the listing: the restore happens on the hash, so a field this
/// misses is still put back. Read-only fields are deliberately absent — a local
/// edit to `updated` was never going anywhere, and naming it as something the
/// user is about to lose would be a lie.
fn dropped_fields(base: &IssueDoc, working: &IssueDoc) -> Vec<String> {
    let mut fields = Vec::new();
    for name in crate::doc::frontmatter::EDITABLE {
        if base.frontmatter.field(name) != working.frontmatter.field(name) {
            fields.push((*name).to_string());
        }
    }
    if base.description != working.description {
        fields.push("description".into());
    }
    if base.children != working.children {
        fields.push("children".into());
    }
    // Bodies and ids only: a comment's rendered heading carries an author and a
    // timestamp that base and a parsed file can spell differently without
    // anybody having typed a thing.
    let comments_moved = base.comments.len() != working.comments.len()
        || base
            .comments
            .iter()
            .zip(&working.comments)
            .any(|(b, w)| b.id != w.id || b.body != w.body);
    if comments_moved {
        fields.push("comments".into());
    }
    // Staged time and edits to time already booked are both unpushed work, and
    // a `### [new]` worklog is the only record that anybody did those hours.
    if working.new_worklogs().count() > 0 || base.worklogs != working.worklogs {
        fields.push("worklogs".into());
    }
    if base.extra_sections != working.extra_sections {
        fields.push("extra sections".into());
    }
    fields
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doc::{CommentBlock, CommentId, Frontmatter, SyncMeta};

    fn doc(summary: &str, description: &str) -> IssueDoc {
        IssueDoc {
            frontmatter: Frontmatter {
                key: "PROJ-1".into(),
                id: "1".into(),
                summary: summary.into(),
                jired: SyncMeta::default(),
                ..Default::default()
            },
            heading: summary.into(),
            preamble: String::new(),
            description: description.into(),
            checklist: vec![],
            children: vec![],
            comments: vec![],
            worklogs: vec![],
            attachments: vec![],
            extra_sections: vec![],
        }
    }

    #[test]
    fn an_untouched_file_drops_nothing() {
        let base = doc("Fix login flow", "body");
        assert!(dropped_fields(&base, &base.clone()).is_empty());
    }

    #[test]
    fn every_kind_of_edit_is_named() {
        let base = doc("Fix login flow", "body");
        let mut working = base.clone();
        working.frontmatter.summary = "Fix login flow properly".into();
        working.frontmatter.labels = vec!["auth".into()];
        working.description = "body, rewritten".into();
        working.extra_sections = vec![("Notes".into(), "mine".into())];
        working.comments.push(CommentBlock {
            id: CommentId::New,
            author: String::new(),
            timestamp: String::new(),
            note: String::new(),
            body: "unpushed".into(),
        });

        assert_eq!(
            dropped_fields(&base, &working),
            [
                "summary",
                "labels",
                "description",
                "comments",
                "extra sections"
            ]
        );
    }

    /// A comment jired re-renders with a different author spelling has not been
    /// edited, and listing it as work about to be lost would be wrong.
    #[test]
    fn a_comment_whose_body_is_unchanged_is_not_reported() {
        let mut base = doc("s", "b");
        base.comments.push(CommentBlock {
            id: CommentId::Existing("c1".into()),
            author: "Jane D".into(),
            timestamp: "2026-08-01T00:00:00Z".into(),
            note: String::new(),
            body: "hello".into(),
        });
        let mut working = base.clone();
        working.comments[0].author = "Jane Doe".into();
        working.comments[0].note = "edited".into();

        assert!(dropped_fields(&base, &working).is_empty());
    }

    /// Read-only fields are the server's to set, so a stale `updated` in the
    /// file is not something the user is about to lose.
    #[test]
    fn a_read_only_field_is_not_named_as_dropped() {
        let base = doc("s", "b");
        let mut working = base.clone();
        working.frontmatter.updated = Some("1999-01-01T00:00:00Z".into());
        assert!(dropped_fields(&base, &working).is_empty());
    }
}
