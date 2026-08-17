//! Jira Data Center specifics [M1.6].
//!
//! Data Center runs REST v2, stores rich text as wiki markup, and identifies
//! users by username. Shares the transport with Cloud via [`crate::api::rest`].

use crate::api::http::HttpClient;
use crate::api::rest::Rest;
use serde_json::{json, Value};

pub fn client(http: HttpClient) -> Rest {
    Rest::new(http)
}

/// Assignee/reporter payload for Data Center: `name`, and an explicit null
/// clears the field.
pub fn user_field_value(user_id: Option<&str>) -> Value {
    match user_id {
        Some(id) => json!({ "name": id }),
        None => Value::Null,
    }
}

/// Wiki-markup mention syntax.
pub fn mention_markup(user_id: &str) -> String {
    format!("[~{user_id}]")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assignee_uses_username() {
        assert_eq!(user_field_value(Some("ilya")), json!({"name": "ilya"}));
    }

    #[test]
    fn mentions_use_wiki_syntax() {
        assert_eq!(mention_markup("ilya"), "[~ilya]");
    }
}
