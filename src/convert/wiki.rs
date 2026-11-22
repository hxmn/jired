//! Jira wiki markup ⇄ [`Document`] [M1.10] (read path) and [M2.6] (write path).
//!
//! Data Center stores rich text as wiki markup rather than ADF, so this module
//! is the DC counterpart of the ADF converter: both parse into the same
//! `JiredAst` (D25) and everything downstream — the Markdown renderer, the raw
//! store, push reassembly — stays deployment-agnostic.
//!
//! It is hand-written because no crate implements Jira's dialect: the published
//! ones cover Confluence storage format or generic wiki syntaxes, and none of
//! them round-trips macros byte-identically, which is the whole point of the
//! lossy-region mechanism. The design is deliberately small: a line-oriented
//! block scanner plus an inline tokenizer, no parser framework.
//!
//! Anything that is not `{code}`, `{noformat}` or `{quote}` — every panel,
//! color, expand or unknown macro — is captured verbatim into the [`RawStore`]
//! and referenced by a [`Block::Raw`] or [`Inline::Raw`], so an untouched region
//! pushes back byte-identical by construction. So is every phrase mark Markdown
//! has no spelling for: underline, superscript, subscript, citation.
//!
//! The other direction has the opposite duty: what is literal text in the file
//! has to reach Jira as text, so the writer escapes any delimiter Jira would
//! pair up, and the reader agrees with the writer on what pairs.

use crate::convert::ast::{
    inline_text, normalize_inlines, Block, Document, Inline, ListItem, RawRef, RawStore, Table,
};

/// Parse Jira wiki markup into the internal AST, capturing macros and other
/// unrepresentable constructs into `store`.
pub fn to_ast(text: &str, store: &mut RawStore) -> Document {
    Document::new(parse_blocks(text, store))
}

/// Render the AST back to wiki markup. Raw regions are reproduced verbatim
/// from `store`, never re-converted.
pub fn from_ast(doc: &Document, store: &RawStore) -> String {
    render_blocks(&doc.blocks, store, Ctx::default())
}

// ---------------------------------------------------------------------------
// Block scanner
// ---------------------------------------------------------------------------

fn parse_blocks(text: &str, store: &mut RawStore) -> Vec<Block> {
    let mut out = Vec::new();
    let mut pos = 0usize;

    while pos < text.len() {
        let (line, next) = read_line(text, pos);
        if line.trim().is_empty() {
            pos = next;
            continue;
        }

        if let Some(m) = macro_at(text, pos) {
            let (block, end) = parse_macro(text, &m, store);
            if let Some(block) = block {
                out.push(block);
            }
            pos = end;
            continue;
        }

        let trimmed = line.trim_start();

        if trimmed.trim_end() == "----" {
            out.push(Block::Rule);
            pos = next;
            continue;
        }

        if let Some((level, rest)) = heading_at(trimmed) {
            out.push(Block::Heading {
                level,
                content: parse_inlines(rest.trim_end(), store),
            });
            pos = next;
            continue;
        }

        if let Some(rest) = trimmed.strip_prefix("bq.") {
            out.push(Block::BlockQuote(vec![Block::Paragraph(parse_inlines(
                rest.trim(),
                store,
            ))]));
            pos = next;
            continue;
        }

        if list_marker(trimmed).is_some() {
            let (block, end) = parse_list(text, pos, store);
            out.push(block);
            pos = end;
            continue;
        }

        if trimmed.starts_with('|') {
            let (block, end) = parse_table(text, pos, store);
            out.push(block);
            pos = end;
            continue;
        }

        let (block, end) = parse_paragraph(text, pos, store);
        out.push(block);
        pos = end;
    }

    out
}

/// Turn a macro region into a block; returns the position just past it.
///
/// `{code}`, `{noformat}` and `{quote}` have faithful AST forms; every other
/// macro is preserved verbatim, including its opening and closing tags.
fn parse_macro(text: &str, m: &MacroSpan, store: &mut RawStore) -> (Option<Block>, usize) {
    let body = &text[m.body.0..m.body.1];
    let name = m.name.to_ascii_lowercase();

    if m.closed {
        match name.as_str() {
            "code" => {
                return (
                    Some(Block::CodeBlock {
                        language: code_language(&m.params),
                        code: trim_block_body(body).to_string(),
                    }),
                    m.end,
                );
            }
            "noformat" => {
                return (
                    Some(Block::CodeBlock {
                        language: None,
                        code: trim_block_body(body).to_string(),
                    }),
                    m.end,
                );
            }
            "quote" => {
                return (
                    Some(Block::BlockQuote(parse_blocks(
                        trim_block_body(body),
                        store,
                    ))),
                    m.end,
                );
            }
            _ => {}
        }
    }

    let source = text[m.start..m.end].to_string();
    let approx = macro_approx(&m.name, &m.params, body);
    let r = store.insert(source, approx, name);
    (Some(Block::Raw(r)), m.end)
}

fn parse_paragraph(text: &str, start: usize, store: &mut RawStore) -> (Block, usize) {
    let mut lines: Vec<&str> = Vec::new();
    let mut pos = start;

    while pos < text.len() {
        let (line, next) = read_line(text, pos);
        if line.trim().is_empty() {
            break;
        }
        if !lines.is_empty() && starts_block(text, pos, line) {
            break;
        }
        lines.push(line.trim_end());
        pos = next;
    }

    (
        Block::Paragraph(parse_inlines(&lines.join("\n"), store)),
        pos,
    )
}

fn parse_list(text: &str, start: usize, store: &mut RawStore) -> (Block, usize) {
    let mut entries: Vec<(String, Vec<Inline>)> = Vec::new();
    let mut pos = start;
    let mut top: Option<char> = None;

    while pos < text.len() {
        let (line, next) = read_line(text, pos);
        let Some((marker, content)) = list_marker(line.trim_start()) else {
            break;
        };
        let head = marker.chars().next().unwrap_or('*');
        match top {
            // A top-level switch between `*` and `#` starts a new list rather
            // than continuing the current one.
            Some(t) if marker.len() == 1 && head != t => break,
            None if marker.len() == 1 => top = Some(head),
            _ => {}
        }
        entries.push((marker.to_string(), parse_inlines(content.trim_end(), store)));
        pos = next;
    }

    (build_list(&entries, 0), pos)
}

fn build_list(entries: &[(String, Vec<Inline>)], depth: usize) -> Block {
    let kind = entries
        .first()
        .and_then(|e| e.0.chars().nth(depth))
        .unwrap_or('*');

    let mut items = Vec::new();
    let mut i = 0;
    while i < entries.len() {
        let mut content = vec![Block::Paragraph(entries[i].1.clone())];
        let mut j = i + 1;
        while j < entries.len() && entries[j].0.len() > depth + 1 {
            j += 1;
        }
        if j > i + 1 {
            content.push(build_list(&entries[i + 1..j], depth + 1));
        }
        items.push(ListItem { content });
        i = j;
    }

    if kind == '#' {
        Block::OrderedList { start: 1, items }
    } else {
        Block::BulletList(items)
    }
}

fn parse_table(text: &str, start: usize, store: &mut RawStore) -> (Block, usize) {
    let mut table = Table::default();
    let mut pos = start;

    while pos < text.len() {
        let (line, next) = read_line(text, pos);
        let trimmed = line.trim();
        if !trimmed.starts_with('|') {
            break;
        }
        let header = trimmed.starts_with("||");
        let cells = split_row(trimmed, header)
            .into_iter()
            .map(|c| {
                let c = c.trim();
                if c.is_empty() {
                    Vec::new()
                } else {
                    vec![Block::Paragraph(parse_inlines(c, store))]
                }
            })
            .collect::<Vec<_>>();

        if header && table.header.is_none() && table.rows.is_empty() {
            table.header = Some(cells);
        } else {
            table.rows.push(cells);
        }
        pos = next;
    }

    (Block::Table(table), pos)
}

/// Cut a table row into its cells.
///
/// A `|` that is escaped, or that sits inside a link or a `{{code}}` span,
/// belongs to the cell: `[label|href]` in a cell is one link, not two cells.
fn split_row(line: &str, header: bool) -> Vec<&str> {
    let sep = if header { "||" } else { "|" };
    let inner = line.strip_prefix(sep).unwrap_or(line);
    let inner = inner.strip_suffix(sep).unwrap_or(inner);
    let b = inner.as_bytes();
    let mut cells = Vec::new();
    let mut cell_start = 0;
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'\\' => i += 1 + inner[i + 1..].chars().next().map_or(0, char::len_utf8),
            b'[' => i = bracket_end(inner, i).map_or(i + 1, |close| close + 1),
            b'{' if inner[i..].starts_with("{{") => {
                i = inner[i + 2..]
                    .find("}}")
                    .map_or(i + 2, |rel| i + 2 + rel + 2)
            }
            _ if inner[i..].starts_with(sep) => {
                cells.push(&inner[cell_start..i]);
                i += sep.len();
                cell_start = i;
            }
            // A whole character, not a byte: the next turn slices `inner[i..]`,
            // and a cell starting with `Ф` would put that slice inside it.
            _ => i += inner[i..].chars().next().map_or(1, char::len_utf8),
        }
    }
    cells.push(&inner[cell_start.min(inner.len())..]);
    cells
}

