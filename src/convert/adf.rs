//! Atlassian Document Format ⇄ [`JiredAst`](super::ast) [M1.9, M2.6].
//!
//! [`to_ast`] is the read path: it maps every ADF node jired can render as
//! Markdown onto the internal AST and captures everything else — panels,
//! expands, media, layouts, macros, node types that do not exist yet — verbatim
//! into a [`RawStore`], leaving a [`Block::Raw`]/[`Inline::Raw`] pointer behind.
//!
//! [`from_ast`] is the write path. Raw regions are never re-converted: their
//! stored `node_json` is parsed back and spliced in unchanged, which is what
//! makes an untouched lossy region push back byte-identical *by construction*
//! rather than by the converter being clever.
//!
//! Both directions are total — malformed input degrades to a preserved raw
//! region instead of an error — so neither function can fail.

use super::ast::{
    normalize_inlines, Block, Document, Inline, ListItem, RawRef, RawStore, Table, TaskItem,
};
use serde_json::{json, Map, Value};

/// Parse an ADF document into the internal AST, capturing unrepresentable
/// nodes into `store`.
///
/// A null value, a non-object, or a doc without `content` yields an empty
/// [`Document`].
pub fn to_ast(value: &Value, store: &mut RawStore) -> Document {
    let Some(content) = value.get("content").and_then(Value::as_array) else {
        return Document::default();
    };
    Document::new(blocks_to_ast(content, store))
}

/// Rebuild an ADF document from the AST. Raw regions are reassembled from
/// `store` verbatim (never re-converted), which is what makes an untouched
/// lossy region push back byte-identical.
pub fn from_ast(doc: &Document, store: &RawStore) -> Value {
    json!({
        "type": "doc",
        "version": 1,
        "content": blocks_to_adf(&doc.blocks, store),
    })
}

// ---------------------------------------------------------------------------
// ADF → AST
// ---------------------------------------------------------------------------

fn blocks_to_ast(nodes: &[Value], store: &mut RawStore) -> Vec<Block> {
    let mut out = Vec::with_capacity(nodes.len());
    for node in nodes {
        out.push(block_to_ast(node, store));
    }
    out
}

fn block_to_ast(node: &Value, store: &mut RawStore) -> Block {
    match node_type(node) {
        "paragraph" => Block::Paragraph(inlines_to_ast(children(node), store)),
        "heading" => Block::Heading {
            level: attr(node, "level")
                .and_then(Value::as_u64)
                .unwrap_or(1)
                .clamp(1, 6) as u8,
            content: inlines_to_ast(children(node), store),
        },
        "codeBlock" => Block::CodeBlock {
            language: attr_str(node, "language").map(str::to_owned),
            code: collect_text(node),
        },
        "blockquote" => Block::BlockQuote(blocks_to_ast(children(node), store)),
        "bulletList" if all_children_are(node, "listItem") => {
            Block::BulletList(list_items_to_ast(node, store))
        }
        "orderedList" if all_children_are(node, "listItem") => Block::OrderedList {
            start: attr(node, "order").and_then(Value::as_u64).unwrap_or(1) as u32,
            items: list_items_to_ast(node, store),
        },
        "taskList" if all_children_are(node, "taskItem") => {
            Block::TaskList(task_items_to_ast(node, store))
        }
        "table" if is_well_formed_table(node) => Block::Table(table_to_ast(node, store)),
        "rule" => Block::Rule,
        _ => Block::Raw(capture(node, store)),
    }
}

fn list_items_to_ast(list: &Value, store: &mut RawStore) -> Vec<ListItem> {
    let mut items = Vec::new();
    for item in children(list) {
        items.push(ListItem {
            content: blocks_to_ast(children(item), store),
        });
    }
    items
}

fn task_items_to_ast(list: &Value, store: &mut RawStore) -> Vec<TaskItem> {
    let mut items = Vec::new();
    for item in children(list) {
        items.push(TaskItem {
            checked: attr_str(item, "state") == Some("DONE"),
            content: inlines_to_ast(children(item), store),
            id: attr_str(item, "localId").map(str::to_owned),
        });
    }
    items
}

