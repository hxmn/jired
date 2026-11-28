//! The Data Center client [M6.4].
//!
//! The calls where Data Center genuinely differs from what [`Rest`], the REST
//! implementation both deployments share, does for Cloud. `Rest` hands each of
//! these over to this file on a Data Center server, so the answer to "what does
//! jired do differently on Data Center" is still the list of functions here —
//! without a wrapper that delegated the other fifty calls a line at a time.
//!
//! What differs, and why:
//!
//! - **`projects`** — Cloud pages `project/search`, which Data Center does not
//!   serve; `project` answers with the whole array at once.
//! - **`board_project`** — the board configuration's `location` is a Cloud
//!   field Data Center omits, so the board's project listing is asked first.
//! - **`worklog_values`** — the description declares no paging on this route
//!   and the server answers with every worklog, so it is one call, not a loop.
//!   Below `worklogs` rather than beside it: completing a search's issues reads
//!   these too, and once took Cloud's paged walk there.
//! - **`search_users`** — `username`, where Cloud takes `query`.
//! - **`filters`** — no `filter/search`; the favourites are the only listing.
//! - **`move_subtask_parent`** — no REST route exists; the Move wizard does it.

use crate::api::client::UserScope;
use crate::api::datacenter::routes;
use crate::api::model::*;
use crate::api::normalize;
use crate::api::rest::{Rest, Source};
use crate::error::Result;
use serde_json::Value;

/// Data Center's user search: `username`, and the assignable route wherever the
/// caller named something to be assigned to.
///
/// Its own function so a test can read the URL without a server.
pub fn user_search_url(prefix: &str, query: &str, scope: UserScope<'_>) -> String {
    match scope {
        UserScope::Any => {
            routes::FIND_USERS.url(prefix, &[], &[("username", query), ("maxResults", "20")])
        }
        UserScope::Issue(key) => routes::FIND_ASSIGNABLE_USERS.url(
            prefix,
            &[],
            &[("username", query), ("issueKey", key), ("maxResults", "20")],
        ),
        UserScope::Project(project) => routes::FIND_ASSIGNABLE_USERS.url(
            prefix,
            &[],
            &[
                ("username", query),
                ("project", project),
                ("maxResults", "20"),
            ],
        ),
    }
}

// ---- What Data Center does differently -----------------------------------
//
// Each is called by [`Rest`] in place of its own on a Data Center server.

/// The whole listing in one array. Cloud's `project/search` does not exist
/// here, and there is nothing to page.
pub(crate) async fn projects(rest: &Rest) -> Result<Vec<Project>> {
    let v = rest.get(&routes::PROJECTS, &[], &[]).await?;
    let mut out: Vec<Project> = crate::api::wire::decode_each::<crate::api::wire::WireProject>(
        v.as_array().map(Vec::as_slice).unwrap_or_default(),
        "projects",
    )
    .into_iter()
    .map(Project::from)
    .collect();
    out.sort_by(|a, b| a.key.cmp(&b.key));
    Ok(out)
}

/// Asked of the board's project listing first.
///
/// `location` in a board's configuration is a Cloud field that Data Center
/// omits entirely, so asking for it here spends a request to learn nothing.
/// The configuration is still the fallback, because which endpoint a given
/// Jira version populates is not something to bet a null on.
pub(crate) async fn board_project(rest: &Rest, board_id: &str) -> Result<Option<String>> {
    if let Some(key) = rest.board_project_from(board_id, Source::Listing).await? {
        return Ok(Some(key));
    }
    rest.board_project_from(board_id, Source::Configuration)
        .await
}

/// Out of the favourites, there being nothing else to ask.
///
/// Cloud's `filter/search` takes an owner and a name; Data Center's
/// description has neither it nor `filter/my`, and `filter/favourite` is
/// the one listing the server gives an account.
pub(crate) async fn filters(rest: &Rest, listing: FilterListing<'_>) -> Result<Vec<Filter>> {
    let v = rest.get(&routes::FAVOURITE_FILTERS, &[], &[]).await?;
    Ok(crate::api::filter::narrow(
        listing,
        crate::api::filter::filters_from_json(&v),
    ))
}

