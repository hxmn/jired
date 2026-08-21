//! `jired diff` [M2.5] — offline comparison of working copy, base, and remote.

use crate::api::normalize::FieldMap;
use crate::api::Markup;
use crate::cli::cmd::diff::{DiffArgs, DiffMode};
use crate::cli::context::Context;
use crate::doc::IssueDoc;
use crate::error::Result;
use crate::json::Out;
use crate::sync::render::{render_issue, RenderContext};
use crate::sync::Engine;
use serde::Serialize;
use similar::TextDiff;

#[derive(Debug, Clone, Serialize)]
pub struct DiffReport {
    pub mode: DiffMode,
    pub issues: Vec<IssueDiff>,
    pub stat: Stat,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct Stat {
    pub issues: usize,
    pub fields: usize,
    pub added: usize,
    pub removed: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct IssueDiff {
    pub key: String,
    pub file: Option<String>,
    pub status: &'static str,
    pub fields: Vec<FieldDiff>,
    pub sections: Vec<SectionDiff>,
}

#[derive(Debug, Clone, Serialize)]
pub struct FieldDiff {
    pub field: String,
    pub from: serde_json::Value,
    pub to: serde_json::Value,
}

#[derive(Debug, Clone, Serialize)]
pub struct SectionDiff {
    pub section: String,
    pub unified: String,
    pub added: usize,
    pub removed: usize,
}

impl DiffReport {
    pub fn render(&self, out: Out, stat_only: bool) {
        use crate::style::{self, BOLD, CYAN, DIM, GREEN, RED};

        if self.issues.is_empty() {
            out.say(format!("no differences between {}", self.mode.describe()));
            if self.mode == crate::cli::cmd::diff::DiffMode::WorkingBase {
                out.hint("use \"jired diff --against remote\" to see what a pull would bring in");
            }
            return;
        }

        // Widths from the data: a fixed 14 wraps on a long key, and a fixed 18
        // wraps on almost every custom field name.
        let key_width = self
            .issues
            .iter()
            .map(|i| i.key.len())
            .max()
            .unwrap_or(10)
            .clamp(8, 30);

        if stat_only {
            for issue in &self.issues {
                let added: usize = issue.sections.iter().map(|s| s.added).sum();
                let removed: usize = issue.sections.iter().map(|s| s.removed).sum();
                out.say(format!(
                    "{:<key_width$} {:>2} field(s), {} {}",
                    issue.key,
                    issue.fields.len(),
                    style::out(GREEN, format!("+{added}")),
                    style::out(RED, format!("-{removed}"))
                ));
            }
            out.say(format!(
                "{} issue(s), {} field(s) changed, {} {}",
                self.stat.issues,
                self.stat.fields,
                style::out(GREEN, format!("+{}", self.stat.added)),
                style::out(RED, format!("-{}", self.stat.removed))
            ));
            return;
        }

        let field_width = self
            .issues
            .iter()
            .flat_map(|i| i.fields.iter().map(|f| f.field.len()))
            .max()
            .unwrap_or(14)
            .clamp(10, 32);

        for issue in &self.issues {
            out.say(format!(
                "── {} {} {}",
                style::out(CYAN, &issue.key),
                style::out(DIM, format!("({})", issue.status)),
                issue.file.as_deref().unwrap_or("")
            ));
            for f in &issue.fields {
                out.say(format!(
                    "   {:<field_width$} {} → {}",
                    f.field,
                    style::out(RED, render_value(&f.from)),
                    style::out(GREEN, render_value(&f.to))
                ));
            }
            for s in &issue.sections {
                out.say(style::out(BOLD, format!("   --- {} ---", s.section)));
                for line in s.unified.trim_end().lines() {
                    // Colouring the diff body is the difference between reading
                    // it and counting `+` signs.
                    out.say(match line.chars().next() {
                        Some('+') => style::out(GREEN, line),
                        Some('-') => style::out(RED, line),
                        Some('@') => style::out(CYAN, line),
                        _ => line.to_string(),
                    });
                }
            }
        }
    }
}

fn render_value(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) if s.is_empty() => "(empty)".into(),
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Null => "(none)".into(),
        other => other.to_string(),
    }
}

