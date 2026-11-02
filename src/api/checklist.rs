//! Reading and writing a Smart Checklist custom field [M5.4].
//!
//! "Smart Checklist for Jira" keeps its data in custom fields whose ids differ
//! per instance, so they are found by display name — `Checklist`, `Checklists`,
//! `Checklist Text`, `Checklist Progress` — the same way Story Points and
//! Sprint are.
//!
//! What arrives there is not one shape but three, and which one a server sends
//! depends on the product and the plugin version:
//!
//! * The text field holds the plugin's own compact syntax — `# header`,
//!   `- to do`, `~ in progress`, `+ done`, `x skipped`, `- [IN QA] custom
//!   status`, `- \[text\] in brackets`, and a note on the line below its item. This is the only one that can be written back, so it is
//!   also the one read first: a checklist that was pushed then reads back as
//!   what was sent rather than as a second rendering of it.
//! * Data Center hands the plugin's Java object back as its `toString()`:
//!   `Checklist(id=793, …, _items=[Item(id=24, value=…, rank=1, status=Status(…
//!   statusState=UNCHECKED, name=TO DO, …), …)])`. Ugly, and read-only, but it
//!   is all an instance that exposes no text field has.
//! * Newer versions may send structured JSON.
//!
//! All three land as [`ChecklistEntry`] values, which is what the `## Checklist`
//! section of the issue file renders. [`to_text`] builds the syntax back out of
//! that section — only the part of it these types model, refusing anything else
//! rather than flattening it, because the field belongs to a plugin whose own
//! reading of it is the one that counts.

use crate::doc::{ChecklistEntry, ChecklistItem};
use serde_json::Value;

/// Status names that say nothing beyond what the checkbox already shows.
const IMPLIED: [(&str, bool); 2] = [("TO DO", false), ("DONE", true)];

/// The spelling of an item's note when there is none to learn it from.
///
/// `> `, which is what every current vendor guide writes for an item's
/// details, Cloud and Data Center alike. `>>` was the default once, read off
/// the recorded fixture, and it is not a note marker: it is a nested quote —
/// the guides spell one `> >` — so the plugin took `>> text` for a detail line
/// whose text began with `> ` and stored it as `> > text`. Every note on a
/// newly created issue came back with a stray `> ` in front of it, and pushing
/// the repaired file sent `>>` again. [`note_marker`] still learns the spelling
/// from the text the server holds, so an instance that does write `>>` keeps
/// getting it back; this is only the fallback.
pub const NOTE: &str = ">";

/// The issue property Smart Checklist keeps a checklist's text in.
///
/// Data Center writes go here rather than to the text field: the field is an
/// ordinary custom field, so `PUT /issue` takes it only where an administrator
/// has put it on the issue type's edit screen, and the plugin does not need it
/// there — on the instance this was reported against it was on none, and every
/// checklist write was refused. The property is what the vendor's own Data
/// Center automation guide writes, and since 5.0, when the plugin moved its
/// primary storage into the database, it is still one of the storages the
/// plugin keeps in sync. Cloud is the other way round: the Forge app no longer
/// reads the property, and the field is the documented route there.
pub const PROPERTY: &str = "com.railsware.SmartChecklist.checklist";

/// Parse whatever the checklist field held into checklist lines.
pub fn from_field(value: &Value) -> Vec<ChecklistEntry> {
    match value {
        Value::String(s) => from_str(s),
        // Data Center wraps the stringified checklist in a single-element array.
        Value::Array(items) => items.iter().flat_map(from_field).collect(),
        Value::Object(_) => from_json(value),
        _ => Vec::new(),
    }
}

fn from_str(s: &str) -> Vec<ChecklistEntry> {
    if is_dump(s) {
        parse_dump(s)
    } else {
        parse_text(s)
    }
}

