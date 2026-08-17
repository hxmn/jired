//! Markdown ⇄ `JiredAst`: the dialect the working copy is written in
//! [M1.9, M2.1, M2.6].
//!
//! [`render`] is the tail of the read path (ADF/wiki → `JiredAst` → Markdown)
//! and [`parse`] is the head of the write path (file → `JiredAst` → ADF/wiki).
//! The two have to agree exactly — `parse(render(doc)) == doc` is what lets
//! `diff` and `push` see only the edits a user actually made instead of the
//! churn of a lossy re-render.
//!
//! The dialect is CommonMark plus the three GFM extensions Jira needs: tables,
//! strikethrough and task lists. Anything Markdown cannot express is a lossy
//! region: it renders between `<!--jired:raw:ID-->` markers with a read-only
//! approximation inside, and parses back to a [`Block::Raw`]/[`Inline::Raw`]
//! pointer, so push can rebuild the original from `raw_block.node_json` rather
//! than from file text.
//!
//! Decision D9: parsing goes through `comrak` because an *owned* AST is
//! required — the tool merges and then re-renders Markdown, which an
//! event-only parser like `pulldown-cmark` cannot do. The renderer is
//! hand-written rather than comrak's CommonMark formatter so that escaping,
//! marker placement and list numbering stay under our control and stay
//! byte-stable across comrak upgrades.
//!
//! Heading levels are written verbatim here; the +2 demotion that keeps
//! issue-body headings under the file's `##` skeleton is applied by the
//! projector, not by this module.
//!
//! A handful of documents cannot survive the trip unchanged because Markdown
//! itself cannot tell the two forms apart. These normalise instead, always in
//! the same direction, so a second round trip is a no-op:
//!
//! - text that reads as a bare issue key comes back as [`Inline::IssueKey`],
//!   because Jira auto-links it either way;
//! - a link or mention inside link text degrades to its text, since Markdown
//!   forbids nested links;
//! - strikethrough nested in strikethrough collapses into one mark;
//! - a table whose header cells are all empty comes back with no header;
//! - a soft line break (a wrapped source line) becomes a space, as it does
//!   everywhere else in Markdown;
//! - trailing newlines are stripped from code-block text;
//! - [`TaskItem::id`] has nowhere to live in Markdown and comes back `None`.

use crate::convert::ast::{
    inline_text, normalize_inlines, Block, Document, Inline, ListItem, RawRef, RawStore, Table,
    TaskItem,
};
use comrak::nodes::{AstNode, ListType, NodeTaskItem, NodeValue};
use comrak::{parse_document, Arena};

const MARKER_OPEN: &str = "<!--jired:raw:";
const MARKER_CLOSE: &str = "<!--/jired:raw:";
const MARKER_END: &str = "-->";

/// Stand-in written over a marker region before comrak sees the text. Nothing
/// in it can be escaped, split or reinterpreted by the parser, and the dots
/// keep the punctuation on both sides of the region that the markers had, so
/// emphasis next to a region flanks the same way it would have.
const TOKEN_PREFIX: &str = ".jiredrawregion";
const TOKEN_SUFFIX: &str = "end.";

/// Empty HTML comment used to break up constructs that would otherwise fuse
/// (two adjacent lists, `@` immediately before a link, nested emphasis). It is
/// dropped again on parse.
const HTML_GAP: &str = "<!-- -->";

/// How a hard break is written where Markdown has no room for one.
const BREAK_TAG: &str = "<br>";

/// Where inline content is being rendered; each flag rules something out.
#[derive(Debug, Clone, Copy, Default)]
struct Ctx {
    /// Inside a GFM table cell: pipes must be escaped even inside code spans.
    in_table: bool,
    /// Inside a construct that cannot span lines (heading, table cell).
    one_line: bool,
    /// Inside link text, where a nested link would not parse.
    in_link: bool,
}

impl Ctx {
    /// Headings and task items: content must stay on a single line.
    fn one_line(self) -> Self {
        Self {
            one_line: true,
            ..self
        }
    }

    fn inside_link(self) -> Self {
        Self {
            in_link: true,
            ..self
        }
    }

    fn table() -> Self {
        Self {
            in_table: true,
            one_line: true,
            in_link: false,
        }
    }
}

/// Render the AST to Markdown, expanding lossy regions into
/// `<!--jired:raw:ID-->` … `<!--/jired:raw:ID-->` marker pairs whose body is
/// the read-only approximation from the store.
///
/// A `Raw` reference whose id is not in `store` renders as nothing: the region
/// is unresolvable, and emitting a bare marker pair would invite the user to
/// edit content that cannot be pushed back.
pub fn render(doc: &Document, store: &RawStore) -> String {
    let body = render_blocks(&doc.blocks, store, Ctx::default());
    if body.is_empty() {
        String::new()
    } else {
        format!("{body}\n")
    }
}

/// Parse Markdown back into the AST, turning marker pairs back into
/// `Block::Raw` / `Inline::Raw` references.
pub fn parse(md: &str, store: &RawStore) -> Document {
    // The store is deliberately not consulted: an id can appear in a file
    // before its `raw_block` row has been loaded, and it must still parse.
    let _ = store;

    let (prepared, regions) = prescan(md);
    let arena = Arena::new();
    let mut options = comrak::Options::default();
    options.extension.table = true;
    options.extension.strikethrough = true;
    options.extension.tasklist = true;
    let root = parse_document(&arena, &prepared, &options);
    Document::new(convert_blocks(root, &regions))
}

