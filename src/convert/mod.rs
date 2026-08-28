//! Rich-text conversion pipeline [M1.9, M1.10, M2.6].
//!
//! `ADF ⇄ JiredAst ⇄ Markdown` and `wiki ⇄ JiredAst ⇄ Markdown`. One AST means
//! the merge engine, the lossy-region tracking and the file format work the
//! same on both deployments.

pub mod adf;
pub mod ast;
pub mod markdown;
pub mod wiki;

use crate::api::{Markup, RichText};
use ast::{RawBlock, RawStore};

/// Markdown plus the lossy regions that had to be set aside to produce it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Converted {
    pub markdown: String,
    pub raw: Vec<RawBlock>,
}

impl Converted {
    pub fn is_lossy(&self) -> bool {
        !self.raw.is_empty()
    }
}

/// Server rich text → Markdown, capturing anything Markdown cannot express.
pub fn to_markdown(rich: &RichText) -> Converted {
    let mut store = RawStore::new();
    let doc = match rich {
        RichText::Adf(v) => adf::to_ast(v, &mut store),
        RichText::Wiki(s) => wiki::to_ast(s, &mut store),
        RichText::Empty => return Converted::default(),
    };
    Converted {
        markdown: markdown::render(&doc, &store),
        raw: store.into_blocks(),
    }
}

/// Markdown → server rich text, reassembling preserved regions from `raw`.
///
/// The regions are rebuilt from their stored originals rather than from the
/// Markdown, which is what guarantees an untouched region pushes back
/// byte-identical.
pub fn from_markdown(md: &str, markup: Markup, raw: &[RawBlock]) -> RichText {
    if md.trim().is_empty() && raw.is_empty() {
        return RichText::Empty;
    }
    let store = RawStore::from_blocks(raw.to_vec());
    let doc = markdown::parse(md, &store);
    match markup {
        Markup::Adf => RichText::Adf(adf::from_ast(&doc, &store)),
        Markup::Wiki => RichText::Wiki(wiki::from_ast(&doc, &store)),
    }
}

/// Regions whose text a user changed since the file was written.
///
/// Compares the current between-marker text against the approximation that was
/// rendered at sync time; a difference means a read-only region was edited.
pub fn edited_regions(md: &str, raw: &[RawBlock]) -> Vec<String> {
    raw.iter()
        .filter(|b| match markdown::region_body(md, &b.id) {
            // A deleted region counts as an edit: pushing would drop content.
            None => true,
            Some(body) => body.trim_end() != b.approx_md.trim_end(),
        })
        .map(|b| b.id.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_rich_text_converts_to_nothing() {
        let c = to_markdown(&RichText::Empty);
        assert!(c.markdown.is_empty());
        assert!(!c.is_lossy());
        assert_eq!(from_markdown("", Markup::Adf, &[]), RichText::Empty);
    }

    /// The gate has to survive a region that is not at the left margin: a panel
    /// inside a quote or a list is rendered with its container's prefix on every
    /// line, markers included, and that prefix is not an edit.
    #[test]
    fn a_region_inside_a_container_is_not_an_edit() {
        use ast::{Block, Document, ListItem, RawStore};

        let mut store = RawStore::new();
        let quoted = store.insert(
            "{\"n\":1}".into(),
            "> **Warning**\n\nmind the gap".into(),
            "panel (warning)".into(),
        );
        let listed = store.insert(
            "{\"n\":2}".into(),
            "**[jired: mediaSingle — read-only]**".into(),
            "mediaSingle".into(),
        );
        let doc = Document::new(vec![
            Block::BlockQuote(vec![Block::Raw(quoted)]),
            Block::BulletList(vec![ListItem {
                content: vec![Block::Raw(listed)],
            }]),
        ]);
        let md = markdown::render(&doc, &store);

        assert_eq!(
            edited_regions(&md, store.blocks()),
            Vec::<String>::new(),
            "nothing was edited:\n{md}"
        );

        // The prefix is stripped, not ignored: an edit inside the quote is
        // still an edit.
        let touched = md.replace("mind the gap", "mind the step");
        assert_eq!(
            edited_regions(&touched, store.blocks()),
            vec![store.blocks()[0].id.clone()]
        );
    }

    #[test]
    fn a_missing_region_counts_as_edited() {
        let raw = vec![RawBlock {
            id: "rabc123".into(),
            ordinal: 0,
            node_json: "{}".into(),
            approx_md: "approx".into(),
            label: "panel".into(),
        }];
        assert_eq!(edited_regions("no markers here", &raw), vec!["rabc123"]);
    }
}