fn table_to_ast(node: &Value, store: &mut RawStore) -> Table {
    let mut rows: Vec<Vec<Vec<Block>>> = Vec::new();
    for row in children(node) {
        let mut cells = Vec::new();
        for cell in children(row) {
            cells.push(blocks_to_ast(children(cell), store));
        }
        rows.push(cells);
    }
    let header = first_row_is_header(node).then(|| rows.remove(0));
    Table { header, rows }
}

fn inlines_to_ast(nodes: &[Value], store: &mut RawStore) -> Vec<Inline> {
    let mut out = Vec::with_capacity(nodes.len());
    for node in nodes {
        out.push(match node_type(node) {
            "text" => text_to_ast(node, store),
            "mention" => Inline::Mention {
                id: attr_str(node, "id").unwrap_or_default().to_owned(),
                display: attr_str(node, "text")
                    .unwrap_or_default()
                    .trim_start_matches('@')
                    .to_owned(),
            },
            "hardBreak" => Inline::LineBreak,
            _ => Inline::Raw(capture(node, store)),
        });
    }
    normalize_inlines(out)
}

fn text_to_ast(node: &Value, store: &mut RawStore) -> Inline {
    let text = node.get("text").and_then(Value::as_str).unwrap_or_default();
    let mut marks = MarkSet::default();
    for mark in node
        .get("marks")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
    {
        match node_type(mark) {
            "strong" => marks.strong = true,
            "em" => marks.em = true,
            "strike" => marks.strike = true,
            "code" => marks.code = true,
            "link" => {
                marks.link = Some(attr_str(mark, "href").unwrap_or_default().to_owned());
            }
            // Underline, colours, subscript … have no Markdown form; keeping the
            // whole text node raw preserves them instead of silently dropping.
            _ => return Inline::Raw(capture(node, store)),
        }
    }
    marks.wrap(text)
}

fn capture(node: &Value, store: &mut RawStore) -> RawRef {
    let node_json = serde_json::to_string(node).unwrap_or_else(|_| "null".to_owned());
    store.insert(node_json, approx_md(node), label(node))
}

/// Read-only Markdown shown between the raw markers. Never parsed back.
fn approx_md(node: &Value) -> String {
    let ty = node_type(node);
    match ty {
        "panel" => {
            let kind = attr_str(node, "panelType").unwrap_or("info");
            quoted(
                &format!("⚠️ **[jired: Jira panel ({kind}) — read-only]**"),
                node,
            )
        }
        "expand" | "nestedExpand" => {
            let title = attr_str(node, "title").unwrap_or_default();
            quoted(
                &format!("**[jired: expand \"{title}\" — read-only]**"),
                node,
            )
        }
        "media" | "mediaSingle" | "mediaGroup" | "mediaInline" => {
            format!("**[jired: media {} — read-only]**", media_name(node))
        }
        "status" => format!(
            "**[jired: status {}]**",
            attr_str(node, "text").unwrap_or_default()
        ),
        "date" => format!("**[jired: date {}]**", scalar(attr(node, "timestamp"))),
        _ => format!("**[jired: {ty} — read-only]**"),
    }
}

/// A banner line plus the node's text content, all as blockquote lines.
fn quoted(banner: &str, node: &Value) -> String {
    let mut out = format!("> {banner}");
    for child in children(node) {
        let line = collect_text(child);
        if !line.is_empty() {
            out.push_str("\n> ");
            out.push_str(&line);
        }
    }
    out
}

/// Short human label for the marker table, e.g. `panel (warning)`.
fn label(node: &Value) -> String {
    match node_type(node) {
        "panel" => match attr_str(node, "panelType") {
            Some(kind) => format!("panel ({kind})"),
            None => "panel".to_owned(),
        },
        "media" | "mediaSingle" | "mediaGroup" | "mediaInline" => "media".to_owned(),
        "" => "unknown".to_owned(),
        other => other.to_owned(),
    }
}

fn media_name(node: &Value) -> String {
    fn find(node: &Value) -> Option<&Value> {
        if node_type(node) == "media" {
            return Some(node);
        }
        children(node).iter().find_map(find)
    }
    let media = find(node).unwrap_or(node);
    attr_str(media, "alt")
        .or_else(|| attr_str(media, "id"))
        .unwrap_or("attachment")
        .to_owned()
}