fn starts_block(text: &str, pos: usize, line: &str) -> bool {
    let trimmed = line.trim_start();
    trimmed.trim_end() == "----"
        || heading_at(trimmed).is_some()
        || trimmed.starts_with("bq.")
        || trimmed.starts_with('|')
        || list_marker(trimmed).is_some()
        || macro_at(text, pos).is_some()
}

fn heading_at(line: &str) -> Option<(u8, &str)> {
    let b = line.as_bytes();
    if b.len() < 3
        || (b[0] != b'h' && b[0] != b'H')
        || !(b'1'..=b'6').contains(&b[1])
        || b[2] != b'.'
    {
        return None;
    }
    Some((b[1] - b'0', line[3..].trim_start()))
}

/// Split a `* `/`## ` style marker from its content.
fn list_marker(line: &str) -> Option<(&str, &str)> {
    let depth = line
        .bytes()
        .take_while(|c| *c == b'*' || *c == b'#')
        .count();
    if depth == 0 {
        return None;
    }
    let rest = &line[depth..];
    if !rest.starts_with(' ') {
        return None;
    }
    Some((&line[..depth], rest.trim_start()))
}

fn read_line(text: &str, pos: usize) -> (&str, usize) {
    match text[pos..].find('\n') {
        Some(rel) => (text[pos..pos + rel].trim_end_matches('\r'), pos + rel + 1),
        None => (&text[pos..], text.len()),
    }
}

/// Drop the newline that follows `{code}` and the one before `{code}`; those
/// belong to the tags, not to the code.
fn trim_block_body(body: &str) -> &str {
    let body = body
        .strip_prefix("\r\n")
        .or_else(|| body.strip_prefix('\n'))
        .unwrap_or(body);
    body.strip_suffix('\n')
        .map(|b| b.strip_suffix('\r').unwrap_or(b))
        .unwrap_or(body)
}

// ---------------------------------------------------------------------------
// Macro tags
// ---------------------------------------------------------------------------

struct MacroSpan {
    name: String,
    params: String,
    /// Byte range of the text between the opening and closing tags.
    body: (usize, usize),
    start: usize,
    end: usize,
    closed: bool,
}

/// Recognize `{name}` / `{name:params}` at `pos` and locate its closing tag.
///
/// An unclosed macro spans only its opening tag, so a standalone `{toc}` never
/// swallows the text that follows it.
fn macro_at(text: &str, pos: usize) -> Option<MacroSpan> {
    let b = text.as_bytes();
    if *b.get(pos)? != b'{' {
        return None;
    }
    let mut i = pos + 1;
    while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'-' || b[i] == b'_') {
        i += 1;
    }
    if i == pos + 1 {
        return None;
    }
    let name = text[pos + 1..i].to_string();

    let (params, open_end) = match b.get(i) {
        Some(b'}') => (String::new(), i + 1),
        Some(b':') => {
            let rel = text[i + 1..].find('}')?;
            if text[i + 1..i + 1 + rel].contains('\n') {
                return None;
            }
            (text[i + 1..i + 1 + rel].to_string(), i + 1 + rel + 1)
        }
        _ => return None,
    };

    let close = format!("{{{name}}}");
    Some(match text[open_end..].find(&close) {
        Some(rel) => MacroSpan {
            name,
            params,
            body: (open_end, open_end + rel),
            start: pos,
            end: open_end + rel + close.len(),
            closed: true,
        },
        None => MacroSpan {
            name,
            params,
            body: (open_end, open_end),
            start: pos,
            end: open_end,
            closed: false,
        },
    })
}

fn is_structural_macro(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "code" | "noformat" | "quote"
    )
}

fn code_language(params: &str) -> Option<String> {
    let params = params.trim();
    if params.is_empty() {
        return None;
    }
    for part in params.split('|') {
        if let Some((k, v)) = part.split_once('=') {
            if k.trim().eq_ignore_ascii_case("language") && !v.trim().is_empty() {
                return Some(v.trim().to_string());
            }
        }
    }
    let first = params.split('|').next().unwrap_or("").trim();
    if first.is_empty() || first.contains('=') {
        None
    } else {
        Some(first.to_string())
    }
}

fn macro_param(params: &str, key: &str) -> Option<String> {
    params.split('|').find_map(|p| {
        let (k, v) = p.split_once('=')?;
        if k.trim().eq_ignore_ascii_case(key) {
            Some(v.trim().to_string())
        } else {
            None
        }
    })
}

/// Read-only rendering shown between the raw markers. Never pushed back.
fn macro_approx(name: &str, params: &str, body: &str) -> String {
    let body = body.trim();
    if name.eq_ignore_ascii_case("panel") {
        let mut out = match macro_param(params, "title") {
            Some(t) => format!("> **[jired: Jira panel \"{t}\" — read-only]**"),
            None => "> **[jired: Jira panel — read-only]**".to_string(),
        };
        for line in body.lines() {
            out.push_str("\n> ");
            out.push_str(line);
        }
        out
    } else {
        let mut out = format!("**[jired: {name} macro — read-only]**");
        for line in body.lines() {
            out.push('\n');
            out.push_str(line);
        }
        out
    }
}

// ---------------------------------------------------------------------------
// Inline tokenizer
// ---------------------------------------------------------------------------

fn parse_inlines(text: &str, store: &mut RawStore) -> Vec<Inline> {
    parse_inlines_in(text, store, None)
}

/// `within` is the delimiter of the mark whose content this is. A mark does
/// not open inside one of its own kind: `-a #-b--` is one strikethrough with
/// hyphens in it, not one inside another, which Markdown has no way to write
/// and would flatten, changing the text an untouched push sends.
fn parse_inlines_in(text: &str, store: &mut RawStore, within: Option<u8>) -> Vec<Inline> {
    let b = text.as_bytes();
    let mut out: Vec<Inline> = Vec::new();
    let mut buf = String::new();
    let mut i = 0usize;

    while i < b.len() {
        match b[i] {
            // A backslash makes the punctuation after it literal. Before
            // anything else it is a backslash: Jira renders `\n` pasted from
            // JSON as those two characters, and reading it as an escape lost
            // the backslash from the file, so the next edit of the description
            // pushed the text without it.
            b'\\' if b.get(i + 1).is_some_and(u8::is_ascii_punctuation) => {
                buf.push(b[i + 1] as char);
                i += 2;
            }
            // `\h2.` and `\bq.` are how a line of prose that begins like a
            // heading or a quote is written, by this renderer among others, so
            // there the backslash is the guard and not text.
            b'\\'
                if (i == 0 || b[i - 1] == b'\n')
                    && (heading_at(&text[i + 1..]).is_some()
                        || text[i + 1..].starts_with("bq.")) =>
            {
                i += 1;
            }
            b'\n' => {
                flush(&mut buf, &mut out);
                out.push(Inline::LineBreak);
                i += 1;
            }
            b'{' if text[i..].starts_with("{{") => match text[i + 2..].find("}}") {
                Some(rel) => {
                    flush(&mut buf, &mut out);
                    out.push(Inline::Code(text[i + 2..i + 2 + rel].to_string()));
                    i = i + 2 + rel + 2;
                }
                None => {
                    buf.push('{');
                    i += 1;
                }
            },
            b'{' => match macro_at(text, i) {
                Some(m) if m.closed && !is_structural_macro(&m.name) => {
                    flush(&mut buf, &mut out);
                    let source = text[m.start..m.end].to_string();
                    let approx = macro_approx(&m.name, &m.params, &text[m.body.0..m.body.1]);
                    let r = store.insert(source, approx, m.name.to_ascii_lowercase());
                    out.push(Inline::Raw(r));
                    i = m.end;
                }
                _ => {
                    buf.push('{');
                    i += 1;
                }
            },
            b'!' => match image_end(text, i) {
                Some(close) => {
                    flush(&mut buf, &mut out);
                    out.push(parse_image(&text[i..=close], store));
                    i = close + 1;
                }
                None => {
                    buf.push('!');
                    i += 1;
                }
            },
            b'[' => match bracket_end(text, i) {
                Some(close) if close > i + 1 => {
                    flush(&mut buf, &mut out);
                    out.push(parse_bracket(&text[i + 1..close], store));
                    i = close + 1;
                }
                _ => {
                    buf.push('[');
                    i += 1;
                }
            },
            d @ (b'*' | b'_' | b'-') => {
                match mark_close(text, i, &text[i..i + 1]).filter(|_| within != Some(d)) {
                    Some(close) => {
                        flush(&mut buf, &mut out);
                        let inner = parse_inlines_in(&text[i + 1..close], store, Some(d));
                        out.push(match d {
                            b'*' => Inline::Strong(inner),
                            b'_' => Inline::Emphasis(inner),
                            _ => Inline::Strike(inner),
                        });
                        i = close + 1;
                    }
                    None => {
                        buf.push(d as char);
                        i += 1;
                    }
                }
            }
            // Underline, superscript, subscript and citation have no Markdown
            // form. Read as text they came back escaped on the next push and
            // stopped being marks, so they are kept as regions instead, the
            // way the ADF reader keeps the same marks.
            b'+' | b'^' | b'~' | b'?' => {
                let delim = if text[i..].starts_with("??") {
                    "??"
                } else if b[i] == b'?' {
                    buf.push('?');
                    i += 1;
                    continue;
                } else {
                    &text[i..i + 1]
                };
                match mark_close(text, i, delim) {
                    Some(close) => {
                        flush(&mut buf, &mut out);
                        let end = close + delim.len();
                        out.push(Inline::Raw(phrase_region(
                            &text[i..end],
                            &text[i + delim.len()..close],
                            delim,
                            store,
                        )));
                        i = end;
                    }
                    None => {
                        buf.push(b[i] as char);
                        i += 1;
                    }
                }
            }
            c if c.is_ascii_uppercase() => match issue_key_end(text, i) {
                Some(end) => {
                    flush(&mut buf, &mut out);
                    out.push(Inline::IssueKey(text[i..end].to_string()));
                    i = end;
                }
                None => {
                    buf.push(c as char);
                    i += 1;
                }
            },
            _ => {
                let ch = text[i..].chars().next().unwrap_or('\u{fffd}');
                buf.push(ch);
                i += ch.len_utf8();
            }
        }
    }

    flush(&mut buf, &mut out);
    normalize_inlines(out)
}

