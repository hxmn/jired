//! `JiredAst` — the one rich-text representation everything converts through.
//!
//! ADF and wiki markup parse *into* it; Markdown renders *from* it and parses
//! back *into* it. Anything a markup can express but Markdown cannot becomes a
//! [`Block::Raw`] or [`Inline::Raw`] whose original payload is kept verbatim in
//! a [`RawBlock`], which is what makes untouched lossy content push back
//! byte-identical.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Document {
    pub blocks: Vec<Block>,
}

impl Document {
    pub fn new(blocks: Vec<Block>) -> Self {
        Self { blocks }
    }

    pub fn is_empty(&self) -> bool {
        self.blocks.is_empty()
    }

    /// Ids of every lossy region referenced anywhere in the document.
    pub fn raw_ids(&self) -> Vec<String> {
        fn walk(blocks: &[Block], out: &mut Vec<String>) {
            for b in blocks {
                match b {
                    Block::Raw(r) => out.push(r.id.clone()),
                    Block::BlockQuote(inner) => walk(inner, out),
                    Block::BulletList(items) | Block::OrderedList { items, .. } => {
                        for i in items {
                            walk(&i.content, out);
                        }
                    }
                    Block::Table(t) => {
                        for row in t.header.iter().chain(t.rows.iter()) {
                            for cell in row {
                                walk(cell, out);
                            }
                        }
                    }
                    Block::Paragraph(inlines)
                    | Block::Heading {
                        content: inlines, ..
                    } => walk_inlines(inlines, out),
                    Block::TaskList(items) => {
                        for i in items {
                            walk_inlines(&i.content, out);
                        }
                    }
                    Block::CodeBlock { .. } | Block::Rule => {}
                }
            }
        }
        fn walk_inlines(inlines: &[Inline], out: &mut Vec<String>) {
            for i in inlines {
                match i {
                    Inline::Raw(r) => out.push(r.id.clone()),
                    Inline::Strong(c) | Inline::Emphasis(c) | Inline::Strike(c) => {
                        walk_inlines(c, out)
                    }
                    Inline::Link { text, .. } => walk_inlines(text, out),
                    _ => {}
                }
            }
        }
        let mut out = Vec::new();
        walk(&self.blocks, &mut out);
        out
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Block {
    Paragraph(Vec<Inline>),
    Heading {
        level: u8,
        content: Vec<Inline>,
    },
    CodeBlock {
        language: Option<String>,
        code: String,
    },
    BlockQuote(Vec<Block>),
    BulletList(Vec<ListItem>),
    OrderedList {
        start: u32,
        items: Vec<ListItem>,
    },
    TaskList(Vec<TaskItem>),
    Table(Table),
    Rule,
    /// A region Markdown cannot represent; the original lives in the raw store.
    Raw(RawRef),
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct ListItem {
    pub content: Vec<Block>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TaskItem {
    pub checked: bool,
    pub content: Vec<Inline>,
    /// ADF task items carry a stable id that must survive a round trip.
    pub id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Table {
    /// Header row cells; empty when the table has no header.
    pub header: Option<Vec<Vec<Block>>>,
    pub rows: Vec<Vec<Vec<Block>>>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Inline {
    Text(String),
    Strong(Vec<Inline>),
    Emphasis(Vec<Inline>),
    Strike(Vec<Inline>),
    Code(String),
    Link {
        text: Vec<Inline>,
        href: String,
    },
    /// `@[Display Name](accountId)` in Markdown.
    Mention {
        id: String,
        display: String,
    },
    /// A bare issue key that the server auto-links.
    IssueKey(String),
    LineBreak,
    Raw(RawRef),
}

/// Pointer from the document to a preserved lossy region.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawRef {
    pub id: String,
}

/// A preserved region: the original markup plus a read-only approximation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RawBlock {
    pub id: String,
    pub ordinal: usize,
    /// Original ADF node (JSON) or wiki fragment (string), verbatim.
    pub node_json: String,
    /// What the user sees between the markers. Never pushed.
    pub approx_md: String,
    /// Human label, e.g. `panel (warning)`.
    pub label: String,
}

/// Collects lossy regions during a conversion.
#[derive(Debug, Clone, Default)]
pub struct RawStore {
    blocks: Vec<RawBlock>,
}

impl RawStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a lossy region and get back its reference.
    ///
    /// The id is derived from the content, not a counter or random value, so
    /// re-converting the same document produces the same markers — otherwise
    /// every pull would show a spurious diff.
    pub fn insert(&mut self, node_json: String, approx_md: String, label: String) -> RawRef {
        let ordinal = self.blocks.len();
        let id = raw_id(&node_json, ordinal);
        self.blocks.push(RawBlock {
            id: id.clone(),
            ordinal,
            node_json,
            approx_md,
            label,
        });
        RawRef { id }
    }

    pub fn blocks(&self) -> &[RawBlock] {
        &self.blocks
    }

    pub fn into_blocks(self) -> Vec<RawBlock> {
        self.blocks
    }

    pub fn get(&self, id: &str) -> Option<&RawBlock> {
        self.blocks.iter().find(|b| b.id == id)
    }

    pub fn is_empty(&self) -> bool {
        self.blocks.is_empty()
    }

    pub fn from_blocks(blocks: Vec<RawBlock>) -> Self {
        Self { blocks }
    }
}

/// Content-addressed marker id: stable across runs, short enough to read.
pub fn raw_id(node_json: &str, ordinal: usize) -> String {
    let mut hasher = Sha256::new();
    hasher.update(node_json.as_bytes());
    hasher.update(ordinal.to_le_bytes());
    let digest = hasher.finalize();
    format!("r{}", hex::encode(&digest[..3]))
}

/// Flatten inline content to plain text, for approximations and summaries.
pub fn inline_text(inlines: &[Inline]) -> String {
    let mut out = String::new();
    for i in inlines {
        match i {
            Inline::Text(t) => out.push_str(t),
            Inline::Code(t) => out.push_str(t),
            Inline::Strong(c) | Inline::Emphasis(c) | Inline::Strike(c) => {
                out.push_str(&inline_text(c))
            }
            Inline::Link { text, href } => {
                let t = inline_text(text);
                out.push_str(if t.is_empty() { href } else { &t });
            }
            Inline::Mention { display, .. } => {
                out.push('@');
                out.push_str(display);
            }
            Inline::IssueKey(k) => out.push_str(k),
            Inline::LineBreak => out.push(' '),
            Inline::Raw(_) => {}
        }
    }
    out
}

/// Merge adjacent text nodes so parser artifacts do not affect equality.
pub fn normalize_inlines(inlines: Vec<Inline>) -> Vec<Inline> {
    let mut out: Vec<Inline> = Vec::with_capacity(inlines.len());
    for i in inlines {
        let i = match i {
            Inline::Strong(c) => Inline::Strong(normalize_inlines(c)),
            Inline::Emphasis(c) => Inline::Emphasis(normalize_inlines(c)),
            Inline::Strike(c) => Inline::Strike(normalize_inlines(c)),
            Inline::Link { text, href } => Inline::Link {
                text: normalize_inlines(text),
                href,
            },
            other => other,
        };
        match (out.last_mut(), &i) {
            (Some(Inline::Text(prev)), Inline::Text(next)) => prev.push_str(next),
            _ => {
                if !matches!(&i, Inline::Text(t) if t.is_empty()) {
                    out.push(i);
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_ids_are_content_addressed_and_stable() {
        let a = raw_id(r#"{"type":"panel"}"#, 0);
        let b = raw_id(r#"{"type":"panel"}"#, 0);
        assert_eq!(a, b, "same content must produce the same marker id");
        assert_ne!(a, raw_id(r#"{"type":"panel"}"#, 1));
        assert_ne!(a, raw_id(r#"{"type":"expand"}"#, 0));
        assert!(a.starts_with('r') && a.len() == 7, "{a}");
    }

    #[test]
    fn raw_store_round_trips_ids() {
        let mut store = RawStore::new();
        let r = store.insert("{}".into(), "approx".into(), "panel".into());
        assert_eq!(store.get(&r.id).unwrap().approx_md, "approx");
        assert_eq!(store.blocks().len(), 1);
    }

    #[test]
    fn document_reports_nested_raw_ids() {
        let mut store = RawStore::new();
        let outer = store.insert("{\"a\":1}".into(), "".into(), "panel".into());
        let inner = store.insert("{\"b\":2}".into(), "".into(), "media".into());
        let doc = Document::new(vec![
            Block::Raw(outer.clone()),
            Block::BlockQuote(vec![Block::Paragraph(vec![Inline::Raw(inner.clone())])]),
        ]);
        let mut ids = doc.raw_ids();
        ids.sort();
        let mut expect = vec![outer.id, inner.id];
        expect.sort();
        assert_eq!(ids, expect);
    }

    #[test]
    fn inline_text_flattens_marks_and_mentions() {
        let inlines = vec![
            Inline::Text("hello ".into()),
            Inline::Strong(vec![Inline::Text("bold".into())]),
            Inline::Text(" ".into()),
            Inline::Mention {
                id: "5b10a2".into(),
                display: "Ilya A".into(),
            },
            Inline::Text(" ".into()),
            Inline::IssueKey("PROJ-1".into()),
        ];
        assert_eq!(inline_text(&inlines), "hello bold @Ilya A PROJ-1");
    }

    #[test]
    fn normalization_merges_adjacent_text_and_drops_empties() {
        let got = normalize_inlines(vec![
            Inline::Text("a".into()),
            Inline::Text("b".into()),
            Inline::Text(String::new()),
            Inline::Code("c".into()),
        ]);
        assert_eq!(
            got,
            vec![Inline::Text("ab".into()), Inline::Code("c".into())]
        );
    }

    #[test]
    fn normalization_recurses_into_marks() {
        let got = normalize_inlines(vec![Inline::Strong(vec![
            Inline::Text("a".into()),
            Inline::Text("b".into()),
        ])]);
        assert_eq!(got, vec![Inline::Strong(vec![Inline::Text("ab".into())])]);
    }
}
