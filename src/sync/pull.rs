//! `jired pull` [M1.11, M2.4] — fetch, then merge into the working copy.
//!
//! Under the default strategy a pull is all-or-nothing: if any issue conflicts,
//! nothing is written at all. That is what makes it safe to run `pull` on a
//! dirty workspace without reading the output first.

use crate::api::normalize::FieldMap;
use crate::api::{Markup, RemoteIssue};
use crate::cli::cmd::pull::{PullArgs, Strategy};
use crate::cli::context::Context;
use crate::doc::{CommentBlock, CommentId, IssueDoc};
use crate::error::{JiredError, Result};
use crate::json::Out;
use crate::store::state::IssueRow;
use crate::sync::merge::{self, Decision};
use crate::sync::render::{render_issue, RenderContext};
use crate::sync::{attachments, fetch, Engine};
use serde::Serialize;

#[derive(Debug, Clone, Serialize, Default)]
pub struct PullReport {
    pub strategy: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fetched: Option<fetch::FetchReport>,
    pub created: Vec<IssueChange>,
    pub updated: Vec<IssueChange>,
    pub deleted: Vec<IssueChange>,
    pub renamed: Vec<Rename>,
    pub unchanged: usize,
    pub conflicts: Vec<Conflict>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub groups: Option<crate::sync::group::GroupReport>,
}