/// The text currently between a region's markers, used to detect that a user
/// edited a read-only region (which `push` refuses unless `--allow-lossy`).
///
/// Returns `None` when either marker is absent. The newline that follows the
/// opening marker and the one that precedes the closing marker belong to the
/// markers, not to the body, so block regions compare equal to the stored
/// approximation.
pub fn region_body(md: &str, id: &str) -> Option<String> {
    let open = format!("{MARKER_OPEN}{id}{MARKER_END}");
    let close = format!("{MARKER_CLOSE}{id}{MARKER_END}");
    let start = md.find(&open)? + open.len();
    let end = start + md[start..].find(&close)?;
    let mut body = &md[start..end];
    if let Some(rest) = body.strip_prefix("\r\n") {
        body = rest;
    } else if let Some(rest) = body.strip_prefix('\n') {
        body = rest;
    }
    if let Some(rest) = body.strip_suffix("\r\n") {
        body = rest;
    } else if let Some(rest) = body.strip_suffix('\n') {
        body = rest;
    }
    Some(body.to_string())
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

fn render_blocks(blocks: &[Block], store: &RawStore, ctx: Ctx) -> String {
    let mut parts: Vec<String> = Vec::with_capacity(blocks.len());
    let mut prev_kind: Option<u8> = None;
    for block in blocks {
        let Some(text) = render_block(block, store, ctx) else {
            continue;
        };
        let kind = list_kind(block);
        // Two lists of the same marker family separated only by a blank line
        // would be re-parsed as one list; the comment breaks them apart.
        if kind.is_some() && kind == prev_kind {
            parts.push(HTML_GAP.to_string());
        }
        prev_kind = kind;
        parts.push(text);
    }
    parts.join("\n\n")
}

fn list_kind(block: &Block) -> Option<u8> {
    match block {
        Block::BulletList(_) | Block::TaskList(_) => Some(b'-'),
        Block::OrderedList { .. } => Some(b'1'),
        _ => None,
    }
}

fn render_block(block: &Block, store: &RawStore, ctx: Ctx) -> Option<String> {
    let text = match block {
        Block::Paragraph(inlines) => render_inlines(inlines, store, ctx),
        Block::Heading { level, content } => {
            let hashes = "#".repeat((*level).clamp(1, 6) as usize);
            let mut body = render_inlines(content, store, ctx.one_line());
            protect_trailing_hash(&mut body);
            if body.is_empty() {
                hashes
            } else {
                format!("{hashes} {body}")
            }
        }
        Block::CodeBlock { language, code } => {
            let fence = "`".repeat(longest_run(code, '`').max(2) + 1);
            let info = language.as_deref().unwrap_or("");
            let mut out = format!("{fence}{info}\n");
            out.push_str(code);
            if !code.is_empty() && !code.ends_with('\n') {
                out.push('\n');
            }
            out.push_str(&fence);
            out
        }
        Block::BlockQuote(inner) => {
            let body = render_blocks(inner, store, ctx);
            prefix_lines(&body, "> ", ">")
        }
        Block::BulletList(items) => {
            let bodies: Vec<String> = items
                .iter()
                .map(|item| render_blocks(&item.content, store, ctx))
                .collect();
            // `- ---` is a thematic break, not an item holding a rule, so a
            // list like that has to use the other bullet character.
            let marker = if bodies.iter().any(|b| starts_like_dash_rule(b)) {
                "* "
            } else {
                "- "
            };
            let rendered: Vec<String> = bodies
                .iter()
                .map(|body| indent_item(body, marker, "  "))
                .collect();
            rendered.join("\n")
        }
        Block::OrderedList { start, items } => {
            let rendered: Vec<String> = items
                .iter()
                .enumerate()
                .map(|(i, item)| {
                    let marker = format!("{}. ", start.saturating_add(i as u32));
                    let indent = " ".repeat(marker.len());
                    let body = render_blocks(&item.content, store, ctx);
                    indent_item(&body, &marker, &indent)
                })
                .collect();
            rendered.join("\n")
        }
        Block::TaskList(items) => {
            let rendered: Vec<String> = items
                .iter()
                .map(|item| {
                    let box_ = if item.checked { "- [x]" } else { "- [ ]" };
                    let body = render_inlines(&item.content, store, ctx.one_line());
                    if body.is_empty() {
                        box_.to_string()
                    } else {
                        indent_item(&body, &format!("{box_} "), "      ")
                    }
                })
                .collect();
            rendered.join("\n")
        }
        Block::Table(table) => render_table(table, store),
        Block::Rule => "---".to_string(),
        Block::Raw(r) => match store.get(&r.id) {
            Some(raw) => format!(
                "{MARKER_OPEN}{id}{MARKER_END}\n{body}\n{MARKER_CLOSE}{id}{MARKER_END}",
                id = r.id,
                body = raw.approx_md
            ),
            None => String::new(),
        },
    };
    if text.is_empty() {
        None
    } else {
        Some(text)
    }
}

fn render_table(table: &Table, store: &RawStore) -> String {
    let width = table
        .header
        .as_ref()
        .map(|h| h.len())
        .unwrap_or(0)
        .max(table.rows.iter().map(|r| r.len()).max().unwrap_or(0))
        .max(1);

    let header: Vec<String> = match &table.header {
        Some(cells) => (0..width)
            .map(|i| {
                cells
                    .get(i)
                    .map(|c| render_cell(c, store))
                    .unwrap_or_default()
            })
            .collect(),
        None => vec![String::new(); width],
    };

    let mut out = render_row(&header);
    out.push('\n');
    out.push('|');
    for _ in 0..width {
        out.push_str(" --- |");
    }
    for row in &table.rows {
        let cells: Vec<String> = (0..width)
            .map(|i| {
                row.get(i)
                    .map(|c| render_cell(c, store))
                    .unwrap_or_default()
            })
            .collect();
        out.push('\n');
        out.push_str(&render_row(&cells));
    }
    out
}

fn render_row(cells: &[String]) -> String {
    let mut out = String::from("|");
    for cell in cells {
        out.push(' ');
        out.push_str(cell);
        out.push_str(" |");
    }
    out
}

fn render_cell(cell: &[Block], store: &RawStore) -> String {
    match cell {
        [Block::Paragraph(inlines)] => render_inlines(inlines, store, Ctx::table()),
        _ => render_blocks(cell, store, Ctx::table()).replace('\n', " "),
    }
}

fn render_inlines(inlines: &[Inline], store: &RawStore, ctx: Ctx) -> String {
    let mut out = String::new();
    // What the *next* thing written has to start with for the thing just
    // written to keep its meaning.
    let mut pending = Need::None;

    for inline in inlines {
        match inline {
            Inline::Text(t) => {
                let mut fragment = String::new();
                escape_text(&mut fragment, t, out.is_empty() || out.ends_with('\n'));
                push_fragment(&mut out, &mut pending, &fragment, Need::None, Need::None);
            }
            Inline::LineBreak => {
                // A hard break needs content before it on its line, and a
                // heading or table cell cannot hold one at all. Two tags in a
                // row also keep the line from becoming an HTML block.
                if ctx.one_line || out.is_empty() || out.ends_with('\n') || out.ends_with(BREAK_TAG)
                {
                    out.push_str(BREAK_TAG);
                } else {
                    protect_trailing_space(&mut out);
                    out.push_str("  \n");
                }
                pending = Need::None;
            }
            Inline::Strong(content) => push_mark(
                &mut out,
                &mut pending,
                "**",
                Some("__"),
                content,
                store,
                ctx,
            ),
            Inline::Emphasis(content) => {
                push_mark(&mut out, &mut pending, "*", Some("_"), content, store, ctx)
            }
            Inline::Strike(content) => {
                // `~~` has no second spelling and nested strikethrough adds no
                // meaning, so it collapses into the outer mark.
                let flat = flatten_strike(content);
                push_mark(&mut out, &mut pending, "~~", None, &flat, store, ctx)
            }
            Inline::Code(code) => {
                let fragment = render_code_span(code, ctx);
                push_fragment(&mut out, &mut pending, &fragment, Need::None, Need::None);
            }
            Inline::Link { text, href } => {
                // Markdown forbids a link inside a link, so nested targets are
                // dropped down to their text rather than corrupting the file.
                let inner = render_inlines(text, store, ctx.inside_link());
                let fragment = if ctx.in_link {
                    inner
                } else {
                    format!("[{inner}]({})", render_href(href, ctx))
                };
                push_fragment(&mut out, &mut pending, &fragment, Need::None, Need::None);
            }
            Inline::Mention { id, display } => {
                let mut label = String::new();
                escape_text(&mut label, display, true);
                protect_trailing_space(&mut label);
                let fragment = if ctx.in_link {
                    format!("@{label}")
                } else {
                    format!("@[{label}]({})", render_href(id, ctx))
                };
                push_fragment(&mut out, &mut pending, &fragment, Need::None, Need::None);
            }
            // A key only auto-links on a word boundary, so it has to keep one
            // on either side.
            Inline::IssueKey(key) => {
                push_fragment(&mut out, &mut pending, key, Need::NonWord, Need::NonWord)
            }
            Inline::Raw(r) => {
                if let Some(raw) = store.get(&r.id) {
                    let fragment = format!(
                        "{MARKER_OPEN}{id}{MARKER_END}{body}{MARKER_CLOSE}{id}{MARKER_END}",
                        id = r.id,
                        body = raw.approx_md
                    );
                    push_fragment(&mut out, &mut pending, &fragment, Need::None, Need::None);
                }
            }
        }
    }

    // A hard break at the very end of a block would be dropped by the parser.
    if out.ends_with("  \n") {
        out.truncate(out.len() - "  \n".len());
        out.push_str(BREAK_TAG);
    }
    protect_trailing_space(&mut out);
    out
}

/// Wrap `content` in `delim`, keeping the delimiters parseable.
///
/// A nested mark of the same kind would fuse its delimiters with ours (`*` +
/// `*a*` + `*` reads as strong emphasis), so such a mark switches to the other
/// spelling — `_x_` for `*x*` — which is why `alternate` exists. Strikethrough
/// has no second spelling and falls back to gap comments. Either way the
/// delimiters only flank correctly with punctuation or a space outside them,
/// which is what the [`Need`] handed to [`push_fragment`] enforces.
fn push_mark(
    out: &mut String,
    pending: &mut Need,
    primary: &str,
    alternate: Option<&str>,
    content: &[Inline],
    store: &RawStore,
    ctx: Ctx,
) {
    let inner = render_inlines(content, store, ctx);
    if inner.is_empty() {
        return;
    }
    let marker_of = |delim: &str| delim.chars().next().unwrap_or('*');

    let mut delim = primary;
    let mut alternate_used = false;
    let mut gaps = false;
    if contains_live(&inner, marker_of(primary)) {
        match alternate {
            Some(other) if !contains_live(&inner, marker_of(other)) => {
                delim = other;
                alternate_used = true;
            }
            _ => gaps = true,
        }
    }

    let mut fragment = String::from(delim);
    if gaps {
        fragment.push_str(HTML_GAP);
    }
    fragment.push_str(&inner);
    if gaps {
        fragment.push_str(HTML_GAP);
    }
    fragment.push_str(delim);

    // `_` never works inside a word, so the alternate spelling always needs a
    // boundary around it.
    let inside_open = flank_after(&fragment[delim.len()..]);
    let inside_close = flank_before(&fragment[..fragment.len() - delim.len()]);
    let before = need_for_flank(alternate_used || inside_open.is_some_and(is_punctuation));
    let after = need_for_flank(alternate_used || inside_close.is_some_and(is_punctuation));
    push_fragment(out, pending, &fragment, before, after);
}

/// Whether `marker` appears in already-rendered Markdown as something the
/// parser would read as a delimiter — escapes and code spans do not count.
fn contains_live(rendered: &str, marker: char) -> bool {
    let chars: Vec<char> = rendered.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '\\' => i += 2,
            '`' => {
                let fence = chars[i..].iter().take_while(|c| **c == '`').count();
                let mut j = i + fence;
                let mut end = None;
                while j < chars.len() {
                    if chars[j] == '`' {
                        let run = chars[j..].iter().take_while(|c| **c == '`').count();
                        if run == fence {
                            end = Some(j + run);
                            break;
                        }
                        j += run;
                    } else {
                        j += 1;
                    }
                }
                i = end.unwrap_or(i + fence);
            }
            c if c == marker => return true,
            _ => i += 1,
        }
    }
    false
}

