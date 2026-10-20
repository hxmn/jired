//! `jired pull` [M1.11, M2.4] — fetch, then merge into the working copy.
//!
//! Under the default strategy a pull is all-or-nothing: if any issue conflicts,
//! nothing is written at all. That is what makes it safe to run `pull` on a
//! dirty workspace without reading the output first.

use crate::api::normalize::FieldMap;
use crate::api::{Markup, RemoteIssue};
use crate::cli::cmd::pull::{PullArgs, Strategy};
use crate::cli::context::Context;
use crate::doc::{ChecklistEntry, CommentBlock, CommentId, IssueDoc, WorklogEntry, WorklogId};
use crate::error::{JiredError, Result};
use crate::json::Out;
use crate::sync::merge::{self, Decision};
use crate::sync::render::{render_issue, RenderContext};
use crate::sync::{attachments, fetch, Engine};
use serde::Serialize;

#[derive(Debug, Clone, Serialize, Default)]
pub struct PullReport {
    pub strategy: String,
    /// True when the run planned the writes but made none.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub dry_run: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fetched: Option<fetch::FetchReport>,
    pub created: Vec<IssueChange>,
    pub updated: Vec<IssueChange>,
    pub deleted: Vec<IssueChange>,
    pub renamed: Vec<Rename>,
    pub unchanged: usize,
    pub conflicts: Vec<Conflict>,
    /// Conflicts `--strategy ours` or `theirs` settled by taking that side.
    ///
    /// Not under `conflicts`, which means one still standing — nothing
    /// written, or markers to edit. These were written: `resolve --theirs`
    /// listed the issue it had just resolved as a conflict, beside a `status`
    /// that rightly showed none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub decided: Vec<Conflict>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub groups: Option<crate::sync::group::GroupReport>,
    /// Things that went wrong without failing the pull — a missing attachment,
    /// a grouping folder that could not be rebuilt.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct IssueChange {
    pub key: String,
    pub file: Option<String>,
    /// Names of what changed, at merge granularity — `custom` covers every
    /// custom field, `description` the whole body.
    pub fields: Vec<String>,
    /// What the pull brought in, field by field, for showing a person.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub changes: Vec<FieldChange>,
}

/// One field's before and after, in the form a summary line wants.
///
/// Body-like fields report line counts instead of values: printing a paragraph
/// twice is not a summary, and `jired diff` already exists for the full text.
#[derive(Debug, Clone, Serialize)]
pub struct FieldChange {
    /// Stable identifier: `status`, `description`, `custom.customfield_10205`.
    pub field: String,
    /// Human name of a custom field, when the instance told us one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub added: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub removed: Option<usize>,
}

impl FieldChange {
    /// The right-hand side of a summary line.
    pub fn describe(&self) -> String {
        if self.added.is_some() || self.removed.is_some() {
            let mut parts = Vec::new();
            if let Some(n) = self.added.filter(|n| *n > 0) {
                parts.push(format!("+{n}"));
            }
            if let Some(n) = self.removed.filter(|n| *n > 0) {
                parts.push(format!("-{n}"));
            }
            if let Some(extra) = &self.to {
                parts.push(extra.clone());
            }
            return parts.join(" ");
        }
        const UNSET: &str = "—";
        format!(
            "{} → {}",
            self.from.as_deref().unwrap_or(UNSET),
            self.to.as_deref().unwrap_or(UNSET)
        )
    }