#[derive(Debug, Clone, Serialize)]
pub struct IssueChange {
    pub key: String,
    pub file: Option<String>,
    pub fields: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Rename {
    pub key: String,
    pub from: String,
    pub to: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Conflict {
    pub key: String,
    pub file: Option<String>,
    pub fields: Vec<String>,
}

impl PullReport {
    pub fn render(&self, out: Out) {
        for c in &self.created {
            out.say(format!("  new       {}", c.key));
        }
        for c in &self.updated {
            out.say(format!("  updated   {}  [{}]", c.key, c.fields.join(", ")));
        }
        for r in &self.renamed {
            out.say(format!("  renamed   {}  {} → {}", r.key, r.from, r.to));
        }
        for d in &self.deleted {
            out.say(format!("  gone      {}  (no longer in scope)", d.key));
        }
        if self.conflicts.is_empty() {
            out.say(format!(
                "{} new, {} updated, {} unchanged",
                self.created.len(),
                self.updated.len(),
                self.unchanged
            ));
        } else {
            out.say(format!(
                "{} conflicting issue(s); nothing was written",
                self.conflicts.len()
            ));
            for c in &self.conflicts {
                out.say(format!("  conflict  {}  [{}]", c.key, c.fields.join(", ")));
            }
            out.say("resolve with `jired pull --strategy merge`, --strategy ours|theirs, or edit and re-pull");
        }
    }
}

pub fn run(ctx: &mut Context, args: &PullArgs) -> Result<PullReport> {
    run_scoped(ctx, args, None)
}

/// Pull, optionally restricted to a set of issues.
///
/// `jired resolve` uses this to re-merge just the conflicted issues under a
/// chosen strategy, rather than reimplementing the merge a second way.
pub fn run_scoped(
    ctx: &mut Context,
    args: &PullArgs,
    only: Option<&[String]>,
) -> Result<PullReport> {
    let engine = Engine::open(ctx)?;
    let strategy = args.effective_strategy();

    let fetched = if args.no_fetch {
        None
    } else {
        let client = engine.client(ctx)?;
        Some(fetch::run_with(ctx, &engine, client.as_ref(), &args.scope)?)
    };

    let markup = engine.markup();
    let fields = engine.field_map()?;
    let base_url = engine.base_url().to_string();
    let render_ctx = RenderContext {
        markup,
        base_url: &base_url,
    };

    let files = engine.projector.scan()?;
    let mut report = PullReport {
        strategy: format!("{strategy:?}").to_lowercase(),
        fetched,
        ..Default::default()
    };

    // Plan everything before touching the filesystem so a conflict anywhere can
    // still abort the whole run.
    let mut planned: Vec<PlannedWrite> = Vec::new();

    for row in engine.state.issues()? {
        if row.key.starts_with("NEW-") {
            continue;
        }
        if only.is_some_and(|keys| !keys.contains(&row.key)) {
            continue;
        }
        let existing = files.get(&row.key).map(|f| f.relative.clone());

        if row.deleted_remote {
            report.deleted.push(IssueChange {
                key: row.key.clone(),
                file: existing,
                fields: vec![],
            });
            continue;
        }

        let Some(remote_snapshot) = &row.remote else {
            continue;
        };
        let remote_issue = parse_snapshot(&remote_snapshot.raw, markup, &fields, &row.key)?;
        let remote_doc = render_issue(&remote_issue, &render_ctx)?;

        match (&row.base, &existing) {
            // Never seen locally: write it fresh.
            (None, _) | (Some(_), None) => {
                planned.push(PlannedWrite {
                    key: row.key.clone(),
                    base_doc: remote_doc.doc.clone(),
                    doc: remote_doc.doc,
                    description_storage: remote_doc.description_storage,
                    description_raw: remote_doc.description_raw,
                    comment_raw: remote_doc.comment_raw,
                    previous: existing.clone(),
                    kind: if existing.is_some() {
                        ChangeKind::Updated
                    } else {
                        ChangeKind::Created
                    },
                    fields: vec![],
                });
            }
            (Some(base), Some(file)) => {
                let working_text = engine.projector.read_raw(file)?;
                let working_hash = crate::store::state::content_hash(&working_text);
                let dirty = working_hash != base.md_hash;
                let behind = remote_snapshot.updated > base.updated;

                if !dirty && !behind {
                    report.unchanged += 1;
                    continue;
                }
                if !dirty {
                    let changed = changed_fields(&row, markup, &fields, &base_url)?;
                    planned.push(PlannedWrite {
                        key: row.key.clone(),
                        base_doc: remote_doc.doc.clone(),
                        doc: remote_doc.doc,
                        description_storage: remote_doc.description_storage,
                        description_raw: remote_doc.description_raw,
                        comment_raw: remote_doc.comment_raw,
                        previous: Some(file.clone()),
                        kind: ChangeKind::Updated,
                        fields: changed,
                    });
                    continue;
                }
                if !behind {
                    // Local-only edits: leave the file exactly as the user left it.
                    report.unchanged += 1;
                    continue;
                }

                let base_issue = parse_snapshot(&base.raw, markup, &fields, &row.key)?;
                let base_doc = render_issue(&base_issue, &render_ctx)?.doc;
                let working_doc = IssueDoc::parse(&working_text)
                    .map_err(|e| JiredError::generic(format!("{file}: {}", e.message)))?;

                let label = format!(
                    "{}@{}",
                    row.key,
                    crate::api::model::format_timestamp(&remote_snapshot.updated)
                );
                let outcome =
                    merge_docs(&base_doc, &remote_doc.doc, &working_doc, strategy, &label);

                if !outcome.conflicts.is_empty() && strategy == Strategy::Abort {
                    for field in &outcome.conflicts {
                        engine
                            .state
                            .record_conflict(&row.key, field, None, None, None)?;
                    }
                    report.conflicts.push(Conflict {
                        key: row.key.clone(),
                        file: Some(file.clone()),
                        fields: outcome.conflicts,
                    });
                    continue;
                }
                if !outcome.conflicts.is_empty() {
                    for field in &outcome.conflicts {
                        engine
                            .state
                            .record_conflict(&row.key, field, None, None, None)?;
                    }
                    report.conflicts.push(Conflict {
                        key: row.key.clone(),
                        file: Some(file.clone()),
                        fields: outcome.conflicts.clone(),
                    });
                }

                planned.push(PlannedWrite {
                    key: row.key.clone(),
                    base_doc: remote_doc.doc,
                    doc: outcome.doc,
                    description_storage: remote_doc.description_storage,
                    description_raw: remote_doc.description_raw,
                    comment_raw: remote_doc.comment_raw,
                    previous: Some(file.clone()),
                    kind: ChangeKind::Updated,
                    fields: outcome.merged_fields,
                });
            }
        }
    }

    if !report.conflicts.is_empty() && strategy == Strategy::Abort {
        report.created.clear();
        report.updated.clear();
        return Ok(report);
    }

    let mut attachment_folders: Vec<(String, String)> = Vec::new();
    for mut write in planned {
        // The hash in the frontmatter must describe the file that contains it,
        // so it is written in two passes: render, hash, rewrite.
        write.doc.frontmatter.jired.content_hash = String::new();
        let provisional = write.doc.render()?;
        write.doc.frontmatter.jired.content_hash = crate::store::state::content_hash(&provisional);

        let outcome = engine
            .projector
            .write(&write.doc, write.previous.as_deref())?;
        attachment_folders.push((write.key.clone(), outcome.relative.clone()));
        // Ours and theirs decide, so the recorded conflict is spent. Merge
        // deliberately does not: it writes markers precisely so the conflict
        // survives until someone edits the file and runs `resolve --done`.
        if matches!(strategy, Strategy::Ours | Strategy::Theirs) {
            engine.state.clear_conflicts(&write.key)?;
        }
        engine
            .state
            .replace_raw_blocks(&write.key, "description", &write.description_raw)?;
        for (container, blocks) in &write.comment_raw {
            engine
                .state
                .replace_raw_blocks(&write.key, container, blocks)?;
        }
        // Hash the base rendering, not the file that was written: they are the
        // same for a clean pull and deliberately differ after a merge that kept
        // a local value.
        let base_hash = {
            let mut base_doc = write.base_doc.clone();
            base_doc.frontmatter.jired.content_hash = String::new();
            let provisional = base_doc.render()?;
            base_doc.frontmatter.jired.content_hash =
                crate::store::state::content_hash(&provisional);
            crate::store::state::content_hash(&base_doc.render()?)
        };

        engine.state.advance_base(
            &write.key,
            &write.description_storage,
            &base_hash,
            &outcome.relative,
        )?;

        if let Some(from) = &outcome.renamed_from {
            report.renamed.push(Rename {
                key: write.key.clone(),
                from: from.clone(),
                to: outcome.relative.clone(),
            });
        }
        let change = IssueChange {
            key: write.key.clone(),
            file: Some(outcome.relative),
            fields: write.fields,
        };
        match write.kind {
            ChangeKind::Created => report.created.push(change),
            ChangeKind::Updated => report.updated.push(change),
        }
    }

    // Attachment content is fetched only when the network is already in play,
    // so a `--no-fetch` pull stays offline.
    if !args.no_fetch && !attachment_folders.is_empty() {
        let auto_max = ctx
            .resolver
            .get_u64("attachments.auto_download_max")?
            .unwrap_or(256 * 1024);
        let client = engine.client(ctx)?;
        for (key, file) in &attachment_folders {
            let dir = engine.projector.attachment_dir(file);
            // A missing attachment must not fail a pull that otherwise
            // succeeded; the stub still records what the issue has.
            if let Err(e) =
                attachments::sync_folder(&engine.state, client.as_ref(), key, &dir, auto_max)
            {
                ctx.out.warn(format!("{key}: {}", e.message));
            }
        }
    }

    // Grouping folders are views over what was just written, so they are
    // rebuilt last and only when the working copy actually moved.
    if !attachment_folders.is_empty() {
        let options = crate::sync::group::GroupOptions::from_context(ctx)?;
        match crate::sync::group::regroup(&engine, engine.projector.root(), &options) {
            Ok(groups) => report.groups = Some(groups),
            Err(e) => ctx
                .out
                .warn(format!("cannot rebuild grouping folders: {}", e.message)),
        }
    }

    report.created.sort_by(|a, b| a.key.cmp(&b.key));
    report.updated.sort_by(|a, b| a.key.cmp(&b.key));
    report.conflicts.sort_by(|a, b| a.key.cmp(&b.key));
    Ok(report)
}

enum ChangeKind {
    Created,
    Updated,
}

struct PlannedWrite {
    key: String,
    doc: IssueDoc,
    /// The file as it would look rendered purely from the incoming snapshot.
    ///
    /// `base_md_hash` has to describe *base*, not whatever the merge decided to
    /// write. When a strategy keeps a local value the two differ, and recording
    /// the merged file instead would make the issue look clean while still
    /// holding unpushed edits.
    base_doc: IssueDoc,
    description_storage: String,
    description_raw: Vec<crate::convert::ast::RawBlock>,
    comment_raw: std::collections::BTreeMap<String, Vec<crate::convert::ast::RawBlock>>,
    previous: Option<String>,
    kind: ChangeKind,
    fields: Vec<String>,
}

fn parse_snapshot(
    raw: &serde_json::Value,
    markup: Markup,
    fields: &FieldMap,
    key: &str,
) -> Result<RemoteIssue> {
    crate::api::normalize::issue_from_json(raw, markup, fields).map_err(|e| {
        JiredError::corrupt(format!(
            "{key}: stored snapshot is unreadable: {}",
            e.message
        ))
        .with_hint("delete .jired/state.db and run `jired fetch` to rebuild it")
    })
}

/// Field names that differ between base and remote, for the change summary.
fn changed_fields(
    row: &IssueRow,
    markup: Markup,
    fields: &FieldMap,
    base_url: &str,
) -> Result<Vec<String>> {
    let (Some(base), Some(remote)) = (&row.base, &row.remote) else {
        return Ok(vec![]);
    };
    let ctx = RenderContext { markup, base_url };
    let b = render_issue(&parse_snapshot(&base.raw, markup, fields, &row.key)?, &ctx)?.doc;
    let r = render_issue(
        &parse_snapshot(&remote.raw, markup, fields, &row.key)?,
        &ctx,
    )?
    .doc;

    let mut changed = Vec::new();
    for name in crate::doc::frontmatter::EDITABLE {
        if b.frontmatter.field(name) != r.frontmatter.field(name) {
            changed.push((*name).to_string());
        }
    }
    if b.description != r.description {
        changed.push("description".into());
    }
    if b.comments.len() != r.comments.len() {
        changed.push("comments".into());
    }
    Ok(changed)
}

pub struct MergeOutcome {
    pub doc: IssueDoc,
    pub conflicts: Vec<String>,
    pub merged_fields: Vec<String>,
}

/// Three-way merge of two issue documents against their common base.
pub fn merge_docs(
    base: &IssueDoc,
    remote: &IssueDoc,
    working: &IssueDoc,
    strategy: Strategy,
    label: &str,
) -> MergeOutcome {
    // Start from remote so read-only fields (updated, url, reporter) are fresh.
    let mut out = remote.clone();
    let mut conflicts = Vec::new();
    let mut merged = Vec::new();

    macro_rules! field {
        ($name:literal, $($path:tt)+) => {{
            let d = merge::decide(&base.$($path)+, &remote.$($path)+, &working.$($path)+);
            if d.is_conflict() {
                conflicts.push($name.to_string());
            }
            if d != Decision::Unchanged {
                merged.push($name.to_string());
            }
            out.$($path)+ =
                merge::apply(d, &remote.$($path)+, &working.$($path)+, strategy).clone();
        }};
    }

    field!("type", frontmatter.issue_type);
    field!("status", frontmatter.status);
    field!("priority", frontmatter.priority);
    field!("summary", frontmatter.summary);
    field!("labels", frontmatter.labels);
    field!("components", frontmatter.components);
    field!("assignee", frontmatter.assignee);
    field!("sprint", frontmatter.sprint);
    field!("parent", frontmatter.parent);
    field!("epic", frontmatter.epic);
    field!("fix_versions", frontmatter.fix_versions);
    field!("affects_versions", frontmatter.affects_versions);
    field!("links", frontmatter.links);
    field!("story_points", frontmatter.story_points);
    field!("custom", frontmatter.custom);
    field!("checklist", checklist);
    field!("extra_sections", extra_sections);
    field!("preamble", preamble);

    // Body text merges at hunk granularity rather than all-or-nothing.
    let text = merge::merge_text(&base.description, &working.description, &remote.description);
    if text.has_conflict() {
        conflicts.push("description".into());
        out.description = match strategy {
            Strategy::Ours => text.render_side(false),
            Strategy::Theirs => text.render_side(true),
            _ => text.render(label),
        };
        merged.push("description".into());
    } else {
        let body = text.render(label);
        if body != base.description {
            merged.push("description".into());
        }
        out.description = body;
    }

    out.comments = merge_comments(base, remote, working, strategy, &mut conflicts, &mut merged);

    // Unpushed local comments must never be dropped by a pull.
    for c in working.new_comments() {
        out.comments.push(c.clone());
    }

    MergeOutcome {
        doc: out,
        conflicts,
        merged_fields: merged,
    }
}

fn merge_comments(
    base: &IssueDoc,
    remote: &IssueDoc,
    working: &IssueDoc,
    strategy: Strategy,
    conflicts: &mut Vec<String>,
    merged: &mut Vec<String>,
) -> Vec<CommentBlock> {
    let mut out = Vec::new();
    for r in &remote.comments {
        let CommentId::Existing(id) = &r.id else {
            continue;
        };
        let b = base.comment(id);
        let w = working.comment(id);
        match (b, w) {
            (Some(b), Some(w)) => {
                let d = merge::decide(&b.body, &r.body, &w.body);
                if d.is_conflict() {
                    conflicts.push(format!("comment:{id}"));
                }
                if d != Decision::Unchanged {
                    merged.push(format!("comment:{id}"));
                }
                let mut c = r.clone();
                c.body = merge::apply(d, &r.body, &w.body, strategy).clone();
                out.push(c);
            }
            // Present remotely but not locally: a new comment from someone else.
            _ => out.push(r.clone()),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doc::{Frontmatter, SyncMeta};

    fn doc(summary: &str, description: &str, labels: &[&str]) -> IssueDoc {
        IssueDoc {
            frontmatter: Frontmatter {
                key: "PROJ-1".into(),
                id: "1".into(),
                summary: summary.into(),
                labels: labels.iter().map(|s| s.to_string()).collect(),
                jired: SyncMeta::default(),
                ..Default::default()
            },
            heading: summary.into(),
            preamble: String::new(),
            description: description.into(),
            checklist: vec![],
            comments: vec![],
            attachments: vec![],
            extra_sections: vec![],
        }
    }

    #[test]
    fn disjoint_field_edits_both_survive() {
        let base = doc("Old", "body", &["a"]);
        let mut remote = base.clone();
        remote.frontmatter.summary = "Server summary".into();
        let mut working = base.clone();
        working.frontmatter.labels = vec!["a".into(), "local".into()];

        let out = merge_docs(&base, &remote, &working, Strategy::Abort, "PROJ-1");
        assert!(out.conflicts.is_empty(), "{:?}", out.conflicts);
        assert_eq!(out.doc.frontmatter.summary, "Server summary");
        assert_eq!(out.doc.frontmatter.labels, ["a", "local"]);
    }

    #[test]
    fn the_same_field_changed_on_both_sides_conflicts() {
        let base = doc("Old", "body", &[]);
        let mut remote = base.clone();
        remote.frontmatter.summary = "Theirs".into();
        let mut working = base.clone();
        working.frontmatter.summary = "Ours".into();

        let out = merge_docs(&base, &remote, &working, Strategy::Abort, "PROJ-1");
        assert_eq!(out.conflicts, vec!["summary"]);
        // Abort leaves the local value in place; the caller discards the doc.
        assert_eq!(out.doc.frontmatter.summary, "Ours");
    }

    #[test]
    fn identical_edits_on_both_sides_are_not_a_conflict() {
        let base = doc("Old", "body", &[]);
        let mut remote = base.clone();
        remote.frontmatter.summary = "Same".into();
        let working = remote.clone();

        let out = merge_docs(&base, &remote, &working, Strategy::Abort, "PROJ-1");
        assert!(out.conflicts.is_empty());
        assert_eq!(out.doc.frontmatter.summary, "Same");
    }

    #[test]
    fn strategy_theirs_resolves_conflicts_toward_the_server() {
        let base = doc("Old", "body", &[]);
        let mut remote = base.clone();
        remote.frontmatter.summary = "Theirs".into();
        let mut working = base.clone();
        working.frontmatter.summary = "Ours".into();

        let out = merge_docs(&base, &remote, &working, Strategy::Theirs, "PROJ-1");
        assert_eq!(out.doc.frontmatter.summary, "Theirs");
        assert_eq!(out.conflicts, vec!["summary"]);
    }

    #[test]
    fn body_edits_in_different_places_merge_cleanly() {
        let base = doc(
            "s",
            "line one\nline two\nline three\nline four\nline five",
            &[],
        );
        let mut remote = base.clone();
        remote.description = "LINE ONE\nline two\nline three\nline four\nline five".into();
        let mut working = base.clone();
        working.description = "line one\nline two\nline three\nline four\nLINE FIVE".into();

        let out = merge_docs(&base, &remote, &working, Strategy::Abort, "PROJ-1");
        assert!(out.conflicts.is_empty(), "{:?}", out.conflicts);
        assert!(out.doc.description.contains("LINE ONE"));
        assert!(out.doc.description.contains("LINE FIVE"));
    }

    #[test]
    fn overlapping_body_edits_produce_markers_under_merge_strategy() {
        let base = doc("s", "a\nb\nc", &[]);
        let mut remote = base.clone();
        remote.description = "a\ntheirs\nc".into();
        let mut working = base.clone();
        working.description = "a\nours\nc".into();

        let out = merge_docs(&base, &remote, &working, Strategy::Merge, "PROJ-1@t");
        assert_eq!(out.conflicts, vec!["description"]);
        assert!(merge::has_markers(&out.doc.description));
        assert!(out.doc.description.contains(">>>>>>> REMOTE (PROJ-1@t)"));
    }

    #[test]
    fn unpushed_local_comments_survive_a_pull() {
        let base = doc("s", "b", &[]);
        let remote = base.clone();
        let mut working = base.clone();
        working.comments.push(CommentBlock {
            id: CommentId::New,
            author: String::new(),
            timestamp: String::new(),
            note: String::new(),
            body: "my unpushed note".into(),
        });

        let out = merge_docs(&base, &remote, &working, Strategy::Abort, "PROJ-1");
        assert_eq!(out.doc.comments.len(), 1);
        assert_eq!(out.doc.comments[0].id, CommentId::New);
        assert_eq!(out.doc.comments[0].body, "my unpushed note");
    }

    #[test]
    fn a_comment_edited_on_both_sides_conflicts() {
        let mut base = doc("s", "b", &[]);
        let existing = CommentBlock {
            id: CommentId::Existing("c1".into()),
            author: "A".into(),
            timestamp: "t".into(),
            note: String::new(),
            body: "original".into(),
        };
        base.comments.push(existing.clone());

        let mut remote = base.clone();
        remote.comments[0].body = "server edit".into();
        let mut working = base.clone();
        working.comments[0].body = "local edit".into();

        let out = merge_docs(&base, &remote, &working, Strategy::Abort, "PROJ-1");
        assert_eq!(out.conflicts, vec!["comment:c1"]);
    }

    #[test]
    fn a_new_remote_comment_is_added_without_conflict() {
        let base = doc("s", "b", &[]);
        let mut remote = base.clone();
        remote.comments.push(CommentBlock {
            id: CommentId::Existing("c9".into()),
            author: "Someone".into(),
            timestamp: "t".into(),
            note: String::new(),
            body: "hello".into(),
        });
        let working = base.clone();

        let out = merge_docs(&base, &remote, &working, Strategy::Abort, "PROJ-1");
        assert!(out.conflicts.is_empty());
        assert_eq!(out.doc.comments.len(), 1);
    }

    #[test]
    fn read_only_fields_always_come_from_the_server() {
        let mut base = doc("s", "b", &[]);
        base.frontmatter.updated = Some("2026-08-01T00:00:00Z".into());
        let mut remote = base.clone();
        remote.frontmatter.updated = Some("2026-08-17T00:00:00Z".into());
        let mut working = base.clone();
        working.frontmatter.updated = Some("1999-01-01T00:00:00Z".into());

        let out = merge_docs(&base, &remote, &working, Strategy::Abort, "PROJ-1");
        assert_eq!(
            out.doc.frontmatter.updated.as_deref(),
            Some("2026-08-17T00:00:00Z"),
            "a local edit to a read-only field must be discarded"
        );
        assert!(out.conflicts.is_empty());
    }
}