/// Drop strikethrough marks nested inside a strikethrough, at any depth.
fn flatten_strike(inlines: &[Inline]) -> Vec<Inline> {
    let mut out = Vec::new();
    for inline in inlines {
        match inline {
            Inline::Strike(inner) => out.extend(flatten_strike(inner)),
            Inline::Strong(inner) => out.push(Inline::Strong(flatten_strike(inner))),
            Inline::Emphasis(inner) => out.push(Inline::Emphasis(flatten_strike(inner))),
            Inline::Link { text, href } => out.push(Inline::Link {
                text: flatten_strike(text),
                href: href.clone(),
            }),
            other => out.push(other.clone()),
        }
    }
    out
}

fn need_for_flank(touches_punctuation: bool) -> Need {
    if touches_punctuation {
        Need::PunctuationOrSpace
    } else {
        Need::None
    }
}

/// Append `fragment`, inserting gap comments so that both sides of the seam
/// keep their meaning.
fn push_fragment(out: &mut String, pending: &mut Need, fragment: &str, before: Need, after: Need) {
    let Some(first) = fragment.chars().next() else {
        return;
    };
    if pending.violated(first, flank_after(fragment)) {
        out.push_str(HTML_GAP);
    }
    if let Some(prev) = out.chars().last() {
        if before.violated(prev, flank_before(out)) {
            out.push_str(HTML_GAP);
        }
    }
    separate(out, fragment);
    out.push_str(fragment);
    *pending = after;
}

/// What has to sit next to a fragment for it to parse back the same way.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Need {
    None,
    /// An emphasis delimiter that touches punctuation on the inside only
    /// flanks if punctuation or whitespace is on the outside too.
    PunctuationOrSpace,
    /// An issue key only auto-links on a word boundary.
    NonWord,
}

impl Need {
    /// `adjacent` is the character that actually abuts the seam; `flanking` is
    /// the one the emphasis rules look at, which is not the same thing.
    fn violated(self, adjacent: char, flanking: Option<char>) -> bool {
        match self {
            Need::None => false,
            Need::PunctuationOrSpace => {
                flanking.is_some_and(|c| !c.is_whitespace() && !is_punctuation(c))
            }
            Need::NonWord => adjacent.is_alphanumeric() || adjacent == '-' || adjacent == '_',
        }
    }
}

/// The character an emphasis delimiter placed after `s` would flank against.
///
/// With the strikethrough extension on, comrak looks straight through `~`, so
/// `~~word~~` flanks as if the `word` were touching the delimiter.
fn flank_before(s: &str) -> Option<char> {
    s.trim_end_matches('~').chars().last()
}