    /// What to print in the left-hand column.
    ///
    /// A custom field goes by its name where the instance gave us one; failing
    /// that by its number, because `customfield_` in front of every id is six
    /// syllables of nothing.
    pub fn title(&self) -> String {
        match (&self.label, self.field.strip_prefix("custom.")) {
            (Some(name), _) => format!("custom.{name}"),
            (None, Some(id)) => format!("custom.{}", id.trim_start_matches("customfield_")),
            (None, None) => self.field.clone(),
        }
    }
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
        // What came off the server is half the story of a pull, and the report
        // used to keep it only in the JSON.
        if let Some(f) = &self.fetched {
            out.say(format!(
                "fetched {} issue(s) from the server, {} changed",
                f.fetched, f.changed
            ));
        }
        for c in &self.created {
            out.say(format!("  new       {}", c.key));
        }
        for c in &self.updated {
            if c.changes.is_empty() {
                out.say(format!("  updated   {}  [{}]", c.key, c.fields.join(", ")));
                continue;
            }
            out.say(format!("  updated   {}", c.key));
            for change in &c.changes {
                out.say(format!(
                    "              {:<18} {}",
                    change.title(),
                    change.describe()
                ));
            }
        }
        for r in &self.renamed {
            out.say(format!("  renamed   {}  {} → {}", r.key, r.from, r.to));
        }
        // Shown with the report rather than as they happen: a long pull would
        // otherwise scroll them off the top before anyone read them.
        for w in &self.warnings {
            out.say(crate::style::out(
                crate::style::YELLOW,
                format!("  warning   {w}"),
            ));
        }
        for d in &self.deleted {
            out.say(format!("  gone      {}  (no longer in scope)", d.key));
        }
        let side = match self.strategy.as_str() {
            "ours" => "kept the local copy",
            _ => "took the server's copy",
        };
        for d in &self.decided {
            out.say(format!(
                "  decided   {}  [{}] — {side}",
                d.key,
                d.fields.join(", ")
            ));
        }
        if self.conflicts.is_empty() {
            out.say(format!(
                "{} new, {} updated, {} unchanged",
                self.created.len(),
                self.updated.len(),
                self.unchanged
            ));
            if self.dry_run {
                out.say("dry run — no files were written");
            }
            return;
        }

