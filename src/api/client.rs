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

#[async_trait]
pub trait JiraApi: Send + Sync {
    fn deployment(&self) -> Deployment;
    fn base_url(&self) -> &str;

    async fn server_info(&self) -> Result<ServerInfo>;
    async fn myself(&self) -> Result<Myself>;
    async fn projects(&self) -> Result<Vec<Project>>;
    async fn fields(&self) -> Result<Vec<FieldMeta>>;

    async fn boards(&self, project: Option<&str>) -> Result<Vec<Board>>;
    async fn board_columns(&self, board_id: &str) -> Result<Vec<BoardColumn>>;
    async fn sprints(&self, board_id: &str) -> Result<Vec<Sprint>>;

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

    async fn search_users(&self, query: &str) -> Result<Vec<UserRef>>;
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