pub fn run(ctx: &mut Context, args: &DiffArgs) -> Result<DiffReport> {
    let engine = Engine::open(ctx)?;
    let markup = engine.markup();
    let fields = engine.field_map()?;
    let base_url = engine.base_url().to_string();
    let rctx = RenderContext {
        markup,
        base_url: &base_url,
        ignored_custom: &fields.ignored,
    };
    let files = engine.projector.scan()?;
    let wanted: Vec<String> = args
        .issues
        .iter()
        .map(|k| crate::project::naming::key_from_input(k))
        .collect();

    let mut issues = Vec::new();
    let mut stat = Stat::default();

    for row in engine.state.issues()? {
        if !wanted.is_empty() && !wanted.contains(&row.key) {
            continue;
        }
        let (left, right) = match args.mode() {
            DiffMode::WorkingBase => (
                row.base
                    .as_ref()
                    .map(|b| snapshot_doc(&b.raw, markup, &fields, &rctx))
                    .transpose()?,
                working_doc(&engine, &files, &row.key)?,
            ),
            DiffMode::BaseRemote => (
                row.base
                    .as_ref()
                    .map(|b| snapshot_doc(&b.raw, markup, &fields, &rctx))
                    .transpose()?,
                row.remote
                    .as_ref()
                    .map(|r| snapshot_doc(&r.raw, markup, &fields, &rctx))
                    .transpose()?,
            ),
            DiffMode::WorkingRemote => (
                row.remote
                    .as_ref()
                    .map(|r| snapshot_doc(&r.raw, markup, &fields, &rctx))
                    .transpose()?,
                working_doc(&engine, &files, &row.key)?,
            ),
        };

        let status = match (&left, &right) {
            (None, Some(_)) => "added",
            (Some(_), None) => "deleted",
            _ => "modified",
        };
        let Some(diff) = compare(&row.key, left.as_ref(), right.as_ref(), status) else {
            continue;
        };
        let file = files.get(&row.key).map(|f| f.relative.clone());

        stat.issues += 1;
        stat.fields += diff.fields.len();
        stat.added += diff.sections.iter().map(|s| s.added).sum::<usize>();
        stat.removed += diff.sections.iter().map(|s| s.removed).sum::<usize>();
        issues.push(IssueDiff { file, ..diff });
    }

    issues.sort_by(|a, b| a.key.cmp(&b.key));
    Ok(DiffReport {
        mode: args.mode(),
        issues,
        stat,
    })
}

fn snapshot_doc(
    raw: &serde_json::Value,
    markup: Markup,
    fields: &FieldMap,
    rctx: &RenderContext<'_>,
) -> Result<IssueDoc> {
    let issue = crate::api::normalize::issue_from_json(raw, markup, fields)?;
    Ok(render_issue(&issue, rctx)?.doc)
}

fn working_doc(
    engine: &Engine,
    files: &std::collections::BTreeMap<String, crate::project::FoundFile>,
    key: &str,
) -> Result<Option<IssueDoc>> {
    match files.get(key) {
        Some(f) => Ok(Some(engine.projector.read(&f.relative)?)),
        None => Ok(None),
    }
}

/// Compare two versions of an issue, returning None when they match.
fn compare(
    key: &str,
    left: Option<&IssueDoc>,
    right: Option<&IssueDoc>,
    status: &'static str,
) -> Option<IssueDiff> {
    let mut fields = Vec::new();
    let mut sections = Vec::new();

    match (left, right) {
        (None, None) => return None,
        (l, r) => {
            let empty = IssueDoc {
                frontmatter: Default::default(),
                heading: String::new(),
                preamble: String::new(),
                description: String::new(),
                checklist: vec![],
                children: vec![],
                comments: vec![],
                worklogs: vec![],
                attachments: vec![],
                extra_sections: vec![],
            };
            let a = l.unwrap_or(&empty);
            let b = r.unwrap_or(&empty);

            for name in crate::doc::frontmatter::EDITABLE {
                let (from, to) = (a.frontmatter.field(name), b.frontmatter.field(name));
                if from != to {
                    fields.push(FieldDiff {
                        field: (*name).to_string(),
                        from: from.unwrap_or(serde_json::Value::Null),
                        to: to.unwrap_or(serde_json::Value::Null),
                    });
                }
            }
            if let Some(d) = unified("Description", &a.description, &b.description) {
                sections.push(d);
            }
            let (ca, cb) = (checklist_text(a), checklist_text(b));
            if let Some(d) = unified("Checklist", &ca, &cb) {
                sections.push(d);
            }
            let (na, nb) = (children_text(a), children_text(b));
            if let Some(d) = unified("Children", &na, &nb) {
                sections.push(d);
            }
            for c in &b.comments {
                let crate::doc::CommentId::Existing(id) = &c.id else {
                    sections.push(SectionDiff {
                        section: "Comments/[new]".into(),
                        unified: c.body.lines().map(|l| format!("+{l}\n")).collect(),
                        added: c.body.lines().count(),
                        removed: 0,
                    });
                    continue;
                };
                let before = a.comment(id).map(|x| x.body.clone()).unwrap_or_default();
                if let Some(d) = unified(&format!("Comments/c-{id}"), &before, &c.body) {
                    sections.push(d);
                }
            }
            // A worklog's duration and start time are as much of the entry as
            // its note, so the whole heading is what gets compared.
            let (wa, wb) = (worklog_text(a), worklog_text(b));
            if let Some(d) = unified("Worklogs", &wa, &wb) {
                sections.push(d);
            }
        }
    }

    (!fields.is_empty() || !sections.is_empty()).then(|| IssueDiff {
        key: key.to_string(),
        file: None,
        status,
        fields,
        sections,
    })
}

