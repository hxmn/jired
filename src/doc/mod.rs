//! The issue Markdown file: parse and render [M1.11, M2.1].
//!
//! Fixed section order — `# summary`, `## Description`, `## Checklist`,
//! `## Comments`, `## Attachments` — so both humans and agents can locate
//! content by heading. Section scanning is fence-aware: a `## ` inside a code
//! block is content, not a heading.

pub mod frontmatter;

pub use frontmatter::{Frontmatter, SyncMeta};

use crate::error::{JiredError, Result};

pub const SECTION_DESCRIPTION: &str = "Description";
pub const SECTION_CHECKLIST: &str = "Checklist";
pub const SECTION_COMMENTS: &str = "Comments";
pub const SECTION_ATTACHMENTS: &str = "Attachments";

/// A parsed issue file.
#[derive(Debug, Clone, PartialEq)]
pub struct IssueDoc {
    pub frontmatter: Frontmatter,
    /// Text of the `# ` heading. Cosmetic: frontmatter `summary` wins (D6).
    pub heading: String,
    /// Anything between the title and the first `## ` section, kept in place.
    pub preamble: String,
    pub description: String,
    pub checklist: Vec<ChecklistItem>,
    pub comments: Vec<CommentBlock>,
    pub attachments: Vec<AttachmentLine>,
    /// Sections jired does not know about, preserved in place.
    pub extra_sections: Vec<(String, String)>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ChecklistItem {
    pub checked: bool,
    /// Issue key when the item is a subtask; None for a line the user added.
    pub key: Option<String>,
    pub title: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommentId {
    Existing(String),
    /// Authored locally, not yet sent.
    New,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CommentBlock {
    pub id: CommentId,
    pub author: String,
    pub timestamp: String,
    /// Free-text note after `[new]`, ignored on push.
    pub note: String,
    pub body: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AttachmentLine {
    pub filename: String,
    pub path: String,
    pub note: String,
}

impl IssueDoc {
    pub fn parse(text: &str) -> Result<Self> {
        let (yaml, body) = split_frontmatter(text)?;
        let frontmatter = Frontmatter::parse(&yaml)?;

        let mut heading = String::new();
        let mut sections: Vec<(String, String)> = Vec::new();
        let mut current: Option<(String, Vec<&str>)> = None;
        let mut preamble: Vec<&str> = Vec::new();

        for line in ScanLines::new(&body) {
            match line {
                Scanned::Heading { level: 1, text, .. }
                    if heading.is_empty() && current.is_none() =>
                {
                    heading = text.to_string();
                }
                Scanned::Heading { level: 2, text, .. } => {
                    if let Some((name, lines)) = current.take() {
                        sections.push((name, lines.join("\n")));
                    }
                    current = Some((text.to_string(), Vec::new()));
                }
                Scanned::Line(l) => match &mut current {
                    Some((_, lines)) => lines.push(l),
                    None => preamble.push(l),
                },
                // Deeper headings belong to whichever section is open.
                Scanned::Heading { raw, .. } => match &mut current {
                    Some((_, lines)) => lines.push(raw),
                    None => preamble.push(raw),
                },
            }
        }
        if let Some((name, lines)) = current.take() {
            sections.push((name, lines.join("\n")));
        }

        let mut doc = IssueDoc {
            frontmatter,
            heading,
            preamble: preamble.join("\n").trim().to_string(),
            description: String::new(),
            checklist: Vec::new(),
            comments: Vec::new(),
            attachments: Vec::new(),
            extra_sections: Vec::new(),
        };

        for (name, content) in sections {
            match name.as_str() {
                SECTION_DESCRIPTION => doc.description = content.trim().to_string(),
                SECTION_CHECKLIST => doc.checklist = parse_checklist(&content),
                SECTION_COMMENTS => doc.comments = parse_comments(&content)?,
                SECTION_ATTACHMENTS => doc.attachments = parse_attachments(&content),
                _ => doc.extra_sections.push((name, content.trim().to_string())),
            }
        }
        Ok(doc)
    }

    pub fn render(&self) -> Result<String> {
        let mut out = String::new();
        out.push_str("---\n");
        out.push_str(self.frontmatter.to_yaml()?.trim_end());
        out.push_str("\n---\n\n");

        out.push_str(&format!("# {}\n\n", self.frontmatter.summary));
        if !self.preamble.is_empty() {
            out.push_str(self.preamble.trim());
            out.push_str("\n\n");
        }

        out.push_str(&format!("## {SECTION_DESCRIPTION}\n\n"));
        if !self.description.trim().is_empty() {
            out.push_str(self.description.trim());
            out.push_str("\n\n");
        }

        out.push_str(&format!("## {SECTION_CHECKLIST}\n\n"));
        for item in &self.checklist {
            out.push_str(&render_checklist_item(item));
            out.push('\n');
        }
        if !self.checklist.is_empty() {
            out.push('\n');
        }

        out.push_str(&format!("## {SECTION_COMMENTS}\n\n"));
        for c in &self.comments {
            out.push_str(&render_comment_heading(c));
            out.push_str("\n\n");
            if !c.body.trim().is_empty() {
                out.push_str(c.body.trim());
                out.push_str("\n\n");
            }
        }

        out.push_str(&format!("## {SECTION_ATTACHMENTS}\n\n"));
        for a in &self.attachments {
            out.push_str(&format!("- [{}]({})", a.filename, a.path));
            if !a.note.is_empty() {
                out.push_str(&format!(" ({})", a.note));
            }
            out.push('\n');
        }
        if !self.attachments.is_empty() {
            out.push('\n');
        }

        for (name, content) in &self.extra_sections {
            out.push_str(&format!("## {name}\n\n"));
            if !content.trim().is_empty() {
                out.push_str(content.trim());
                out.push_str("\n\n");
            }
        }

        // Exactly one trailing newline keeps diffs and hashes stable.
        Ok(format!("{}\n", out.trim_end()))
    }

    pub fn comment(&self, id: &str) -> Option<&CommentBlock> {
        self.comments
            .iter()
            .find(|c| matches!(&c.id, CommentId::Existing(x) if x == id))
    }

    pub fn new_comments(&self) -> impl Iterator<Item = &CommentBlock> {
        self.comments.iter().filter(|c| c.id == CommentId::New)
    }
}

/// Comment headings that sit outside `## Comments` and will therefore be
/// ignored on push.
///
/// Appending `### [new]` to the end of the file is a natural thing for an agent
/// to do, and it lands in `## Attachments` instead of `## Comments`. Silently
/// dropping the comment is the worst outcome, so callers surface this.
pub fn misplaced_comment_headings(text: &str) -> Vec<String> {
    let Ok((_, body)) = split_frontmatter(text) else {
        return Vec::new();
    };
    let mut section = String::new();
    let mut out = Vec::new();
    for line in ScanLines::new(&body) {
        match line {
            Scanned::Heading { level: 2, text, .. } => section = text.to_string(),
            Scanned::Heading { level: 3, text, .. }
                if section != SECTION_COMMENTS && text.trim_start().starts_with('[') =>
            {
                out.push(format!("### {text}"));
            }
            _ => {}
        }
    }
    out
}

/// Split `---` delimited frontmatter from the body.
pub fn split_frontmatter(text: &str) -> Result<(String, String)> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let rest = text
        .strip_prefix("---\n")
        .or_else(|| text.strip_prefix("---\r\n"))
        .ok_or_else(|| {
            JiredError::generic("the file does not start with a YAML frontmatter block")
                .with_hint("an issue file must begin with a line containing only `---`")
        })?;

    let mut offset = 0;
    for line in rest.split_inclusive('\n') {
        if line.trim_end() == "---" {
            return Ok((
                rest[..offset].to_string(),
                rest[offset + line.len()..].to_string(),
            ));
        }
        offset += line.len();
    }
    Err(
        JiredError::generic("the frontmatter block is not closed with `---`")
            .with_hint("add a line containing only `---` after the YAML header"),
    )
}

enum Scanned<'a> {
    /// `raw` is the original line, so a heading that belongs to a section's
    /// body can be re-emitted without rebuilding it.
    Heading {
        level: u8,
        text: &'a str,
        raw: &'a str,
    },
    Line(&'a str),
}

/// Line iterator that suppresses headings inside fenced code blocks.
struct ScanLines<'a> {
    lines: std::str::Lines<'a>,
    fence: Option<String>,
}

impl<'a> ScanLines<'a> {
    fn new(text: &'a str) -> Self {
        Self {
            lines: text.lines(),
            fence: None,
        }
    }
}

impl<'a> Iterator for ScanLines<'a> {
    type Item = Scanned<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        let line = self.lines.next()?;
        let trimmed = line.trim_start();

        if let Some(open) = &self.fence {
            if trimmed.starts_with(open.as_str()) {
                self.fence = None;
            }
            return Some(Scanned::Line(line));
        }
        for marker in ["```", "~~~"] {
            if trimmed.starts_with(marker) {
                self.fence = Some(marker.to_string());
                return Some(Scanned::Line(line));
            }
        }
        if let Some(rest) = trimmed.strip_prefix('#') {
            let hashes = 1 + rest.chars().take_while(|c| *c == '#').count();
            let after = &trimmed[hashes..];
            if hashes <= 6 && after.starts_with(' ') {
                return Some(Scanned::Heading {
                    level: hashes as u8,
                    text: after.trim(),
                    raw: line,
                });
            }
        }
        Some(Scanned::Line(line))
    }
}

fn parse_checklist(content: &str) -> Vec<ChecklistItem> {
    content
        .lines()
        .filter_map(|line| {
            let t = line.trim();
            let rest = t.strip_prefix("- [").or_else(|| t.strip_prefix("* ["))?;
            let checked = rest.starts_with('x') || rest.starts_with('X');
            let rest = rest.get(1..)?.strip_prefix(']')?.trim();
            let (key, title) = split_leading_key(rest);
            Some(ChecklistItem {
                checked,
                key,
                title: title.to_string(),
            })
        })
        .collect()
}

fn render_checklist_item(item: &ChecklistItem) -> String {
    let mark = if item.checked { 'x' } else { ' ' };
    match &item.key {
        Some(k) => format!("- [{mark}] {k} {}", item.title),
        None => format!("- [{mark}] {}", item.title),
    }
}

/// Recognize a leading issue key so subtask lines keep their identity.
pub fn split_leading_key(text: &str) -> (Option<String>, &str) {
    let mut parts = text.splitn(2, char::is_whitespace);
    let first = parts.next().unwrap_or("");
    if is_issue_key(first) {
        return (
            Some(first.to_string()),
            parts.next().unwrap_or("").trim_start(),
        );
    }
    (None, text)
}

pub fn is_issue_key(s: &str) -> bool {
    let Some((project, number)) = s.split_once('-') else {
        return false;
    };
    !project.is_empty()
        && project
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
        && project
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_uppercase())
        && !number.is_empty()
        && number.chars().all(|c| c.is_ascii_digit())
}