/// The character an emphasis delimiter placed before `s` would flank against.
fn flank_after(s: &str) -> Option<char> {
    s.trim_start_matches('~').chars().next()
}

/// CommonMark's flanking rules treat everything that is neither alphanumeric
/// nor whitespace as punctuation.
fn is_punctuation(c: char) -> bool {
    !c.is_alphanumeric() && !c.is_whitespace()
}

/// Insert a gap when the character already written would fuse with the one
/// about to be written — `@` before a link would read as a mention, and equal
/// delimiter characters would form a longer run.
fn separate(out: &mut String, next: &str) {
    let (Some(last), Some(first)) = (last_literal_char(out), next.chars().next()) else {
        return;
    };
    let fuses =
        (last == '@' && first == '[') || (last == first && matches!(last, '*' | '_' | '~' | '`'));
    if fuses {
        out.push_str(HTML_GAP);
    }
}

fn render_code_span(code: &str, ctx: Ctx) -> String {
    if code.is_empty() {
        return String::new();
    }
    let body = if ctx.in_table {
        code.replace('|', "\\|")
    } else {
        code.to_string()
    };
    let fence = "`".repeat(longest_run(&body, '`') + 1);
    // CommonMark strips one space from each end of a code span that has them
    // both, so content with edge spaces or backticks needs a padding pair.
    let all_spaces = body.chars().all(|c| c == ' ');
    let needs_pad = !all_spaces
        && (body.starts_with(' ')
            || body.ends_with(' ')
            || body.starts_with('`')
            || body.ends_with('`'));
    if needs_pad {
        format!("{fence} {body} {fence}")
    } else {
        format!("{fence}{body}{fence}")
    }
}

fn render_href(href: &str, ctx: Ctx) -> String {
    let pointy = href.is_empty()
        || href
            .chars()
            .any(|c| c.is_whitespace() || matches!(c, '(' | ')' | '<' | '>'));
    let mut out = String::new();
    if pointy {
        out.push('<');
    }
    for c in href.chars() {
        match c {
            '<' | '>' | '\\' => {
                out.push('\\');
                out.push(c);
            }
            '|' if ctx.in_table => out.push_str("\\|"),
            '\n' | '\r' => out.push(' '),
            _ => out.push(c),
        }
    }
    if pointy {
        out.push('>');
    }
    out
}

/// Escape everything in literal text that Markdown would otherwise read as
/// structure. Never called for code spans or code blocks.
fn escape_text(out: &mut String, text: &str, at_line_start: bool) {
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    // A newline inside literal text is written as a character reference, so
    // only the very first character can sit at the start of a line.
    let mut line_start = at_line_start;
    while i < chars.len() {
        let c = chars[i];
        let at_start = line_start;
        line_start = false;
        if at_start {
            match c {
                // A leading space would be stripped, or start indented code.
                ' ' => {
                    out.push_str("&#32;");
                    i += 1;
                    continue;
                }
                '#' | '-' | '+' | '=' => {
                    out.push('\\');
                    out.push(c);
                    i += 1;
                    continue;
                }
                '0'..='9' => {
                    let mut j = i;
                    while j < chars.len() && chars[j].is_ascii_digit() {
                        j += 1;
                    }
                    if j < chars.len() && (chars[j] == '.' || chars[j] == ')') {
                        for &d in &chars[i..j] {
                            out.push(d);
                        }
                        out.push('\\');
                        out.push(chars[j]);
                        i = j + 1;
                        continue;
                    }
                }
                _ => {}
            }
        }
        match c {
            '\\' | '`' | '*' | '_' | '[' | ']' | '<' | '>' | '~' | '|' => {
                out.push('\\');
                out.push(c);
            }
            '&' => out.push_str("&amp;"),
            '\n' => out.push_str("&#10;"),
            '\t' => out.push_str("&#9;"),
            _ => out.push(c),
        }
        i += 1;
    }
}

/// Trailing spaces are either stripped or read as a hard break, so encode the
/// last one as a character reference.
fn protect_trailing_space(s: &mut String) {
    if s.ends_with(' ') {
        s.pop();
        s.push_str("&#32;");
    }
}

/// `## title #` would lose its trailing hash to the ATX closing sequence.
fn protect_trailing_hash(s: &mut String) {
    if last_literal_char(s) != Some('#') {
        return;
    }
    let idx = s.len() - '#'.len_utf8();
    s.insert(idx, '\\');
}

/// Last character of `s`, unless it is backslash-escaped.
fn last_literal_char(s: &str) -> Option<char> {
    let mut rev = s.chars().rev();
    let last = rev.next()?;
    let backslashes = rev.take_while(|c| *c == '\\').count();
    if backslashes % 2 == 0 {
        Some(last)
    } else {
        None
    }
}

fn longest_run(s: &str, c: char) -> usize {
    let mut best = 0;
    let mut run = 0;
    for ch in s.chars() {
        if ch == c {
            run += 1;
            best = best.max(run);
        } else {
            run = 0;
        }
    }
    best
}