/// Whether a string is Data Center's `toString()` rather than the plugin's
/// syntax.
///
/// An empty checklist reaches here too — `Checklist(id=871, issueId=50455,
/// _items=[])` — and it has no `Item(` in it at all, so a test for one read the
/// dump as text and turned it into a ticked-looking item in the file of every
/// issue whose checklist was empty. The wrapper is what says "dump", not what
/// it happens to contain.
fn is_dump(s: &str) -> bool {
    let s = s.trim_start();
    (s.starts_with("Checklist(") && s.contains("_items=["))
        || (s.contains("Item(") && s.contains("checklistId="))
}

/// The plugin's own text syntax, which is also what the text field holds.
pub fn parse_text(text: &str) -> Vec<ChecklistEntry> {
    let mut out: Vec<ChecklistEntry> = Vec::new();
    for line in text.lines() {
        let t = line.trim();
        if t.is_empty() {
            continue;
        }
        if let Some(header) = t.strip_prefix("# ") {
            out.push(ChecklistEntry::Header(header.trim().to_string()));
            continue;
        }
        // A note continues the previous item rather than starting a new one.
        // Both spellings are read: which one an instance writes is the plugin's
        // business, and a note taken for an item of its own would be pushed
        // back as one.
        if let Some(note) = t.strip_prefix(">>").or_else(|| t.strip_prefix('>')) {
            if let Some(ChecklistEntry::Item(last)) = out.last_mut() {
                append_description(last, note.trim());
                continue;
            }
        }
        let (checked, status, title) = match split_marker(t) {
            Some((checked, status, title)) => match custom_status(title.trim()) {
                Some((name, title)) => (checked, meaningful_status(name, checked), title),
                None => (checked, status.map(str::to_string), title.trim()),
            },
            // A line with no marker is still an item; dropping it would lose
            // content that a person can plainly see in Jira.
            None => (false, None, t),
        };
        out.push(ChecklistEntry::Item(ChecklistItem {
            checked,
            status,
            title: unescape(title),
            description: None,
        }));
    }
    out
}

/// `- item`, `~ item`, `+ item`, `x item` — marker, then a space.
fn split_marker(line: &str) -> Option<(bool, Option<&'static str>, &str)> {
    let mut chars = line.chars();
    let marker = chars.next()?;
    let rest = chars.as_str();
    if !rest.starts_with(' ') {
        return None;
    }
    let (checked, status) = match marker {
        '-' | '*' => (false, None),
        '~' => (false, Some("IN PROGRESS")),
        '+' => (true, None),
        'x' | 'X' => (true, Some("SKIPPED")),
        _ => return None,
    };
    Some((checked, status, rest))
}

/// `[IN QA] Test the API` — the name of a status the instance configured, in
/// brackets ahead of the item's text, which is how the plugin's syntax spells
/// one. A space after the bracket is part of it: `[docs](https://…)` is a link
/// at the start of the text, not a status called `docs`.
fn custom_status(text: &str) -> Option<(&str, &str)> {
    let rest = text.strip_prefix('[')?;
    let end = rest.find(']')?;
    let (name, after) = (rest[..end].trim(), &rest[end + 1..]);
    let title = after.trim_start();
    (!name.is_empty() && after.starts_with(char::is_whitespace) && !title.is_empty())
        .then_some((name, title))
}

/// Item text as the file shows it: the plugin's escaped brackets, `\[` and
/// `\]`, are only brackets.
fn unescape(text: &str) -> String {
    text.replace("\\[", "[").replace("\\]", "]")
}

/// Item text as the plugin's syntax has to carry it: a leading `[…]` escaped.
///
/// Unescaped, the plugin reads it as the name of a custom status, finds no such
/// status and drops it — `- [03.11 18:00] Check the release` arrived as `Check
/// the release`, the date gone. The escape is the vendor's own documented
/// spelling for text in brackets. Only the leading pair: brackets later in the
/// text cannot be a status, and a leading link keeps working as a link.
fn escaped(title: &str) -> String {
    if let Some(rest) = title.strip_prefix('[') {
        if let Some(end) = rest.find(']') {
            if !rest[end + 1..].starts_with('(') {
                return format!("\\[{}\\]{}", &rest[..end], &rest[end + 1..]);
            }
        }
    }
    title.to_string()
}

