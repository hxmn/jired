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
//! pushes back byte-identical by construction.

use crate::convert::ast::{normalize_inlines, Block, Document, Inline, ListItem, RawStore, Table};

/// Parse Jira wiki markup into the internal AST, capturing macros and other
/// unrepresentable constructs into `store`.
pub fn to_ast(text: &str, store: &mut RawStore) -> Document {
    Document::new(parse_blocks(text, store))
}

/// Render the AST back to wiki markup. Raw regions are reproduced verbatim
/// from `store`, never re-converted.
pub fn from_ast(doc: &Document, store: &RawStore) -> String {
    render_blocks(&doc.blocks, store)
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

fn split_row(line: &str, header: bool) -> Vec<&str> {
    let sep = if header { "||" } else { "|" };
    let inner = line
        .strip_prefix(sep)
        .unwrap_or(line)
        .strip_suffix(sep)
        .unwrap_or_else(|| line.strip_prefix(sep).unwrap_or(line));
    inner.split(sep).collect()
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
    let b = text.as_bytes();
    let mut out: Vec<Inline> = Vec::new();
    let mut buf = String::new();
    let mut i = 0usize;

    while i < b.len() {
        match b[i] {
            // Jira wiki markup escapes with a backslash; the next character is
            // literal text whatever it would otherwise mean.
            b'\\' if i + 1 < b.len() => {
                let ch = text[i + 1..].chars().next().unwrap_or('\\');
                buf.push(ch);
                i += 1 + ch.len_utf8();
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
            d @ (b'*' | b'_' | b'-') => match mark_close(text, i, d) {
                Some(close) => {
                    flush(&mut buf, &mut out);
                    let inner = parse_inlines(&text[i + 1..close], store);
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
            },
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
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            if let Some(next) = chars.next() {
                out.push(next);
                continue;
            }
        }
        out.push(c);
    }
    out
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
        None => Inline::Link {
            text: Vec::new(),
            href: unescape(content.trim()),
        },
    }
}

/// Find the closing delimiter of a `*bold*`-style span on the same line.
fn mark_close(text: &str, open: usize, delim: u8) -> Option<usize> {
    let b = text.as_bytes();
    if open > 0 {
        let prev = b[open - 1];
        let opens = prev.is_ascii_whitespace()
            || matches!(prev, b'(' | b'[' | b'{' | b'"' | b'\'' | b',' | b';' | b':');
        if !opens {
            return None;
        }
    }
    let next = *b.get(open + 1)?;
    if next == delim || next.is_ascii_whitespace() {
        return None;
    }
    let limit = text[open..]
        .find('\n')
        .map(|rel| open + rel)
        .unwrap_or(b.len());

    let mut i = open + 1;
    while i < limit {
        if b[i] == delim && !b[i - 1].is_ascii_whitespace() {
            let closes = match b.get(i + 1) {
                None => true,
                Some(after) => !(after.is_ascii_alphanumeric() || *after == delim),
            };
            if closes {
                return Some(i);
            }
        }
        i += 1;
    }
    None
}

/// End offset of a bare `[A-Z][A-Z0-9]+-\d+` issue key starting at `pos`.
fn issue_key_end(text: &str, pos: usize) -> Option<usize> {
    let b = text.as_bytes();
    if pos > 0 {
        let prev = b[pos - 1];
        if prev.is_ascii_alphanumeric() || matches!(prev, b'-' | b'_' | b'/') {
            return None;
        }
    }
    let mut i = pos + 1;
    while i < b.len() && (b[i].is_ascii_uppercase() || b[i].is_ascii_digit()) {
        i += 1;
    }
    if i - pos < 2 || b.get(i) != Some(&b'-') {
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

fn render_blocks(blocks: &[Block], store: &RawStore) -> String {
    let mut chunks: Vec<String> = Vec::new();
    for b in blocks {
        if let Some(s) = render_block(b, store) {
            if !s.is_empty() {
                chunks.push(s);
            }
        }
    }
    chunks.join("\n\n")
}

fn render_block(block: &Block, store: &RawStore) -> Option<String> {
    Some(match block {
        Block::Paragraph(inlines) => guard_block_starts(&render_inlines(inlines, store)),
        Block::Heading { level, content } => {
            let level = (*level).clamp(1, 6);
            format!("h{level}. {}", one_line(render_inlines(content, store)))
        }
        Block::CodeBlock { language, code } => match language {
            Some(lang) => format!("{{code:{lang}}}\n{code}\n{{code}}"),
            None => format!("{{noformat}}\n{code}\n{{noformat}}"),
        },
        Block::BlockQuote(inner) => {
            let body = render_blocks(inner, store);
            let simple = matches!(inner.as_slice(), [Block::Paragraph(_)]) && !body.contains('\n');
            if simple {
                format!("bq. {body}")
            } else {
                format!("{{quote}}\n{body}\n{{quote}}")
            }
        }
        Block::BulletList(items) => render_list(items, '*', "", store),
        Block::OrderedList { items, .. } => render_list(items, '#', "", store),
        Block::TaskList(items) => {
            // Wiki markup has no task list; the checkbox degrades to a Jira
            // emoticon inside a plain bullet.
            let mut lines = Vec::new();
            for item in items {
                let mark = if item.checked { "(/)" } else { "(x)" };
                lines.push(format!(
                    "* {mark} {}",
                    one_line(render_inlines(&item.content, store))
                ));
            }
            lines.join("\n")
        }
        Block::Table(t) => render_table(t, store),
        Block::Rule => "----".to_string(),
        Block::Raw(r) => return store.get(&r.id).map(|b| b.node_json.clone()),
    })
}

fn render_list(items: &[ListItem], marker: char, prefix: &str, store: &RawStore) -> String {
    let bullet = format!("{prefix}{marker}");
    let mut lines: Vec<String> = Vec::new();

    for item in items {
        for block in &item.content {
            match block {
                Block::Paragraph(inlines) => {
                    lines.push(format!(
                        "{bullet} {}",
                        one_line(render_inlines(inlines, store))
                    ));
                }
                Block::BulletList(sub) => lines.push(render_list(sub, '*', &bullet, store)),
                Block::OrderedList { items: sub, .. } => {
                    lines.push(render_list(sub, '#', &bullet, store))
                }
                other => {
                    if let Some(s) = render_block(other, store) {
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
    one_line(render_blocks(cell, store))
}

/// Backslash-escape the characters that open a construct the tokenizer would
/// otherwise consume.
///
/// Only the openers need escaping: `[` starts a link or mention, `{` starts a
/// macro or a code span, and `\` has to escape itself. Mark delimiters
/// (`*`, `_`, `-`) are left alone — they only bind between word boundaries, and
/// escaping every one of them would make ordinary prose unreadable in Jira.
fn escape_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        if matches!(ch, '[' | '{' | '\\') {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

fn is_representable_user(user: &str) -> bool {
    !user.is_empty()
        && !user.chars().any(|c| {
            c.is_whitespace() || matches!(c, '[' | ']' | '|' | '*' | '_' | '-' | '{' | '}' | '\\')
        })
}

fn push_mark(out: &mut String, delimiter: char, content: &[Inline], store: &RawStore) {
    let inner = render_inlines(content, store);
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

fn render_inlines(inlines: &[Inline], store: &RawStore) -> String {
    let mut out = String::new();
    for i in inlines {
        match i {
            Inline::Text(t) => out.push_str(&escape_text(t)),
            // An empty mark has no wiki spelling: `**` or `__` re-parses as
            // literal text, so a mark with no content is simply dropped.
            Inline::Strong(c) => push_mark(&mut out, '*', c, store),
            Inline::Emphasis(c) => push_mark(&mut out, '_', c, store),
            Inline::Strike(c) => push_mark(&mut out, '-', c, store),
            Inline::Code(c) => {
                out.push_str("{{");
                out.push_str(c);
                out.push_str("}}");
            }
            Inline::Link { text, href } => {
                let label = render_inlines(text, store);
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
                } else if label.is_empty() || label == *href {
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
                    out.push_str(&escape_text(&format!("@{user}")));
                    continue;
                }
                let user = &escape_in_brackets(user);
                out.push_str(&format!("[~{user}]"));
            }
            Inline::IssueKey(k) => out.push_str(k),
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
                    text: vec![],
                    href: "https://x".into(),
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