fn prefix_lines(body: &str, prefix: &str, empty_prefix: &str) -> String {
    body.split('\n')
        .map(|line| {
            if line.is_empty() {
                empty_prefix.to_string()
            } else {
                format!("{prefix}{line}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn starts_like_dash_rule(body: &str) -> bool {
    let line = body.split('\n').next().unwrap_or_default();
    line.contains('-') && line.chars().all(|c| c == '-' || c == ' ' || c == '\t')
}

fn indent_item(body: &str, marker: &str, indent: &str) -> String {
    if body.is_empty() {
        return marker.trim_end().to_string();
    }
    let mut out = String::new();
    for (i, line) in body.split('\n').enumerate() {
        if i > 0 {
            out.push('\n');
        }
        if i == 0 {
            out.push_str(marker);
        } else if !line.is_empty() {
            out.push_str(indent);
        }
        out.push_str(line);
    }
    out
}

// ---------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------

/// A marker region found by the pre-scan.
struct Region {
    id: String,
    /// The opening marker ended its line, i.e. the region was rendered from a
    /// [`Block::Raw`] rather than an [`Inline::Raw`].
    block: bool,
}

/// Replace every complete marker pair with an opaque word before comrak sees
/// the text.
///
/// This has to happen first: comrak would read the markers as HTML blocks and
/// the approximation between them as ordinary content, and the approximation is
/// not the source of truth — the raw store is.
fn prescan(md: &str) -> (String, Vec<Region>) {
    let mut out = String::with_capacity(md.len());
    let mut regions: Vec<Region> = Vec::new();
    let mut pos = 0;

    while let Some(rel) = md[pos..].find(MARKER_OPEN) {
        let open_start = pos + rel;
        let id_start = open_start + MARKER_OPEN.len();
        let Some(id_len) = md[id_start..].find(MARKER_END) else {
            break;
        };
        let id = &md[id_start..id_start + id_len];
        let open_end = id_start + id_len + MARKER_END.len();

        if id.is_empty() || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            out.push_str(&md[pos..open_end]);
            pos = open_end;
            continue;
        }

        let close = format!("{MARKER_CLOSE}{id}{MARKER_END}");
        let Some(close_rel) = md[open_end..].find(&close) else {
            out.push_str(&md[pos..open_end]);
            pos = open_end;
            continue;
        };
        let close_start = open_end + close_rel;

        let tail = &md[open_end..close_start];
        let block = match tail.find('\n') {
            Some(nl) => tail[..nl].trim().is_empty(),
            None => false,
        };

        out.push_str(&md[pos..open_start]);
        out.push_str(TOKEN_PREFIX);
        out.push_str(&regions.len().to_string());
        out.push_str(TOKEN_SUFFIX);
        regions.push(Region {
            id: id.to_string(),
            block,
        });
        pos = close_start + close.len();
    }

    out.push_str(&md[pos..]);
    (out, regions)
}

fn convert_blocks<'a>(node: &'a AstNode<'a>, regions: &[Region]) -> Vec<Block> {
    let mut out = Vec::new();
    for child in node.children() {
        let value = child.data.borrow().value.clone();
        match value {
            NodeValue::Paragraph => {
                let inlines = normalize_inlines(convert_inlines(child, regions));
                if let [Inline::Raw(r)] = inlines.as_slice() {
                    if regions.iter().any(|reg| reg.id == r.id && reg.block) {
                        out.push(Block::Raw(r.clone()));
                        continue;
                    }
                }
                if !inlines.is_empty() {
                    out.push(Block::Paragraph(inlines));
                }
            }
            NodeValue::Heading(h) => out.push(Block::Heading {
                level: h.level,
                content: normalize_inlines(convert_inlines(child, regions)),
            }),
            NodeValue::CodeBlock(c) => {
                let language = c
                    .info
                    .split_whitespace()
                    .next()
                    .filter(|s| !s.is_empty())
                    .map(str::to_string);
                let mut code = c.literal.clone();
                // comrak always terminates a fenced block's literal with a
                // newline; the AST stores code without it.
                if code.ends_with('\n') {
                    code.pop();
                    if code.ends_with('\r') {
                        code.pop();
                    }
                }
                out.push(Block::CodeBlock { language, code });
            }
            // A break tag alone on its line is an HTML *block*, which is how a
            // paragraph holding nothing but a hard break comes back.
            NodeValue::HtmlBlock(html) if is_break_tag(&html.literal) => {
                out.push(Block::Paragraph(vec![Inline::LineBreak]))
            }
            NodeValue::BlockQuote => out.push(Block::BlockQuote(convert_blocks(child, regions))),
            NodeValue::List(list) => out.push(convert_list(child, &list, regions)),
            NodeValue::ThematicBreak => out.push(Block::Rule),
            NodeValue::Table(_) => out.push(convert_table(child, regions)),
            _ => {}
        }
    }
    out
}

fn convert_list<'a>(
    node: &'a AstNode<'a>,
    list: &comrak::nodes::NodeList,
    regions: &[Region],
) -> Block {
    let is_task = node
        .children()
        .any(|c| matches!(c.data.borrow().value, NodeValue::TaskItem(_)));

    if is_task {
        let items = node
            .children()
            .map(|item| {
                let checked = matches!(
                    item.data.borrow().value,
                    NodeValue::TaskItem(NodeTaskItem {
                        symbol: Some(_),
                        ..
                    })
                );
                TaskItem {
                    checked,
                    content: normalize_inlines(first_paragraph_inlines(item, regions)),
                    // Markdown has nowhere to keep the ADF task id; the write
                    // path restores it from the base snapshot.
                    id: None,
                }
            })
            .collect();
        return Block::TaskList(items);
    }

    let items: Vec<ListItem> = node
        .children()
        .map(|item| ListItem {
            content: convert_blocks(item, regions),
        })
        .collect();

    if list.list_type == ListType::Ordered {
        Block::OrderedList {
            start: list.start as u32,
            items,
        }
    } else {
        Block::BulletList(items)
    }
}

fn first_paragraph_inlines<'a>(node: &'a AstNode<'a>, regions: &[Region]) -> Vec<Inline> {
    for child in node.children() {
        if matches!(child.data.borrow().value, NodeValue::Paragraph) {
            return convert_inlines(child, regions);
        }
    }
    Vec::new()
}

fn convert_table<'a>(node: &'a AstNode<'a>, regions: &[Region]) -> Block {
    let mut table = Table::default();
    for row in node.children() {
        let is_header = match row.data.borrow().value {
            NodeValue::TableRow(header) => header,
            _ => continue,
        };
        let cells: Vec<Vec<Block>> = row
            .children()
            .map(|cell| {
                let inlines = normalize_inlines(convert_inlines(cell, regions));
                if inlines.is_empty() {
                    Vec::new()
                } else {
                    vec![Block::Paragraph(inlines)]
                }
            })
            .collect();
        if is_header {
            // GFM always has a header row; an all-empty one is how a headerless
            // table is written, so it maps back to `None`.
            if cells.iter().any(|c| !c.is_empty()) {
                table.header = Some(cells);
            }
        } else {
            table.rows.push(cells);
        }
    }
    Block::Table(table)
}

fn convert_inlines<'a>(node: &'a AstNode<'a>, regions: &[Region]) -> Vec<Inline> {
    let kids: Vec<&'a AstNode<'a>> = node.children().collect();
    let mut out: Vec<Inline> = Vec::new();

    for (i, child) in kids.iter().copied().enumerate() {
        let value = child.data.borrow().value.clone();
        match value {
            NodeValue::Text(text) => push_text(&mut out, &text, regions),
            // A wrapped source line is a space, not a break: Markdown reflow
            // must not change what gets pushed.
            NodeValue::SoftBreak => out.push(Inline::Text(" ".to_string())),
            NodeValue::LineBreak => out.push(Inline::LineBreak),
            NodeValue::Code(code) => out.push(Inline::Code(code.literal.clone())),
            NodeValue::Emph => out.push(Inline::Emphasis(convert_inlines(child, regions))),
            NodeValue::Strong => out.push(Inline::Strong(convert_inlines(child, regions))),
            NodeValue::Strikethrough => out.push(Inline::Strike(convert_inlines(child, regions))),
            NodeValue::Escaped => out.extend(convert_inlines(child, regions)),
            // Every other raw tag is dropped: it has no AST equivalent and the
            // gap comments this module writes must not survive.
            NodeValue::HtmlInline(html) if is_break_tag(&html) => out.push(Inline::LineBreak),
            // An image keeps its target as a plain link rather than vanishing;
            // real Jira media arrives as a raw region, not as Markdown.
            NodeValue::Link(link) | NodeValue::Image(link) => {
                let text = convert_inlines(child, regions);
                let adjacent_at = i > 0
                    && matches!(&kids[i - 1].data.borrow().value,
                        NodeValue::Text(t) if t.ends_with('@'));
                let mut mention = false;
                if adjacent_at {
                    if let Some(Inline::Text(prev)) = out.last_mut() {
                        if prev.ends_with('@') {
                            prev.pop();
                            mention = true;
                        }
                    }
                }
                if mention {
                    if matches!(out.last(), Some(Inline::Text(t)) if t.is_empty()) {
                        out.pop();
                    }
                    out.push(Inline::Mention {
                        id: link.url.clone(),
                        display: inline_text(&normalize_inlines(text)),
                    });
                } else {
                    out.push(Inline::Link {
                        text,
                        href: link.url.clone(),
                    });
                }
            }
            _ => {}
        }
    }
    out
}

