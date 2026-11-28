//! Responses as Jira sends them, read into structs at the edge.
//!
//! Each listing was read by a hand-written walk of `Value::get` calls, and a
//! `filter_map` over the page dropped any entry the walk could not read: a
//! board or a version that came back in a shape nobody expected simply was
//! not there, with nothing said. Here the shape is a struct, an entry that
//! does not fit it is reported with the reason, and what the rest of jired is
//! handed is the model type it always was.
//!
//! Strict where the old walk was strict and lenient where it was lenient: an
//! entry without its id or its name is not one jired can use, but a field it
//! only shows — a goal, a project type — that is missing or of an odd type is
//! read as absent, as it always was, rather than costing the whole entry.

use crate::api::model::{parse_timestamp, Board, Project, Sprint, SprintState, Version};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer};
use serde_json::Value;

/// Read every entry of a page, reporting the ones that do not fit.
///
/// `what` names the listing in the warning, so a reader can tell which call
/// it was. The entry is identified by its id or key alone: its other fields
/// are whatever the server put there, and need not reach a log.
pub fn decode_each<T: DeserializeOwned>(values: &[Value], what: &str) -> Vec<T> {
    values
        .iter()
        .filter_map(|v| match T::deserialize(v) {
            Ok(t) => Some(t),
            Err(e) => {
                tracing::warn!(
                    "{what}: left out {}, which came back in a shape jired cannot read ({e})",
                    identify(v)
                );
                None
            }
        })
        .collect()
}

/// Read one entry, with the reason when it does not fit.
pub fn decode<T: DeserializeOwned>(v: &Value) -> Result<T, String> {
    T::deserialize(v).map_err(|e| e.to_string())
}

fn identify(v: &Value) -> String {
    for name in ["key", "id"] {
        if let Some(id) = v.get(name).and_then(lenient_id_of) {
            return format!("{name} {id}");
        }
    }
    "an entry".to_string()
}

/// An id as Jira writes it: a number on some routes, a string on others.
fn id<'de, D: Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    let v = Value::deserialize(d)?;
    lenient_id_of(&v)
        .ok_or_else(|| serde::de::Error::custom("an id that is neither a number nor a string"))
}

fn lenient_id_of(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

/// An optional id: anything but a number or a string reads as none.
fn lenient_id<'de, D: Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    Ok(lenient_id_of(&Value::deserialize(d)?))
}

/// An optional string: anything else reads as none.
fn lenient_str<'de, D: Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    Ok(match Value::deserialize(d)? {
        Value::String(s) => Some(s),
        _ => None,
    })
}

/// An optional flag: anything but `true` or `false` reads as none.
fn lenient_bool<'de, D: Deserializer<'de>>(d: D) -> Result<Option<bool>, D::Error> {
    Ok(Value::deserialize(d)?.as_bool())
}

fn non_empty(s: Option<String>) -> Option<String> {
    s.filter(|t| !t.is_empty())
}

/// An agile board, from `board`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WireBoard {
    #[serde(deserialize_with = "id")]
    id: String,
    name: String,
    #[serde(rename = "type", default, deserialize_with = "lenient_str")]
    board_type: Option<String>,
    #[serde(default)]
    location: Option<Value>,
}

impl From<WireBoard> for Board {
    fn from(b: WireBoard) -> Self {
        Board {
            id: b.id,
            name: b.name,
            board_type: b.board_type.unwrap_or_default(),
            project_key: b
                .location
                .as_ref()
                .and_then(|l| l.get("projectKey"))
                .and_then(Value::as_str)
                .map(str::to_string),
        }
    }
}

/// A sprint, from a board's sprint listing or the sprint routes.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WireSprint {
    #[serde(deserialize_with = "id")]
    id: String,
    name: String,
    #[serde(default, deserialize_with = "lenient_str")]
    state: Option<String>,
    #[serde(default, deserialize_with = "lenient_id")]
    origin_board_id: Option<String>,
    #[serde(default, deserialize_with = "lenient_str")]
    start_date: Option<String>,
    #[serde(default, deserialize_with = "lenient_str")]
    end_date: Option<String>,
    #[serde(default, deserialize_with = "lenient_str")]
    goal: Option<String>,
}