// ---------------------------------------------------------------------------
// AST → ADF
// ---------------------------------------------------------------------------

fn blocks_to_adf(blocks: &[Block], store: &RawStore) -> Vec<Value> {
    blocks
        .iter()
        .filter_map(|block| block_to_adf(block, store))
        .collect()
}

/// `None` only when a raw reference is dangling, in which case the region is
/// skipped rather than fabricated.
fn block_to_adf(block: &Block, store: &RawStore) -> Option<Value> {
    let node = match block {
        Block::Paragraph(content) => container("paragraph", inlines_to_adf(content, store)),
        Block::Heading { level, content } => json!({
            "type": "heading",
            "attrs": { "level": level },
            "content": inlines_to_adf(content, store),
        }),
        Block::CodeBlock { language, code } => {
            let content = if code.is_empty() {
                Vec::new()
            } else {
                vec![json!({ "type": "text", "text": code })]
            };
            match language {
                Some(language) => json!({
                    "type": "codeBlock",
                    "attrs": { "language": language },
                    "content": content,
                }),
                None => container("codeBlock", content),
            }
        }
        Block::BlockQuote(inner) => container("blockquote", blocks_to_adf(inner, store)),
        Block::BulletList(items) => container("bulletList", list_items_to_adf(items, store)),
        Block::OrderedList { start, items } => {
            let content = list_items_to_adf(items, store);
            if *start == 1 {
                container("orderedList", content)
            } else {
                json!({
                    "type": "orderedList",
                    "attrs": { "order": start },
                    "content": content,
                })
            }
        }
        Block::TaskList(items) => {
            let content = items
                .iter()
                .map(|item| {
                    let mut attrs = Map::new();
                    if let Some(id) = &item.id {
                        attrs.insert("localId".into(), Value::from(id.as_str()));
                    }
                    attrs.insert(
                        "state".into(),
                        Value::from(if item.checked { "DONE" } else { "TODO" }),
                    );
                    json!({
                        "type": "taskItem",
                        "attrs": Value::Object(attrs),
                        "content": inlines_to_adf(&item.content, store),
                    })
                })
                .collect();
            container("taskList", content)
        }
        Block::Table(table) => {
            let mut rows = Vec::new();
            if let Some(header) = &table.header {
                rows.push(row_to_adf(header, "tableHeader", store));
            }
            for row in &table.rows {
                rows.push(row_to_adf(row, "tableCell", store));
            }
            container("table", rows)
        }
        Block::Rule => json!({ "type": "rule" }),
        Block::Raw(reference) => return resolve(reference, store),
    };
    Some(node)
}

fn list_items_to_adf(items: &[ListItem], store: &RawStore) -> Vec<Value> {
    items
        .iter()
        .map(|item| container("listItem", blocks_to_adf(&item.content, store)))
        .collect()
}

fn row_to_adf(cells: &[Vec<Block>], cell_type: &str, store: &RawStore) -> Value {
    let cells = cells
        .iter()
        .map(|cell| container(cell_type, blocks_to_adf(cell, store)))
        .collect();
    container("tableRow", cells)
}

fn inlines_to_adf(inlines: &[Inline], store: &RawStore) -> Vec<Value> {
    let mut out = Vec::with_capacity(inlines.len());
    for inline in inlines {
        push_inline(inline, &MarkSet::default(), store, &mut out);
    }
    out
}

fn push_inline(inline: &Inline, marks: &MarkSet, store: &RawStore, out: &mut Vec<Value>) {
    match inline {
        Inline::Text(text) => push_text(text, marks, out),
        Inline::Code(text) => push_text(text, &marks.clone().with_code(), out),
        Inline::Strong(content) => push_all(content, &marks.clone().with_strong(), store, out),
        Inline::Emphasis(content) => push_all(content, &marks.clone().with_em(), store, out),
        Inline::Strike(content) => push_all(content, &marks.clone().with_strike(), store, out),
        Inline::Link { text, href } => push_all(text, &marks.clone().with_link(href), store, out),
        Inline::Mention { id, display } => out.push(json!({
            "type": "mention",
            "attrs": { "id": id, "text": format!("@{display}") },
        })),
        // Bare issue keys are a Markdown-side affordance; ADF gets plain text
        // back and the server re-links it.
        Inline::IssueKey(key) => push_text(key, marks, out),
        Inline::LineBreak => out.push(json!({ "type": "hardBreak" })),
        Inline::Raw(reference) => out.extend(resolve(reference, store)),
    }
}