fn is_break_tag(html: &str) -> bool {
    let tag = html.trim().to_ascii_lowercase();
    matches!(tag.as_str(), "<br>" | "<br/>" | "<br />")
}

fn push_text(out: &mut Vec<Inline>, text: &str, regions: &[Region]) {
    let mut rest = text;
    while let Some((before, index, after)) = split_placeholder(rest) {
        push_plain(out, before);
        if let Some(region) = regions.get(index) {
            out.push(Inline::Raw(RawRef {
                id: region.id.clone(),
            }));
        }
        rest = after;
    }
    push_plain(out, rest);
}

fn split_placeholder(s: &str) -> Option<(&str, usize, &str)> {
    let start = s.find(TOKEN_PREFIX)?;
    let after = &s[start + TOKEN_PREFIX.len()..];
    let digits = after.len() - after.trim_start_matches(|c: char| c.is_ascii_digit()).len();
    if digits == 0 {
        return None;
    }
    let index: usize = after[..digits].parse().ok()?;
    let tail = after[digits..].strip_prefix(TOKEN_SUFFIX)?;
    Some((&s[..start], index, tail))
}

/// Split literal text on bare issue keys, which the server auto-links.
///
/// Text that merely looks like a key becomes [`Inline::IssueKey`]; that is
/// deliberate, since Markdown cannot distinguish the two and Jira links both.
fn push_plain(out: &mut Vec<Inline>, text: &str) {
    if text.is_empty() {
        return;
    }
    let chars: Vec<char> = text.chars().collect();
    let mut segment = String::new();
    let mut i = 0;
    while i < chars.len() {
        if let Some(len) = issue_key_at(&chars, i) {
            if !segment.is_empty() {
                out.push(Inline::Text(std::mem::take(&mut segment)));
            }
            out.push(Inline::IssueKey(chars[i..i + len].iter().collect()));
            i += len;
            continue;
        }
        segment.push(chars[i]);
        i += 1;
    }
    if !segment.is_empty() {
        out.push(Inline::Text(segment));
    }
}