/// One call, no paging.
///
/// The description declares no `startAt` or `maxResults` on this route, and
/// the server answers with every worklog the issue has. Cloud's pages, and
/// [`Rest`] loops for it; sending parameters here that the server never
/// declared would be asking a question in a language it does not speak.
pub(crate) async fn worklog_values(rest: &Rest, key: &str) -> Result<Vec<Value>> {
    let v = rest
        .get(&routes::GET_WORKLOGS, &[("issueIdOrKey", key)], &[])
        .await?;
    Ok(v.get("worklogs")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default())
}

pub(crate) async fn search_users(
    rest: &Rest,
    query: &str,
    scope: UserScope<'_>,
) -> Result<Vec<UserRef>> {
    let v = rest
        .http
        .get_json(&user_search_url(
            rest.prefix(routes::FIND_USERS.module),
            query,
            scope,
        ))
        .await?;
    Ok(v.as_array()
        .unwrap_or(&vec![])
        .iter()
        .filter_map(|u| normalize::user_ref(Some(u)))
        .collect())
}

/// Through the Move wizard, addressed by the issue's numeric id.
///
/// It is a web action, and web actions predate the key ever being accepted.
/// `PUT /issue` takes a sub-task's `parent`, answers 204 and does nothing
/// with it, which is why this is not the field edit Cloud uses.
pub(crate) async fn move_subtask_parent(rest: &Rest, key: &str, parent: &str) -> Result<()> {
    let id = rest.issue_id(key).await?;
    super::wizard::move_subtask_parent(&rest.http, key, &id, parent).await
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The correction the description forced: Data Center declares no paging on
    /// its worklog route, so a request that pages it is asking in a language
    /// the server does not speak.
    #[test]
    fn worklogs_are_asked_for_in_one_call() {
        let url = routes::GET_WORKLOGS.url("/rest/api/2", &[("issueIdOrKey", "DC-77")], &[]);
        assert_eq!(url, "/rest/api/2/issue/DC-77/worklog");
        assert!(!url.contains('?'), "no paging parameters: {url}");
    }

    /// Assignment is a permission, so the users who can be assigned an issue are
    /// fewer than the users who exist. Scoping is the whole point of the call.
    #[test]
    fn assignable_search_is_scoped_by_issue_or_project() {
        assert_eq!(
            user_search_url("/rest/api/2", "ja", UserScope::Any),
            "/rest/api/2/user/search?username=ja&maxResults=20"
        );
        assert_eq!(
            user_search_url("/rest/api/2", "ja", UserScope::Issue("DC-77")),
            "/rest/api/2/user/assignable/search?username=ja&issueKey=DC-77&maxResults=20"
        );
        assert_eq!(
            user_search_url("/rest/api/2", "ja", UserScope::Project("DC")),
            "/rest/api/2/user/assignable/search?username=ja&project=DC&maxResults=20"
        );
        // Cloud asks the same two routes and spells the term differently.
        assert_eq!(
            crate::api::rest::cloud_user_search("/rest/api/3", "ja", UserScope::Issue("PROJ-1")),
            "/rest/api/3/user/assignable/search?query=ja&issueKey=PROJ-1&maxResults=20"
        );
    }

    /// Cloud pages this listing and Data Center does not serve the route that
    /// pages it, so the two read different documents for the same question.
    #[test]
    fn projects_are_read_from_the_unpaged_listing() {
        assert_eq!(
            routes::PROJECTS.url("/rest/api/2", &[], &[]),
            "/rest/api/2/project"
        );
        assert_eq!(
            crate::api::cloud::PROJECT_SEARCH.url(
                "/rest/api/3",
                &[],
                &[("startAt", "0"), ("maxResults", "50")]
            ),
            "/rest/api/3/project/search?startAt=0&maxResults=50"
        );
    }
}