fn flush(buf: &mut String, out: &mut Vec<Inline>) {
    if !buf.is_empty() {
        out.push(Inline::Text(std::mem::take(buf)));
    }
}

/// Find the `]` that closes a bracket span, skipping escaped ones.
///
/// A link label may legitimately contain `]` or `|`; the renderer escapes them,
/// so the scanner has to honour those escapes or the span ends in the wrong
/// place and the link decays into text.
fn bracket_end(text: &str, open: usize) -> Option<usize> {
    let b = text.as_bytes();
    let mut i = open + 1;
    while i < b.len() {
        match b[i] {
            b'\\' => i += 2,
            b'\n' => return None,
            b']' => return Some(i),
            _ => i += 1,
        }
    }
    None
}

/// Split a bracket body on its first unescaped `|`.
fn split_unescaped(content: &str) -> Option<(&str, &str)> {
    let b = content.as_bytes();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'\\' => i += 2,
            b'|' => return Some((&content[..i], &content[i + 1..])),
            _ => i += 1,
        }
    }
    None
}

/// Remove backslash escapes from a value that is not parsed as inline markup.
fn unescape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' {
            if let Some(next) = chars.next_if(char::is_ascii_punctuation) {
                out.push(next);
                continue;
            }
        }
        out.push(c);
    }
    out
}

/// Find the `!` that closes an embedded image opened at `open`.
///
/// Jira takes `!name!` for an image wherever it sees one, but text is only
/// read back as one when it names a file — a dot in the name or a URL — so a
/// sentence with two exclamation marks in it stays a sentence.
fn image_end(text: &str, open: usize) -> Option<usize> {
    let rest = &text[open + 1..];
    let first = rest.chars().next()?;
    if first.is_whitespace() || first == '!' {
        return None;
    }
    let rel = rest.find(['!', '\n'])?;
    if rest.as_bytes()[rel] != b'!' {
        return None;
    }
    let content = &rest[..rel];
    let src = content.split('|').next().unwrap_or(content);
    if src.ends_with(char::is_whitespace) || !(src.contains('.') || src.contains("://")) {
        return None;
    }
    Some(open + 1 + rel)
}

/// `!name!` is an image Markdown can show; `!name|thumbnail!` and other
/// parameters have nowhere to go there, so that form is kept as it is.
fn parse_image(source: &str, store: &mut RawStore) -> Inline {
    let content = &source[1..source.len() - 1];
    match content.split_once('|') {
        None => Inline::Image {
            src: content.to_string(),
        },
        Some((src, _)) => Inline::Raw(store.insert(
            source.to_string(),
            format!("**[jired: image {src} — read-only]**"),
            "image".to_string(),
        )),
    }
}

/// `[~user]`, `[text|href]`, `[href]` — the three bracket forms we understand.
fn parse_bracket(content: &str, store: &mut RawStore) -> Inline {
    if let Some(user) = content.strip_prefix('~') {
        // Wiki markup has nowhere to put a display name, so the id is both.
        let user = unescape(user.trim());
        return Inline::Mention {
            id: user.clone(),
            display: user,
        };
    }
    match split_unescaped(content) {
        Some((label, rest)) => Inline::Link {
            text: parse_inlines(label, store),
            // A third `|`-separated field is a link tip; Markdown has no place
            // for it, so it is dropped rather than pushed into the href.
            href: unescape(rest.split('|').next().unwrap_or(rest).trim()),
        },
        // A bare `[href]` shows its target, and `[^name]` the attachment's
        // name, so that is the link's text too: left empty, the file had
        // `[](href)`, which shows nothing.
        None => {
            let href = unescape(content.trim());
            let shown = href.strip_prefix('^').unwrap_or(&href).to_string();
            Inline::Link {
                text: if shown.is_empty() {
                    Vec::new()
                } else {
                    vec![Inline::Text(shown)]
                },
                href,
            }
        }
    }
}

/// Find the closing delimiter of a `*bold*`-style span on the same line.
///
/// A delimiter opens where no word runs into it and closes where none runs on
/// from it, so `well-known` and `C++` are text. Jira's rule is that much and
/// no stricter, which is why a quote mark counts as a boundary: `«+»` and
/// `«++»` pair up into an underline there.
fn mark_close(text: &str, open: usize, delim: &str) -> Option<usize> {
    let d = delim.chars().next()?;
    if let Some(prev) = text[..open].chars().next_back() {
        // An escaped character is text, not a run of delimiters: `\**b*` is a
        // literal star and then bold.
        let escaped = text[..open - prev.len_utf8()].ends_with('\\');
        if !opens_after(prev, d) && !(escaped && !prev.is_alphanumeric()) {
            return None;
        }
    }
    let body = open + delim.len();
    let next = text[body..].chars().next()?;
    if next == d || next.is_whitespace() {
        return None;
    }
    let limit = text[body..]
        .find('\n')
        .map(|rel| body + rel)
        .unwrap_or(text.len());

    // An escaped delimiter is text, so the scan steps over it: the writer
    // escapes a literal `*` inside a mark as `\*`, and closing the mark there
    // cut it short.
    let mut prev = next;
    let mut chars = text[body..limit].char_indices().peekable();
    while let Some((rel, c)) = chars.next() {
        let i = body + rel;
        if c == '\\' {
            if let Some((_, escaped)) = chars.next_if(|(_, n)| n.is_ascii_punctuation()) {
                prev = escaped;
                continue;
            }
        }
        // Nor does a delimiter inside a code span or a link close the mark:
        // those are written as they are, and their text is not prose.
        let skip_to = match c {
            '{' if text[i..limit].starts_with("{{") => {
                text[i + 2..limit].find("}}").map(|r| i + 2 + r + 2)
            }
            '[' => bracket_end(&text[..limit], i).map(|close| close + 1),
            _ => None,
        };
        if let Some(to) = skip_to {
            while chars.next_if(|(r, _)| body + r < to).is_some() {}
            prev = text[..to].chars().next_back().unwrap_or(c);
            continue;
        }
        // A delimiter straight after its own character is the end of a run,
        // `--` or `**`, not a closing mark: Jira draws `--` as a dash.
        if rel > 0 && text[i..limit].starts_with(delim) && !prev.is_whitespace() && prev != d {
            let after = text[i + delim.len()..].chars().next();
            if after.is_none_or(|a| !(a.is_alphanumeric() || a == d)) {
                return Some(i);
            }
        }
        prev = c;
    }
    None
}

/// Whether a phrase delimiter right after `prev` can open a span.
///
/// A path or a dotted name is not a word boundary even though its separators
/// are punctuation: `a/-b-/c` is a path, not a strikethrough.
fn opens_after(prev: char, delim: char) -> bool {
    prev.is_whitespace()
        || !(prev.is_alphanumeric() || prev == delim || matches!(prev, '/' | '\\' | '.'))
}

/// A Jira phrase mark with no Markdown form, kept verbatim.
fn phrase_region(source: &str, inner: &str, delim: &str, store: &mut RawStore) -> RawRef {
    let text = inline_text(&parse_inlines(inner, &mut RawStore::new()));
    let (tag, label) = match delim {
        "+" => ("u", "underline"),
        "^" => ("sup", "superscript"),
        "~" => ("sub", "subscript"),
        _ => ("cite", "citation"),
    };
    store.insert(
        source.to_string(),
        format!("<{tag}>{text}</{tag}>"),
        label.to_string(),
    )
}

