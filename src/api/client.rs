//! The `JiraApi` trait — the seam every deployment difference stops at.

use crate::api::model::*;
use crate::api::Deployment;
use crate::error::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};

/// How much of each issue to request. Fetching only what a command needs keeps
/// large projects within rate limits and the state database small.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldSet {
    /// Key, summary, status, assignee — enough for listings.
    Summary,
    /// Everything jired models, including description and comments.
    Full,
}

#[derive(Debug, Clone)]
pub struct SearchQuery {
    pub jql: String,
    pub start_at: usize,
    pub max_results: usize,
    pub fields: FieldSet,
    pub expand_changelog: bool,
}

impl Default for SearchQuery {
    fn default() -> Self {
        Self {
            jql: String::new(),
            start_at: 0,
            max_results: 100,
            fields: FieldSet::Full,
            expand_changelog: false,
        }
    }
}

#[derive(Debug, Clone)]
pub struct SearchPage {
    pub issues: Vec<RemoteIssue>,
    pub start_at: usize,
    pub total: usize,
    /// Cloud's newer search endpoint paginates by token rather than offset.
    pub next_page_token: Option<String>,
}

impl SearchPage {
    pub fn is_last(&self) -> bool {
        self.next_page_token.is_none() && self.start_at + self.issues.len() >= self.total
    }
}

/// Who a user search may answer with.
///
/// Jira has two user searches and they are not interchangeable: one finds every
/// user on the instance, the other finds the ones who can actually be assigned
/// a given issue — which is a smaller set, because assignment is a permission
/// held per project and per issue-security level. Asking the wrong one is how
/// `assign` came to offer names the server would then refuse.
#[derive(Debug, Clone, Copy)]
pub enum UserScope<'a> {
    /// Everyone the caller may see. For naming a person, not for assigning one.
    Any,
    /// Those who can be assigned this issue.
    Issue(&'a str),
    /// Those who can be assigned anything in this project — the answer when
    /// there is no issue yet, as when creating one.
    Project(&'a str),
}

/// One REST module a server publishes a machine-readable description of.
///
/// There is no single document describing a Jira: Data Center runs on Jersey,
/// which serves a WADL beside each mounted REST module, so the platform API and
/// the Agile API are two separate fetches. Everything the fetch needs travels
/// with the module so the transport stays deployment-agnostic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpecModule {
    /// Slug naming the module — `api-2` — which the written file is named for.
    pub name: String,
    /// Where the description is served, relative to the base URL.
    pub path: String,
    /// What to ask for. Jersey negotiates its WADL as XML and refuses the
    /// `application/json` every other jired request sends.
    pub accept: String,
    /// Suffix for the file the description is written to.
    pub extension: String,
}

/// A module's description, exactly as the server sent it.
#[derive(Debug, Clone)]
pub struct ApiSpec {
    pub module: SpecModule,
    /// What the server said it sent, so a login page can be told from a spec.
    pub media_type: String,
    pub bytes: Vec<u8>,
}

#[async_trait]
pub trait JiraApi: Send + Sync {
    fn deployment(&self) -> Deployment;
    fn base_url(&self) -> &str;

    async fn server_info(&self) -> Result<ServerInfo>;

    /// The REST modules this server describes about itself.
    ///
    /// Empty when it describes none, which is the whole answer for Cloud: an
    /// Atlassian-hosted site serves no WADL, and the OpenAPI document that
    /// covers it is published by Atlassian rather than by the site.
    fn spec_modules(&self) -> Vec<SpecModule>;

    /// Fetch one module's description. `None` when this server does not serve
    /// it — a module the Jira version never mounted, or dropped.
    async fn api_spec(&self, module: &SpecModule) -> Result<Option<ApiSpec>>;

    async fn myself(&self) -> Result<Myself>;
    async fn projects(&self) -> Result<Vec<Project>>;
    async fn fields(&self) -> Result<Vec<FieldMeta>>;

    async fn boards(&self, project: Option<&str>) -> Result<Vec<Board>>;
    async fn board_columns(&self, board_id: &str) -> Result<Vec<BoardColumn>>;
    /// The project a board is filed under, asked of the board itself.
    ///
    /// The board listing carries this on Cloud and not on Data Center, where it
    /// is only in the board's own configuration — one request per board, which
    /// is why it is asked for a board at a time rather than folded into
    /// [`JiraApi::boards`].
    async fn board_project(&self, board_id: &str) -> Result<Option<String>>;
    async fn sprints(&self, board_id: &str) -> Result<Vec<Sprint>>;
    /// Create a sprint on a board. Returns it as the server now holds it.
    async fn create_sprint(&self, input: &SprintInput) -> Result<Sprint>;

