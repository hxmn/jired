//! The `JiraApi` trait and its parts — the seam every deployment difference
//! stops at.

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
    /// What a listing or a count reads — type, parent, sprint, versions,
    /// labels and the rest of the frontmatter's scalar fields — without the
    /// description, comments, worklogs and changelog `*all` drags in.
    Listing,
}

#[derive(Debug, Clone)]
pub struct SearchQuery {
    pub jql: String,
    pub start_at: usize,
    pub max_results: usize,
    pub fields: FieldSet,
    pub expand_changelog: bool,
    /// The page to ask for on Cloud, whose search pages by token: the
    /// `next_page_token` the page before it gave. Data Center pages by
    /// `start_at` and never reads it.
    pub page_token: Option<String>,
}

impl Default for SearchQuery {
    fn default() -> Self {
        Self {
            jql: String::new(),
            start_at: 0,
            max_results: 100,
            fields: FieldSet::Full,
            expand_changelog: false,
            page_token: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct SearchPage {
    pub issues: Vec<RemoteIssue>,
    pub start_at: usize,
    /// How many issues match. Cloud's search does not say, so there it is
    /// the issues up to and including this page, and one more while a page
    /// token says there are more: a lower bound that is exact on the last.
    pub total: usize,
    /// Cloud's search paginates by token rather than offset.
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

/// The server itself: which deployment, where, and what it says about its own
/// REST modules and fields.
#[async_trait]
pub trait Server: Send + Sync {
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

    async fn fields(&self) -> Result<Vec<FieldMeta>>;
}

/// Who is signed in, and finding people to name.
#[async_trait]
pub trait Users: Send + Sync {
    async fn myself(&self) -> Result<Myself>;

    /// Users matching `query`, restricted to those `scope` permits.
    async fn search_users(&self, query: &str, scope: UserScope<'_>) -> Result<Vec<UserRef>>;
}

/// Issues and everything hung on one: fields, transitions, links, worklogs,
/// comments, attachments, watchers.
#[async_trait]
pub trait Issues: Send + Sync {
    async fn search(&self, query: &SearchQuery) -> Result<SearchPage>;

    /// How many issues `jql` matches, in one request. Data Center reports it
    /// beside an empty page; Cloud's search reports no total, and has a count
    /// of its own.
    async fn count(&self, jql: &str) -> Result<usize> {
        let page = self
            .search(&SearchQuery {
                jql: jql.to_string(),
                max_results: 0,
                fields: FieldSet::Summary,
                ..Default::default()
            })
            .await?;
        Ok(page.total)
    }

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

    /// The fields this issue's edit screen carries.
    ///
    /// `None` when the server describes no screen — a version that does not
    /// serve the endpoint, or a credential that may not read it. A caller must
    /// treat that as "unknown" and not as "no fields": refusing a write because
    /// a metadata call was unavailable would be worse than the error it was
    /// trying to pre-empt.
    async fn edit_meta(&self, key: &str) -> Result<Option<ScreenMeta>>;

    /// The fields the create screen carries for a project and issue type, the
    /// type named as a person names it.
    async fn create_meta(&self, project: &str, issue_type: &str) -> Result<Option<ScreenMeta>>;

    /// The link types this instance defines, directions and all.
    async fn link_types(&self) -> Result<Vec<IssueLinkType>>;

    /// Link two issues. Jira's own convention, which reads backwards: the
    /// *inward* issue is the one that bears the type's *outward* description —
    /// `{inward: A, outward: B}` with `Blocks` records "A blocks B".
    async fn create_link(&self, type_name: &str, inward: &str, outward: &str) -> Result<()>;

    /// Delete a link by its own id — the one the issue payload carries on each
    /// `issuelinks` entry, not either issue's.
    async fn delete_link(&self, link_id: &str) -> Result<()>;

    /// An issue's web (remote) links as the server lists them: each with its
    /// own `id`, and an `object` holding `url` and `title`.
    async fn remote_links(&self, key: &str) -> Result<Vec<serde_json::Value>>;

    /// Add a web link to an issue.
    async fn create_remote_link(&self, key: &str, url: &str, title: &str) -> Result<()>;

    /// Remove one of an issue's web links, by the id [`Issues::remote_links`]
    /// gave it.
    async fn delete_remote_link(&self, key: &str, link_id: &str) -> Result<()>;

    /// Write one of an issue's entity properties, replacing whatever it held.
    ///
    /// `value` is the property's whole document — a bare JSON string is a
    /// document too, and is what Smart Checklist keeps its text in. Not a field
    /// edit, so the edit screen has no say in it: that is the reason to use it.
    async fn set_issue_property(
        &self,
        key: &str,
        property: &str,
        value: serde_json::Value,
    ) -> Result<()>;

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

    /// Add a user to an issue's watchers, and take one off again.
    ///
    /// The id is the one the deployment identifies people by — an account id on
    /// Cloud, a username on Data Center — which is exactly what
    /// [`Myself::account_id`] already holds for each.
    async fn watch(&self, key: &str, user_id: &str) -> Result<()>;

    async fn unwatch(&self, key: &str, user_id: &str) -> Result<()>;
}

/// Boards and sprints, the agile API's half of Jira.
#[async_trait]
pub trait Agile: Send + Sync {
    async fn boards(&self, project: Option<&str>) -> Result<Vec<Board>>;

    async fn board_columns(&self, board_id: &str) -> Result<Vec<BoardColumn>>;

    /// The project a board is filed under, asked of the board itself.
    ///
    /// The board listing carries this on Cloud and not on Data Center, where it
    /// is only in the board's own configuration — one request per board, which
    /// is why it is asked for a board at a time rather than folded into
    /// [`Agile::boards`].
    async fn board_project(&self, board_id: &str) -> Result<Option<String>>;

    async fn sprints(&self, board_id: &str) -> Result<Vec<Sprint>>;

    /// Create a sprint on a board. Returns it as the server now holds it.
    async fn create_sprint(&self, input: &SprintInput) -> Result<Sprint>;

    /// One sprint by id, asked of the server rather than of the local cache.
    ///
    /// The cache carries a name and a state and no dates, and the checks a
    /// `sprint start` makes before sending — has it dates, is it already
    /// running — are checks against what the server holds *now*.
    async fn sprint(&self, id: &str) -> Result<Sprint>;

    /// Change a sprint that exists: its name, goal, dates, or state.
    ///
    /// A merge rather than a replacement, so a caller only names what it means
    /// to change. Returns the sprint as the server now holds it.
    async fn update_sprint(&self, id: &str, update: &SprintUpdate) -> Result<Sprint>;

    /// Move issues into a sprint, or out of every sprint when `sprint` is
    /// `None`. The Sprint field itself is read-only on many instances; the
    /// board API is the way that works on both deployments.
    async fn move_to_sprint(&self, sprint: Option<&str>, keys: &[String]) -> Result<()>;
}

/// Projects and their versions.
#[async_trait]
pub trait Projects: Send + Sync {
    async fn projects(&self) -> Result<Vec<Project>>;

    /// One project by key — for its id, which creating a version needs.
    async fn project(&self, key: &str) -> Result<Project>;

    /// Every version a project has, released and archived ones included.
    async fn versions(&self, project: &str) -> Result<Vec<Version>>;

    /// One version by id, as the server holds it now.
    async fn version(&self, id: &str) -> Result<Version>;

    async fn create_version(&self, input: &VersionInput) -> Result<Version>;

    /// Change a version that exists. A merge, like [`Agile::update_sprint`]:
    /// what the update does not name is what the version keeps.
    async fn update_version(&self, id: &str, update: &VersionUpdate) -> Result<Version>;
}

/// Saved filters and who they are shared with.
#[async_trait]
pub trait Filters: Send + Sync {
    /// Saved filters. Data Center answers every listing out of the account's
    /// favourites, having no other; see [`crate::api::filter::narrow`].
    async fn filters(&self, listing: FilterListing<'_>) -> Result<Vec<Filter>>;

    async fn filter(&self, id: &str) -> Result<Filter>;

    /// Send [`crate::api::filter::create`]'s plan; the filter as it now stands.
    async fn create_filter(&self, input: &FilterInput) -> Result<Filter>;

    /// Send [`crate::api::filter::update`]'s plan; the filter as it now stands.
    async fn update_filter(&self, filter: &Filter, update: &FilterUpdate) -> Result<Filter>;

    async fn delete_filter(&self, id: &str) -> Result<()>;

    async fn add_filter_share(&self, id: &str, share: &ShareInput) -> Result<()>;

    async fn delete_filter_share(&self, id: &str, permission_id: &str) -> Result<()>;
}

/// All of Jira, as the engine holds it.
///
/// Split into the parts above so a function that needs one can say so, and a
/// double written for a test covers only the part its subject uses; anything
/// that implements every part is a `JiraApi`, with nothing more to write.
pub trait JiraApi: Server + Users + Issues + Agile + Projects + Filters {}

impl<T: Server + Users + Issues + Agile + Projects + Filters + ?Sized> JiraApi for T {}

/// Walk every page of a search, respecting both pagination styles.
///
/// It ends when the server says there is no more — no page token, and as
/// many issues as its `total` — and never on a short page alone: a server
/// that clamps `maxResults` below what was asked answers every page short,
/// and stopping at the first one read as a project of a hundred issues
/// fewer, which `fetch` then marked deleted. A walk that runs out before the
/// total is an error rather than a smaller answer, for the same reason.
pub async fn search_all(
    api: &dyn Issues,
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
        match next_page(count, all.len(), page.total, page.next_page_token, &token) {
            Page::Next(next) => {
                query.page_token = next.clone();
                token = next;
                query.start_at += count;
            }
            Page::Done => return Ok(all),
            Page::Short => {
                return Err(crate::error::JiredError::network(format!(
                    "the search stopped after {} of the {} issues it matched",
                    all.len(),
                    page.total
                ))
                .with_detail("jql", query.jql.clone())
                .with_hint("run the command again; if it keeps stopping, `-vv` shows the pages"));
            }
        }
    }
}

#[derive(Debug, PartialEq)]
pub(crate) enum Page {
    /// Ask again, with this token if the server gave one.
    Next(Option<String>),
    Done,
    /// The server has no more to give and has not given what it counted.
    Short,
}

/// What follows a page of `count` issues, with `seen` read so far.
pub(crate) fn next_page(
    count: usize,
    seen: usize,
    total: usize,
    next_token: Option<String>,
    last_token: &Option<String>,
) -> Page {
    match next_token {
        // A token the server already gave is a walk that is not advancing.
        Some(next) if Some(&next) == last_token.as_ref() => Page::Short,
        Some(_) if count == 0 => Page::Short,
        Some(next) => Page::Next(Some(next)),
        None if seen >= total => Page::Done,
        None if count == 0 => Page::Short,
        None => Page::Next(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A server that clamps the page size answers short pages until the
    /// total is reached; each one is a reason to ask again, not to stop.
    #[test]
    fn a_short_page_is_not_the_last_one() {
        assert_eq!(next_page(50, 50, 120, None, &None), Page::Next(None));
        assert_eq!(next_page(20, 120, 120, None, &None), Page::Done);
        assert_eq!(next_page(0, 0, 0, None, &None), Page::Done);
    }

    /// Running out before the total, or a token that does not move, is a
    /// walk that did not cover the search.
    #[test]
    fn a_walk_that_runs_out_early_is_short() {
        assert_eq!(next_page(0, 50, 120, None, &None), Page::Short);
        let t = Some("t1".to_string());
        assert_eq!(
            next_page(10, 10, 0, t.clone(), &None),
            Page::Next(t.clone())
        );
        assert_eq!(next_page(10, 20, 0, t.clone(), &t), Page::Short);
        assert_eq!(next_page(0, 20, 0, Some("t2".into()), &t), Page::Short);
    }
}