/// The plugin's syntax, built back out of the `## Checklist` section.
///
/// `Err` names why a checklist cannot be written: a status the syntax cannot
/// spell, an item with no text. Refusing is the whole point — the field is read
/// by a plugin, and an item silently demoted from its status to another is a
/// change nobody asked for, made to somebody else's data.
pub fn to_text(entries: &[ChecklistEntry], note: &str) -> std::result::Result<String, String> {
    let mut out = String::new();
    for entry in entries {
        match entry {
            ChecklistEntry::Header(h) => match h.trim() {
                "" => return Err("a `### ` group has no name".to_string()),
                name => out.push_str(&format!("# {name}\n")),
            },
            ChecklistEntry::Item(item) => {
                let title = item.title.trim();
                if title.is_empty() {
                    return Err("a checklist item has no text".to_string());
                }
                let Some(marker) = marker_for(item) else {
                    return Err(format!(
                        "`{}` is not a status the checklist syntax can spell",
                        item.status.as_deref().unwrap_or_default()
                    ));
                };
                out.push_str(&format!("{marker} {}\n", escaped(title)));
                for line in item.description.iter().flat_map(|d| d.lines()) {
                    match line.trim() {
                        "" => continue,
                        note_line => out.push_str(&format!("{note} {note_line}\n")),
                    }
                }
            }
        }
    }
    Ok(out)
}

/// The inverse of [`split_marker`] and [`custom_status`]: what spells this
/// item's state ahead of its text.
fn marker_for(item: &ChecklistItem) -> Option<String> {
    let named = |want: &str| {
        item.status
            .as_deref()
            .is_some_and(|s| s.trim().eq_ignore_ascii_case(want))
    };
    let builtin = match (item.checked, item.status.is_none()) {
        (false, true) => Some("-"),
        (true, true) => Some("+"),
        (false, _) if named("TO DO") => Some("-"),
        (false, _) if named("IN PROGRESS") => Some("~"),
        (true, _) if named("DONE") => Some("+"),
        (true, _) if named("SKIPPED") => Some("x"),
        _ => None,
    };
    if let Some(marker) = builtin {
        return Some(marker.to_string());
    }
    // One of the four against the wrong checkbox is not something the plugin
    // could have written, so it is not something jired guesses at.
    if ["TO DO", "IN PROGRESS", "DONE", "SKIPPED"]
        .iter()
        .any(|s| named(s))
    {
        return None;
    }
    // A status the instance configured itself. A name the instance does not
    // have is dropped by the plugin, and push's re-read reports that.
    let name = item.status.as_deref()?.trim();
    if name.is_empty() || name.contains(['[', ']']) {
        return None;
    }
    let marker = if item.checked { "+" } else { "-" };
    Some(format!("{marker} [{name}]"))
}

/// Which note spelling the text a server holds is written in.
pub fn note_marker(stored: &str) -> &'static str {
    for line in stored.lines() {
        let t = line.trim();
        if t.starts_with(">>") {
            return ">>";
        }
        if t.starts_with('>') {
            return ">";
        }
    }
    NOTE
}

/// Two spellings of the same checklist text, reduced to what they both mean.
///
/// The plugin is not consistent about trailing spaces — a live instance stores
/// `+ Hand over to @abercrombie.d \n` — and a blank line in the field is not an
/// entry. Neither survives a render, so neither may decide whether jired's
/// reading of a checklist reproduces the server's own text.
///
/// An escaped bracket past the start of an item's text means the same as a
/// bare one: only a leading `[…]` can be read as a status, so only there does
/// the escape say something.
pub fn normalized(text: &str) -> String {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(canonical_line)
        .collect::<Vec<_>>()
        .join("\n")
}

fn canonical_line(line: &str) -> String {
    // Every marker is one ASCII character followed by a space.
    let (marker, text) = match split_marker(line) {
        Some(_) => line.split_at(2),
        None => ("", line),
    };
    let lead = text
        .strip_prefix("\\[")
        .and_then(|r| r.find("\\]"))
        .map_or(0, |end| end + 4);
    let (lead, rest) = text.split_at(lead);
    format!("{marker}{lead}{}", unescape(rest))
}

