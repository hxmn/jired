//! The Data Center client [M6.4].
//!
//! Wraps [`Rest`], which is the REST implementation both deployments share, and
//! overrides the calls where Data Center genuinely differs. Everything else is
//! delegated a line at a time — verbose, and worth it: the alternative was four
//! `match self.deployment` arms buried inside otherwise shared methods, where a
//! reader looking for "what does jired do differently on Data Center" had to
//! find them one by one. Here the answer is the list of methods in this file.
//!
//! What differs, and why:
//!
//! - **`projects`** — Cloud pages `project/search`, which Data Center does not
//!   serve; `project` answers with the whole array at once.
//! - **`board_project`** — the board configuration's `location` is a Cloud
//!   field Data Center omits, so the board's project listing is asked first.
//! - **`worklogs`** — the description declares no paging on this route and the
//!   server answers with every worklog, so it is one call, not a loop.
//! - **`search_users`** — `username`, where Cloud takes `query`.
//! - **`move_subtask_parent`** — no REST route exists; the Move wizard does it.

use crate::api::client::{ApiSpec, JiraApi, SearchPage, SearchQuery, SpecModule, UserScope};
use crate::api::datacenter::routes;
use crate::api::http::HttpClient;
use crate::api::model::*;
use crate::api::normalize;
use crate::api::rest::{Rest, Source};
use crate::api::Deployment;
use crate::error::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::Value;

pub struct DataCenter {
    rest: Rest,
}

impl DataCenter {
    pub fn new(http: HttpClient) -> Self {
        Self {
            rest: Rest::new(http),
        }
    }
}

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

#[async_trait]
impl JiraApi for DataCenter {
    // ---- What Data Center does differently -------------------------------