    async fn search(&self, query: &SearchQuery) -> Result<SearchPage>;
    async fn issue(&self, key: &str) -> Result<RemoteIssue>;
    async fn changelog(&self, key: &str) -> Result<Vec<ChangelogEntry>>;
    async fn transitions(&self, key: &str) -> Result<Vec<Transition>>;

    /// The server's current `updated` value — the optimistic-concurrency check
    /// that immediately precedes every write (decision D8).
    async fn issue_updated(&self, key: &str) -> Result<DateTime<Utc>>;

    async fn update_fields(&self, key: &str, fields: serde_json::Value) -> Result<()>;
    /// Returns `(key, id)` of the created issue.
    async fn create_issue(&self, fields: serde_json::Value) -> Result<(String, String)>;
    async fn do_transition(
        &self,
        key: &str,
        transition_id: &str,
        fields: Option<serde_json::Value>,
    ) -> Result<()>;

    /// Move issues into a sprint, or out of every sprint when `sprint` is
    /// `None`. The Sprint field itself is read-only on many instances; the
    /// board API is the way that works on both deployments.
    async fn move_to_sprint(&self, sprint: Option<&str>, keys: &[String]) -> Result<()>;

    /// Put a sub-task under a different parent.
    ///
    /// Its own call because the two deployments do not share one. Cloud takes
    /// `parent` as an ordinary field edit; Data Center accepts that payload,
    /// answers 204 and ignores it, and offers the operation only as the web
    /// action behind the Move dialog. Both ends of that difference are hidden
    /// here, which is what lets `push` plan one operation for both.
    async fn move_subtask_parent(&self, key: &str, parent: &str) -> Result<()>;

    /// Every worklog on an issue, for the case the issue payload paged them.
    async fn worklogs(&self, key: &str) -> Result<Vec<RemoteWorklog>>;
    async fn add_worklog(&self, key: &str, worklog: &WorklogInput) -> Result<RemoteWorklog>;
    async fn update_worklog(
        &self,
        key: &str,
        id: &str,
        worklog: &WorklogInput,
    ) -> Result<RemoteWorklog>;
    async fn delete_worklog(&self, key: &str, id: &str) -> Result<()>;

    /// Set or clear an issue's assignee.
    ///
    /// Its own route rather than a field edit. Jira offers `PUT
    /// /issue/{key}/assignee` precisely because assigning is not editing: it is
    /// its own permission, and an assignee missing from the edit screen — which
    /// is a normal configuration — is still assignable through this. A field
    /// update carrying `assignee` fails on those instances for a reason that
    /// names a screen rather than a person.
    async fn assign(&self, key: &str, user_id: Option<&str>) -> Result<()>;

    /// Every comment on an issue, for the case the issue payload paged them.
    async fn comments(&self, key: &str) -> Result<Vec<RemoteComment>>;

    async fn add_comment(&self, key: &str, body: &RichText) -> Result<RemoteComment>;
    async fn update_comment(&self, key: &str, id: &str, body: &RichText) -> Result<RemoteComment>;
    async fn delete_comment(&self, key: &str, id: &str) -> Result<()>;

    async fn upload_attachment(
        &self,
        key: &str,
        filename: &str,
        bytes: Vec<u8>,
    ) -> Result<RemoteAttachment>;
    async fn download_attachment(&self, attachment: &RemoteAttachment) -> Result<Vec<u8>>;

    /// Users matching `query`, restricted to those `scope` permits.
    async fn search_users(&self, query: &str, scope: UserScope<'_>) -> Result<Vec<UserRef>>;
}

/// Walk every page of a search, respecting both pagination styles.
pub async fn search_all(
    api: &dyn JiraApi,
    mut query: SearchQuery,
    mut on_page: impl FnMut(&SearchPage),
) -> Result<Vec<RemoteIssue>> {
    let mut all = Vec::new();
    let mut token: Option<String> = None;
    loop {
        let page = api.search(&query).await?;
        on_page(&page);
        let count = page.issues.len();
        all.extend(page.issues);
        if count == 0 || page.next_page_token.is_none() && all.len() >= page.total {
            break;
        }
        match &page.next_page_token {
            Some(next) if Some(next) == token.as_ref() => break, // server not advancing
            Some(next) => {
                token = Some(next.clone());
                query.start_at += count;
            }
            None => query.start_at += count,
        }
        if count < query.max_results && page.next_page_token.is_none() {
            break;
        }
    }
    Ok(all)
}
