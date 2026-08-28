//! Turning a fetched issue into an issue file [M1.11].
//!
//! This is the one place that decides what a `.md` file looks like for a given
//! server payload, so `pull` and `push` (which rewrites the file after a
//! confirmed write) cannot disagree about it.

use crate::api::{Markup, RemoteIssue};
use crate::convert::{self, ast::RawBlock};
use crate::doc::frontmatter::format_user;
use crate::doc::{
    AttachmentLine, ChildItem, CommentBlock, CommentId, Frontmatter, IssueDoc, SyncMeta,
    WorklogEntry, WorklogId,
};
use crate::error::Result;
use crate::project::naming;
use std::collections::{BTreeMap, BTreeSet};

/// The document plus the lossy regions that must be stored alongside it.
pub struct Rendered {
    pub doc: IssueDoc,
    pub description_raw: Vec<RawBlock>,
    /// Keyed by container, e.g. `comment:10812` or `worklog:10500`.
    pub body_raw: BTreeMap<String, Vec<RawBlock>>,
    /// Description as it will be stored in `base_desc`.
    pub description_storage: String,
}

pub struct RenderContext<'a> {
    pub markup: Markup,
    pub base_url: &'a str,
    /// Custom-field ids `ignored_custom_fields` names. They are left out of the
    /// file entirely — not emptied, which would push a clearing edit — while
    /// the values stay in the database and in the server payload.
    pub ignored_custom: &'a BTreeSet<String>,
}

impl RenderContext<'_> {
    /// A context that ignores nothing, for callers with no workspace config.
    pub fn none_ignored() -> &'static BTreeSet<String> {
        static NONE: std::sync::OnceLock<BTreeSet<String>> = std::sync::OnceLock::new();
        NONE.get_or_init(BTreeSet::new)
    }
}

/// Build the file contents for a freshly fetched issue.
///
/// Body text is trimmed to exactly what [`IssueDoc::parse`] yields when it reads
/// the same file back. Without that symmetry a trailing newline alone would make
/// every issue register as locally edited.
pub fn render_issue(issue: &RemoteIssue, ctx: &RenderContext<'_>) -> Result<Rendered> {
    let description = convert::to_markdown(&issue.description);

    let mut body_raw = BTreeMap::new();
    let mut comments = Vec::with_capacity(issue.comments.len());
    for c in &issue.comments {
        let converted = convert::to_markdown(&c.body);
        if !converted.raw.is_empty() {
            body_raw.insert(format!("comment:{}", c.id), converted.raw.clone());
        }
        comments.push(CommentBlock {
            id: CommentId::Existing(c.id.clone()),
            author: c
                .author
                .as_ref()
                .map(|a| a.display_name.clone())
                .unwrap_or_else(|| "Unknown".into()),
            timestamp: c
                .updated
                .or(c.created)
                .as_ref()
                .map(crate::api::model::format_timestamp)
                .unwrap_or_default(),
            note: String::new(),
            body: converted.markdown.trim().to_string(),
        });
    }

    let worklogs = worklog_entries(issue);
    for w in &issue.worklogs {
        let raw = convert::to_markdown(&w.comment).raw;
        if !raw.is_empty() {
            body_raw.insert(format!("worklog:{}", w.id), raw);
        }
    }

    let children = issue
        .fields
        .subtasks
        .iter()
        .map(|s| ChildItem {
            checked: s.done,
            key: Some(s.key.clone()),
            // Trimmed, because `## Children` is a line-based section and
            // reading one back cannot recover the space at its end: a subtask
            // whose summary ends in one would otherwise leave base holding a
            // title the file can never parse back to, and `diff` would report
            // the issue as modified for ever while `status` called it clean and
            // `push` had nothing to send. The checklist already does this to
            // every title it builds, for the same reason.
            title: s.summary.trim().to_string(),
        })
        .collect();

    let attachment_folder = naming::attachment_dir(&issue.key, &issue.fields.summary);
    let attachments = issue
        .attachments
        .iter()
        .map(|a| AttachmentLine {
            filename: a.filename.clone(),
            path: format!("{attachment_folder}/{}", a.filename),
            note: human_size(a.size),
        })
        .collect();

    let lossy = !description.raw.is_empty() || !body_raw.is_empty();
    let frontmatter = build_frontmatter(issue, ctx, lossy);

    let doc = IssueDoc {
        frontmatter,
        heading: issue.fields.summary.clone(),
        preamble: String::new(),
        description: description.markdown.trim().to_string(),
        checklist: issue.fields.checklist.clone(),
        children,
        comments,
        worklogs,
        attachments,
        extra_sections: Vec::new(),
    };

    Ok(Rendered {
        doc,
        description_raw: description.raw,
        body_raw,
        description_storage: issue.description.to_storage(),
    })
}