fn push_all(inlines: &[Inline], marks: &MarkSet, store: &RawStore, out: &mut Vec<Value>) {
    for inline in inlines {
        push_inline(inline, marks, store, out);
    }
}

/// Emit a text node, merging into the previous one when the marks are identical
/// so that nesting artifacts never change the serialized shape.
fn push_text(text: &str, marks: &MarkSet, out: &mut Vec<Value>) {
    if text.is_empty() {
        return;
    }
    let marks = marks.to_adf();
    if let Some(last) = out.last_mut() {
        if last.get("type").and_then(Value::as_str) == Some("text")
            && last.get("marks") == marks.as_ref()
        {
            if let Some(Value::String(previous)) = last.get_mut("text") {
                previous.push_str(text);
                return;
            }
        }
    }
    let mut node = Map::new();
    node.insert("type".into(), Value::from("text"));
    node.insert("text".into(), Value::from(text));
    if let Some(marks) = marks {
        node.insert("marks".into(), marks);
    }
    out.push(Value::Object(node));
}

fn resolve(reference: &RawRef, store: &RawStore) -> Option<Value> {
    let block = store.get(&reference.id)?;
    serde_json::from_str(&block.node_json).ok()
}

// ---------------------------------------------------------------------------
// Marks
// ---------------------------------------------------------------------------

/// The marks in scope for a piece of text.
///
/// Marks form a *set* in ADF but a *tree* in the AST, so both directions fix
/// one order — innermost `code`, then `strike`, `em`, `strong`, with `link`
/// outermost — and the serializer emits the array in the mirrored order.
/// Without that, `[strong, em]` and `[em, strong]` would round-trip to
/// different documents and every pull would show a spurious diff.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct MarkSet {
    strong: bool,
    em: bool,
    strike: bool,
    code: bool,
    link: Option<String>,
}

impl MarkSet {
    fn with_strong(mut self) -> Self {
        self.strong = true;
        self
    }

    fn with_em(mut self) -> Self {
        self.em = true;
        self
    }

    fn with_strike(mut self) -> Self {
        self.strike = true;
        self
    }

    fn with_code(mut self) -> Self {
        self.code = true;
        self
    }

    fn with_link(mut self, href: &str) -> Self {
        self.link = Some(href.to_owned());
        self
    }

    /// Wrap `text` in the canonical nesting order (link outermost, code innermost).
    fn wrap(&self, text: &str) -> Inline {
        let mut inline = if self.code {
            Inline::Code(text.to_owned())
        } else {
            Inline::Text(text.to_owned())
        };
        if self.strike {
            inline = Inline::Strike(vec![inline]);
        }
        if self.em {
            inline = Inline::Emphasis(vec![inline]);
        }
        if self.strong {
            inline = Inline::Strong(vec![inline]);
        }
        if let Some(href) = &self.link {
            inline = Inline::Link {
                text: vec![inline],
                href: href.clone(),
            };
        }
        inline
    }

    /// The `marks` array, outermost first; `None` when unmarked.
    fn to_adf(&self) -> Option<Value> {
        let mut marks = Vec::new();
        if let Some(href) = &self.link {
            marks.push(json!({ "type": "link", "attrs": { "href": href } }));
        }
        if self.strong {
            marks.push(json!({ "type": "strong" }));
        }
        if self.em {
            marks.push(json!({ "type": "em" }));
        }
        if self.strike {
            marks.push(json!({ "type": "strike" }));
        }
        if self.code {
            marks.push(json!({ "type": "code" }));
        }
        (!marks.is_empty()).then(|| Value::Array(marks))
    }
}