fn parse_comments(content: &str) -> Result<Vec<CommentBlock>> {
    let mut out: Vec<CommentBlock> = Vec::new();
    let mut current: Option<CommentBlock> = None;
    let mut body: Vec<&str> = Vec::new();

    for line in ScanLines::new(content) {
        match line {
            Scanned::Heading { level: 3, text, .. } => {
                if let Some(mut c) = current.take() {
                    c.body = body.join("\n").trim().to_string();
                    out.push(c);
                }
                body.clear();
                current = Some(parse_comment_heading(text)?);
            }
            Scanned::Line(l) => body.push(l),
            Scanned::Heading { raw, .. } => body.push(raw),
        }
    }
    if let Some(mut c) = current.take() {
        c.body = body.join("\n").trim().to_string();
        out.push(c);
    }
    Ok(out)
}

/// `[c-10812] Jane Doe — 2026-08-15T10:32:00Z` or `[new] optional note`.
fn parse_comment_heading(text: &str) -> Result<CommentBlock> {
    let rest = text.trim();
    let inner_end = rest.find(']').ok_or_else(|| {
        JiredError::generic(format!(
            "comment heading has no `[id]` marker: `### {rest}`"
        ))
        .with_hint("use `### [new]` for a new comment")
    })?;
    let marker = rest[1..inner_end].trim();
    let tail = rest[inner_end + 1..].trim();

    if !rest.starts_with('[') {
        return Err(JiredError::generic(format!(
            "comment heading must start with `[`: `### {rest}`"
        )));
    }

    if marker.eq_ignore_ascii_case("new") {
        return Ok(CommentBlock {
            id: CommentId::New,
            author: String::new(),
            timestamp: String::new(),
            note: tail.to_string(),
            body: String::new(),
        });
    }

    let id = marker.strip_prefix("c-").unwrap_or(marker).to_string();
    // The separator is an em dash; accept a hyphen too since editors mangle it.
    let (author, timestamp) = match tail.split_once('—').or_else(|| tail.split_once(" - ")) {
        Some((a, t)) => (a.trim().to_string(), t.trim().to_string()),
        None => (tail.to_string(), String::new()),
    };
    Ok(CommentBlock {
        id: CommentId::Existing(id),
        author,
        timestamp,
        note: String::new(),
        body: String::new(),
    })
}

