//! Refusing a field the screen does not carry [M6.9].
//!
//! Jira's field configuration is per project, per issue type and per screen, so
//! an issue can have a priority while the screen you edit it through offers no
//! way to set one. A `PUT` carrying that field is a 400 whose message names a
//! screen, arrives after the push has already sent everything else, and leaves
//! the reader to work out which of their edits it was about.
//!
//! The server will say the same thing if asked first, and asking first turns it
//! into a refusal at plan time: `push --dry-run` shows it, the field is named as
//! a person names it, the other issues in the push carry on, and the edit stays
//! in the file.
//!
//! Deliberately narrow. Only the fields that travel in a `PUT`'s payload are
//! checked — an assignee has its own route and its own permission, a transition
//! has its own screen, a sprint is a board call — and a screen the server will
//! not describe refuses nothing at all. The point is to pre-empt an error that
//! was going to happen, never to invent one.

use crate::api::ScreenMeta;
use crate::error::JiredError;

/// Which screen a check is about, for the message.
#[derive(Debug, Clone, Copy)]
pub enum Screen {
    Edit,
    Create,
}

impl Screen {
    fn as_str(self) -> &'static str {
        match self {
            Screen::Edit => "edit",
            Screen::Create => "create",
        }
    }
}

/// The fields in `sent` that `meta` does not carry, as `(id, display name)`.
///
/// The display name comes from the workspace's field map rather than the
/// screen, because the screen is precisely the thing that does not mention this
/// field. Falling back to the id is better than saying nothing.
pub fn refused_by_screen<'a>(
    meta: &ScreenMeta,
    sent: impl Iterator<Item = &'a str>,
    names: &std::collections::BTreeMap<String, String>,
) -> Vec<(String, String)> {
    sent.filter(|id| !meta.carries(id))
        .map(|id| {
            let name = names.get(id).cloned().unwrap_or_else(|| id.to_string());
            (id.to_string(), name)
        })
        .collect()
}

/// The refusal itself: a usage error, like the other things `plan` will not send.
pub fn refusal(key: &str, screen: Screen, refused: &[(String, String)]) -> JiredError {
    let listed = refused
        .iter()
        .map(|(_, name)| name.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let plural = if refused.len() == 1 { "is" } else { "are" };

    JiredError::usage(format!(
        "{key}: {listed} {plural} not on the {} screen for this issue type",
        screen.as_str()
    ))
    .with_detail("issue", key.to_string())
    .with_detail(
        "fields",
        serde_json::json!(refused.iter().map(|(id, _)| id).collect::<Vec<_>>()),
    )
    .with_detail(
        "names",
        serde_json::json!(refused.iter().map(|(_, name)| name).collect::<Vec<_>>()),
    )
    .with_detail("screen", screen.as_str())
    .with_hint(
        "change it in Jira, or ask an administrator to add the field to the \
         screen; the edit is still in the file",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::{FieldSchema, ScreenMeta};
    use std::collections::BTreeMap;

    fn screen(ids: &[&str]) -> ScreenMeta {
        let mut meta = ScreenMeta::default();
        for id in ids {
            meta.fields.insert(
                (*id).to_string(),
                FieldSchema {
                    id: (*id).to_string(),
                    name: id.to_uppercase(),
                    required: false,
                    operations: vec!["set".into()],
                },
            );
        }
        meta
    }

    fn names() -> BTreeMap<String, String> {
        BTreeMap::from([
            ("priority".to_string(), "Priority".to_string()),
            ("customfield_10300".to_string(), "Change Window".to_string()),
        ])
    }

    #[test]
    fn a_payload_field_missing_from_the_screen_is_named() {
        let meta = screen(&["summary", "description"]);
        let refused = refused_by_screen(
            &meta,
            ["summary", "priority", "customfield_10300"].into_iter(),
            &names(),
        );
        assert_eq!(
            refused,
            vec![
                ("priority".to_string(), "Priority".to_string()),
                ("customfield_10300".to_string(), "Change Window".to_string()),
            ]
        );

        // A custom field is an id nobody reads, so the message has to carry the
        // name the person sees in Jira.
        let e = refusal("DC-77", Screen::Edit, &refused);
        assert!(e.message.contains("Priority, Change Window"), "{e}");
        assert!(e.message.contains("edit screen"), "{e}");
        assert_eq!(e.exit_code().as_i32(), 2);
        assert_eq!(e.details["fields"][1], "customfield_10300");
        assert_eq!(e.details["screen"], "edit");
    }

    /// A field nothing knows a name for still gets reported, by id: the reader
    /// can search for that, and silence is not an improvement.
    #[test]
    fn an_unnamed_field_is_reported_by_id() {
        let refused = refused_by_screen(
            &screen(&["summary"]),
            ["customfield_99999"].into_iter(),
            &BTreeMap::new(),
        );
        assert_eq!(refused[0].1, "customfield_99999");
        let e = refusal("DC-77", Screen::Create, &refused);
        assert!(e.message.contains("customfield_99999 is not"), "{e}");
        assert!(e.message.contains("create screen"), "{e}");
    }

    /// The rule that keeps this from inventing failures: a screen nobody
    /// described refuses nothing. An empty `ScreenMeta` is a different thing
    /// from an absent one, and the caller is what tells them apart — this only
    /// has to be safe when handed the empty one it can see.
    #[test]
    fn a_screen_nobody_described_refuses_nothing() {
        assert!(refused_by_screen(
            &screen(&["summary", "priority"]),
            ["summary", "priority"].into_iter(),
            &names(),
        )
        .is_empty());
    }
}