fn checklist_text(doc: &IssueDoc) -> String {
    doc.checklist
        .iter()
        .map(|entry| match entry {
            crate::doc::ChecklistEntry::Header(h) => format!("### {h}"),
            crate::doc::ChecklistEntry::Item(i) => format!(
                "- [{}] {}{}",
                if i.checked { 'x' } else { ' ' },
                i.status
                    .as_deref()
                    .map(|s| format!("`{s}` "))
                    .unwrap_or_default(),
                i.title
            ),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn children_text(doc: &IssueDoc) -> String {
    doc.children
        .iter()
        .map(|i| {
            format!(
                "- [{}] {}{}",
                if i.checked { 'x' } else { ' ' },
                i.key
                    .as_deref()
                    .map(|k| format!("{k} "))
                    .unwrap_or_default(),
                i.title
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn worklog_text(doc: &IssueDoc) -> String {
    doc.worklogs
        .iter()
        .map(|w| {
            let id = match &w.id {
                crate::doc::WorklogId::Existing(id) => format!("w-{id}"),
                crate::doc::WorklogId::New => "new".to_string(),
            };
            let mut line = format!("[{id}] {} {} {}", w.time_spent, w.author, w.started);
            for note in w.body.lines() {
                line.push_str(&format!("\n  {note}"));
            }
            line
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn unified(section: &str, from: &str, to: &str) -> Option<SectionDiff> {
    if from == to {
        return None;
    }
    let diff = TextDiff::from_lines(from, to);
    let mut added = 0;
    let mut removed = 0;
    let mut text = String::new();
    for change in diff.iter_all_changes() {
        let sign = match change.tag() {
            similar::ChangeTag::Delete => {
                removed += 1;
                '-'
            }
            similar::ChangeTag::Insert => {
                added += 1;
                '+'
            }
            similar::ChangeTag::Equal => ' ',
        };
        text.push(sign);
        text.push_str(change.value());
        if !change.value().ends_with('\n') {
            text.push('\n');
        }
    }
    Some(SectionDiff {
        section: section.to_string(),
        unified: text,
        added,
        removed,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doc::{Frontmatter, SyncMeta};

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
            worklogs: Vec::new(),
            attachments: vec![],
            extra_sections: vec![],
        }
    }

    #[test]
    fn identical_documents_produce_no_diff() {
        let a = doc("s", "body");
        assert!(compare("PROJ-1", Some(&a), Some(&a), "modified").is_none());
    }

    #[test]
    fn a_field_change_is_reported_with_both_values() {
        let a = doc("Old", "body");
        let b = doc("New", "body");
        let d = compare("PROJ-1", Some(&a), Some(&b), "modified").unwrap();
        assert_eq!(d.fields.len(), 1);
        assert_eq!(d.fields[0].field, "summary");
        assert_eq!(d.fields[0].from, "Old");
        assert_eq!(d.fields[0].to, "New");
        assert!(d.sections.is_empty());
    }

    #[test]
    fn a_body_change_produces_a_unified_diff() {
        let a = doc("s", "one\ntwo\n");
        let b = doc("s", "one\nTWO\n");
        let d = compare("PROJ-1", Some(&a), Some(&b), "modified").unwrap();
        assert_eq!(d.sections.len(), 1);
        assert_eq!(d.sections[0].section, "Description");
        assert_eq!(d.sections[0].added, 1);
        assert_eq!(d.sections[0].removed, 1);
        assert!(d.sections[0].unified.contains("-two"));
        assert!(d.sections[0].unified.contains("+TWO"));
    }

    #[test]
    fn a_new_local_comment_shows_as_added_lines() {
        let a = doc("s", "b");
        let mut b = a.clone();
        b.comments.push(crate::doc::CommentBlock {
            id: crate::doc::CommentId::New,
            author: String::new(),
            timestamp: String::new(),
            note: String::new(),
            body: "hello".into(),
        });
        let d = compare("PROJ-1", Some(&a), Some(&b), "modified").unwrap();
        assert_eq!(d.sections[0].section, "Comments/[new]");
        assert_eq!(d.sections[0].added, 1);
    }

    #[test]
    fn a_missing_side_still_diffs_against_empty() {
        let b = doc("s", "body");
        let d = compare("PROJ-1", None, Some(&b), "added").unwrap();
        assert_eq!(d.status, "added");
        assert!(d.fields.iter().any(|f| f.field == "summary"));
    }

    #[test]
    fn child_changes_are_visible() {
        let a = doc("s", "b");
        let mut b = a.clone();
        b.children.push(crate::doc::ChildItem {
            checked: true,
            key: Some("PROJ-2".into()),
            title: "Sub".into(),
        });
        let d = compare("PROJ-1", Some(&a), Some(&b), "modified").unwrap();
        assert!(d.sections.iter().any(|s| s.section == "Children"));
    }

    #[test]
    fn checklist_changes_are_visible() {
        let a = doc("s", "b");
        let mut b = a.clone();
        b.checklist.push(crate::doc::ChecklistEntry::Item(
            crate::doc::ChecklistItem {
                checked: false,
                status: Some("IN PROGRESS".into()),
                title: "Analyse".into(),
                description: None,
            },
        ));
        let d = compare("PROJ-1", Some(&a), Some(&b), "modified").unwrap();
        let section = d
            .sections
            .iter()
            .find(|s| s.section == "Checklist")
            .expect("the checklist section is diffed");
        assert!(section.unified.contains("+- [ ] `IN PROGRESS` Analyse"));
    }
}