// ---------------------------------------------------------------------------
// JSON helpers
// ---------------------------------------------------------------------------

fn container(ty: &str, content: Vec<Value>) -> Value {
    json!({ "type": ty, "content": content })
}

fn node_type(node: &Value) -> &str {
    node.get("type").and_then(Value::as_str).unwrap_or_default()
}

fn attr<'a>(node: &'a Value, key: &str) -> Option<&'a Value> {
    node.get("attrs")?.get(key)
}

fn attr_str<'a>(node: &'a Value, key: &str) -> Option<&'a str> {
    attr(node, key)?.as_str()
}

fn children(node: &Value) -> &[Value] {
    node.get("content")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

/// Recursively concatenate every `text` field, for approximations and code blocks.
fn collect_text(node: &Value) -> String {
    let mut out = String::new();
    if let Some(text) = node.get("text").and_then(Value::as_str) {
        out.push_str(text);
    }
    for child in children(node) {
        out.push_str(&collect_text(child));
    }
    out
}

fn scalar(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
        None => String::new(),
    }
}

/// Degenerate containers (an empty list, a list holding a foreign node) are kept
/// verbatim instead of being coerced into a shape that would not survive a push.
fn all_children_are(node: &Value, ty: &str) -> bool {
    let children = children(node);
    !children.is_empty() && children.iter().all(|child| node_type(child) == ty)
}

fn is_well_formed_table(node: &Value) -> bool {
    let rows = children(node);
    !rows.is_empty()
        && rows.iter().all(|row| {
            node_type(row) == "tableRow"
                && children(row)
                    .iter()
                    .all(|cell| matches!(node_type(cell), "tableHeader" | "tableCell"))
        })
}