/// The `## Worklogs` section for an issue, oldest first.
///
/// Ordering by when the work started, rather than by whatever order the server
/// paged the entries in, is what makes the section read as a log; ties break on
/// the id so the file does not reshuffle between pulls.
pub fn worklog_entries(issue: &RemoteIssue) -> Vec<WorklogEntry> {
    let mut sorted: Vec<&crate::api::RemoteWorklog> = issue.worklogs.iter().collect();
    sorted.sort_by(|a, b| (a.started, &a.id).cmp(&(b.started, &b.id)));
    sorted
        .into_iter()
        .map(|w| WorklogEntry {
            id: WorklogId::Existing(w.id.clone()),
            time_spent: w.time_spent.clone(),
            author: w
                .author
                .as_ref()
                .map(|a| a.display_name.clone())
                .unwrap_or_else(|| "Unknown".into()),
            started: w
                .started
                .or(w.created)
                .as_ref()
                .map(crate::api::model::format_timestamp)
                .unwrap_or_default(),
            body: convert::to_markdown(&w.comment).markdown.trim().to_string(),
        })
        .collect()
}

fn build_frontmatter(issue: &RemoteIssue, ctx: &RenderContext<'_>, lossy: bool) -> Frontmatter {
    let f = &issue.fields;
    let mut fm = Frontmatter {
        key: issue.key.clone(),
        id: issue.id.clone(),
        issue_type: f.issue_type.clone(),
        status: f.status.clone(),
        priority: f.priority.clone(),
        summary: f.summary.clone(),
        labels: f.labels.clone(),
        components: f.components.clone(),
        assignee: format_user(f.assignee.as_ref()),
        reporter: format_user(f.reporter.as_ref()),
        created: f.created.as_ref().map(crate::api::model::format_timestamp),
        updated: Some(crate::api::model::format_timestamp(&issue.updated)),
        sprint: f.sprint.clone(),
        parent: f.parent.clone(),
        epic: f.epic.clone(),
        fix_versions: f.fix_versions.clone(),
        affects_versions: f.affects_versions.clone(),
        links: Vec::new(),
        url: format!(
            "{}/browse/{}",
            ctx.base_url.trim_end_matches('/'),
            issue.key
        ),
        story_points: f.story_points,
        // The file shows the editable form; `api::custom` puts the Jira
        // wrapper back when a change is pushed.
        custom: f
            .custom
            .iter()
            .filter(|(id, _)| !ctx.ignored_custom.contains(*id))
            .map(|(id, v)| (id.clone(), crate::api::custom::simplify(v)))
            .collect(),
        jired: SyncMeta {
            base_updated: crate::api::model::format_timestamp(&issue.updated),
            base_version: None,
            // Filled in by the caller once the file text exists; a hash of the
            // file cannot be computed before the file is rendered.
            content_hash: String::new(),
            lossy,
        },
        extra: BTreeMap::new(),
    };
    let pairs: Vec<(String, String)> = f
        .links
        .iter()
        .map(|l| (l.link_type.clone(), l.other_key.clone()))
        .collect();
    fm.set_links(&pairs);
    fm
}