fn render_comment_heading(c: &CommentBlock) -> String {
    match &c.id {
        CommentId::Existing(id) => {
            let mut h = format!("### [c-{id}] {}", c.author);
            if !c.timestamp.is_empty() {
                h.push_str(&format!(" — {}", c.timestamp));
            }
            h
        }
        CommentId::New => {
            if c.note.is_empty() {
                "### [new]".to_string()
            } else {
                format!("### [new] {}", c.note)
            }
        }
    }
}

fn parse_attachments(content: &str) -> Vec<AttachmentLine> {
    content
        .lines()
        .filter_map(|line| {
            let t = line.trim().strip_prefix("- ")?;
            let name_end = t.find("](")?;
            let filename = t[1..name_end].to_string();
            let rest = &t[name_end + 2..];
            let path_end = rest.find(')')?;
            let path = rest[..path_end].to_string();
            let note = rest[path_end + 1..]
                .trim()
                .trim_start_matches('(')
                .trim_end_matches(')')
                .to_string();
            Some(AttachmentLine {
                filename,
                path,
                note,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> String {
        let mut s = String::from("---\nkey: PROJ-142\nid: \"10023\"\nsummary: Fix login flow\n");
        s.push_str("type: Story\nstatus: In Progress\nurl: https://x/browse/PROJ-142\n");
        s.push_str("jired:\n  base_updated: 2026-08-16T14:03:00Z\n  content_hash: \"sha256:a\"\n  lossy: false\n");
        s.push_str("---\n\n# Fix login flow\n\n## Description\n\nLogin fails when SSO is on.\n\n");
        s.push_str("## Checklist\n\n- [ ] PROJ-143 Add integration test\n- [x] PROJ-144 Update runbook\n- [ ] a local note\n\n");
        s.push_str(
            "## Comments\n\n### [c-10812] Jane Doe — 2026-08-15T10:32:00Z\n\nRepro confirmed.\n\n",
        );
        s.push_str("### [new] follow-up\n\nFixed by rotating the cert.\n\n");
        s.push_str(
            "## Attachments\n\n- [trace.har](.PROJ-142. Fix login flow/trace.har) (2.1 MiB)\n",
        );
        s
    }

    #[test]
    fn parses_every_section() {
        let doc = IssueDoc::parse(&sample()).unwrap();
        assert_eq!(doc.frontmatter.key, "PROJ-142");
        assert_eq!(doc.heading, "Fix login flow");
        assert_eq!(doc.description, "Login fails when SSO is on.");
        assert_eq!(doc.checklist.len(), 3);
        assert_eq!(doc.checklist[0].key.as_deref(), Some("PROJ-143"));
        assert!(!doc.checklist[0].checked);
        assert!(doc.checklist[1].checked);
        assert_eq!(doc.checklist[2].key, None);
        assert_eq!(doc.checklist[2].title, "a local note");
        assert_eq!(doc.comments.len(), 2);
        assert_eq!(doc.comments[0].id, CommentId::Existing("10812".into()));
        assert_eq!(doc.comments[0].author, "Jane Doe");
        assert_eq!(doc.comments[0].body, "Repro confirmed.");
        assert_eq!(doc.comments[1].id, CommentId::New);
        assert_eq!(doc.comments[1].note, "follow-up");
        assert_eq!(doc.attachments[0].filename, "trace.har");
        assert_eq!(doc.attachments[0].note, "2.1 MiB");
    }

    #[test]
    fn render_then_parse_is_stable() {
        let doc = IssueDoc::parse(&sample()).unwrap();
        let text = doc.render().unwrap();
        let again = IssueDoc::parse(&text).unwrap();
        assert_eq!(doc, again);
        // And rendering is idempotent, so a no-op pull writes no diff.
        assert_eq!(text, again.render().unwrap());
    }

    #[test]
    fn headings_inside_code_fences_are_not_sections() {
        let mut s = String::from("---\nkey: A-1\nid: \"1\"\nsummary: s\njired: {}\n---\n\n# s\n\n");
        s.push_str("## Description\n\n```md\n## Comments\n### [c-1] fake\n```\n\nreal text\n\n");
        s.push_str("## Comments\n\n### [c-9] Real Person — t\n\nbody\n");
        let doc = IssueDoc::parse(&s).unwrap();
        assert!(doc.description.contains("## Comments"));
        assert_eq!(doc.comments.len(), 1);
        assert_eq!(doc.comments[0].id, CommentId::Existing("9".into()));
    }

    #[test]
    fn unknown_sections_are_preserved() {
        let mut s = String::from("---\nkey: A-1\nid: \"1\"\nsummary: s\njired: {}\n---\n\n# s\n\n");
        s.push_str("## Description\n\nd\n\n## Notes\n\nkeep me\n");
        let doc = IssueDoc::parse(&s).unwrap();
        assert_eq!(doc.extra_sections, vec![("Notes".into(), "keep me".into())]);
        assert!(doc.render().unwrap().contains("keep me"));
    }

    #[test]
    fn deeper_headings_stay_inside_their_section() {
        let mut s = String::from("---\nkey: A-1\nid: \"1\"\nsummary: s\njired: {}\n---\n\n# s\n\n");
        s.push_str("## Description\n\n### Background\n\ntext\n");
        let doc = IssueDoc::parse(&s).unwrap();
        assert!(doc.description.contains("### Background"));
    }

    #[test]
    fn missing_frontmatter_is_a_clear_error() {
        let err = IssueDoc::parse("# just markdown\n").unwrap_err();
        assert!(err.message.contains("frontmatter"));
        let err = IssueDoc::parse("---\nkey: A-1\n").unwrap_err();
        assert!(err.message.contains("not closed"));
    }

    #[test]
    fn comment_heading_without_a_marker_is_rejected_with_a_hint() {
        let mut s = String::from("---\nkey: A-1\nid: \"1\"\nsummary: s\njired: {}\n---\n\n# s\n\n");
        s.push_str("## Comments\n\n### Jane Doe wrote\n\nbody\n");
        let err = IssueDoc::parse(&s).unwrap_err();
        assert!(err.message.contains("[id]"), "{}", err.message);
        assert!(err.hint.is_some());
    }

    #[test]
    fn a_comment_heading_outside_the_comments_section_is_reported() {
        let mut s = String::from("---\nkey: A-1\nid: \"1\"\nsummary: s\njired: {}\n---\n\n# s\n\n");
        s.push_str("## Comments\n\n### [c-1] A — t\n\nbody\n\n");
        s.push_str("## Attachments\n\n### [new]\n\nappended in the wrong place\n");

        assert_eq!(misplaced_comment_headings(&s), vec!["### [new]"]);

        // A correctly placed one is not reported.
        let good = s.replace("## Attachments\n\n### [new]", "## Attachments");
        assert!(misplaced_comment_headings(&good).is_empty());
    }

    #[test]
    fn issue_key_recognition() {
        assert!(is_issue_key("PROJ-142"));
        assert!(is_issue_key("A1B2-9"));
        assert!(!is_issue_key("proj-142"));
        assert!(!is_issue_key("PROJ-"));
        assert!(!is_issue_key("PROJ-12a"));
        assert!(!is_issue_key("hello"));
    }

    #[test]
    fn the_rendered_heading_comes_from_frontmatter_not_the_old_heading() {
        let mut doc = IssueDoc::parse(&sample()).unwrap();
        doc.heading = "stale heading".into();
        doc.frontmatter.summary = "New summary".into();
        let text = doc.render().unwrap();
        assert!(text.contains("# New summary"));
        assert!(!text.contains("stale heading"));
    }
}
