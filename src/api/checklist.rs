//! Reading a Smart Checklist custom field [M5.4].
//!
//! "Smart Checklist for Jira" keeps its data in custom fields whose ids differ
//! per instance, so they are found by display name — `Checklist`, `Checklist
//! Text`, `Checklist Progress` — the same way Story Points and Sprint are.
//!
//! What arrives there is not one shape but three, and which one a server sends
//! depends on the product and the plugin version:
//!
//! * Data Center hands back the plugin's Java object as its `toString()`:
//!   `Checklist(id=793, …, _items=[Item(id=24, value=…, rank=1, status=Status(…
//!   statusState=UNCHECKED, name=TO DO, …), …)])`. Ugly, but it is the only
//!   form that carries per-item status names and rank.
//! * The companion text field holds the plugin's own compact syntax —
//!   `# header`, `- to do`, `~ in progress`, `+ done`, `x skipped`, `>>` for an
//!   item's description.
//! * Newer versions may send structured JSON.
//!
//! All three land as [`ChecklistEntry`] values, which is what the `## Checklist`
//! section of the issue file renders. The rendering is read-only (decision D12
//! extends to it): jired does not know how to write a checklist back, and
//! guessing at a plugin's write format would corrupt real data.

use crate::doc::{ChecklistEntry, ChecklistItem};
use serde_json::Value;

/// Status names that say nothing beyond what the checkbox already shows.
const IMPLIED: [(&str, bool); 2] = [("TO DO", false), ("DONE", true)];

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
    if s.contains("Item(") && s.contains("checklistId=") {
        parse_dump(s)
    } else {
        parse_text(s)
    }
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
        // `>>` continues the previous item rather than starting a new one.
        if let Some(note) = t.strip_prefix(">>") {
            if let Some(ChecklistEntry::Item(last)) = out.last_mut() {
                append_description(last, note.trim());
                continue;
            }
        }
        let (checked, status, title) = match split_marker(t) {
            Some(parsed) => parsed,
            // A line with no marker is still an item; dropping it would lose
            // content that a person can plainly see in Jira.
            None => (false, None, t),
        };
        out.push(ChecklistEntry::Item(ChecklistItem {
            checked,
            status: status.map(str::to_string),
            title: title.trim().to_string(),
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
