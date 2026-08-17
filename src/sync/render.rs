//! Turning a fetched issue into an issue file [M1.11].
//!
//! This is the one place that decides what a `.md` file looks like for a given
//! server payload, so `pull` and `push` (which rewrites the file after a
//! confirmed write) cannot disagree about it.

use crate::api::{Markup, RemoteIssue};
use crate::convert::{self, ast::RawBlock};
use crate::doc::frontmatter::format_user;
use crate::doc::{
    AttachmentLine, ChecklistItem, CommentBlock, CommentId, Frontmatter, IssueDoc, SyncMeta,
};
use crate::error::Result;
use crate::project::naming;
use std::collections::BTreeMap;

/// The document plus the lossy regions that must be stored alongside it.
pub struct Rendered {
    pub doc: IssueDoc,
    pub description_raw: Vec<RawBlock>,
    /// Keyed by container, e.g. `comment:10812`.
    pub comment_raw: BTreeMap<String, Vec<RawBlock>>,
    /// Description as it will be stored in `base_desc`.
    pub description_storage: String,
}

pub struct RenderContext<'a> {
    pub markup: Markup,
    pub base_url: &'a str,
}

/// Build the file contents for a freshly fetched issue.
///
/// Body text is trimmed to exactly what [`IssueDoc::parse`] yields when it reads
/// the same file back. Without that symmetry a trailing newline alone would make
/// every issue register as locally edited.
pub fn render_issue(issue: &RemoteIssue, ctx: &RenderContext<'_>) -> Result<Rendered> {
    let description = convert::to_markdown(&issue.description);

    let mut comment_raw = BTreeMap::new();
    let mut comments = Vec::with_capacity(issue.comments.len());
    for c in &issue.comments {
        let converted = convert::to_markdown(&c.body);
        if !converted.raw.is_empty() {
            comment_raw.insert(format!("comment:{}", c.id), converted.raw.clone());
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

    let checklist = issue
        .fields
        .subtasks
        .iter()
        .map(|s| ChecklistItem {
            checked: s.done,
            key: Some(s.key.clone()),
            title: s.summary.clone(),
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

    let lossy = !description.raw.is_empty() || !comment_raw.is_empty();
    let frontmatter = build_frontmatter(issue, ctx, lossy);

    let doc = IssueDoc {
        frontmatter,
        heading: issue.fields.summary.clone(),
        preamble: String::new(),
        description: description.markdown.trim().to_string(),
        checklist,
        comments,
        attachments,
        extra_sections: Vec::new(),
    };

    Ok(Rendered {
        doc,
        description_raw: description.raw,
        comment_raw,
        description_storage: issue.description.to_storage(),
    })
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
        custom: f.custom.clone().into_iter().collect(),
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
    fn subtasks_become_the_checklist_with_their_done_state() {
        let r = render_issue(&issue(), &ctx()).unwrap();
        assert_eq!(r.doc.checklist.len(), 2);
        assert_eq!(r.doc.checklist[0].key.as_deref(), Some("PROJ-143"));
        assert!(!r.doc.checklist[0].checked);
        assert!(r.doc.checklist[1].checked);
    }

    #[test]
    fn comments_get_stable_headings() {
        let r = render_issue(&issue(), &ctx()).unwrap();
        assert_eq!(r.doc.comments[0].id, CommentId::Existing("10812".into()));
        assert_eq!(r.doc.comments[0].author, "Jane D");
        assert_eq!(r.doc.comments[0].timestamp, "2026-08-15T10:32:00Z");
    }

    #[test]
    fn attachments_link_into_the_dot_folder() {
        let r = render_issue(&issue(), &ctx()).unwrap();
        assert_eq!(
            r.doc.attachments[0].path,
            ".PROJ-142. Fix login flow/trace.har"
        );
        assert_eq!(r.doc.attachments[0].note, "2.1 MiB");
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
        assert_eq!(back.comments.len(), 1);
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
    }

    #[test]
    fn size_formatting() {
        assert_eq!(human_size(0), "0 B");
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(2048), "2.0 KiB");
        assert_eq!(human_size(2_200_000), "2.1 MiB");
    }
}