/// Data Center's `toString()` dump.
///
/// Parsing a debug rendering is unpleasant, but it is what the REST API returns
/// for this field, and the alternative — showing the dump verbatim in the file —
/// is worse for everyone who has to read it.
fn parse_dump(dump: &str) -> Vec<ChecklistEntry> {
    let mut items: Vec<(i64, ChecklistItem)> = Vec::new();
    for (index, chunk) in dump.split("Item(id=").skip(1).enumerate() {
        let Some(value) = between(chunk, "value=", ", rank=") else {
            continue;
        };
        let rank = between(chunk, ", rank=", ", status=")
            .and_then(|r| r.trim().parse::<i64>().ok())
            .unwrap_or(index as i64);
        let status = between(chunk, "status=Status(", ")");
        let checked = status
            .and_then(|s| between(s, "statusState=", ","))
            .is_some_and(|s| s.trim().eq_ignore_ascii_case("CHECKED"));
        let name = status.and_then(|s| between(s, "name=", ","));
        let description = between(chunk, ", description=", ", weight=")
            .filter(|d| d.trim() != "null")
            .map(|d| clean_description(d.trim()));
        items.push((
            rank,
            ChecklistItem {
                checked,
                status: name.and_then(|n| meaningful_status(n.trim(), checked)),
                title: value.trim().to_string(),
                description,
            },
        ));
    }
    // The server lists items in whatever order the database returned them; rank
    // is what the checklist actually looks like in Jira.
    items.sort_by_key(|(rank, _)| *rank);
    items
        .into_iter()
        .map(|(_, item)| ChecklistEntry::Item(item))
        .collect()
}

/// Structured JSON, should a plugin version send it.
fn from_json(value: &Value) -> Vec<ChecklistEntry> {
    let items = value
        .get("items")
        .or_else(|| value.get("_items"))
        .and_then(Value::as_array);
    let Some(items) = items else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|item| {
            let title = ["value", "text", "name", "title"]
                .iter()
                .find_map(|k| item.get(*k).and_then(Value::as_str))?;
            let status = item.get("status");
            let state = status
                .and_then(|s| s.get("statusState"))
                .and_then(Value::as_str);
            let checked = item
                .get("checked")
                .and_then(Value::as_bool)
                .unwrap_or_else(|| state.is_some_and(|s| s.eq_ignore_ascii_case("CHECKED")));
            let name = status
                .and_then(|s| s.get("name"))
                .and_then(Value::as_str)
                .or_else(|| status.and_then(Value::as_str));
            Some(ChecklistEntry::Item(ChecklistItem {
                checked,
                status: name.and_then(|n| meaningful_status(n.trim(), checked)),
                title: title.trim().to_string(),
                description: item
                    .get("description")
                    .and_then(Value::as_str)
                    .map(clean_description),
            }))
        })
        .collect()
}

/// Keep a status name only when the checkbox does not already say it.
fn meaningful_status(name: &str, checked: bool) -> Option<String> {
    if name.is_empty() {
        return None;
    }
    let implied = IMPLIED
        .iter()
        .any(|(implied, state)| *state == checked && name.eq_ignore_ascii_case(implied));
    (!implied).then(|| name.to_string())
}

fn append_description(item: &mut ChecklistItem, note: &str) {
    match &mut item.description {
        Some(existing) => {
            existing.push('\n');
            existing.push_str(note);
        }
        None => item.description = Some(note.to_string()),
    }
}

