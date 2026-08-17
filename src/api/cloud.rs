//! Jira Cloud specifics [M1.6].
//!
//! Cloud runs REST v3, stores rich text as ADF, and identifies users by GDPR
//! account ids. The request/response plumbing itself is shared with Data Center
//! in [`crate::api::rest`]; only the facts that differ live here.

use crate::api::http::HttpClient;
use crate::api::rest::Rest;
use serde_json::{json, Value};

pub fn client(http: HttpClient) -> Rest {
    Rest::new(http)
}

/// Assignee/reporter payload for Cloud: `accountId`, never a username.
pub fn user_field_value(user_id: Option<&str>) -> Value {
    match user_id {
        Some(id) => json!({ "accountId": id }),
        None => Value::Null,
    }
}

/// Cloud renders a mention from the account id.
pub fn mention_node(user_id: &str, display_name: &str) -> Value {
    json!({
        "type": "mention",
        "attrs": { "id": user_id, "text": format!("@{display_name}") }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assignee_uses_account_id() {
        assert_eq!(
            user_field_value(Some("5b10a2")),
            json!({"accountId": "5b10a2"})
        );
        assert_eq!(user_field_value(None), Value::Null);
    }
}