/// End offset of a bare `[A-Z][A-Z0-9_]+-\d+` issue key starting at `pos`.
fn issue_key_end(text: &str, pos: usize) -> Option<usize> {
    let b = text.as_bytes();
    if pos > 0 {
        let prev = b[pos - 1];
        if prev.is_ascii_alphanumeric() || matches!(prev, b'-' | b'_' | b'/') {
            return None;
        }
    }
    // A project key may hold an underscore, `A_PROJ-1`; the Markdown reader
    // already says so, and the two have to agree on what is a key.
    let mut i = pos + 1;
    while i < b.len() && (b[i].is_ascii_uppercase() || b[i].is_ascii_digit() || b[i] == b'_') {
        i += 1;
    }
    // `A_-1` is not a key: a project key ends in a letter or a digit.
    if i - pos < 2 || b[i - 1] == b'_' || b.get(i) != Some(&b'-') {
        return None;
    }
    let digits = i + 1;
    let mut j = digits;
    while j < b.len() && b[j].is_ascii_digit() {
        j += 1;
    }
    if j == digits {
        return None;
    }
    if let Some(after) = b.get(j) {
        if after.is_ascii_alphanumeric() || matches!(after, b'-' | b'_') {
            return None;
        }
        // `PROJ-1\-` is `PROJ-1-` with its hyphen escaped, and no more a key.
        if *after == b'\\' && matches!(b.get(j + 1), Some(b'-' | b'_')) {
            return None;
        }
    }
    Some(j)
}

// ---------------------------------------------------------------------------
// Renderer
// ---------------------------------------------------------------------------