        // `merge` is the one strategy that writes despite the conflict, so the
        // summary must not claim nothing was written.
        let wrote_markers = self.strategy == "merge";
        out.say(crate::style::out(
            crate::style::RED,
            format!(
                "{} conflicting issue(s); {}",
                self.conflicts.len(),
                if wrote_markers {
                    "conflict markers were written into the files"
                } else {
                    "nothing was written"
                }
            ),
        ));
        for c in &self.conflicts {
            out.say(format!("  conflict  {}  [{}]", c.key, c.fields.join(", ")));
        }
        if wrote_markers {
            out.hint("edit the <<<<<<< ======= >>>>>>> regions, then \"jired resolve --done\"");
        } else {
            out.hint("use \"jired pull --strategy merge\" to write conflict markers");
            out.hint("or \"jired resolve --ours\" / \"--theirs\" to take one side");
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
        ignored_custom: &fields.ignored,
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
                changes: vec![],
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
                    body_raw: remote_doc.body_raw,
                    previous: existing.clone(),
                    kind: if existing.is_some() {
                        ChangeKind::Updated
                    } else {
                        ChangeKind::Created
                    },
                    fields: vec![],
                    changes: vec![],
                });
            }
            (Some(base), Some(file)) => {
                let working_text = engine.projector.read_raw(file)?;
                let working_hash = crate::store::state::content_hash(&working_text);
                let dirty = working_hash != base.md_hash;
                let behind = row.is_behind();

                if !dirty && !behind {
                    report.unchanged += 1;
                    continue;
                }
                if !dirty {
                    let base_doc = render_issue(
                        &parse_snapshot(&base.raw, markup, &fields, &row.key)?,
                        &render_ctx,
                    )?
                    .doc;
                    let changes = field_changes(&base_doc, &remote_doc.doc, &fields);
                    planned.push(PlannedWrite {
                        key: row.key.clone(),
                        base_doc: remote_doc.doc.clone(),
                        doc: remote_doc.doc,
                        description_storage: remote_doc.description_storage,
                        description_raw: remote_doc.description_raw,
                        body_raw: remote_doc.body_raw,
                        previous: Some(file.clone()),
                        kind: ChangeKind::Updated,
                        fields: change_names(&changes),
                        changes,
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
                    let conflict = Conflict {
                        key: row.key.clone(),
                        file: Some(file.clone()),
                        fields: outcome.conflicts.clone(),
                    };
                    // Markers stand until someone edits them, so `merge` records
                    // the conflict for `resolve --done`; `ours` and `theirs` have
                    // already decided it.
                    if strategy == Strategy::Merge {
                        for field in &outcome.conflicts {
                            engine
                                .state
                                .record_conflict(&row.key, field, None, None, None)?;
                        }
                        report.conflicts.push(conflict);
                    } else {
                        report.decided.push(conflict);
                    }
                }

                let changes = field_changes(&base_doc, &remote_doc.doc, &fields);
                planned.push(PlannedWrite {
                    key: row.key.clone(),
                    base_doc: remote_doc.doc,
                    doc: outcome.doc,
                    description_storage: remote_doc.description_storage,
                    description_raw: remote_doc.description_raw,
                    body_raw: remote_doc.body_raw,
                    previous: Some(file.clone()),
                    kind: ChangeKind::Updated,
                    fields: outcome.merged_fields,
                    changes,
                });
            }
        }
    }

    if !report.conflicts.is_empty() && strategy == Strategy::Abort {
        report.created.clear();
        report.updated.clear();
        return Ok(report);
    }

    // `--dry-run` stops here: everything above is planning, everything below
    // writes. The plan is reported in the same shape a real pull produces, so
    // the preview and the outcome can be compared line for line.
    if args.dry_run {
        report.dry_run = true;
        for write in planned {
            let change = IssueChange {
                key: write.key,
                file: write.previous,
                fields: write.fields,
                changes: write.changes,
            };
            match write.kind {
                ChangeKind::Created => report.created.push(change),
                ChangeKind::Updated => report.updated.push(change),
            }
        }
        report.created.sort_by(|a, b| a.key.cmp(&b.key));
        report.updated.sort_by(|a, b| a.key.cmp(&b.key));
        return Ok(report);
    }

    let mut attachment_folders: Vec<(String, String)> = Vec::new();
    let writing = crate::sync::progress::Progress::bar(ctx.out, planned.len(), "writing");
    for mut write in planned {
        writing.set_message(format!("writing {}", write.key));
        writing.inc();
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
        for (container, blocks) in &write.body_raw {
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
        // Level with the server, so whatever an earlier push left on the
        // journal for this issue is spent: `--resume` would re-plan it from the
        // file and find nothing. The case it exists for is a checklist the
        // plugin applied after push stopped waiting — the op stayed `failed`
        // in `status` after the pull had taken the change in.
        if base_hash == outcome.content_hash {
            engine.state.clear_ops(&write.key)?;
        }

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
            changes: write.changes,
        };
        match write.kind {
            ChangeKind::Created => report.created.push(change),
            ChangeKind::Updated => report.updated.push(change),
        }
    }
    writing.finish();

    // Attachment content is fetched only when the network is already in play,
    // so a `--no-fetch` pull stays offline.
    if !args.no_fetch && !attachment_folders.is_empty() {
        let auto_max = ctx
            .resolver
            .get_u64("attachments.auto_download_max")?
            .unwrap_or(256 * 1024);
        let client = engine.client(ctx)?;
        let downloading =
            crate::sync::progress::Progress::bar(ctx.out, attachment_folders.len(), "attachments");
        let mut attachment_warnings = Vec::new();
        for (key, file) in &attachment_folders {
            let dir = engine.projector.attachment_dir(file);
            downloading.inc();
            // A missing attachment must not fail a pull that otherwise
            // succeeded; the stub still records what the issue has.
            if let Err(e) = attachments::sync_folder(
                &engine.state,
                client.as_ref(),
                key,
                &dir,
                auto_max,
                &mut attachment_warnings,
            ) {
                attachment_warnings.push(match &e.hint {
                    Some(h) => format!("{key}: {} — {h}", e.message),
                    None => format!("{key}: {}", e.message),
                });
            }
        }
        downloading.finish();
        // Collected, not printed here: `render` shows them once, in the report,
        // where a long pull cannot scroll them off the top.
        report.warnings.extend(attachment_warnings);
    }

    // Grouping folders are views over what was just written, so they are
    // rebuilt last and only when the working copy actually moved.
    if !attachment_folders.is_empty() {
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

    report.created.sort_by(|a, b| a.key.cmp(&b.key));
    report.updated.sort_by(|a, b| a.key.cmp(&b.key));
    report.conflicts.sort_by(|a, b| a.key.cmp(&b.key));
    report.decided.sort_by(|a, b| a.key.cmp(&b.key));
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
    /// Lossy regions of the comment and worklog bodies, keyed by container.
    body_raw: std::collections::BTreeMap<String, Vec<crate::convert::ast::RawBlock>>,
    previous: Option<String>,
    kind: ChangeKind,
    fields: Vec<String>,
    changes: Vec<FieldChange>,
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

/// What the server changed between the two snapshots, field by field.
///
/// This is deliberately base→remote rather than "what the merge did": the
/// question a pull answers is what arrived from Jira, and after a merge that is
/// still the interesting half.
fn field_changes(base: &IssueDoc, remote: &IssueDoc, names: &FieldMap) -> Vec<FieldChange> {
    let mut out = Vec::new();

    for name in crate::doc::frontmatter::EDITABLE {
        if *name == "custom" {
            continue;
        }
        let (b, r) = (base.frontmatter.field(name), remote.frontmatter.field(name));
        if b != r {
            out.push(FieldChange {
                field: (*name).to_string(),
                label: None,
                from: b.as_ref().and_then(show),
                to: r.as_ref().and_then(show),
                added: None,
                removed: None,
            });
        }
    }

    // Custom fields are listed one by one: "custom changed" tells nobody
    // anything, and their ids are unreadable without the instance's own name.
    let mut ids: Vec<&String> = base
        .frontmatter
        .custom
        .keys()
        .chain(remote.frontmatter.custom.keys())
        .collect();
    ids.sort();
    ids.dedup();
    for id in ids {
        let b = base.frontmatter.custom.get(id);
        let r = remote.frontmatter.custom.get(id);
        if b == r {
            continue;
        }
        out.push(FieldChange {
            field: format!("custom.{id}"),
            label: names.names.get(id).cloned(),
            from: b.and_then(show),
            to: r.and_then(show),
            added: None,
            removed: None,
        });
    }

    if base.description != remote.description {
        let (added, removed) = line_counts(&base.description, &remote.description);
        out.push(FieldChange {
            field: "description".into(),
            label: None,
            from: None,
            to: None,
            added: Some(added),
            removed: Some(removed),
        });
    }

    if let Some(change) = comment_changes(base, remote) {
        out.push(change);
    }

    if base.worklogs != remote.worklogs {
        let booked = |d: &IssueDoc| d.worklogs.len();
        out.push(FieldChange {
            field: "worklogs".into(),
            label: None,
            from: Some(format!("{} entries", booked(base))),
            to: Some(format!("{} entries", booked(remote))),
            added: None,
            removed: None,
        });
    }

    if base.children != remote.children {
        let done = |d: &IssueDoc| d.children.iter().filter(|i| i.checked).count();
        out.push(FieldChange {
            field: "children".into(),
            label: None,
            from: Some(format!("{}/{} done", done(base), base.children.len())),
            to: Some(format!("{}/{} done", done(remote), remote.children.len())),
            added: None,
            removed: None,
        });
    }

    if base.checklist != remote.checklist {
        let done = |d: &IssueDoc| {
            d.checklist
                .iter()
                .filter(|e| matches!(e, ChecklistEntry::Item(i) if i.checked))
                .count()
        };
        let total = |d: &IssueDoc| {
            d.checklist
                .iter()
                .filter(|e| matches!(e, ChecklistEntry::Item(_)))
                .count()
        };
        out.push(FieldChange {
            field: "checklist".into(),
            label: None,
            from: Some(format!("{}/{} done", done(base), total(base))),
            to: Some(format!("{}/{} done", done(remote), total(remote))),
            added: None,
            removed: None,
        });
    }

    out
}

/// New, removed and edited comments as one line.
fn comment_changes(base: &IssueDoc, remote: &IssueDoc) -> Option<FieldChange> {
    fn ids(d: &IssueDoc) -> Vec<&str> {
        d.comments
            .iter()
            .filter_map(|c| match &c.id {
                CommentId::Existing(id) => Some(id.as_str()),
                CommentId::New => None,
            })
            .collect()
    }
    let (before, after) = (ids(base), ids(remote));
    let added = after.iter().filter(|id| !before.contains(id)).count();
    let removed = before.iter().filter(|id| !after.contains(id)).count();
    let edited = remote
        .comments
        .iter()
        .filter(|c| match &c.id {
            CommentId::Existing(id) => base.comment(id).is_some_and(|b| b.body != c.body),
            CommentId::New => false,
        })
        .count();

    (added + removed + edited > 0).then(|| FieldChange {
        field: "comments".into(),
        label: None,
        from: None,
        to: (edited > 0).then(|| format!("{edited} edited")),
        added: Some(added),
        removed: Some(removed),
    })
}

fn line_counts(from: &str, to: &str) -> (usize, usize) {
    let diff = similar::TextDiff::from_lines(from, to);
    let mut added = 0;
    let mut removed = 0;
    for change in diff.iter_all_changes() {
        match change.tag() {
            similar::ChangeTag::Insert => added += 1,
            similar::ChangeTag::Delete => removed += 1,
            similar::ChangeTag::Equal => {}
        }
    }
    (added, removed)
}

/// Longest a value may be on a summary line before it is cut short.
const VALUE_WIDTH: usize = 32;

/// A field value as one short line, or `None` when there is nothing there.
fn show(v: &serde_json::Value) -> Option<String> {
    let text = match v {
        serde_json::Value::Null => return None,
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Bool(b) => b.to_string(),
        serde_json::Value::Number(n) => n.to_string(),
        serde_json::Value::Array(items) if items.is_empty() => return None,
        serde_json::Value::Array(items) => items
            .iter()
            .map(|i| show(i).unwrap_or_default())
            .collect::<Vec<_>>()
            .join(", "),
        serde_json::Value::Object(map) if map.is_empty() => return None,
        other => other.to_string(),
    };
    let text = text.replace('\n', " ");
    if text.trim().is_empty() {
        return None;
    }
    Some(crate::project::naming::truncate(text.trim(), VALUE_WIDTH))
}

/// The merge-granularity names, derived from the per-field list.
fn change_names(changes: &[FieldChange]) -> Vec<String> {
    let mut names: Vec<String> = changes
        .iter()
        .map(|c| match c.field.split_once('.') {
            Some((head, _)) => head.to_string(),
            None => c.field.clone(),
        })
        .collect();
    names.dedup();
    names
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
    field!("children", children);
    field!("extra_sections", extra_sections);

    // A checklist edit is something push can send, so it merges like anything
    // else a person writes. Taking `remote` unconditionally, which is what a
    // read-only rendering wanted, would throw an unpushed one away.
    field!("checklist", checklist);
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

    out.worklogs = merge_worklogs(base, remote, working, strategy, &mut conflicts, &mut merged);

    // Nor unbooked local time.
    for w in working.new_worklogs() {
        out.worklogs.push(w.clone());
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

/// The worklog half of the merge.
///
/// An entry is three editable things at once — how long, when, and the note —
/// so they are compared together: a duration corrected on one side and a note
/// added on the other is still one person's edit of one entry.
fn merge_worklogs(
    base: &IssueDoc,
    remote: &IssueDoc,
    working: &IssueDoc,
    strategy: Strategy,
    conflicts: &mut Vec<String>,
    merged: &mut Vec<String>,
) -> Vec<WorklogEntry> {
    fn editable(w: &WorklogEntry) -> (&str, &str, &str) {
        (&w.time_spent, &w.started, &w.body)
    }

    let mut out = Vec::new();
    for r in &remote.worklogs {
        let WorklogId::Existing(id) = &r.id else {
            continue;
        };
        match (base.worklog(id), working.worklog(id)) {
            (Some(b), Some(w)) => {
                let d = merge::decide(&editable(b), &editable(r), &editable(w));
                if d.is_conflict() {
                    conflicts.push(format!("worklog:{id}"));
                }
                if d != Decision::Unchanged {
                    merged.push(format!("worklog:{id}"));
                }
                let mut kept = r.clone();
                let (time_spent, started, body) =
                    *merge::apply(d, &editable(r), &editable(w), strategy);
                kept.time_spent = time_spent.to_string();
                kept.started = started.to_string();
                kept.body = body.to_string();
                out.push(kept);
            }
            // Present remotely but not locally: somebody else booked time.
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
            children: vec![],
            comments: vec![],
            worklogs: vec![],
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

    fn worklog(id: &str, time_spent: &str, body: &str) -> WorklogEntry {
        WorklogEntry {
            id: WorklogId::Existing(id.into()),
            time_spent: time_spent.into(),
            author: "A".into(),
            started: "2026-08-15T09:00:00Z".into(),
            body: body.into(),
        }
    }

    #[test]
    fn unbooked_local_time_survives_a_pull() {
        let base = doc("s", "b", &[]);
        let remote = base.clone();
        let mut working = base.clone();
        working.worklogs.push(WorklogEntry {
            id: WorklogId::New,
            time_spent: "45m".into(),
            author: String::new(),
            started: String::new(),
            body: "not sent yet".into(),
        });

        let out = merge_docs(&base, &remote, &working, Strategy::Abort, "PROJ-1");
        assert_eq!(out.doc.worklogs.len(), 1);
        assert_eq!(out.doc.worklogs[0].id, WorklogId::New);
        assert_eq!(out.doc.worklogs[0].time_spent, "45m");
    }

    /// A duration corrected on one side and a note added on the other are one
    /// person's edit of one entry, so they are compared together.
    #[test]
    fn a_worklog_edited_on_both_sides_conflicts_across_all_of_its_parts() {
        let mut base = doc("s", "b", &[]);
        base.worklogs.push(worklog("w1", "1h", "first pass"));

        let mut remote = base.clone();
        remote.worklogs[0].time_spent = "2h".into();
        let mut working = base.clone();
        working.worklogs[0].body = "first pass, plus the retry".into();

        let out = merge_docs(&base, &remote, &working, Strategy::Abort, "PROJ-1");
        assert_eq!(out.conflicts, vec!["worklog:w1"]);

        // With only one side moving there is nothing to argue about.
        let out = merge_docs(&base, &remote, &base, Strategy::Abort, "PROJ-1");
        assert!(out.conflicts.is_empty());
        assert_eq!(out.doc.worklogs[0].time_spent, "2h");
    }

    #[test]
    fn time_booked_by_someone_else_arrives_without_a_conflict() {
        let base = doc("s", "b", &[]);
        let mut remote = base.clone();
        remote.worklogs.push(worklog("w9", "30m", "triage"));

        let out = merge_docs(&base, &remote, &base, Strategy::Abort, "PROJ-1");
        assert!(out.conflicts.is_empty());
        assert_eq!(out.doc.worklogs.len(), 1);
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