fn issue_key_at(chars: &[char], i: usize) -> Option<usize> {
    if i > 0 {
        let prev = chars[i - 1];
        if prev.is_alphanumeric() || prev == '_' || prev == '-' {
            return None;
        }
    }
    if !chars[i].is_ascii_uppercase() {
        return None;
    }
    let mut j = i + 1;
    while j < chars.len()
        && (chars[j].is_ascii_uppercase() || chars[j].is_ascii_digit() || chars[j] == '_')
    {
        j += 1;
    }
    if chars.get(j) != Some(&'-') {
        return None;
    }
    j += 1;
    let digits_start = j;
    while j < chars.len() && chars[j].is_ascii_digit() {
        j += 1;
    }
    if j == digits_start {
        return None;
    }
    if let Some(next) = chars.get(j) {
        if next.is_alphanumeric() || *next == '-' || *next == '_' {
            return None;
        }
    }
    Some(j - i)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip(doc: &Document, store: &RawStore) -> String {
        let md = render(doc, store);
        let back = parse(&md, store);
        assert_eq!(&back, doc, "round trip differs; markdown was:\n{md}");
        md
    }

    fn para(text: &str) -> Block {
        Block::Paragraph(vec![Inline::Text(text.to_string())])
    }

    fn cell(text: &str) -> Vec<Block> {
        vec![para(text)]
    }

    #[test]
    fn every_block_type_round_trips() {
        let store = RawStore::new();
        let doc = Document::new(vec![
            Block::Heading {
                level: 2,
                content: vec![Inline::Text("Title".into())],
            },
            para("A plain paragraph."),
            Block::CodeBlock {
                language: Some("rust".into()),
                code: "fn main() {\n    let x = 1;\n}".into(),
            },
            Block::CodeBlock {
                language: None,
                code: "no language".into(),
            },
            Block::BlockQuote(vec![para("quoted line"), para("second quoted line")]),
            Block::BulletList(vec![
                ListItem {
                    content: vec![para("first")],
                },
                ListItem {
                    content: vec![para("second")],
                },
            ]),
            Block::OrderedList {
                start: 1,
                items: vec![ListItem {
                    content: vec![para("step one")],
                }],
            },
            Block::TaskList(vec![TaskItem {
                checked: false,
                content: vec![Inline::Text("open task".into())],
                id: None,
            }]),
            Block::Table(Table {
                header: Some(vec![cell("name"), cell("value")]),
                rows: vec![vec![cell("alpha"), cell("beta")]],
            }),
            Block::Rule,
            para("after the rule"),
        ]);
        round_trip(&doc, &store);
    }

    #[test]
    fn every_inline_type_round_trips() {
        let store = RawStore::new();
        let doc = Document::new(vec![Block::Paragraph(vec![
            Inline::Text("plain ".into()),
            Inline::Strong(vec![Inline::Text("bold".into())]),
            Inline::Text(" ".into()),
            Inline::Emphasis(vec![Inline::Text("italic".into())]),
            Inline::Text(" ".into()),
            Inline::Strike(vec![Inline::Text("gone".into())]),
            Inline::Text(" ".into()),
            Inline::Code("let x = 1;".into()),
            Inline::Text(" ".into()),
            Inline::Link {
                text: vec![Inline::Text("a site".into())],
                href: "https://example.com/x".into(),
            },
            Inline::Text(" ".into()),
            Inline::Mention {
                id: "557058:abc-def".into(),
                display: "Ilya A".into(),
            },
            Inline::Text(" owns ".into()),
            Inline::IssueKey("PROJ-42".into()),
            Inline::LineBreak,
            Inline::Text("after a hard break".into()),
        ])]);
        let md = round_trip(&doc, &store);
        assert!(md.contains("@[Ilya A](557058:abc-def)"), "{md}");
        assert!(md.contains("~~gone~~"), "{md}");
        assert!(md.contains("  \n"), "hard break not rendered: {md:?}");
    }

    #[test]
    fn nested_marks_round_trip() {
        let store = RawStore::new();
        let doc = Document::new(vec![Block::Paragraph(vec![
            Inline::Strong(vec![
                Inline::Text("bold ".into()),
                Inline::Emphasis(vec![Inline::Text("and italic".into())]),
            ]),
            Inline::Text(" tail".into()),
        ])]);
        round_trip(&doc, &store);
    }

    #[test]
    fn markdown_significant_characters_survive() {
        let store = RawStore::new();
        let doc = Document::new(vec![
            para("stars * and _under_ and [brackets] and `ticks` and # hash and \\ slash"),
            para("# leading hash is not a heading"),
            para("- leading dash is not a list"),
            para("pipes | and <angles> and ~tilde~ and &ersand"),
        ]);
        round_trip(&doc, &store);
    }

    #[test]
    fn block_raw_renders_markers_and_parses_back() {
        let mut store = RawStore::new();
        let r = store.insert(
            r#"{"type":"panel"}"#.to_string(),
            "> **Warning**\n> mind the gap".to_string(),
            "panel (warning)".to_string(),
        );
        let doc = Document::new(vec![para("before"), Block::Raw(r.clone()), para("after")]);

        let md = render(&doc, &store);
        assert!(
            md.contains(&format!(
                "<!--jired:raw:{id}-->\n> **Warning**\n> mind the gap\n<!--/jired:raw:{id}-->",
                id = r.id
            )),
            "{md}"
        );

        let back = parse(&md, &store);
        assert_eq!(back, doc);
        assert!(
            !format!("{back:?}").contains("Warning"),
            "approximation leaked into the AST: {back:?}"
        );
    }

    #[test]
    fn raw_id_missing_from_store_still_parses() {
        let md =
            "before\n\n<!--jired:raw:rdeadbe-->\nsomething\n<!--/jired:raw:rdeadbe-->\n\nafter\n";
        let doc = parse(md, &RawStore::new());
        assert_eq!(
            doc.blocks[1],
            Block::Raw(RawRef {
                id: "rdeadbe".into()
            })
        );
        assert_eq!(doc.blocks.len(), 3);
    }

    #[test]
    fn inline_raw_round_trips_inside_a_paragraph() {
        let mut store = RawStore::new();
        let r = store.insert(
            r#"{"type":"status"}"#.to_string(),
            "[IN PROGRESS]".to_string(),
            "status".to_string(),
        );
        let doc = Document::new(vec![Block::Paragraph(vec![
            Inline::Text("state is ".into()),
            Inline::Raw(r.clone()),
            Inline::Text(" today".into()),
        ])]);

        let md = render(&doc, &store);
        assert!(
            md.contains(&format!(
                "<!--jired:raw:{id}-->[IN PROGRESS]<!--/jired:raw:{id}-->",
                id = r.id
            )),
            "{md}"
        );
        round_trip(&doc, &store);
    }

    #[test]
    fn region_body_reads_the_approximation() {
        let mut store = RawStore::new();
        let r = store.insert(
            "{}".to_string(),
            "approx line\nsecond".to_string(),
            "x".into(),
        );
        let doc = Document::new(vec![Block::Raw(r.clone())]);
        let md = render(&doc, &store);

        assert_eq!(
            region_body(&md, &r.id).as_deref(),
            Some("approx line\nsecond")
        );
        assert_eq!(region_body(&md, "rmissing"), None);
    }

    #[test]
    fn editing_a_region_changes_its_body() {
        let mut store = RawStore::new();
        let r = store.insert("{}".to_string(), "untouched".to_string(), "x".into());
        let md = render(&Document::new(vec![Block::Raw(r.clone())]), &store);
        let edited = md.replace("untouched", "touched by hand");

        assert_eq!(region_body(&md, &r.id).as_deref(), Some("untouched"));
        assert_eq!(
            region_body(&edited, &r.id).as_deref(),
            Some("touched by hand")
        );
        assert_ne!(region_body(&md, &r.id), region_body(&edited, &r.id));
    }

    #[test]
    fn ordered_list_keeps_its_start() {
        let store = RawStore::new();
        let doc = Document::new(vec![Block::OrderedList {
            start: 3,
            items: vec![
                ListItem {
                    content: vec![para("three")],
                },
                ListItem {
                    content: vec![para("four")],
                },
            ],
        }]);
        let md = round_trip(&doc, &store);
        assert!(md.starts_with("3. three"), "{md}");
        assert!(md.contains("4. four"), "{md}");
    }

    #[test]
    fn task_list_keeps_checked_states() {
        let store = RawStore::new();
        let doc = Document::new(vec![Block::TaskList(vec![
            TaskItem {
                checked: true,
                content: vec![Inline::Text("done".into())],
                id: None,
            },
            TaskItem {
                checked: false,
                content: vec![Inline::Text("todo".into())],
                id: None,
            },
            TaskItem {
                checked: true,
                content: vec![
                    Inline::Text("done with ".into()),
                    Inline::Strong(vec![Inline::Text("emphasis".into())]),
                ],
                id: None,
            },
        ])]);
        let md = round_trip(&doc, &store);
        assert!(md.contains("- [x] done"), "{md}");
        assert!(md.contains("- [ ] todo"), "{md}");
    }

    #[test]
    fn gfm_table_with_header_round_trips() {
        let store = RawStore::new();
        let doc = Document::new(vec![Block::Table(Table {
            header: Some(vec![cell("field"), cell("value")]),
            rows: vec![
                vec![cell("status"), cell("In Progress")],
                vec![
                    cell("owner"),
                    vec![Block::Paragraph(vec![Inline::Emphasis(vec![
                        Inline::Text("nobody".into()),
                    ])])],
                ],
            ],
        })]);
        let md = round_trip(&doc, &store);
        assert!(md.contains("| field | value |"), "{md}");
        assert!(md.contains("| --- | --- |"), "{md}");
    }

    #[test]
    fn table_without_header_round_trips() {
        let store = RawStore::new();
        let doc = Document::new(vec![Block::Table(Table {
            header: None,
            rows: vec![vec![cell("a"), cell("b")]],
        })]);
        round_trip(&doc, &store);
    }

    #[test]
    fn nested_blocks_round_trip() {
        let store = RawStore::new();
        let doc = Document::new(vec![
            Block::BulletList(vec![ListItem {
                content: vec![
                    para("outer"),
                    Block::BulletList(vec![ListItem {
                        content: vec![para("inner")],
                    }]),
                ],
            }]),
            Block::BlockQuote(vec![Block::BulletList(vec![ListItem {
                content: vec![para("quoted item")],
            }])]),
        ]);
        round_trip(&doc, &store);
    }

    #[test]
    fn adjacent_lists_stay_separate() {
        let store = RawStore::new();
        let doc = Document::new(vec![
            Block::BulletList(vec![ListItem {
                content: vec![para("first list")],
            }]),
            Block::BulletList(vec![ListItem {
                content: vec![para("second list")],
            }]),
        ]);
        round_trip(&doc, &store);
    }

    #[test]
    fn empty_document_round_trips() {
        let store = RawStore::new();
        let doc = Document::default();
        assert_eq!(render(&doc, &store), "");
        assert_eq!(parse("", &store), doc);
        assert!(parse("   \n\n", &store).is_empty());
    }

    #[test]
    fn text_ending_in_at_is_not_read_as_a_mention() {
        let store = RawStore::new();
        let doc = Document::new(vec![Block::Paragraph(vec![
            Inline::Text("mail@".into()),
            Inline::Link {
                text: vec![Inline::Text("here".into())],
                href: "https://example.com".into(),
            },
        ])]);
        round_trip(&doc, &store);
    }

    #[test]
    fn code_spans_keep_their_content() {
        let store = RawStore::new();
        let doc = Document::new(vec![
            Block::Paragraph(vec![Inline::Code("a ` b".into())]),
            Block::Paragraph(vec![Inline::Code("*not emphasis*".into())]),
            Block::Table(Table {
                header: Some(vec![cell("expr")]),
                rows: vec![vec![vec![Block::Paragraph(vec![Inline::Code(
                    "a | b".into(),
                )])]]],
            }),
        ]);
        round_trip(&doc, &store);
    }

    #[test]
    fn heading_levels_are_verbatim() {
        let store = RawStore::new();
        for level in 1..=6u8 {
            let doc = Document::new(vec![Block::Heading {
                level,
                content: vec![Inline::Text("h".into())],
            }]);
            let md = round_trip(&doc, &store);
            assert!(
                md.starts_with(&format!("{} h", "#".repeat(level as usize))),
                "{md}"
            );
        }
    }

    #[test]
    fn marks_of_the_same_kind_nest() {
        let store = RawStore::new();
        let doc = Document::new(vec![Block::Paragraph(vec![
            Inline::Emphasis(vec![
                Inline::Text("outer ".into()),
                Inline::Emphasis(vec![Inline::Text("inner".into())]),
            ]),
            Inline::Text(" ".into()),
            Inline::Strong(vec![
                Inline::Strong(vec![Inline::Text("deep".into())]),
                Inline::Text(" tail".into()),
            ]),
            Inline::Text(" ".into()),
            Inline::Emphasis(vec![Inline::Strong(vec![Inline::Text("both".into())])]),
        ])]);
        round_trip(&doc, &store);
    }

    #[test]
    fn marks_next_to_each_other_and_to_words() {
        let store = RawStore::new();
        let doc = Document::new(vec![Block::Paragraph(vec![
            Inline::Strong(vec![Inline::Text("a".into())]),
            Inline::Emphasis(vec![Inline::Text("b".into())]),
            Inline::Strike(vec![Inline::Code("c".into())]),
            Inline::Text("word".into()),
            Inline::Strong(vec![Inline::Code("d".into())]),
            Inline::Text("word".into()),
            Inline::IssueKey("PROJ-9".into()),
            Inline::Text("word".into()),
        ])]);
        round_trip(&doc, &store);
    }

    #[test]
    fn whitespace_at_the_edges_survives() {
        let store = RawStore::new();
        let doc = Document::new(vec![
            para("  leading and trailing  "),
            Block::Paragraph(vec![
                Inline::Strong(vec![Inline::Text(" padded ".into())]),
                Inline::Text(" x ".into()),
            ]),
            Block::Paragraph(vec![Inline::LineBreak]),
            Block::Paragraph(vec![
                Inline::LineBreak,
                Inline::LineBreak,
                Inline::Text("after two breaks".into()),
            ]),
        ]);
        round_trip(&doc, &store);
    }

    #[test]
    fn headings_and_code_blocks_at_their_limits() {
        let store = RawStore::new();
        let doc = Document::new(vec![
            Block::Heading {
                level: 3,
                content: vec![Inline::Text("ends with #".into())],
            },
            Block::Heading {
                level: 1,
                content: vec![Inline::Text("###".into())],
            },
            Block::Heading {
                level: 2,
                content: vec![
                    Inline::Text("a".into()),
                    Inline::LineBreak,
                    Inline::Text("b".into()),
                ],
            },
            Block::CodeBlock {
                language: None,
                code: "```\nnested fence\n```".into(),
            },
            Block::CodeBlock {
                language: Some("md".into()),
                code: String::new(),
            },
            Block::CodeBlock {
                language: None,
                code: "   indented".into(),
            },
        ]);
        round_trip(&doc, &store);
    }

    #[test]
    fn raw_regions_survive_inside_containers() {
        let mut store = RawStore::new();
        let block = store.insert(
            r#"{"type":"panel"}"#.into(),
            "PANEL\nsecond line".into(),
            "panel".into(),
        );
        let inline = store.insert(
            r#"{"type":"status"}"#.into(),
            "DONE".into(),
            "status".into(),
        );
        let doc = Document::new(vec![
            Block::BulletList(vec![ListItem {
                content: vec![para("item"), Block::Raw(block.clone())],
            }]),
            Block::BlockQuote(vec![Block::Raw(block.clone())]),
            Block::Paragraph(vec![
                Inline::Text("state ".into()),
                Inline::Raw(inline.clone()),
            ]),
            Block::Raw(block.clone()),
        ]);
        round_trip(&doc, &store);
    }

    #[test]
    fn text_that_looks_like_structure_stays_text() {
        let store = RawStore::new();
        let doc = Document::new(vec![
            para("1. not a list"),
            para("> not a quote"),
            para("--- not a rule"),
            para("| not | a | table |"),
            para("<!-- not a comment -->"),
            para("<br> not a break"),
        ]);
        round_trip(&doc, &store);
    }

    #[test]
    fn a_list_item_can_hold_a_rule() {
        let store = RawStore::new();
        let doc = Document::new(vec![Block::BulletList(vec![
            ListItem {
                content: vec![Block::Rule],
            },
            ListItem {
                content: vec![para("second")],
            },
        ])]);
        round_trip(&doc, &store);
    }

    #[test]
    fn table_cells_keep_pipes_and_breaks() {
        let store = RawStore::new();
        let doc = Document::new(vec![Block::Table(Table {
            header: Some(vec![cell("pipe | here"), vec![]]),
            rows: vec![vec![
                vec![Block::Paragraph(vec![Inline::Code("a | b".into())])],
                vec![Block::Paragraph(vec![
                    Inline::Text("x".into()),
                    Inline::LineBreak,
                    Inline::Text("y".into()),
                ])],
            ]],
        })]);
        round_trip(&doc, &store);
    }

    #[test]
    fn nested_strikethrough_collapses() {
        let store = RawStore::new();
        let doc = Document::new(vec![Block::Paragraph(vec![Inline::Strike(vec![
            Inline::Strike(vec![Inline::Text("gone".into())]),
        ])])]);
        let back = parse(&render(&doc, &store), &store);
        assert_eq!(
            back,
            Document::new(vec![Block::Paragraph(vec![Inline::Strike(vec![
                Inline::Text("gone".into())
            ])])])
        );
    }
}