/// Human-readable attachment size, matching what the file shows.
pub fn human_size(bytes: u64) -> String {
    const UNITS: [(&str, u64); 4] = [
        ("GiB", 1 << 30),
        ("MiB", 1 << 20),
        ("KiB", 1 << 10),
        ("B", 1),
    ];
    for (unit, scale) in UNITS {
        if bytes >= scale {
            if scale == 1 {
                return format!("{bytes} B");
            }
            return format!("{:.1} {unit}", bytes as f64 / scale as f64);
        }
    }
    "0 B".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::model::*;
    use crate::api::RichText;
    use serde_json::json;

    fn issue() -> RemoteIssue {
        RemoteIssue {
            key: "PROJ-142".into(),
            id: "10023".into(),
            updated: parse_timestamp("2026-08-16T12:03:00Z").unwrap(),
            description: RichText::Wiki("Login fails when SSO is on.".into()),
            fields: IssueFields {
                summary: "Fix login flow".into(),
                issue_type: "Story".into(),
                status: "In Progress".into(),
                labels: vec!["auth".into()],
                assignee: Some(UserRef {
                    id: "5b10a2".into(),
                    display_name: "Ilya A".into(),
                }),
                links: vec![IssueLink {
                    link_type: "blocks".into(),
                    other_key: "PROJ-150".into(),
                }],
                subtasks: vec![
                    SubtaskRef {
                        key: "PROJ-143".into(),
                        summary: "Add test".into(),
                        status: "To Do".into(),
                        done: false,
                    },
                    SubtaskRef {
                        key: "PROJ-144".into(),
                        summary: "Update runbook".into(),
                        status: "Done".into(),
                        done: true,
                    },
                ],
                checklist: vec![
                    crate::doc::ChecklistEntry::Item(crate::doc::ChecklistItem {
                        checked: false,
                        status: Some("IN PROGRESS".into()),
                        title: "Analyse the request".into(),
                        description: Some("ask the analyst first".into()),
                    }),
                    crate::doc::ChecklistEntry::Item(crate::doc::ChecklistItem {
                        checked: true,
                        status: None,
                        title: "Ship it".into(),
                        description: None,
                    }),
                ],
                ..Default::default()
            },
            comments: vec![RemoteComment {
                id: "10812".into(),
                author: Some(UserRef {
                    id: "5b1af9".into(),
                    display_name: "Jane D".into(),
                }),
                created: parse_timestamp("2026-08-15T10:32:00Z"),
                updated: parse_timestamp("2026-08-15T10:32:00Z"),
                body: RichText::Wiki("Repro confirmed.".into()),
            }],
            // Deliberately out of order: the section is a log, so rendering has
            // to put them back in the order the work happened.
            worklogs: vec![
                RemoteWorklog {
                    id: "10501".into(),
                    author: Some(UserRef {
                        id: "5b10a2".into(),
                        display_name: "Ilya A".into(),
                    }),
                    started: parse_timestamp("2026-08-16T08:00:00Z"),
                    created: parse_timestamp("2026-08-16T09:00:00Z"),
                    updated: parse_timestamp("2026-08-16T09:00:00Z"),
                    time_spent: "1h".into(),
                    time_spent_seconds: 3600,
                    comment: RichText::Empty,
                },
                RemoteWorklog {
                    id: "10500".into(),
                    author: Some(UserRef {
                        id: "5b1af9".into(),
                        display_name: "Jane D".into(),
                    }),
                    started: parse_timestamp("2026-08-15T09:00:00Z"),
                    created: parse_timestamp("2026-08-15T11:30:00Z"),
                    updated: parse_timestamp("2026-08-15T11:30:00Z"),
                    time_spent: "2h 30m".into(),
                    time_spent_seconds: 9000,
                    comment: RichText::Wiki("Chased the redirect loop.".into()),
                },
            ],
            worklog_total: 2,
            attachments: vec![RemoteAttachment {
                id: "5001".into(),
                filename: "trace.har".into(),
                size: 2_200_000,
                mime: Some("application/json".into()),
                created: None,
                author: None,
                content_url: "https://x/att/5001".into(),
            }],
            raw: json!({}),
        }
    }

    fn ctx() -> RenderContext<'static> {
        RenderContext {
            markup: Markup::Wiki,
            base_url: "https://x.atlassian.net",
            ignored_custom: RenderContext::none_ignored(),
        }
    }

    #[test]
    fn frontmatter_mirrors_the_server_fields() {
        let r = render_issue(&issue(), &ctx()).unwrap();
        let fm = &r.doc.frontmatter;
        assert_eq!(fm.key, "PROJ-142");
        assert_eq!(fm.summary, "Fix login flow");
        assert_eq!(fm.assignee.as_deref(), Some("@[Ilya A](5b10a2)"));
        assert_eq!(fm.url, "https://x.atlassian.net/browse/PROJ-142");
        assert_eq!(fm.updated.as_deref(), Some("2026-08-16T12:03:00Z"));
        assert_eq!(fm.jired.base_updated, "2026-08-16T12:03:00Z");
        assert_eq!(
            fm.link_pairs(),
            vec![("blocks".to_string(), "PROJ-150".to_string())]
        );
    }

    #[test]
    fn subtasks_become_the_children_section_with_their_done_state() {
        let r = render_issue(&issue(), &ctx()).unwrap();
        assert_eq!(r.doc.children.len(), 2);
        assert_eq!(r.doc.children[0].key.as_deref(), Some("PROJ-143"));
        assert!(!r.doc.children[0].checked);
        assert!(r.doc.children[1].checked);
    }

    /// The plugin's checklist is a different list from the subtasks, and lands
    /// in its own section.
    #[test]
    fn the_plugin_checklist_lands_under_its_own_heading() {
        let r = render_issue(&issue(), &ctx()).unwrap();
        let text = r.doc.render().unwrap();
        let checklist = text
            .split("## Checklist\n")
            .nth(1)
            .and_then(|s| s.split("## Children").next())
            .unwrap_or_default()
            .to_string();
        assert!(checklist.contains("- [ ] `IN PROGRESS` Analyse the request"));
        assert!(checklist.contains("  ask the analyst first"));
        assert!(checklist.contains("- [x] Ship it"));
        assert!(!checklist.contains("PROJ-143"));
    }

    #[test]
    fn comments_get_stable_headings() {
        let r = render_issue(&issue(), &ctx()).unwrap();
        assert_eq!(r.doc.comments[0].id, CommentId::Existing("10812".into()));
        assert_eq!(r.doc.comments[0].author, "Jane D");
        assert_eq!(r.doc.comments[0].timestamp, "2026-08-15T10:32:00Z");
    }

    /// The section is a log of the work, so it reads oldest first whatever
    /// order the server listed the entries in.
    #[test]
    fn worklogs_render_in_the_order_the_work_happened() {
        let r = render_issue(&issue(), &ctx()).unwrap();
        assert_eq!(r.doc.worklogs.len(), 2);
        assert_eq!(r.doc.worklogs[0].id, WorklogId::Existing("10500".into()));
        assert_eq!(r.doc.worklogs[0].time_spent, "2h 30m");
        assert_eq!(r.doc.worklogs[0].author, "Jane D");
        assert_eq!(r.doc.worklogs[0].started, "2026-08-15T09:00:00Z");
        assert_eq!(r.doc.worklogs[0].body, "Chased the redirect loop.");
        assert_eq!(r.doc.worklogs[1].id, WorklogId::Existing("10501".into()));

        let text = r.doc.render().unwrap();
        assert!(
            text.contains("## Worklogs\n\n### [w-10500] 2h 30m — Jane D — 2026-08-15T09:00:00Z")
        );
        assert!(text.find("[w-10500]") < text.find("[w-10501]"));
    }

    #[test]
    fn attachments_link_into_the_dot_folder() {
        let r = render_issue(&issue(), &ctx()).unwrap();
        assert_eq!(
            r.doc.attachments[0].path,
            ".PROJ-0142. Fix login flow/trace.har"
        );
        assert_eq!(r.doc.attachments[0].note, "2.1 MiB");
    }

    /// An ignored field is absent from the file, not present and empty: an
    /// empty value would be read as "clear this field" on the next push.
    #[test]
    fn an_ignored_custom_field_never_reaches_the_file() {
        let mut issue = issue();
        issue
            .fields
            .custom
            .insert("customfield_10456".into(), json!({"value": "Platform"}));
        issue
            .fields
            .custom
            .insert("customfield_10300".into(), json!("2026-08-20"));

        let ignored: BTreeSet<String> = ["customfield_10456".to_string()].into_iter().collect();
        let r = render_issue(
            &issue,
            &RenderContext {
                markup: Markup::Wiki,
                base_url: "https://x.atlassian.net",
                ignored_custom: &ignored,
            },
        )
        .unwrap();

        assert!(!r.doc.frontmatter.custom.contains_key("customfield_10456"));
        assert_eq!(
            r.doc.frontmatter.custom.get("customfield_10300"),
            Some(&json!("2026-08-20"))
        );
        assert!(!r.doc.render().unwrap().contains("customfield_10456"));
    }

    #[test]
    fn a_clean_issue_is_not_marked_lossy() {
        let r = render_issue(&issue(), &ctx()).unwrap();
        assert!(!r.doc.frontmatter.jired.lossy);
        assert!(r.description_raw.is_empty());
    }

    #[test]
    fn rendered_document_parses_back() {
        let r = render_issue(&issue(), &ctx()).unwrap();
        let text = r.doc.render().unwrap();
        let back = IssueDoc::parse(&text).unwrap();
        assert_eq!(back.frontmatter.key, "PROJ-142");
        assert_eq!(back.checklist.len(), 2);
        assert_eq!(back.children.len(), 2);
        assert_eq!(back.comments.len(), 1);
        assert_eq!(back.worklogs.len(), 2);
    }

    /// The round trip must be exact, not merely similar: `diff`, `status` and
    /// `push` all decide "is this issue edited?" by comparing these strings.
    #[test]
    fn rendering_and_parsing_agree_on_body_whitespace() {
        let r = render_issue(&issue(), &ctx()).unwrap();
        let back = IssueDoc::parse(&r.doc.render().unwrap()).unwrap();
        assert_eq!(r.doc.description, back.description);
        assert_eq!(r.doc.comments[0].body, back.comments[0].body);
        assert_eq!(r.doc.checklist, back.checklist);
        assert_eq!(r.doc.children, back.children);
        assert_eq!(r.doc.worklogs, back.worklogs);
    }

    #[test]
    fn size_formatting() {
        assert_eq!(human_size(0), "0 B");
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(2048), "2.0 KiB");
        assert_eq!(human_size(2_200_000), "2.1 MiB");
    }
}