/// Blank lines and trailing spaces would not survive a render/parse round trip
/// of the file, so they are removed on the way in rather than compared later.
fn clean_description(text: &str) -> String {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

fn between<'a>(text: &'a str, start: &str, end: &str) -> Option<&'a str> {
    let from = text.find(start)? + start.len();
    let rest = &text[from..];
    let to = rest.find(end)?;
    Some(&rest[..to])
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn item(entry: &ChecklistEntry) -> &ChecklistItem {
        match entry {
            ChecklistEntry::Item(i) => i,
            ChecklistEntry::Header(h) => panic!("expected an item, got header {h}"),
        }
    }

    const DUMP: &str = "Checklist(id=793, issueId=50383, _items=[\
        Item(id=24, checklistId=793, value=Refine the ticket, rank=1, \
        status=Status(id=1, rank=0, statusState=UNCHECKED, name=TO DO, color=GRAY, default=true, \
        global=true, projectIds=[]), quotes=[], assignees=[], \
        history=History(id=48, itemId=24, type=ITEM_CREATED, from=, to=TO DO, user=Ilya A), \
        mandatory=false, description=null, weight=null), \
        Item(id=23, checklistId=793, value=Analyse the request, rank=0, \
        status=Status(id=2, rank=1, statusState=UNCHECKED, name=IN PROGRESS, color=BLUE, \
        default=true, global=true, projectIds=[]), quotes=[], assignees=[], \
        history=History(id=52, itemId=23, type=STATUS_CHANGED, from=TO DO, to=IN PROGRESS), \
        mandatory=false, description=Ask the analyst first, weight=null), \
        Item(id=27, checklistId=793, value=Hand over to @abercrombie.d, rank=2, \
        status=Status(id=3, rank=2, statusState=CHECKED, name=DONE, color=GREEN, default=true, \
        global=true, projectIds=[]), quotes=[], \
        assignees=[Assignee(id=3, userName=abercrombie.d, displayName=abercrombie.d)], \
        history=History(id=53, itemId=27, type=ITEM_UPDATED), \
        mandatory=false, description=null, weight=null)])";

    #[test]
    fn the_data_center_dump_becomes_items_in_rank_order() {
        let entries = from_field(&json!([DUMP]));
        assert_eq!(entries.len(), 3);
        // rank 0 first, whatever order the server listed them in.
        assert_eq!(item(&entries[0]).title, "Analyse the request");
        assert_eq!(item(&entries[1]).title, "Refine the ticket");
        assert_eq!(item(&entries[2]).title, "Hand over to @abercrombie.d");
    }

    #[test]
    fn a_status_is_kept_only_when_the_checkbox_does_not_say_it() {
        let entries = from_field(&json!([DUMP]));
        assert_eq!(item(&entries[0]).status.as_deref(), Some("IN PROGRESS"));
        assert!(!item(&entries[0]).checked);
        // TO DO on an empty box and DONE on a ticked one are noise.
        assert_eq!(item(&entries[1]).status, None);
        assert_eq!(item(&entries[2]).status, None);
        assert!(item(&entries[2]).checked);
    }

    #[test]
    fn an_item_description_survives() {
        let entries = from_field(&json!([DUMP]));
        assert_eq!(
            item(&entries[0]).description.as_deref(),
            Some("Ask the analyst first")
        );
        assert_eq!(item(&entries[1]).description, None);
    }

    #[test]
    fn the_text_syntax_maps_markers_onto_statuses() {
        let entries =
            parse_text("# Analysis\n~ Analyse\n- Refine\n>> ask first\n+ Ship\nx Skip me\n");
        assert_eq!(entries.len(), 5);
        assert_eq!(entries[0], ChecklistEntry::Header("Analysis".into()));
        assert_eq!(item(&entries[1]).status.as_deref(), Some("IN PROGRESS"));
        assert!(!item(&entries[1]).checked);
        assert_eq!(item(&entries[2]).description.as_deref(), Some("ask first"));
        assert!(item(&entries[3]).checked);
        assert_eq!(item(&entries[3]).status, None);
        assert_eq!(item(&entries[4]).status.as_deref(), Some("SKIPPED"));
        assert!(item(&entries[4]).checked);
    }

    /// The plugin writes a single `>`; an older reading of its guide wrote
    /// `>>`, and text in that spelling may still be on a server. A note read as
    /// an item of its own would be pushed back as one.
    #[test]
    fn both_spellings_of_a_note_are_read_as_a_note() {
        for text in ["+ Analyse\n> Ask first\n", "+ Analyse\n>> Ask first\n"] {
            let entries = parse_text(text);
            assert_eq!(entries.len(), 1, "{text}");
            assert_eq!(item(&entries[0]).description.as_deref(), Some("Ask first"));
        }
        assert_eq!(note_marker("+ Analyse\n> Ask first\n"), ">");
        assert_eq!(note_marker("+ Analyse\n>> Ask first\n"), ">>");
        // Nothing to learn from: the plugin's own spelling, since `>>` is a
        // nested quote to it.
        assert_eq!(note_marker("+ Analyse\n"), ">");
    }

    /// An empty checklist arrives as the plugin's dump of an empty list, and
    /// reading that as text put the dump itself in the file — with a checkbox
    /// in front of it — for every issue whose checklist was empty.
    #[test]
    fn an_empty_checklist_is_empty_rather_than_the_dump_as_an_item() {
        let empty = json!(["Checklist(id=871, issueId=50455, _items=[])"]);
        assert!(from_field(&empty).is_empty());
    }

    #[test]
    fn the_syntax_is_written_back_out_of_the_entries() {
        let entries =
            parse_text("# Analysis\n~ Analyse\n>> ask first\n- Refine\n+ Ship\nx Skip me\n");
        assert_eq!(
            to_text(&entries, ">>").unwrap(),
            "# Analysis\n~ Analyse\n>> ask first\n- Refine\n+ Ship\nx Skip me\n"
        );
        // The note is written in whichever spelling the instance uses.
        assert!(to_text(&entries, ">").unwrap().contains("\n> ask first\n"));
    }

    /// A status the instance configured is spelled the plugin's way, in
    /// brackets; one the syntax cannot spell is refused, because the
    /// alternative is demoting somebody's item to one it can — and the plugin,
    /// not jired, is what reads the field back.
    #[test]
    fn a_custom_status_is_written_in_brackets_and_an_unspellable_one_refused() {
        let with = |checked, status: &str| {
            vec![ChecklistEntry::Item(ChecklistItem {
                checked,
                status: Some(status.into()),
                title: "Wait for review".into(),
                description: None,
            })]
        };
        assert_eq!(
            to_text(&with(false, "BLOCKED"), NOTE).unwrap(),
            "- [BLOCKED] Wait for review\n"
        );
        assert_eq!(
            to_text(&with(true, "PASSED"), NOTE).unwrap(),
            "+ [PASSED] Wait for review\n"
        );
        assert!(to_text(&with(false, "A]B"), NOTE)
            .unwrap_err()
            .contains("A]B"));

        let untitled = vec![ChecklistEntry::Item(ChecklistItem {
            checked: false,
            status: None,
            title: "  ".into(),
            description: None,
        })];
        assert!(to_text(&untitled, NOTE).is_err());
    }

    /// The four the reader hides behind the checkbox are still writable: a file
    /// may spell one out, and `TO DO` on an empty box is the same `-`.
    #[test]
    fn a_status_the_checkbox_already_says_is_written_all_the_same() {
        let spelled = |checked, status: &str| {
            vec![ChecklistEntry::Item(ChecklistItem {
                checked,
                status: Some(status.into()),
                title: "Item".into(),
                description: None,
            })]
        };
        assert_eq!(to_text(&spelled(false, "TO DO"), NOTE).unwrap(), "- Item\n");
        assert_eq!(to_text(&spelled(true, "done"), NOTE).unwrap(), "+ Item\n");
        // The right name against the wrong checkbox is not something the plugin
        // could have written, so it is not something jired guesses at.
        assert!(to_text(&spelled(true, "TO DO"), NOTE).is_err());
    }

    /// What `push` checks before it writes: jired's rendering of what the server
    /// holds has to be the server's own text back again.
    #[test]
    fn the_round_trip_survives_the_noise_a_plugin_leaves_in_its_text() {
        let stored = "+ Provide the analysis\n+ Hand over to @abercrombie.d \n\n";
        let entries = parse_text(stored);
        assert_eq!(
            normalized(&to_text(&entries, note_marker(stored)).unwrap()),
            normalized(stored)
        );
    }

    /// Text that starts in brackets — a date, a tag — is the reported case.
    /// Sent bare, the plugin took `[03.11 18:00]` for the name of a status,
    /// found none, and dropped it: the item reached the server as `Check the
    /// release`. Escaped, it is text, and reads back as the file had it.
    #[test]
    fn leading_brackets_in_an_item_are_escaped_and_read_back_bare() {
        let file = vec![ChecklistEntry::Item(ChecklistItem {
            checked: false,
            status: None,
            title: "[03.11 18:00] Check the release".into(),
            description: None,
        })];
        let text = to_text(&file, NOTE).unwrap();
        assert_eq!(text, "- \\[03.11 18:00\\] Check the release\n");
        assert_eq!(parse_text(&text), file);
        assert_eq!(
            normalized(&to_text(&parse_text(&text), NOTE).unwrap()),
            normalized(&text)
        );

        // Brackets later in the text are not a status, and a leading link
        // stays a link.
        for title in ["Check [this] first", "[docs](https://wiki/x) read"] {
            let item = vec![ChecklistEntry::Item(ChecklistItem {
                checked: false,
                status: None,
                title: title.into(),
                description: None,
            })];
            assert_eq!(to_text(&item, NOTE).unwrap(), format!("- {title}\n"));
            assert_eq!(parse_text(&format!("- {title}\n")), item);
        }
    }

    /// Bare brackets ahead of the text are the plugin's own spelling of a
    /// status the instance configured, so they are read as one — and written
    /// back as one, rather than escaped into text the plugin no longer counts.
    #[test]
    fn a_bracketed_name_ahead_of_the_text_is_a_custom_status() {
        let entries = parse_text("- [IN QA] Test the API\n+ [PASSED] Ship it\n");
        assert_eq!(item(&entries[0]).status.as_deref(), Some("IN QA"));
        assert_eq!(item(&entries[0]).title, "Test the API");
        assert!(!item(&entries[0]).checked);
        assert_eq!(item(&entries[1]).status.as_deref(), Some("PASSED"));
        assert!(item(&entries[1]).checked);
        assert_eq!(
            to_text(&entries, NOTE).unwrap(),
            "- [IN QA] Test the API\n+ [PASSED] Ship it\n"
        );
    }

    /// An escape past the start of the text says nothing — only a leading
    /// bracket can be a status — so a server that escapes every bracket still
    /// reproduces. At the start it is the difference between text and status.
    #[test]
    fn only_a_leading_escape_decides_whether_two_texts_match() {
        assert_eq!(
            normalized("- Check \\[this\\] first"),
            normalized("- Check [this] first")
        );
        assert_ne!(
            normalized("- \\[IN QA\\] Test"),
            normalized("- [IN QA] Test")
        );
    }

    /// A line the plugin's syntax does not cover is still content.
    #[test]
    fn an_unmarked_line_is_kept_as_an_item() {
        let entries = parse_text("just a line\n");
        assert_eq!(item(&entries[0]).title, "just a line");
        assert!(!item(&entries[0]).checked);
    }

    #[test]
    fn structured_json_is_read_when_a_server_sends_it() {
        let v = json!({"items": [
            {"value": "Analyse", "status": {"name": "IN PROGRESS", "statusState": "UNCHECKED"}},
            {"value": "Ship", "checked": true, "status": {"name": "DONE"}}
        ]});
        let entries = from_field(&v);
        assert_eq!(item(&entries[0]).status.as_deref(), Some("IN PROGRESS"));
        assert!(item(&entries[1]).checked);
        assert_eq!(item(&entries[1]).status, None);
    }

    #[test]
    fn anything_unrecognizable_yields_nothing_rather_than_guesses() {
        assert!(from_field(&json!(null)).is_empty());
        assert!(from_field(&json!(7)).is_empty());
        assert!(from_field(&json!({"unexpected": true})).is_empty());
        assert!(parse_text("   \n\n").is_empty());
    }
}