impl WireSprint {
    /// The sprint, on `board` when the payload does not say which board it
    /// came from; an empty `board` is no board at all.
    pub fn into_sprint(self, board: &str) -> Sprint {
        Sprint {
            id: self.id,
            name: self.name,
            state: SprintState::parse(self.state.as_deref().unwrap_or("")),
            board_id: self
                .origin_board_id
                .or_else(|| Some(board.to_string()))
                .filter(|b| !b.is_empty()),
            start_date: self.start_date.as_deref().and_then(parse_timestamp),
            end_date: self.end_date.as_deref().and_then(parse_timestamp),
            goal: non_empty(self.goal),
        }
    }
}

/// A project version.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WireVersion {
    #[serde(deserialize_with = "id")]
    id: String,
    name: String,
    #[serde(default, deserialize_with = "lenient_id")]
    project_id: Option<String>,
    #[serde(default, deserialize_with = "lenient_str")]
    description: Option<String>,
    #[serde(default, deserialize_with = "lenient_bool")]
    released: Option<bool>,
    #[serde(default, deserialize_with = "lenient_bool")]
    archived: Option<bool>,
    #[serde(default, deserialize_with = "lenient_str")]
    start_date: Option<String>,
    #[serde(default, deserialize_with = "lenient_str")]
    release_date: Option<String>,
}

impl From<WireVersion> for Version {
    fn from(v: WireVersion) -> Self {
        let day = |d: Option<String>| {
            non_empty(d)
                .as_deref()
                .and_then(crate::api::model::parse_day)
        };
        Version {
            id: v.id,
            name: v.name,
            project_id: v.project_id,
            description: non_empty(v.description),
            released: v.released.unwrap_or(false),
            archived: v.archived.unwrap_or(false),
            start_date: day(v.start_date),
            release_date: day(v.release_date),
        }
    }
}

/// A project, from either deployment's project listing or `project/{key}`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WireProject {
    #[serde(deserialize_with = "id")]
    id: String,
    key: String,
    #[serde(default, deserialize_with = "lenient_str")]
    name: Option<String>,
    #[serde(default, deserialize_with = "lenient_str")]
    project_type_key: Option<String>,
}

impl From<WireProject> for Project {
    fn from(p: WireProject) -> Self {
        Project {
            id: p.id,
            key: p.key,
            name: p.name.unwrap_or_default(),
            project_type: p.project_type_key,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The point of the module: one bad entry costs that entry, and says so,
    /// rather than vanishing; the rest of the page is read as before.
    #[test]
    fn an_entry_that_does_not_fit_is_left_out_and_the_rest_read() {
        let page = [
            json!({"id": 1, "name": "Board one", "type": "scrum"}),
            json!({"id": 2}),
            json!({"id": "3", "name": "Board three", "type": null}),
        ];
        let boards: Vec<WireBoard> = decode_each(&page, "boards");
        let boards: Vec<Board> = boards.into_iter().map(Board::from).collect();
        assert_eq!(boards.len(), 2);
        assert_eq!(boards[0].id, "1");
        assert_eq!(boards[1].board_type, "");
    }

    #[test]
    fn a_field_only_shown_is_read_as_absent_when_its_type_is_odd() {
        let v: WireVersion = decode(&json!({
            "id": 7, "name": "1.0", "released": "yes", "description": 3,
            "releaseDate": "2026-09-14", "projectId": 10000
        }))
        .unwrap();
        let v = Version::from(v);
        assert!(!v.released);
        assert_eq!(v.description, None);
        assert_eq!(v.project_id.as_deref(), Some("10000"));
        assert!(v.release_date.is_some());
    }

    #[test]
    fn the_reason_an_entry_does_not_fit_is_given() {
        let e = decode::<WireProject>(&json!({"id": 1})).unwrap_err();
        assert!(e.contains("key"), "{e}");
    }

    #[test]
    fn a_sprint_falls_back_to_the_board_it_was_listed_on() {
        let s: WireSprint =
            decode(&json!({"id": 5, "name": "Sprint 5", "state": "ACTIVE"})).unwrap();
        let s = s.into_sprint("12");
        assert_eq!(s.board_id.as_deref(), Some("12"));
        assert_eq!(s.state, SprintState::Active);
    }
}