    /// The whole listing in one array. Cloud's `project/search` does not exist
    /// here, and there is nothing to page.
    async fn projects(&self) -> Result<Vec<Project>> {
        let v = self.rest.get(&routes::PROJECTS, &[], &[]).await?;
        let mut out: Vec<Project> = v
            .as_array()
            .unwrap_or(&vec![])
            .iter()
            .filter_map(crate::api::rest::project_from)
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
    async fn board_project(&self, board_id: &str) -> Result<Option<String>> {
        if let Some(key) = self
            .rest
            .board_project_from(board_id, Source::Listing)
            .await?
        {
            return Ok(Some(key));
        }
        self.rest
            .board_project_from(board_id, Source::Configuration)
            .await
    }

    /// One call, no paging.
    ///
    /// The description declares no `startAt` or `maxResults` on this route, and
    /// the server answers with every worklog the issue has. Cloud's pages, and
    /// [`Rest`] loops for it; sending parameters here that the server never
    /// declared would be asking a question in a language it does not speak.
    async fn worklogs(&self, key: &str) -> Result<Vec<RemoteWorklog>> {
        let v = self
            .rest
            .get(&routes::GET_WORKLOGS, &[("issueIdOrKey", key)], &[])
            .await?;
        let (worklogs, _) = normalize::worklogs_from(Some(&v), self.rest.markup());
        Ok(worklogs)
    }

    async fn search_users(&self, query: &str, scope: UserScope<'_>) -> Result<Vec<UserRef>> {
        let v = self
            .rest
            .http
            .get_json(&user_search_url(
                self.rest.prefix(routes::FIND_USERS.module),
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
    async fn move_subtask_parent(&self, key: &str, parent: &str) -> Result<()> {
        let id = self.rest.issue_id(key).await?;
        super::wizard::move_subtask_parent(&self.rest.http, key, &id, parent).await
    }

    // ---- Everything else, as both deployments do it ----------------------

    fn deployment(&self) -> Deployment {
        self.rest.deployment()
    }

    fn base_url(&self) -> &str {
        self.rest.base_url()
    }

    async fn server_info(&self) -> Result<ServerInfo> {
        self.rest.server_info().await
    }

    fn spec_modules(&self) -> Vec<SpecModule> {
        self.rest.spec_modules()
    }

    async fn api_spec(&self, module: &SpecModule) -> Result<Option<ApiSpec>> {
        self.rest.api_spec(module).await
    }

    async fn myself(&self) -> Result<Myself> {
        self.rest.myself().await
    }

    async fn fields(&self) -> Result<Vec<FieldMeta>> {
        self.rest.fields().await
    }

    async fn boards(&self, project: Option<&str>) -> Result<Vec<Board>> {
        self.rest.boards(project).await
    }

    async fn board_columns(&self, board_id: &str) -> Result<Vec<BoardColumn>> {
        self.rest.board_columns(board_id).await
    }

    async fn sprints(&self, board_id: &str) -> Result<Vec<Sprint>> {
        self.rest.sprints(board_id).await
    }

    async fn create_sprint(&self, input: &SprintInput) -> Result<Sprint> {
        self.rest.create_sprint(input).await
    }

    async fn sprint(&self, id: &str) -> Result<Sprint> {
        self.rest.sprint(id).await
    }

    async fn update_sprint(&self, id: &str, update: &SprintUpdate) -> Result<Sprint> {
        self.rest.update_sprint(id, update).await
    }

    async fn project(&self, key: &str) -> Result<Project> {
        self.rest.project(key).await
    }

    async fn versions(&self, project: &str) -> Result<Vec<Version>> {
        self.rest.versions(project).await
    }

    async fn version(&self, id: &str) -> Result<Version> {
        self.rest.version(id).await
    }

    async fn create_version(&self, input: &VersionInput) -> Result<Version> {
        self.rest.create_version(input).await
    }

    async fn update_version(&self, id: &str, update: &VersionUpdate) -> Result<Version> {
        self.rest.update_version(id, update).await
    }

    async fn search(&self, query: &SearchQuery) -> Result<SearchPage> {
        self.rest.search(query).await
    }

    async fn issue(&self, key: &str) -> Result<RemoteIssue> {
        self.rest.issue(key).await
    }

    async fn changelog(&self, key: &str) -> Result<Vec<ChangelogEntry>> {
        self.rest.changelog(key).await
    }

    async fn transitions(&self, key: &str) -> Result<Vec<Transition>> {
        self.rest.transitions(key).await
    }

    async fn issue_updated(&self, key: &str) -> Result<DateTime<Utc>> {
        self.rest.issue_updated(key).await
    }

    async fn update_fields(&self, key: &str, fields: Value) -> Result<()> {
        self.rest.update_fields(key, fields).await
    }

    async fn create_issue(&self, fields: Value) -> Result<(String, String)> {
        self.rest.create_issue(fields).await
    }

    async fn do_transition(
        &self,
        key: &str,
        transition_id: &str,
        fields: Option<Value>,
    ) -> Result<()> {
        self.rest.do_transition(key, transition_id, fields).await
    }

    async fn move_to_sprint(&self, sprint: Option<&str>, keys: &[String]) -> Result<()> {
        self.rest.move_to_sprint(sprint, keys).await
    }

    async fn add_worklog(&self, key: &str, worklog: &WorklogInput) -> Result<RemoteWorklog> {
        self.rest.add_worklog(key, worklog).await
    }

    async fn update_worklog(
        &self,
        key: &str,
        id: &str,
        worklog: &WorklogInput,
    ) -> Result<RemoteWorklog> {
        self.rest.update_worklog(key, id, worklog).await
    }

    async fn delete_worklog(&self, key: &str, id: &str) -> Result<()> {
        self.rest.delete_worklog(key, id).await
    }

    async fn edit_meta(&self, key: &str) -> Result<Option<ScreenMeta>> {
        self.rest.edit_meta(key).await
    }

    async fn create_meta(&self, project: &str, issue_type: &str) -> Result<Option<ScreenMeta>> {
        self.rest.create_meta(project, issue_type).await
    }

    async fn link_types(&self) -> Result<Vec<IssueLinkType>> {
        self.rest.link_types().await
    }

    async fn create_link(&self, type_name: &str, inward: &str, outward: &str) -> Result<()> {
        self.rest.create_link(type_name, inward, outward).await
    }

    async fn delete_link(&self, link_id: &str) -> Result<()> {
        self.rest.delete_link(link_id).await
    }

    async fn set_issue_property(&self, key: &str, property: &str, value: Value) -> Result<()> {
        self.rest.set_issue_property(key, property, value).await
    }

    async fn assign(&self, key: &str, user_id: Option<&str>) -> Result<()> {
        self.rest.assign(key, user_id).await
    }

    async fn comments(&self, key: &str) -> Result<Vec<RemoteComment>> {
        self.rest.comments(key).await
    }

    async fn add_comment(&self, key: &str, body: &RichText) -> Result<RemoteComment> {
        self.rest.add_comment(key, body).await
    }

    async fn update_comment(&self, key: &str, id: &str, body: &RichText) -> Result<RemoteComment> {
        self.rest.update_comment(key, id, body).await
    }

    async fn delete_comment(&self, key: &str, id: &str) -> Result<()> {
        self.rest.delete_comment(key, id).await
    }

    async fn upload_attachment(
        &self,
        key: &str,
        filename: &str,
        bytes: Vec<u8>,
    ) -> Result<RemoteAttachment> {
        self.rest.upload_attachment(key, filename, bytes).await
    }

    async fn download_attachment(&self, attachment: &RemoteAttachment) -> Result<Vec<u8>> {
        self.rest.download_attachment(attachment).await
    }

    /// Both watcher calls are shared: [`Rest`] already picks the right query
    /// parameter for the deployment it was built for.
    async fn watch(&self, key: &str, user_id: &str) -> Result<()> {
        self.rest.watch(key, user_id).await
    }

    async fn unwatch(&self, key: &str, user_id: &str) -> Result<()> {
        self.rest.unwatch(key, user_id).await
    }
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
