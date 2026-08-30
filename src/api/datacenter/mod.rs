//! Jira Data Center specifics [M1.6, M6.2].
//!
//! Data Center runs REST v2, stores rich text as wiki markup, and identifies
//! users by username.
//!
//! What each file here is for: [`routes`] is the table of every REST route
//! jired calls, named by the id the server's own API description gives it;
//! [`client`] is the [`crate::api::JiraApi`] implementation built on it; and
//! [`wizard`] drives the one operation Data Center offers as a web action
//! rather than a route.

pub mod wizard;

use crate::api::client::SpecModule;
use crate::api::http::HttpClient;
use crate::api::rest::Rest;
use serde_json::{json, Value};

pub fn client(http: HttpClient) -> Rest {
    Rest::new(http)
}

/// What Jersey answers a WADL request with, and what it refuses.
///
/// The WADL resource produces XML only; the `application/json` on every other
/// jired request comes back a 406, so this `Accept` is not a preference but the
/// condition of getting an answer at all.
const WADL_ACCEPT: &str = "application/vnd.sun.wadl+xml, application/xml;q=0.9";

/// The REST modules a Data Center instance describes about itself.
///
/// Atlassian publishes no OpenAPI document for Data Center — the request for one
/// has been open for years — but every Jira mounts its REST modules on Jersey,
/// which serves each one's WADL beside it. These are the three jired speaks: the
/// platform API, the Agile API that the board and sprint commands live on, and
/// the session endpoints. Each is fetched separately because a Jira that has
/// stopped serving one still answers for the others.
pub fn spec_modules() -> Vec<SpecModule> {
    ["api/2", "agile/1.0", "auth/1"]
        .into_iter()
        .map(|module| SpecModule {
            name: module.replace('/', "-"),
            path: format!("/rest/{module}/application.wadl"),
            accept: WADL_ACCEPT.to_string(),
            extension: "wadl".to_string(),
        })
        .collect()
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

    /// The paths are the contract with the server, and a slug is what names the
    /// file on disk, so both are spelled out rather than merely derived.
    #[test]
    fn spec_modules_name_the_wadl_endpoints() {
        let modules = spec_modules();
        let named: Vec<(&str, &str)> = modules
            .iter()
            .map(|m| (m.name.as_str(), m.path.as_str()))
            .collect();
        assert_eq!(
            named,
            [
                ("api-2", "/rest/api/2/application.wadl"),
                ("agile-1.0", "/rest/agile/1.0/application.wadl"),
                ("auth-1", "/rest/auth/1/application.wadl"),
            ]
        );
        assert!(modules.iter().all(|m| m.extension == "wadl"));
        // Asking for JSON here is a 406, so the Accept must never inherit the
        // client-wide default.
        assert!(modules.iter().all(|m| !m.accept.contains("json")));
    }
}