fn first_row_is_header(table: &Value) -> bool {
    children(table).first().is_some_and(|row| {
        let cells = children(row);
        !cells.is_empty() && cells.iter().all(|cell| node_type(cell) == "tableHeader")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip(value: &Value) -> Value {
        let mut store = RawStore::new();
        let doc = to_ast(value, &mut store);
        from_ast(&doc, &store)
    }

    /// Every bidirectionally supported node, already in the converter's
    /// canonical shape, so equality pins the serializer as well as the parser.
    fn canonical_document() -> Value {
        json!({
            "type": "doc",
            "version": 1,
            "content": [
                {
                    "type": "heading",
                    "attrs": { "level": 2 },
                    "content": [{ "type": "text", "text": "Fix login flow" }]
                },
                {
                    "type": "paragraph",
                    "content": [
                        { "type": "text", "text": "plain " },
                        { "type": "text", "text": "bold", "marks": [{ "type": "strong" }] },
                        { "type": "text", "text": " " },
                        { "type": "text", "text": "italic", "marks": [{ "type": "em" }] },
                        { "type": "text", "text": " " },
                        { "type": "text", "text": "gone", "marks": [{ "type": "strike" }] },
                        { "type": "text", "text": " " },
                        { "type": "text", "text": "code()", "marks": [{ "type": "code" }] },
                        { "type": "text", "text": " " },
                        {
                            "type": "text",
                            "text": "docs",
                            "marks": [{
                                "type": "link",
                                "attrs": { "href": "https://example.com/a" }
                            }]
                        },
                        { "type": "text", "text": " cc " },
                        { "type": "mention", "attrs": { "id": "5b10a2", "text": "@Ilya A" } },
                        { "type": "hardBreak" },
                        { "type": "text", "text": "second line" }
                    ]
                },
                {
                    "type": "bulletList",
                    "content": [
                        {
                            "type": "listItem",
                            "content": [{
                                "type": "paragraph",
                                "content": [{ "type": "text", "text": "one" }]
                            }]
                        },
                        {
                            "type": "listItem",
                            "content": [{
                                "type": "paragraph",
                                "content": [{ "type": "text", "text": "two" }]
                            }]
                        }
                    ]
                },
                {
                    "type": "orderedList",
                    "attrs": { "order": 3 },
                    "content": [{
                        "type": "listItem",
                        "content": [{
                            "type": "paragraph",
                            "content": [{ "type": "text", "text": "third" }]
                        }]
                    }]
                },
                {
                    "type": "codeBlock",
                    "attrs": { "language": "rust" },
                    "content": [{ "type": "text", "text": "fn main() {}" }]
                },
                {
                    "type": "blockquote",
                    "content": [{
                        "type": "paragraph",
                        "content": [{ "type": "text", "text": "quoted" }]
                    }]
                },
                { "type": "rule" },
                {
                    "type": "table",
                    "content": [
                        {
                            "type": "tableRow",
                            "content": [
                                {
                                    "type": "tableHeader",
                                    "content": [{
                                        "type": "paragraph",
                                        "content": [{ "type": "text", "text": "Env" }]
                                    }]
                                },
                                {
                                    "type": "tableHeader",
                                    "content": [{
                                        "type": "paragraph",
                                        "content": [{ "type": "text", "text": "Status" }]
                                    }]
                                }
                            ]
                        },
                        {
                            "type": "tableRow",
                            "content": [
                                {
                                    "type": "tableCell",
                                    "content": [{
                                        "type": "paragraph",
                                        "content": [{ "type": "text", "text": "prod" }]
                                    }]
                                },
                                {
                                    "type": "tableCell",
                                    "content": [{
                                        "type": "paragraph",
                                        "content": [{ "type": "text", "text": "ok" }]
                                    }]
                                }
                            ]
                        }
                    ]
                },
                {
                    "type": "taskList",
                    "content": [
                        {
                            "type": "taskItem",
                            "attrs": { "localId": "t1", "state": "DONE" },
                            "content": [{ "type": "text", "text": "Capture HAR file" }]
                        },
                        {
                            "type": "taskItem",
                            "attrs": { "localId": "t2", "state": "TODO" },
                            "content": [{ "type": "text", "text": "Reproduce with SAML" }]
                        }
                    ]
                }
            ]
        })
    }

    #[test]
    fn canonical_document_round_trips_unchanged() {
        let value = canonical_document();
        assert_eq!(round_trip(&value), value);
    }

    #[test]
    fn canonical_document_uses_no_raw_blocks() {
        let mut store = RawStore::new();
        let doc = to_ast(&canonical_document(), &mut store);
        assert!(store.is_empty(), "{:?}", store.blocks());
        assert!(doc.raw_ids().is_empty());
        assert_eq!(doc.blocks.len(), 9);
        assert!(matches!(doc.blocks[0], Block::Heading { level: 2, .. }));
        assert!(matches!(doc.blocks[3], Block::OrderedList { start: 3, .. }));
        let Block::Table(table) = &doc.blocks[7] else {
            panic!("expected a table, got {:?}", doc.blocks[7]);
        };
        assert_eq!(table.header.as_ref().map(Vec::len), Some(2));
        assert_eq!(table.rows.len(), 1);
        let Block::TaskList(tasks) = &doc.blocks[8] else {
            panic!("expected a task list");
        };
        assert!(tasks[0].checked && !tasks[1].checked);
        assert_eq!(tasks[0].id.as_deref(), Some("t1"));
    }

    #[test]
    fn panel_is_captured_verbatim_and_pushed_back_byte_identical() {
        let panel = json!({
            "type": "panel",
            "attrs": { "panelType": "warning" },
            "content": [{
                "type": "paragraph",
                "content": [{ "type": "text", "text": "Do not deploy before the cert rotation." }]
            }]
        });
        let value = json!({ "type": "doc", "version": 1, "content": [panel.clone()] });

        let mut store = RawStore::new();
        let doc = to_ast(&value, &mut store);

        assert!(matches!(doc.blocks.as_slice(), [Block::Raw(_)]));
        assert_eq!(store.blocks().len(), 1);
        let raw = &store.blocks()[0];
        assert_eq!(raw.label, "panel (warning)");
        assert_eq!(
            raw.approx_md,
            "> ⚠️ **[jired: Jira panel (warning) — read-only]**\n> Do not deploy before the cert rotation."
        );
        assert_eq!(
            serde_json::from_str::<Value>(&raw.node_json).unwrap(),
            panel,
            "node_json must parse back to the original node"
        );
        assert_eq!(raw.node_json, serde_json::to_string(&panel).unwrap());

        let rebuilt = from_ast(&doc, &store);
        assert_eq!(rebuilt, value);
        assert_eq!(
            serde_json::to_string(&rebuilt["content"][0]).unwrap(),
            serde_json::to_string(&panel).unwrap(),
            "the pushed panel must be byte-identical"
        );
    }

    #[test]
    fn unknown_block_type_is_preserved() {
        let node = json!({ "type": "someFutureThing", "attrs": { "x": 1 } });
        let value = json!({ "type": "doc", "version": 1, "content": [node.clone()] });

        let mut store = RawStore::new();
        let doc = to_ast(&value, &mut store);

        assert!(matches!(doc.blocks.as_slice(), [Block::Raw(_)]));
        let raw = &store.blocks()[0];
        assert_eq!(raw.label, "someFutureThing");
        assert_eq!(raw.approx_md, "**[jired: someFutureThing — read-only]**");
        assert_eq!(raw.node_json, serde_json::to_string(&node).unwrap());
        assert_eq!(from_ast(&doc, &store), value);
    }

    #[test]
    fn unknown_inline_node_is_preserved() {
        let card = json!({
            "type": "inlineCard",
            "attrs": { "url": "https://example.com/browse/PROJ-1" }
        });
        let value = json!({
            "type": "doc",
            "version": 1,
            "content": [{
                "type": "paragraph",
                "content": [{ "type": "text", "text": "see " }, card.clone()]
            }]
        });

        let mut store = RawStore::new();
        let doc = to_ast(&value, &mut store);

        let Block::Paragraph(inlines) = &doc.blocks[0] else {
            panic!("expected a paragraph");
        };
        assert!(matches!(inlines[1], Inline::Raw(_)));
        assert_eq!(
            store.blocks()[0].node_json,
            serde_json::to_string(&card).unwrap()
        );
        assert_eq!(from_ast(&doc, &store), value);
    }

    #[test]
    fn mention_strips_and_restores_the_at_sign() {
        let value = json!({
            "type": "doc",
            "version": 1,
            "content": [{
                "type": "paragraph",
                "content": [{ "type": "mention", "attrs": { "id": "5b10a2", "text": "@Ilya A" } }]
            }]
        });

        let mut store = RawStore::new();
        let doc = to_ast(&value, &mut store);

        let Block::Paragraph(inlines) = &doc.blocks[0] else {
            panic!("expected a paragraph");
        };
        assert_eq!(
            inlines,
            &vec![Inline::Mention {
                id: "5b10a2".into(),
                display: "Ilya A".into(),
            }]
        );
        assert_eq!(from_ast(&doc, &store), value);
    }

    #[test]
    fn mark_order_does_not_affect_the_ast_or_the_output() {
        let with = |marks: Value| {
            json!({
                "type": "doc",
                "version": 1,
                "content": [{
                    "type": "paragraph",
                    "content": [{ "type": "text", "text": "x", "marks": marks }]
                }]
            })
        };
        let a = with(json!([{ "type": "strong" }, { "type": "em" }]));
        let b = with(json!([{ "type": "em" }, { "type": "strong" }]));

        let mut store = RawStore::new();
        let doc_a = to_ast(&a, &mut store);
        let doc_b = to_ast(&b, &mut store);
        assert_eq!(doc_a, doc_b);
        assert_eq!(
            doc_a.blocks[0],
            Block::Paragraph(vec![Inline::Strong(vec![Inline::Emphasis(vec![
                Inline::Text("x".into())
            ])])])
        );

        let out = from_ast(&doc_a, &store);
        assert_eq!(out, from_ast(&doc_b, &store));
        assert_eq!(
            out["content"][0]["content"][0]["marks"],
            json!([{ "type": "strong" }, { "type": "em" }]),
            "marks must serialize outermost first"
        );
    }

    #[test]
    fn all_marks_nest_link_outermost_and_code_innermost() {
        let value = json!({
            "type": "doc",
            "version": 1,
            "content": [{
                "type": "paragraph",
                "content": [{
                    "type": "text",
                    "text": "x",
                    "marks": [
                        { "type": "link", "attrs": { "href": "https://example.com" } },
                        { "type": "strong" },
                        { "type": "em" },
                        { "type": "strike" },
                        { "type": "code" }
                    ]
                }]
            }]
        });
        let mut store = RawStore::new();
        let doc = to_ast(&value, &mut store);
        assert_eq!(
            doc.blocks[0],
            Block::Paragraph(vec![Inline::Link {
                text: vec![Inline::Strong(vec![Inline::Emphasis(vec![
                    Inline::Strike(vec![Inline::Code("x".into())])
                ])])],
                href: "https://example.com".into(),
            }])
        );
        assert_eq!(from_ast(&doc, &store), value);
    }

    #[test]
    fn adjacent_text_under_equal_marks_serializes_as_one_node() {
        let doc = Document::new(vec![Block::Paragraph(vec![
            Inline::Strong(vec![Inline::Text("a".into())]),
            Inline::Strong(vec![Inline::Text("b".into())]),
            Inline::Text("c".into()),
        ])]);
        let out = from_ast(&doc, &RawStore::new());
        assert_eq!(
            out["content"][0]["content"],
            json!([
                { "type": "text", "text": "ab", "marks": [{ "type": "strong" }] },
                { "type": "text", "text": "c" }
            ])
        );
    }

    #[test]
    fn text_with_an_unsupported_mark_stays_raw() {
        let text = json!({
            "type": "text",
            "text": "underlined",
            "marks": [{ "type": "underline" }]
        });
        let value = json!({
            "type": "doc",
            "version": 1,
            "content": [{ "type": "paragraph", "content": [text.clone()] }]
        });
        let mut store = RawStore::new();
        let doc = to_ast(&value, &mut store);
        assert_eq!(store.blocks().len(), 1);
        assert_eq!(from_ast(&doc, &store), value);
    }

    #[test]
    fn media_and_status_get_useful_approximations() {
        let value = json!({
            "type": "doc",
            "version": 1,
            "content": [
                {
                    "type": "mediaSingle",
                    "attrs": { "layout": "center" },
                    "content": [{
                        "type": "media",
                        "attrs": { "id": "abc", "type": "file", "alt": "login-trace.png" }
                    }]
                },
                {
                    "type": "expand",
                    "attrs": { "title": "Details" },
                    "content": [{
                        "type": "paragraph",
                        "content": [{ "type": "text", "text": "hidden text" }]
                    }]
                }
            ]
        });
        let mut store = RawStore::new();
        let doc = to_ast(&value, &mut store);

        assert_eq!(store.blocks()[0].label, "media");
        assert_eq!(
            store.blocks()[0].approx_md,
            "**[jired: media login-trace.png — read-only]**"
        );
        assert_eq!(store.blocks()[1].label, "expand");
        assert_eq!(
            store.blocks()[1].approx_md,
            "> **[jired: expand \"Details\" — read-only]**\n> hidden text"
        );
        assert_eq!(from_ast(&doc, &store), value);
    }

    #[test]
    fn empty_input_yields_an_empty_document() {
        let mut store = RawStore::new();
        assert_eq!(to_ast(&Value::Null, &mut store), Document::default());
        assert_eq!(to_ast(&json!("nope"), &mut store), Document::default());
        assert_eq!(to_ast(&json!({}), &mut store), Document::default());
        assert_eq!(
            to_ast(
                &json!({ "type": "doc", "version": 1, "content": [] }),
                &mut store
            ),
            Document::default()
        );
        assert!(store.is_empty());
        assert_eq!(
            from_ast(&Document::default(), &store),
            json!({ "type": "doc", "version": 1, "content": [] })
        );
    }

    #[test]
    fn dangling_raw_reference_is_skipped_not_panicked_on() {
        let doc = Document::new(vec![
            Block::Raw(RawRef {
                id: "rdeadbe".into(),
            }),
            Block::Rule,
        ]);
        assert_eq!(
            from_ast(&doc, &RawStore::new()),
            json!({ "type": "doc", "version": 1, "content": [{ "type": "rule" }] })
        );
    }
}