/// Escape a paragraph line that would otherwise read as a block marker.
///
/// Wiki block syntax is line-anchored, so ordinary prose beginning with `|`,
/// `*`, `#`, `h2.` or `----` would be re-read as a table, list, heading or rule.
/// A leading backslash makes the line literal without changing how it renders.
fn guard_block_starts(text: &str) -> String {
    text.lines()
        .map(|line| {
            let trimmed = line.trim_start();
            let needs_guard = trimmed.starts_with('|')
                || trimmed.starts_with("----")
                || trimmed.starts_with("bq.")
                || list_marker(trimmed).is_some()
                || heading_at(trimmed).is_some();
            if needs_guard {
                format!("\\{line}")
            } else {
                line.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn render_blocks(blocks: &[Block], store: &RawStore, ctx: Ctx) -> String {
    let mut chunks: Vec<String> = Vec::new();
    for b in blocks {
        if let Some(s) = render_block(b, store, ctx) {
            if !s.is_empty() {
                chunks.push(s);
            }
        }
    }
    chunks.join("\n\n")
}

fn render_block(block: &Block, store: &RawStore, ctx: Ctx) -> Option<String> {
    Some(match block {
        Block::Paragraph(inlines) => guard_block_starts(&render_inlines(inlines, store, ctx)),
        Block::Heading { level, content } => {
            let level = (*level).clamp(1, 6);
            format!(
                "h{level}. {}",
                one_line(render_inlines(content, store, ctx))
            )
        }
        Block::CodeBlock { language, code } => match language {
            Some(lang) => format!("{{code:{lang}}}\n{code}\n{{code}}"),
            None => format!("{{noformat}}\n{code}\n{{noformat}}"),
        },
        Block::BlockQuote(inner) => {
            let body = render_blocks(inner, store, ctx);
            let simple = matches!(inner.as_slice(), [Block::Paragraph(_)]) && !body.contains('\n');
            if simple {
                format!("bq. {body}")
            } else {
                format!("{{quote}}\n{body}\n{{quote}}")
            }
        }
        Block::BulletList(items) => render_list(items, '*', "", store, ctx),
        Block::OrderedList { items, .. } => render_list(items, '#', "", store, ctx),
        Block::TaskList(items) => {
            // Wiki markup has no task list; the checkbox degrades to a Jira
            // emoticon inside a plain bullet.
            let mut lines = Vec::new();
            for item in items {
                let mark = if item.checked { "(/)" } else { "(x)" };
                lines.push(format!(
                    "* {mark} {}",
                    one_line(render_inlines(&item.content, store, ctx))
                ));
            }
            lines.join("\n")
        }
        Block::Table(t) => render_table(t, store),
        Block::Rule => "----".to_string(),
        Block::Raw(r) => return store.get(&r.id).map(|b| b.node_json.clone()),
    })
}

fn render_list(
    items: &[ListItem],
    marker: char,
    prefix: &str,
    store: &RawStore,
    ctx: Ctx,
) -> String {
    let bullet = format!("{prefix}{marker}");
    let mut lines: Vec<String> = Vec::new();

    for item in items {
        for block in &item.content {
            match block {
                Block::Paragraph(inlines) => {
                    lines.push(format!(
                        "{bullet} {}",
                        one_line(render_inlines(inlines, store, ctx))
                    ));
                }
                Block::BulletList(sub) => lines.push(render_list(sub, '*', &bullet, store, ctx)),
                Block::OrderedList { items: sub, .. } => {
                    lines.push(render_list(sub, '#', &bullet, store, ctx))
                }
                other => {
                    if let Some(s) = render_block(other, store, ctx) {
                        lines.push(s);
                    }
                }
            }
        }
    }

    lines.join("\n")
}

fn render_table(t: &Table, store: &RawStore) -> String {
    let mut lines: Vec<String> = Vec::new();
    if let Some(header) = &t.header {
        let cells: Vec<String> = header.iter().map(|c| render_cell(c, store)).collect();
        lines.push(format!("||{}||", cells.join("||")));
    }
    for row in &t.rows {
        let cells: Vec<String> = row.iter().map(|c| render_cell(c, store)).collect();
        lines.push(format!("|{}|", cells.join("|")));
    }
    lines.join("\n")
}

fn render_cell(cell: &[Block], store: &RawStore) -> String {
    let ctx = Ctx {
        in_table: true,
        ..Ctx::default()
    };
    one_line(render_blocks(cell, store, ctx))
}

/// Where a run of text sits, as far as escaping it is concerned.
#[derive(Debug, Clone, Copy, Default)]
struct Ctx {
    /// Something is written right before the text, on its line: a mark, a
    /// link, another run. What it ends with is not known here.
    before: bool,
    /// Something is written right after the text.
    after: bool,
    /// Inside a table cell, where a bare `|` ends the cell.
    in_table: bool,
}

impl Ctx {
    /// The text is enclosed: a mark's delimiters or a link's brackets sit on
    /// both sides of it.
    fn enclosed(self) -> Self {
        Self {
            before: true,
            after: true,
            ..self
        }
    }
}

/// Phrase delimiters: strong, emphasis, strikethrough, underline,
/// superscript, subscript. `??` (citation) and `!` (an image) are their own
/// cases.
const PHRASE_MARKS: [char; 6] = ['*', '_', '-', '+', '^', '~'];

/// Backslash-escape what Jira would read as markup in literal text.
///
/// `[` starts a link or a mention and `{` a macro or a code span, so those are
/// always escaped, and `\` is doubled where it would escape what follows it.
///
/// A phrase delimiter is escaped only where it would pair up: a word boundary
/// on its outside, text on its inside, and a partner on the same line. Every
/// `-` in prose escaped would make the Jira source unreadable; none escaped,
/// and `a +b+ c` went out as an underline — it is Markdown's literal text, and
/// a push turned it into formatting. Where the text meets something else the
/// neighbour is not known, and a delimiter there is escaped as if the
/// neighbour completed the pair.
///
/// A token holding `://` is a URL, which Jira links before it looks for marks,
/// so the `/-/` of a merge-request link is left alone.
fn escape_text(text: &str, ctx: Ctx) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut escape = vec![false; chars.len()];
    let in_url = url_mask(&chars);

    let mut start = 0;
    while start <= chars.len() {
        let end = chars[start..]
            .iter()
            .position(|c| *c == '\n')
            .map_or(chars.len(), |rel| start + rel);
        let line = Line {
            chars: &chars,
            start,
            end,
            open_before: start == 0 && ctx.before,
            open_after: end == chars.len() && ctx.after,
        };
        // An escape changes what its neighbour sees — `--a-` with the second
        // hyphen escaped leaves the first one opening onto a backslash — so
        // the flags are settled by running until nothing more changes.
        loop {
            let settled = escape.clone();
            for d in PHRASE_MARKS {
                line.mark_pairs(&[d], &in_url, &mut escape);
            }
            line.mark_pairs(&['?', '?'], &in_url, &mut escape);
            line.mark_images(&in_url, &mut escape);
            if escape == settled {
                break;
            }
        }
        start = end + 1;
    }

    let mut out = String::with_capacity(text.len() + 8);
    for (i, &ch) in chars.iter().enumerate() {
        let needed = match ch {
            '[' | '{' => true,
            // A backslash only escapes punctuation, so one before a letter is
            // written as it is and the server's text stays byte-identical. At
            // the end of the text the next character is not known — a mark or
            // a link may follow — so it is escaped there.
            '\\' => chars.get(i + 1).is_none_or(char::is_ascii_punctuation),
            '|' => ctx.in_table,
            _ => escape[i],
        };
        if needed {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

/// Which characters belong to a whitespace-delimited token holding `://`.
fn url_mask(chars: &[char]) -> Vec<bool> {
    let mut mask = vec![false; chars.len()];
    let mut i = 0;
    while i < chars.len() {
        if chars[i].is_whitespace() {
            i += 1;
            continue;
        }
        let end = chars[i..]
            .iter()
            .position(|c| c.is_whitespace())
            .map_or(chars.len(), |rel| i + rel);
        if chars[i..end].windows(3).any(|w| w == [':', '/', '/']) {
            mask[i..end].iter_mut().for_each(|m| *m = true);
        }
        i = end;
    }
    mask
}

/// One line of a run of text, with what is known about its edges.
struct Line<'a> {
    chars: &'a [char],
    start: usize,
    end: usize,
    /// The line is the first of the run and something precedes the run.
    open_before: bool,
    /// The line is the last of the run and something follows it.
    open_after: bool,
}

impl Line<'_> {
    /// The character at `i`, `None` past either end of the line.
    fn at(&self, i: isize) -> Option<char> {
        let i = usize::try_from(i).ok()?;
        (self.start..self.end).contains(&i).then(|| self.chars[i])
    }

    /// Flag every delimiter `delim` that could open or close a span and has a
    /// partner it could pair with.
    fn mark_pairs(&self, delim: &[char], in_url: &[bool], escape: &mut [bool]) {
        let n = delim.len();
        let mut opens = Vec::new();
        let mut closes = Vec::new();
        let mut i = self.start;
        while i + n <= self.end {
            if self.chars[i..i + n] != *delim || in_url[i] {
                i += 1;
                continue;
            }
            let prev = self.at(i as isize - 1);
            // What the reader meets after the delimiter is the backslash of
            // an escape, when there is one.
            let next = match self.at((i + n) as isize) {
                Some(_) if escape[i + n] => Some('\\'),
                other => other,
            };
            let first = i == self.start;
            let last = i + n == self.end;
            // `--` is a dash, not an empty strikethrough: a delimiter followed
            // by its own character does not open, here as in the reader.
            let can_open = prev.is_none_or(|p| !p.is_alphanumeric())
                && match next {
                    Some(c) => !c.is_whitespace() && c != delim[0],
                    None => self.open_after && last,
                };
            // Nor does one close straight after its own character, so the
            // second hyphen of `--` is left for Jira to draw as a dash.
            let can_close = match prev {
                Some(c) => !c.is_whitespace() && (c != delim[n - 1] || escape[i - 1]),
                None => self.open_before && first,
            } && next.is_none_or(|c| !c.is_alphanumeric());
            if can_open {
                opens.push(i);
            }
            if can_close {
                closes.push(i);
            }
            i += n;
        }
        for &o in &opens {
            if self.open_after || closes.iter().any(|&c| c > o + n) {
                escape[o] = true;
            }
        }
        for &c in &closes {
            if self.open_before || opens.iter().any(|&o| o + n < c) {
                escape[c] = true;
            }
        }
    }

    /// Flag a `!` that would start an embedded image: text right after it and
    /// another `!` later on the line.
    fn mark_images(&self, in_url: &[bool], escape: &mut [bool]) {
        for i in self.start..self.end {
            if self.chars[i] != '!' || in_url[i] {
                continue;
            }
            let opens = match self.at(i as isize + 1) {
                Some(c) => !c.is_whitespace(),
                None => self.open_after,
            };
            let closed = self.open_after || self.chars[i + 1..self.end].contains(&'!');
            if opens && closed {
                escape[i] = true;
            }
        }
    }
}

fn is_representable_user(user: &str) -> bool {
    !user.is_empty()
        && !user.chars().any(|c| {
            c.is_whitespace() || matches!(c, '[' | ']' | '|' | '*' | '_' | '-' | '{' | '}' | '\\')
        })
}

fn push_mark(out: &mut String, delimiter: char, content: &[Inline], store: &RawStore, ctx: Ctx) {
    let inner = render_inlines(content, store, ctx.enclosed());
    if inner.is_empty() {
        return;
    }
    out.push(delimiter);
    out.push_str(&inner);
    out.push(delimiter);
}

/// Escape the characters that would end a bracket span early.
///
/// Inside `[label|href]` a raw `|` starts the target and a raw `]` ends the
/// link, so a label containing either has to escape it.
fn escape_in_brackets(label: &str) -> String {
    let mut out = String::with_capacity(label.len());
    for ch in label.chars() {
        if matches!(ch, '|' | ']') {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

fn render_inlines(inlines: &[Inline], store: &RawStore, ctx: Ctx) -> String {
    let mut out = String::new();
    for (k, i) in inlines.iter().enumerate() {
        // A partner can be anywhere on the line, not only next door: a `*`
        // in bold text closes the bold even with a code span in between.
        let earlier = &inlines[..k];
        let later = &inlines[k + 1..];
        let here = Ctx {
            before: match earlier.iter().rposition(|n| matches!(n, Inline::LineBreak)) {
                Some(br) => earlier[br + 1..].iter().any(may_hold_delimiter),
                None => ctx.before || earlier.iter().any(may_hold_delimiter),
            },
            after: match later.iter().position(|n| matches!(n, Inline::LineBreak)) {
                Some(br) => later[..br].iter().any(may_hold_delimiter),
                None => ctx.after || later.iter().any(may_hold_delimiter),
            },
            ..ctx
        };
        match i {
            Inline::Text(t) => {
                let mut escaped = escape_text(t, here);
                // A delimiter against the same delimiter of the mark beside
                // it makes a run, and a run neither opens nor closes: `*a*`
                // then `*b` left the bold open.
                let first = t.chars().next();
                if first.is_some_and(|c| PHRASE_MARKS.contains(&c))
                    && first == last_bare_char(&out)
                    && !escaped.starts_with('\\')
                {
                    escaped.insert(0, '\\');
                }
                let last = t.chars().next_back();
                if last.is_some_and(|c| PHRASE_MARKS.contains(&c))
                    && last == inlines.get(k + 1).and_then(|n| first_char(n, store))
                    && !escaped.ends_with(&format!("\\{}", last.unwrap_or_default()))
                {
                    let at = escaped.len() - last.map_or(0, char::len_utf8);
                    escaped.insert(at, '\\');
                }
                out.push_str(&escaped);
            }
            // An empty mark has no wiki spelling: `**` or `__` re-parses as
            // literal text, so a mark with no content is simply dropped.
            Inline::Strong(c) => push_mark(&mut out, '*', c, store, ctx),
            Inline::Emphasis(c) => push_mark(&mut out, '_', c, store, ctx),
            Inline::Strike(c) => push_mark(&mut out, '-', c, store, ctx),
            Inline::Code(c) => {
                out.push_str("{{");
                out.push_str(c);
                out.push_str("}}");
            }
            Inline::Link { text, href } => {
                // Inside the brackets a `|` is escaped by the brackets' own
                // rule, below, not by the table's.
                let inner = Ctx {
                    in_table: false,
                    ..ctx.enclosed()
                };
                let label = render_inlines(text, store, inner);
                let mut label = escape_in_brackets(&label);
                // `[~name]` is mention syntax, so a label starting with `~`
                // has to be escaped or the link comes back as a mention.
                if label.starts_with('~') {
                    label.insert(0, '\\');
                }
                if href.is_empty() {
                    // `[]` and `[label]` with no target are not links in wiki
                    // markup; emitting them would re-parse as plain text and
                    // make the conversion churn.
                    out.push_str(&label);
                } else if label.is_empty()
                    || label == *href
                    || href.strip_prefix('^') == Some(label.as_str())
                {
                    out.push_str(&format!("[{href}]"));
                } else {
                    out.push_str(&format!("[{label}|{href}]"));
                }
            }
            Inline::Mention { id, display } => {
                let user = if id.is_empty() { display } else { id };
                // `[~name]` has no way to quote markup characters: a `*` in the
                // name would close the surrounding mark before the bracket ends.
                // Real Jira usernames never contain them, so an unrepresentable
                // one degrades to text rather than corrupting the line.
                if !is_representable_user(user) {
                    out.push_str(&escape_text(&format!("@{user}"), here));
                    continue;
                }
                let user = &escape_in_brackets(user);
                out.push_str(&format!("[~{user}]"));
            }
            Inline::IssueKey(k) => out.push_str(k),
            Inline::Image { src } => {
                let fits = !src.is_empty()
                    && !src.contains(['!', '|', '\n'])
                    && src.trim() == src
                    && (src.contains('.') || src.contains("://"));
                if fits {
                    out.push('!');
                    out.push_str(src);
                    out.push('!');
                } else {
                    // Not a name Jira can embed; the target survives as a link.
                    out.push_str(&format!("[{}]", escape_in_brackets(src)));
                }
            }
            Inline::LineBreak => out.push('\n'),
            Inline::Raw(r) => {
                if let Some(b) = store.get(&r.id) {
                    out.push_str(&b.node_json);
                }
            }
        }
    }
    out
}

/// The last character written, unless it is escaped.
fn last_bare_char(out: &str) -> Option<char> {
    let last = out.chars().next_back()?;
    (!out[..out.len() - last.len_utf8()].ends_with('\\')).then_some(last)
}

/// The first character a node is written with.
fn first_char(inline: &Inline, store: &RawStore) -> Option<char> {
    match inline {
        Inline::Strong(_) => Some('*'),
        Inline::Emphasis(_) => Some('_'),
        Inline::Strike(_) => Some('-'),
        Inline::Image { .. } => Some('!'),
        Inline::Raw(r) => store.get(&r.id)?.node_json.chars().next(),
        _ => None,
    }
}

/// Whether a node on the same line can put a bare phrase delimiter there.
///
/// A mark writes its own, a region writes the server's text verbatim, and an
/// image is a pair of `!`.
/// Code, links and mentions cannot: the reader looks for no partner inside
/// them, and a key is letters, digits and one hyphen between them. A line
/// break starts a line, and marks do not cross one.
fn may_hold_delimiter(inline: &Inline) -> bool {
    matches!(
        inline,
        Inline::Strong(_)
            | Inline::Emphasis(_)
            | Inline::Strike(_)
            | Inline::Raw(_)
            | Inline::Text(_)
            | Inline::Image { .. }
    )
}

fn one_line(s: String) -> String {
    if s.contains('\n') {
        s.replace('\n', " ")
    } else {
        s
    }
}

#[cfg(test)]
mod escaping_tests {
    use super::*;

    fn trip(text: &str) -> String {
        let mut store = RawStore::new();
        let doc = to_ast(text, &mut store);
        from_ast(&doc, &store)
    }

    fn ast_of(text: &str) -> Document {
        let mut store = RawStore::new();
        to_ast(text, &mut store)
    }

    /// Found by the property suite: a literal bracket in prose used to be
    /// re-read as a link or a mention on the next pull.
    #[test]
    fn literal_brackets_and_braces_survive_a_round_trip() {
        for text in [
            "a [b] c",
            "see [~notauser] here",
            "use {panel} literally",
            "back\\slash",
        ] {
            let once = trip(text);
            assert_eq!(
                ast_of(&once),
                ast_of(&trip(&once)),
                "not stable for {text:?} (rendered {once:?})"
            );
        }
        assert_eq!(
            ast_of("a \\[b\\] c"),
            Document::new(vec![Block::Paragraph(vec![Inline::Text("a [b] c".into())])])
        );
    }

    /// A description holding `\n` pasted from JSON came back as `n`, reported
    /// clean and not lossy, so the next edit pushed it without the backslash.
    /// A backslash before a letter is not an escape.
    #[test]
    fn a_backslash_before_a_letter_is_kept() {
        let server = "accepts:\\n- from clients C:\\temp\\x";
        assert_eq!(
            ast_of(server),
            Document::new(vec![Block::Paragraph(vec![Inline::Text(
                "accepts:\\n- from clients C:\\temp\\x".into()
            )])])
        );
        assert_eq!(trip(server), server, "the server's bytes must come back");

        // And through the file: what a push sends is what the server held.
        let mut store = RawStore::new();
        let md = crate::convert::markdown::render(&to_ast(server, &mut store), &store);
        assert!(md.contains("accepts:\\\\n"), "{md}");
        let pushed = from_ast(&crate::convert::markdown::parse(&md, &store), &store);
        assert_eq!(pushed, server);
    }

    /// The guard written before prose that starts like a heading or a quote
    /// is a backslash before a letter, and it is still a guard.
    #[test]
    fn a_guarded_heading_or_quote_line_stays_prose() {
        for line in ["h2. not a heading", "bq. not a quote"] {
            let doc = Document::new(vec![Block::Paragraph(vec![Inline::Text(line.into())])]);
            let text = from_ast(&doc, &RawStore::new());
            assert_eq!(text, format!("\\{line}"));
            assert_eq!(ast_of(&text), doc, "{text:?}");
        }
    }

    /// Before punctuation a backslash still escapes, and a literal one there
    /// is still written doubled.
    #[test]
    fn a_backslash_before_punctuation_still_escapes() {
        assert_eq!(
            ast_of("a \\*b\\* \\\\ c"),
            Document::new(vec![Block::Paragraph(vec![Inline::Text(
                "a *b* \\ c".into()
            )])])
        );
        let doc = Document::new(vec![Block::Paragraph(vec![Inline::Text(
            "x\\[y] z\\".into(),
        )])]);
        let text = from_ast(&doc, &RawStore::new());
        assert_eq!(text, "x\\\\\\[y] z\\\\");
        assert_eq!(ast_of(&text), doc);
    }

    fn para(text: &str) -> Document {
        Document::new(vec![Block::Paragraph(vec![Inline::Text(text.into())])])
    }

    /// Markdown's literal text was sent as Jira formatting: `+x+` is an
    /// underline there, `-x-` a strikethrough, `^x^` a superscript.
    #[test]
    fn literal_phrase_markers_are_escaped_where_they_would_pair() {
        for (literal, wire) in [
            ("word +underlined+ end", "word \\+underlined\\+ end"),
            ("and -struck- and", "and \\-struck\\- and"),
            ("and ^up^ and", "and \\^up\\^ and"),
            ("and ~down~ and", "and \\~down\\~ and"),
            ("and *bold* and _it_", "and \\*bold\\* and \\_it\\_"),
            ("a ??cited?? b", "a \\??cited\\?? b"),
            ("an !image.png! here", "an \\!image.png! here"),
            ("signs «+» (none), «++»", "signs «\\+» (none), «\\+\\+»"),
            ("«[22]»", "«\\[22]»"),
            ("a -minus- b and -- dash", "a \\-minus\\- b and -- dash"),
        ] {
            let rendered = from_ast(&para(literal), &RawStore::new());
            assert_eq!(rendered, wire, "for {literal:?}");
            assert_eq!(ast_of(&rendered), para(literal), "for {literal:?}");
        }
    }

    /// Prose punctuation that cannot pair stays unescaped, so the Jira source
    /// reads as it did.
    #[test]
    fn punctuation_that_cannot_pair_is_left_alone() {
        for text in [
            "a well-known fix, 2026-11-15",
            "a - b and 1 + 2 = 3",
            "C++ and x^2",
            "-- and --- are dashes",
            "Wow! Great!",
            "Really? Yes?",
            "see https://git.example/group/proj/-/merge_requests/1 and -x-y",
            "snake_case_name",
        ] {
            assert_eq!(from_ast(&para(text), &RawStore::new()), text);
            assert_eq!(ast_of(text), para(text), "{text:?} must read as text");
        }
    }

    /// Where the text ends and a mark begins, the mark's delimiter could
    /// complete a pair, so the text's side is escaped.
    #[test]
    fn a_delimiter_next_to_another_node_is_escaped() {
        let doc = Document::new(vec![Block::Paragraph(vec![
            Inline::Text("x +a ".into()),
            Inline::Strong(vec![Inline::Text("b+".into())]),
        ])]);
        let text = from_ast(&doc, &RawStore::new());
        assert_eq!(ast_of(&text), doc, "{text:?}");
    }

    /// Jira's own underline, superscript, subscript and citation have no
    /// Markdown form; they come back as regions that push back verbatim.
    #[test]
    fn jira_only_marks_are_kept_as_regions() {
        for (src, label, approx) in [
            ("a +under+ b", "underline", "<u>under</u>"),
            ("a ^up^ b", "superscript", "<sup>up</sup>"),
            ("a ~down~ b", "subscript", "<sub>down</sub>"),
            ("a ??Book?? b", "citation", "<cite>Book</cite>"),
        ] {
            let mut store = RawStore::new();
            let doc = to_ast(src, &mut store);
            assert_eq!(store.blocks().len(), 1, "{src:?}: {doc:?}");
            assert_eq!(store.blocks()[0].label, label);
            assert_eq!(store.blocks()[0].approx_md, approx);
            assert_eq!(from_ast(&doc, &store), src);

            let md = crate::convert::markdown::render(&doc, &store);
            let back = crate::convert::markdown::parse(&md, &store);
            assert_eq!(from_ast(&back, &store), src, "through the file: {md}");
        }
    }

    /// An embedded image and an attachment link come into the file pointing
    /// at the issue's attachment folder, and go back as Jira wrote them.
    #[test]
    fn images_and_attachment_links_point_into_the_attachment_folder() {
        use crate::convert::markdown;
        let src = "see !shot.png! and [^logs.zip], [the log|^logs.zip], [https://git.example/a/-/b]\n\n!https://img.example/x.png!";
        let mut store = RawStore::new();
        let doc = to_ast(src, &mut store);
        assert!(store.is_empty(), "nothing here is lossy: {doc:?}");
        let md = markdown::render_with(&doc, &store, Some(".PROJ-7. A summary"));
        assert_eq!(
            md,
            "see ![](<.PROJ-7. A summary/shot.png>) and [logs.zip](<.PROJ-7. A summary/logs.zip>), \
             [the log](<.PROJ-7. A summary/logs.zip>), <https://git.example/a/-/b>\n\n\
             ![](https://img.example/x.png)\n"
        );
        let back = markdown::parse(&md, &store);
        assert_eq!(from_ast(&back, &store), src);

        // An image with parameters has no Markdown form and is kept as it is.
        let mut store = RawStore::new();
        let doc = to_ast("!shot.png|thumbnail!", &mut store);
        assert_eq!(store.blocks()[0].label, "image");
        assert_eq!(from_ast(&doc, &store), "!shot.png|thumbnail!");

        // Two exclamation marks in a sentence are not an image.
        assert_eq!(ast_of("Wow!Great!"), para("Wow!Great!"));
    }

    /// A `|` in a table cell is escaped, and an escaped one or one inside a
    /// link does not split the cell.
    #[test]
    fn a_pipe_in_a_table_cell_stays_in_the_cell() {
        let cell = |inlines: Vec<Inline>| vec![Block::Paragraph(inlines)];
        let doc = Document::new(vec![Block::Table(Table {
            header: Some(vec![cell(vec![Inline::Text("a | b".into())])]),
            rows: vec![vec![cell(vec![
                Inline::Text("x|y ".into()),
                Inline::Link {
                    text: vec![Inline::Text("l".into())],
                    href: "https://h.example/".into(),
                },
            ])]],
        })]);
        let text = from_ast(&doc, &RawStore::new());
        assert_eq!(text, "||a \\| b||\n|x\\|y [l|https://h.example/]|");
        assert_eq!(ast_of(&text), doc);
    }

    #[test]
    fn a_pipe_in_a_link_label_does_not_split_the_link() {
        let doc = Document::new(vec![Block::Paragraph(vec![Inline::Link {
            text: vec![Inline::Text("a | b".into())],
            href: "https://x.example/".into(),
        }])]);
        let store = RawStore::new();
        let text = from_ast(&doc, &store);
        assert_eq!(ast_of(&text), doc, "rendered as {text:?}");
    }

    /// Prose beginning with a block marker must not become a table or a list.
    #[test]
    fn a_line_starting_with_a_block_marker_is_escaped() {
        for text in [
            "| not a table",
            "* not a list",
            "# not a list",
            "---- not a rule",
        ] {
            let doc = Document::new(vec![Block::Paragraph(vec![Inline::Text(text.into())])]);
            let store = RawStore::new();
            let rendered = from_ast(&doc, &store);
            assert!(
                rendered.starts_with('\\'),
                "{text:?} rendered as {rendered:?}"
            );
            assert_eq!(ast_of(&rendered), doc);
        }
    }

    /// Found by the property suite: these labels collide with other syntax.
    #[test]
    fn link_labels_that_look_like_other_syntax_still_round_trip() {
        for label in ["~", "~user", "a | b", "a ] b"] {
            let doc = Document::new(vec![Block::Paragraph(vec![Inline::Link {
                text: vec![Inline::Text(label.into())],
                href: "https://x.example/".into(),
            }])]);
            let store = RawStore::new();
            let text = from_ast(&doc, &store);
            assert_eq!(ast_of(&text), doc, "label {label:?} rendered as {text:?}");
        }
    }

    /// A username containing markup cannot be spelled as `[~name]` at all.
    #[test]
    fn an_unrepresentable_mention_degrades_to_text() {
        let doc = Document::new(vec![Block::Paragraph(vec![Inline::Mention {
            id: "a|b".into(),
            display: "a|b".into(),
        }])]);
        let store = RawStore::new();
        let text = from_ast(&doc, &store);
        assert_eq!(text, "@a|b");
        // And that form is stable rather than flipping back and forth.
        assert_eq!(ast_of(&text), ast_of(&from_ast(&ast_of(&text), &store)));
    }

    #[test]
    fn a_link_without_a_target_degrades_to_its_label() {
        let doc = Document::new(vec![Block::Paragraph(vec![Inline::Link {
            text: vec![Inline::Text("label".into())],
            href: String::new(),
        }])]);
        let store = RawStore::new();
        assert_eq!(from_ast(&doc, &store), "label");
    }

    #[test]
    fn an_empty_mark_is_dropped_rather_than_emitted() {
        let doc = Document::new(vec![Block::Paragraph(vec![Inline::Emphasis(vec![])])]);
        let store = RawStore::new();
        assert_eq!(from_ast(&doc, &store), "");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(src: &str) -> (Document, RawStore) {
        let mut store = RawStore::new();
        let doc = to_ast(src, &mut store);
        (doc, store)
    }

    /// Render and re-parse; the AST must be identical (byte equality is only
    /// promised for raw regions).
    fn round_trip(src: &str) -> String {
        let (doc, store) = parse(src);
        let out = from_ast(&doc, &store);
        let (doc2, _) = parse(&out);
        assert_eq!(
            doc, doc2,
            "round trip changed the AST\nsrc: {src:?}\nout: {out:?}"
        );
        out
    }

    fn text(s: &str) -> Inline {
        Inline::Text(s.to_string())
    }

    #[test]
    fn headings_round_trip_all_levels() {
        for level in 1..=6u8 {
            let src = format!("h{level}. Title {level}");
            let (doc, _) = parse(&src);
            assert_eq!(
                doc.blocks,
                vec![Block::Heading {
                    level,
                    content: vec![text(&format!("Title {level}"))],
                }]
            );
            assert_eq!(round_trip(&src), src);
        }
    }

    #[test]
    fn inline_marks_parse_and_render() {
        let cases: Vec<(&str, Inline)> = vec![
            ("*bold*", Inline::Strong(vec![text("bold")])),
            ("_italic_", Inline::Emphasis(vec![text("italic")])),
            ("-strike-", Inline::Strike(vec![text("strike")])),
            ("{{mono}}", Inline::Code("mono".into())),
            (
                "[text|https://x]",
                Inline::Link {
                    text: vec![text("text")],
                    href: "https://x".into(),
                },
            ),
            (
                "[https://x]",
                Inline::Link {
                    text: vec![text("https://x")],
                    href: "https://x".into(),
                },
            ),
            (
                "[^logs.zip]",
                Inline::Link {
                    text: vec![text("logs.zip")],
                    href: "^logs.zip".into(),
                },
            ),
            (
                "!shot.png!",
                Inline::Image {
                    src: "shot.png".into(),
                },
            ),
            (
                "[~ilya]",
                Inline::Mention {
                    id: "ilya".into(),
                    display: "ilya".into(),
                },
            ),
        ];
        for (src, expected) in cases {
            let (doc, _) = parse(src);
            assert_eq!(doc.blocks, vec![Block::Paragraph(vec![expected])], "{src}");
            assert_eq!(round_trip(src), src);
        }
    }

    #[test]
    fn marks_inside_a_sentence_do_not_eat_hyphens() {
        let (doc, _) = parse("a well-known *fix* here");
        assert_eq!(
            doc.blocks,
            vec![Block::Paragraph(vec![
                text("a well-known "),
                Inline::Strong(vec![text("fix")]),
                text(" here"),
            ])]
        );
    }

    #[test]
    fn bullet_and_ordered_lists_with_nesting() {
        let src = "* one\n** deep\n* two";
        let (doc, _) = parse(src);
        assert_eq!(
            doc.blocks,
            vec![Block::BulletList(vec![
                ListItem {
                    content: vec![
                        Block::Paragraph(vec![text("one")]),
                        Block::BulletList(vec![ListItem {
                            content: vec![Block::Paragraph(vec![text("deep")])],
                        }]),
                    ],
                },
                ListItem {
                    content: vec![Block::Paragraph(vec![text("two")])],
                },
            ])]
        );
        assert_eq!(round_trip(src), src);

        let src = "# first\n## nested\n# second";
        let (doc, _) = parse(src);
        let Block::OrderedList { start, items } = &doc.blocks[0] else {
            panic!("expected an ordered list, got {:?}", doc.blocks[0]);
        };
        assert_eq!(*start, 1);
        assert_eq!(items.len(), 2);
        assert!(matches!(items[0].content[1], Block::OrderedList { .. }));
        assert_eq!(round_trip(src), src);
    }

    #[test]
    fn code_and_noformat_blocks() {
        let (doc, _) = parse("{code:rust}fn main(){}{code}");
        assert_eq!(
            doc.blocks,
            vec![Block::CodeBlock {
                language: Some("rust".into()),
                code: "fn main(){}".into(),
            }]
        );
        round_trip("{code:rust}fn main(){}{code}");

        let src = "{noformat}\nplain  text\n{noformat}";
        let (doc, _) = parse(src);
        assert_eq!(
            doc.blocks,
            vec![Block::CodeBlock {
                language: None,
                code: "plain  text".into(),
            }]
        );
        assert_eq!(round_trip(src), src);
    }

    #[test]
    fn quote_block_and_bq_line() {
        let (doc, _) = parse("bq. quoted");
        assert_eq!(
            doc.blocks,
            vec![Block::BlockQuote(vec![Block::Paragraph(vec![text(
                "quoted"
            )])])]
        );
        assert_eq!(round_trip("bq. quoted"), "bq. quoted");

        let src = "{quote}\nfirst\n\nsecond\n{quote}";
        let (doc, _) = parse(src);
        assert_eq!(
            doc.blocks,
            vec![Block::BlockQuote(vec![
                Block::Paragraph(vec![text("first")]),
                Block::Paragraph(vec![text("second")]),
            ])]
        );
        assert_eq!(round_trip(src), src);
    }

    /// A header row of Cyrillic cells panicked every pull of the workspace:
    /// the cell splitter stepped a byte at a time and sliced inside `Ф`.
    #[test]
    fn a_table_whose_cells_start_with_a_multibyte_character_is_a_table() {
        let src = "||Имя||Тип||Размер||Владелец||\n|a|b|c|d|";
        let (doc, store) = parse(src);
        let row = |cells: &[&str]| {
            cells
                .iter()
                .map(|c| vec![Block::Paragraph(vec![text(c)])])
                .collect::<Vec<_>>()
        };
        assert_eq!(
            doc.blocks,
            vec![Block::Table(Table {
                header: Some(row(&["Имя", "Тип", "Размер", "Владелец"])),
                rows: vec![row(&["a", "b", "c", "d"])],
            })]
        );
        assert_eq!(round_trip(src), src);
        let md = crate::convert::markdown::render(&doc, &store);
        assert!(md.contains("| Имя | Тип | Размер | Владелец |"), "{md}");
    }

    /// Every delimiter the reader scans for, with a character of two, three
    /// and four bytes on each side of it, at the start and the end of the
    /// text and of a table cell. The reader walks bytes, so each of these is a
    /// place a step of one could land inside a character.
    #[test]
    fn multibyte_text_beside_any_delimiter_does_not_panic() {
        let delimited = [
            "*Ф*",
            "_Ф_",
            "-Ф-",
            "+Ф+",
            "^Ф^",
            "~Ф~",
            "??Ф??",
            "{{Ф}}",
            "{{{}Ф{}}}",
            "[Ф]",
            "[Ф|https://x.example/]",
            "[~Ф]",
            "!Ф.png!",
            "{color:red}Ф{color}",
            "\\Ф",
            "Ф-1",
            "{Ф}",
            "||",
            "|",
            "{",
            "}",
            "[",
            "!",
            "?",
        ];
        for d in delimited {
            for ch in ["Ф", "日", "😀"] {
                for text in [
                    format!("{ch}{d}{ch}"),
                    format!("{d}{ch}"),
                    format!("{ch}{d}"),
                    format!("x {ch}{d}{ch} x"),
                    format!("||{ch}{d}||{d}{ch}||\n|{ch}{d}|{d}{ch}|"),
                    format!("* {ch}{d}\n# {d}{ch}"),
                    format!("h1. {ch}{d}{ch}"),
                ] {
                    let (doc, store) = parse(&text);
                    let wiki = from_ast(&doc, &store);
                    let md = crate::convert::markdown::render(&doc, &store);
                    let _ = from_ast(&crate::convert::markdown::parse(&md, &store), &store);
                    let _ = parse(&wiki);
                }
            }
        }
    }

    #[test]
    fn table_with_header_round_trips() {
        let src = "||h1||h2||\n|a|b|\n|c|d|";
        let (doc, _) = parse(src);
        assert_eq!(
            doc.blocks,
            vec![Block::Table(Table {
                header: Some(vec![
                    vec![Block::Paragraph(vec![text("h1")])],
                    vec![Block::Paragraph(vec![text("h2")])],
                ]),
                rows: vec![
                    vec![
                        vec![Block::Paragraph(vec![text("a")])],
                        vec![Block::Paragraph(vec![text("b")])],
                    ],
                    vec![
                        vec![Block::Paragraph(vec![text("c")])],
                        vec![Block::Paragraph(vec![text("d")])],
                    ],
                ],
            })]
        );
        assert_eq!(round_trip(src), src);
    }

    #[test]
    fn horizontal_rule() {
        let (doc, _) = parse("----");
        assert_eq!(doc.blocks, vec![Block::Rule]);
        assert_eq!(round_trip("----"), "----");
    }

    #[test]
    fn panel_is_preserved_byte_for_byte() {
        let src = "{panel:title=Warning}Do not deploy{panel}";
        let (doc, store) = parse(src);
        assert_eq!(doc.blocks.len(), 1);
        let Block::Raw(r) = &doc.blocks[0] else {
            panic!("expected a raw block, got {:?}", doc.blocks[0]);
        };
        assert_eq!(store.blocks().len(), 1);
        assert_eq!(store.blocks()[0].node_json, src);
        assert_eq!(store.blocks()[0].label, "panel");
        assert_eq!(
            store.get(&r.id).unwrap().approx_md,
            "> **[jired: Jira panel \"Warning\" — read-only]**\n> Do not deploy"
        );
        assert_eq!(from_ast(&doc, &store), src);
    }

    #[test]
    fn unknown_macro_is_preserved_byte_for_byte() {
        let src = "{futuremacro:x=1}body{futuremacro}";
        let (doc, store) = parse(src);
        assert!(matches!(doc.blocks.as_slice(), [Block::Raw(_)]));
        assert_eq!(store.blocks()[0].node_json, src);
        assert_eq!(store.blocks()[0].label, "futuremacro");
        assert_eq!(
            store.blocks()[0].approx_md,
            "**[jired: futuremacro macro — read-only]**\nbody"
        );
        assert_eq!(from_ast(&doc, &store), src);
    }

    #[test]
    fn standalone_macro_without_a_closing_tag_is_raw() {
        let src = "{toc}";
        let (doc, store) = parse(src);
        assert!(matches!(doc.blocks.as_slice(), [Block::Raw(_)]));
        assert_eq!(store.blocks()[0].node_json, "{toc}");
        assert_eq!(from_ast(&doc, &store), src);
    }

    #[test]
    fn inline_macro_becomes_an_inline_raw() {
        let src = "text {color:red}red{color} tail";
        let (doc, store) = parse(src);
        let Block::Paragraph(inlines) = &doc.blocks[0] else {
            panic!("expected a paragraph, got {:?}", doc.blocks[0]);
        };
        assert!(matches!(inlines[1], Inline::Raw(_)));
        assert_eq!(store.blocks()[0].node_json, "{color:red}red{color}");
        assert_eq!(from_ast(&doc, &store), src);
    }

    #[test]
    fn missing_raw_id_is_skipped_not_panicked() {
        let doc = Document::new(vec![
            Block::Paragraph(vec![text("before")]),
            Block::Raw(crate::convert::ast::RawRef {
                id: "rdeadbe".into(),
            }),
            Block::Paragraph(vec![text("after")]),
        ]);
        assert_eq!(from_ast(&doc, &RawStore::new()), "before\n\nafter");
    }

    #[test]
    fn bare_issue_key_round_trips() {
        let src = "See PROJ-142 for background.";
        let (doc, _) = parse(src);
        assert_eq!(
            doc.blocks,
            vec![Block::Paragraph(vec![
                text("See "),
                Inline::IssueKey("PROJ-142".into()),
                text(" for background."),
            ])]
        );
        assert_eq!(round_trip(src), src);
    }

    #[test]
    fn line_break_inside_a_paragraph() {
        let src = "first line\nsecond line";
        let (doc, _) = parse(src);
        assert_eq!(
            doc.blocks,
            vec![Block::Paragraph(vec![
                text("first line"),
                Inline::LineBreak,
                text("second line"),
            ])]
        );
        assert_eq!(round_trip(src), src);
    }

    #[test]
    fn empty_input_and_empty_document() {
        let (doc, store) = parse("");
        assert!(doc.is_empty());
        assert!(store.is_empty());
        assert_eq!(from_ast(&Document::default(), &RawStore::new()), "");
        let (doc, _) = parse("   \n\n  \n");
        assert!(doc.is_empty());
    }

    #[test]
    fn mixed_document_keeps_block_order() {
        let src =
            "Intro paragraph.\n\n{panel:title=Warning}Do not deploy{panel}\n\nOutro paragraph.";
        let (doc, store) = parse(src);
        assert_eq!(doc.blocks.len(), 3);
        assert!(matches!(doc.blocks[0], Block::Paragraph(_)));
        assert!(matches!(doc.blocks[1], Block::Raw(_)));
        assert!(matches!(doc.blocks[2], Block::Paragraph(_)));
        assert_eq!(from_ast(&doc, &store), src);
        assert_eq!(round_trip(src), src);
    }
}
