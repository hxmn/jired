//! Jira Cloud specifics [M1.6].
//!
//! Cloud runs REST v3, stores rich text as ADF, and identifies users by GDPR
//! account ids. The request/response plumbing itself is shared with Data Center
//! in [`crate::api::rest`]; only the facts that differ live here.

use crate::api::client::SpecModule;
use crate::api::datacenter::routes::{Module, Route};
use crate::api::http::HttpClient;
use crate::api::rest::Rest;
use serde_json::{json, Value};

pub fn client(http: HttpClient) -> Rest {
    Rest::new(http)
}

// The routes Cloud has and Data Center does not.
//
// They are `Route`s so that Cloud's paths are built the same way as everything
// else, and they are here rather than in the shared table because the contract
// test would look for them in a Data Center description and not find them.
// Nothing checks these: Cloud publishes no description a test could read — see
// `PUBLISHED_OPENAPI`, a document Atlassian hosts rather than one the site
// serves.

/// Cloud paginates its project listing; Data Center answers `project` with the
/// whole array.
pub const PROJECT_SEARCH: Route = Route {
    id: "searchProjects",
    method: "GET",
    module: Module::Api,
    path: "project/search",
    query: &["startAt", "maxResults"],
};

/// Cloud's worklog endpoint pages, where Data Center's declares no paging and
/// answers with every worklog at once.
pub const GET_WORKLOGS: Route = Route {
    id: "getIssueWorklog",
    method: "GET",
    module: Module::Api,
    path: "issue/{issueIdOrKey}/worklog",
    query: &["startAt", "maxResults"],
};

/// Cloud searches users by `query`, where Data Center takes a `username`.
pub const FIND_USERS: Route = Route {
    id: "findUsers",
    method: "GET",
    module: Module::Api,
    path: "user/search",
    query: &["query", "maxResults"],
};

/// The same difference on the assignable search: `query`, not `username`.
pub const FIND_ASSIGNABLE_USERS: Route = Route {
    id: "findAssignableUsers",
    method: "GET",
    module: Module::Api,
    path: "user/assignable/search",
    query: &["query", "project", "issueKey", "maxResults"],
};

/// Where Atlassian publishes the Cloud OpenAPI document.
///
/// Named here so the error a Cloud user gets can point at it, not fetched: it
/// lives on `developer.atlassian.com`, and the credential this client carries
/// belongs to the Jira site alone.
pub const PUBLISHED_OPENAPI: &str =
    "https://developer.atlassian.com/cloud/jira/platform/swagger-v3.v3.json";

/// A Cloud site describes none of its REST modules.
///
/// Jira Cloud does not serve a WADL — asking for one is a 404 — and the OpenAPI
/// document that covers it is published by Atlassian rather than by the site,
/// so there is nothing here to fetch from the server itself.
pub fn spec_modules() -> Vec<SpecModule> {
    Vec::new()
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
