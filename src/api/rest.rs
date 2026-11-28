//! The REST implementation both deployments are built from.
//!
//! Cloud and Data Center differ in API version, rich-text format, user
//! identifiers and a handful of routes. The first three are data — the API
//! prefix, [`Markup`], the user payload shape — and stay here, parameterized,
//! so a fix lands on both products at once. The routes are not, and they live
//! in [`crate::api::datacenter::client()`], which wraps this and overrides them:
//! this type is what a Cloud site gets, and the base a Data Center one is
//! built on.
//!
//! Every path is rendered from [`crate::api::datacenter::routes`], the table
//! the Data Center description is checked against. Cloud shares it wherever the
//! two agree, which is everywhere but the three routes in
//! [`crate::api::cloud`].

use crate::api::client::{
    Agile, ApiSpec, FieldSet, Filters, Issues, Projects, SearchPage, SearchQuery, Server,
    SpecModule, UserScope, Users,
};
use crate::api::datacenter::routes::{self, Module, Route};
use crate::api::http::HttpClient;
use crate::api::model::*;
use crate::api::normalize::{self, FieldMap};
use crate::api::{filter, Deployment, Markup};
use crate::error::{JiredError, Result};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::{json, Value};
use tokio::sync::OnceCell;

pub struct Rest {
    pub(crate) http: HttpClient,
    deployment: Deployment,
    /// `/field` is instance-wide and stable within a run.
    field_map: OnceCell<FieldMap>,
}

/// Where a board's project can be read from. The two Jira deployments populate
/// one each; see [`Agile::board_project`].
#[derive(Debug, Clone, Copy)]
pub(crate) enum Source {
    /// `GET /board/{id}/configuration` → `location.projectKey` (Cloud).
    Configuration,
    /// `GET /board/{id}/project` → the projects the board's filter covers.
    Listing,
}

impl Rest {
    /// The project key one of the two endpoints gives for a board, if it does.
    pub(crate) async fn board_project_from(
        &self,
        board_id: &str,
        source: Source,
    ) -> Result<Option<String>> {
        match source {
            Source::Configuration => {
                let v = self
                    .get(&routes::BOARD_CONFIGURATION, &[("boardId", board_id)], &[])
                    .await?;
                // Cloud spells it `projectKey` in the board listing and `key`
                // in the configuration.
                Ok(v.get("location")
                    .and_then(|l| {
                        l.get("projectKey")
                            .or_else(|| l.get("key"))
                            .and_then(Value::as_str)
                    })
                    .map(str::to_string)
                    .filter(|k| !k.is_empty()))
            }
            Source::Listing => {
                let v = self
                    .get(
                        &routes::BOARD_PROJECTS,
                        &[("boardId", board_id)],
                        &[("maxResults", "50")],
                    )
                    .await?;
                // A board is a filter, and a filter can span projects: one is
                // an answer, several is genuinely not one, and naming the first
                // would be a guess a script would then act on.
                Ok(single_project(&v))
            }
        }
    }

    /// The numeric id behind an issue key.
    ///
    /// Asked for the cheapest field there is: what is wanted is beside it, in
    /// every payload, whatever was requested.
    pub(crate) async fn issue_id(&self, key: &str) -> Result<String> {
        let v = self
            .get(
                &routes::GET_ISSUE,
                &[("issueIdOrKey", key)],
                &[("fields", "summary")],
            )
            .await?;
        v.get("id")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| JiredError::generic(format!("{key}: the server reported no issue id")))
    }

    pub fn new(http: HttpClient) -> Self {
        let deployment = http.deployment();
        Self {
            http,
            deployment,
            field_map: OnceCell::new(),
        }
    }

    fn api(&self) -> &'static str {
        self.deployment.api_prefix()
    }

    /// Where a module is mounted on this deployment. The platform API is the
    /// one that moved between versions; the Agile API never did.
    pub(crate) fn prefix(&self, module: Module) -> &'static str {
        match module {
            Module::Api => self.api(),
            Module::Agile => "/rest/agile/1.0",
        }
    }

    /// The path for one route on this deployment.
    pub(crate) fn url(
        &self,
        route: &Route,
        args: &[(&str, &str)],
        query: &[(&str, &str)],
    ) -> String {
        route.url(self.prefix(route.module), args, query)
    }

    /// Send one planned call, exactly as a dry run printed it.
    pub(crate) async fn send(&self, call: &filter::Call) -> Result<Value> {
        let method = reqwest::Method::from_bytes(call.method.as_bytes())
            .expect("planned calls use the methods routes declare");
        let body = (!call.body.is_null()).then(|| call.body.clone());
        self.http.request_json(method, &call.path, body).await
    }

    /// `GET` one route.
    pub(crate) async fn get(
        &self,
        route: &Route,
        args: &[(&str, &str)],
        query: &[(&str, &str)],
    ) -> Result<Value> {
        self.http.get_json(&self.url(route, args, query)).await
    }

    pub(crate) fn markup(&self) -> Markup {
        self.deployment.markup()
    }

    pub(crate) async fn field_map(&self) -> Result<&FieldMap> {
        self.field_map
            .get_or_try_init(|| async {
                let fields = self.fields().await?;
                Ok(FieldMap::from_meta(&fields))
            })
            .await
    }

    fn field_param(set: FieldSet, map: &FieldMap) -> String {
        match set {
            FieldSet::Summary => "key,summary,status,assignee,updated,issuetype".to_string(),
            FieldSet::Full => "*all".to_string(),
            // The sprint, the epic link and the story points are custom fields
            // whose ids differ per instance, so they come from the field map;
            // one this instance does not have is simply not asked for.
            FieldSet::Listing => {
                let mut fields: Vec<&str> = vec![
                    "key",
                    "summary",
                    "status",
                    "assignee",
                    "updated",
                    "created",
                    "issuetype",
                    "parent",
                    "labels",
                    "fixVersions",
                    "versions",
                    "priority",
                    "components",
                    "duedate",
                ];
                fields.extend(
                    [&map.sprint, &map.epic_link, &map.story_points]
                        .into_iter()
                        .flatten()
                        .map(String::as_str),
                );
                fields.join(",")
            }
        }
    }

    /// Every comment on an issue as the server sent it, paged.
    ///
    /// Raw values rather than parsed ones because the completed page has to go
    /// back into [`RemoteIssue::raw`] — see [`crate::api::set_comments`].
    pub(crate) async fn comment_values(&self, key: &str) -> Result<Vec<Value>> {
        let mut out: Vec<Value> = Vec::new();
        let mut start = 0usize;
        loop {
            let v = self
                .get(
                    &routes::GET_COMMENTS,
                    &[("issueIdOrKey", key)],
                    &[("startAt", &start.to_string()), ("maxResults", "100")],
                )
                .await?;
            let page = v
                .get("comments")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let total = v.get("total").and_then(Value::as_u64).unwrap_or(0) as usize;
            let n = page.len();
            out.extend(page);
            if n == 0 || out.len() >= total {
                break;
            }
            start += n;
        }
        Ok(out)
    }

    /// Every worklog on an issue as the server sent it.
    pub(crate) async fn worklog_values(&self, key: &str) -> Result<Vec<Value>> {
        if self.deployment == Deployment::DataCenter {
            return crate::api::datacenter::client::worklog_values(self, key).await;
        }
        let mut out: Vec<Value> = Vec::new();
        let mut start = 0usize;
        loop {
            let v = self
                .http
                .get_json(&crate::api::cloud::GET_WORKLOGS.url(
                    self.api(),
                    &[("issueIdOrKey", key)],
                    &[("startAt", &start.to_string()), ("maxResults", "100")],
                ))
                .await?;
            let page = v
                .get("worklogs")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let total = v.get("total").and_then(Value::as_u64).unwrap_or(0) as usize;
            let n = page.len();
            out.extend(page);
            if n == 0 || out.len() >= total {
                break;
            }
            start += n;
        }
        Ok(out)
    }

    /// Ask for the pages an issue payload left out.
    ///
    /// A listing that cannot be read is warned about rather than fatal: the
    /// first page is still better than a failed fetch.
    async fn complete(&self, issue: &mut RemoteIssue) {
        let markup = self.markup();
        if crate::api::comments_are_paged(issue) {
            match self.comment_values(&issue.key).await {
                Ok(all) => crate::api::set_comments(issue, all, markup),
                Err(e) => tracing::warn!(
                    "{}: cannot read the rest of the comments: {}",
                    issue.key,
                    e.message
                ),
            }
        }
        if crate::api::worklogs_are_paged(issue) {
            match self.worklog_values(&issue.key).await {
                Ok(all) => crate::api::set_worklogs(issue, all, markup),
                Err(e) => tracing::warn!(
                    "{}: cannot read the rest of the worklogs: {}",
                    issue.key,
                    e.message
                ),
            }
        }
    }
}

#[async_trait]
impl Server for Rest {
    fn deployment(&self) -> Deployment {
        self.deployment
    }

    fn base_url(&self) -> &str {
        self.http.base_url()
    }

    async fn server_info(&self) -> Result<ServerInfo> {
        let v = self.get(&routes::SERVER_INFO, &[], &[]).await?;
        // `deploymentType` is the server's own word for what it is; trust it
        // over any URL heuristic.
        let deployment = v
            .get("deploymentType")
            .and_then(Value::as_str)
            .and_then(|d| match d.to_ascii_lowercase().as_str() {
                "cloud" => Some(Deployment::Cloud),
                "server" | "datacenter" | "data center" => Some(Deployment::DataCenter),
                _ => None,
            });
        Ok(ServerInfo {
            deployment,
            version: v.get("version").and_then(Value::as_str).map(str::to_string),
            title: v
                .get("serverTitle")
                .and_then(Value::as_str)
                .map(str::to_string),
        })
    }

    fn spec_modules(&self) -> Vec<SpecModule> {
        crate::api::spec_modules(self.deployment)
    }

    async fn api_spec(&self, module: &SpecModule) -> Result<Option<ApiSpec>> {
        let doc = self.http.get_document(&module.path, &module.accept).await?;
        Ok(doc.map(|doc| ApiSpec {
            module: module.clone(),
            media_type: doc.media_type,
            bytes: doc.bytes,
        }))
    }

    async fn fields(&self) -> Result<Vec<FieldMeta>> {
        let v = self.get(&routes::FIELDS, &[], &[]).await?;
        Ok(v.as_array()
            .unwrap_or(&vec![])
            .iter()
            .filter_map(|f| {
                Some(FieldMeta {
                    id: f.get("id")?.as_str()?.to_string(),
                    name: f
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                    custom: f.get("custom").and_then(Value::as_bool).unwrap_or(false),
                    schema_type: f
                        .get("schema")
                        .and_then(|s| s.get("type"))
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    items: f
                        .get("schema")
                        .and_then(|s| s.get("items"))
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    custom_type: f
                        .get("schema")
                        .and_then(|s| s.get("custom"))
                        .and_then(Value::as_str)
                        .map(str::to_string),
                })
            })
            .collect())
    }
}

#[async_trait]
impl Users for Rest {
    async fn myself(&self) -> Result<Myself> {
        let v = self.get(&routes::MYSELF, &[], &[]).await?;
        let account_id = ["accountId", "name", "key"]
            .iter()
            .find_map(|k| v.get(*k).and_then(Value::as_str))
            .ok_or_else(|| JiredError::auth("the server returned no account identity"))?
            .to_string();
        Ok(Myself {
            account_id,
            display_name: v
                .get("displayName")
                .and_then(Value::as_str)
                .map(str::to_string),
            email: v
                .get("emailAddress")
                .and_then(Value::as_str)
                .map(str::to_string),
            time_zone: v
                .get("timeZone")
                .and_then(Value::as_str)
                .map(str::to_string),
        })
    }

    async fn search_users(&self, query: &str, scope: UserScope<'_>) -> Result<Vec<UserRef>> {
        if self.deployment == Deployment::DataCenter {
            return crate::api::datacenter::client::search_users(self, query, scope).await;
        }
        let v = self
            .http
            .get_json(&cloud_user_search(self.api(), query, scope))
            .await?;
        Ok(v.as_array()
            .unwrap_or(&vec![])
            .iter()
            .filter_map(|u| normalize::user_ref(Some(u)))
            .collect())
    }
}

#[async_trait]
impl Issues for Rest {
    async fn count(&self, jql: &str) -> Result<usize> {
        if self.deployment != Deployment::Cloud {
            let page = self
                .search(&SearchQuery {
                    jql: jql.to_string(),
                    max_results: 0,
                    fields: FieldSet::Summary,
                    ..Default::default()
                })
                .await?;
            return Ok(page.total);
        }
        let v = self
            .http
            .post_json_repeatable(
                &self.url(&crate::api::cloud::APPROXIMATE_COUNT, &[], &[]),
                json!({ "jql": jql }),
            )
            .await?;
        v.get("count")
            .and_then(Value::as_u64)
            .map(|n| n as usize)
            .ok_or_else(|| JiredError::generic("the issue count came back without a `count`"))
    }

    async fn search(&self, query: &SearchQuery) -> Result<SearchPage> {
        let map = self.field_map().await?.clone();
        let markup = self.markup();
        let cloud = self.deployment == Deployment::Cloud;
        let mut body = json!({
            "jql": query.jql,
            "maxResults": query.max_results,
            "fields": Self::field_param(query.fields, &map)
                .split(',')
                .collect::<Vec<_>>(),
        });
        // Cloud's search takes `expand` as one string, pages by token, and
        // has no offset; Data Center's takes a list and an offset.
        if cloud {
            if query.expand_changelog {
                body["expand"] = json!("changelog");
            }
            if let Some(token) = &query.page_token {
                body["nextPageToken"] = json!(token);
            }
        } else {
            if query.expand_changelog {
                body["expand"] = json!(["changelog"]);
            }
            body["startAt"] = json!(query.start_at);
        }
        let route = if cloud {
            &crate::api::cloud::SEARCH_JQL
        } else {
            &routes::SEARCH
        };

        let v = self
            .http
            .post_json_repeatable(&self.url(route, &[], &[]), body)
            .await?;

        let mut issues = v
            .get("issues")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .map(|i| normalize::issue_from_json(i, markup, &map))
                    .collect::<Result<Vec<_>>>()
            })
            .transpose()?
            .unwrap_or_default();
        // A page's issues one after another was a round trip each, at four a
        // second on Data Center; a few at a time, under the same limiter.
        {
            use futures::StreamExt;
            let pending: Vec<std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + '_>>> =
                issues
                    .iter_mut()
                    // Boxed, so the futures have one type and the borrow of
                    // each issue is not a closure's to generalise.
                    .map(|issue| Box::pin(self.complete(issue)) as _)
                    .collect();
            futures::stream::iter(pending)
                .buffer_unordered(crate::api::CONCURRENCY)
                .collect::<Vec<()>>()
                .await;
        }

        let next_page_token = v
            .get("nextPageToken")
            .and_then(Value::as_str)
            .map(str::to_string)
            // `isLast` is the word; a token beside it means nothing more.
            .filter(|_| !v.get("isLast").and_then(Value::as_bool).unwrap_or(false));
        let start_at = match v.get("startAt").and_then(Value::as_u64) {
            Some(n) => n as usize,
            None => query.start_at,
        };
        let total = match v.get("total").and_then(Value::as_u64) {
            Some(n) => n as usize,
            None => start_at + issues.len() + usize::from(next_page_token.is_some()),
        };
        Ok(SearchPage {
            start_at,
            total,
            next_page_token,
            issues,
        })
    }

    async fn issue(&self, key: &str) -> Result<RemoteIssue> {
        let map = self.field_map().await?.clone();
        let v = self
            .get(
                &routes::GET_ISSUE,
                &[("issueIdOrKey", key)],
                &[("fields", "*all")],
            )
            .await?;
        let mut issue = normalize::issue_from_json(&v, self.markup(), &map)?;
        self.complete(&mut issue).await;
        Ok(issue)
    }

    async fn issue_updated(&self, key: &str) -> Result<DateTime<Utc>> {
        let v = self
            .get(
                &routes::GET_ISSUE,
                &[("issueIdOrKey", key)],
                &[("fields", "updated")],
            )
            .await?;
        v.get("fields")
            .and_then(|f| f.get("updated"))
            .and_then(Value::as_str)
            .and_then(parse_timestamp)
            .ok_or_else(|| {
                JiredError::generic(format!("{key}: the server returned no `updated` timestamp"))
            })
    }

    async fn changelog(&self, key: &str) -> Result<Vec<ChangelogEntry>> {
        let v = self
            .get(
                &routes::GET_ISSUE,
                &[("issueIdOrKey", key)],
                &[("expand", "changelog"), ("fields", "key")],
            )
            .await?;
        Ok(v.get("changelog")
            .and_then(|c| c.get("histories"))
            .and_then(Value::as_array)
            .map(|h| {
                h.iter()
                    .filter_map(|e| {
                        Some(ChangelogEntry {
                            id: e.get("id")?.as_str()?.to_string(),
                            author: e
                                .get("author")
                                .and_then(|a| a.get("displayName"))
                                .and_then(Value::as_str)
                                .map(str::to_string),
                            created: e
                                .get("created")
                                .and_then(Value::as_str)
                                .and_then(parse_timestamp),
                            items: e
                                .get("items")
                                .and_then(Value::as_array)
                                .map(|items| {
                                    items
                                        .iter()
                                        .map(|i| ChangelogItem {
                                            field: i
                                                .get("field")
                                                .and_then(Value::as_str)
                                                .unwrap_or("")
                                                .to_string(),
                                            from: i
                                                .get("fromString")
                                                .and_then(Value::as_str)
                                                .map(str::to_string),
                                            to: i
                                                .get("toString")
                                                .and_then(Value::as_str)
                                                .map(str::to_string),
                                        })
                                        .collect()
                                })
                                .unwrap_or_default(),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default())
    }

    async fn transitions(&self, key: &str) -> Result<Vec<Transition>> {
        let v = self
            .get(
                &routes::TRANSITIONS,
                &[("issueIdOrKey", key)],
                &[("expand", "transitions.fields")],
            )
            .await?;
        Ok(v.get("transitions")
            .and_then(Value::as_array)
            .map(|ts| {
                ts.iter()
                    .filter_map(|t| {
                        let to = t.get("to");
                        Some(Transition {
                            id: t.get("id")?.as_str()?.to_string(),
                            name: t
                                .get("name")
                                .and_then(Value::as_str)
                                .unwrap_or("")
                                .to_string(),
                            to_status: to
                                .and_then(|s| s.get("name"))
                                .and_then(Value::as_str)
                                .unwrap_or("")
                                .to_string(),
                            to_status_id: to.and_then(|s| s.get("id")).and_then(id_string_ref),
                            required_fields: t
                                .get("fields")
                                .and_then(Value::as_object)
                                .map(|f| {
                                    f.iter()
                                        .filter(|(_, v)| {
                                            v.get("required")
                                                .and_then(Value::as_bool)
                                                .unwrap_or(false)
                                        })
                                        .map(|(k, _)| k.clone())
                                        .collect()
                                })
                                .unwrap_or_default(),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default())
    }

    async fn update_fields(&self, key: &str, fields: Value) -> Result<()> {
        self.http
            .put_json(
                &self.url(&routes::EDIT_ISSUE, &[("issueIdOrKey", key)], &[]),
                json!({ "fields": fields }),
            )
            .await?;
        Ok(())
    }

    async fn create_issue(&self, fields: Value) -> Result<(String, String)> {
        let v = self
            .http
            .post_json(
                &self.url(&routes::CREATE_ISSUE, &[], &[]),
                json!({ "fields": fields }),
            )
            .await?;
        let key = v
            .get("key")
            .and_then(Value::as_str)
            .ok_or_else(|| JiredError::generic("issue creation returned no key"))?
            .to_string();
        let id = v
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        Ok((key, id))
    }

    async fn do_transition(
        &self,
        key: &str,
        transition_id: &str,
        fields: Option<Value>,
    ) -> Result<()> {
        let mut body = json!({ "transition": { "id": transition_id } });
        if let Some(f) = fields {
            body["fields"] = f;
        }
        self.http
            .post_json(
                &self.url(&routes::DO_TRANSITION, &[("issueIdOrKey", key)], &[]),
                body,
            )
            .await?;
        Ok(())
    }

    async fn move_subtask_parent(&self, key: &str, parent: &str) -> Result<()> {
        if self.deployment == Deployment::DataCenter {
            return crate::api::datacenter::client::move_subtask_parent(self, key, parent).await;
        }
        // Cloud grew a REST route for this and Data Center never did, so
        // Cloud's is the ordinary field edit every other field takes; the Move
        // wizard `datacenter::client` drives instead is the other half of this.
        self.update_fields(key, json!({ "parent": { "key": parent } }))
            .await
    }

    async fn worklogs(&self, key: &str) -> Result<Vec<RemoteWorklog>> {
        let values = self.worklog_values(key).await?;
        Ok(normalize::worklogs_from(Some(&json!(values)), self.markup()).0)
    }

    async fn add_worklog(&self, key: &str, worklog: &WorklogInput) -> Result<RemoteWorklog> {
        let v = self
            .http
            .post_json(
                &self.url(&routes::ADD_WORKLOG, &[("issueIdOrKey", key)], &[]),
                worklog.to_body(),
            )
            .await?;
        one_worklog(&v, self.markup())
    }

    async fn update_worklog(
        &self,
        key: &str,
        id: &str,
        worklog: &WorklogInput,
    ) -> Result<RemoteWorklog> {
        let v = self
            .http
            .put_json(
                &self.url(
                    &routes::UPDATE_WORKLOG,
                    &[("issueIdOrKey", key), ("id", id)],
                    &[],
                ),
                worklog.to_body(),
            )
            .await?;
        one_worklog(&v, self.markup())
    }

    async fn delete_worklog(&self, key: &str, id: &str) -> Result<()> {
        self.http
            .delete(&self.url(
                &routes::DELETE_WORKLOG,
                &[("issueIdOrKey", key), ("id", id)],
                &[],
            ))
            .await
    }

    async fn edit_meta(&self, key: &str) -> Result<Option<ScreenMeta>> {
        let v = self
            .get(&routes::EDIT_META, &[("issueIdOrKey", key)], &[])
            .await?;
        Ok(Some(normalize::screen_from_json(&v)))
    }

    /// Paged twice: the issue types of a project, to turn the type's name into
    /// the id the second call takes, and then that call's own pages of fields.
    ///
    /// The name is what a file carries, and the id is what the route wants, so
    /// the lookup is unavoidable. A type the project does not have is `None`
    /// rather than an error — a `create` naming an unknown type fails on the
    /// create itself, where the message is about the type rather than about a
    /// metadata call the user never made.
    async fn create_meta(&self, project: &str, issue_type: &str) -> Result<Option<ScreenMeta>> {
        let type_id = {
            let mut start = 0usize;
            loop {
                let v = self
                    .get(
                        &routes::CREATE_META_TYPES,
                        &[("projectIdOrKey", project)],
                        &[("startAt", &start.to_string()), ("maxResults", "50")],
                    )
                    .await?;
                let values = v
                    .get("values")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                let n = values.len();
                let found = values.iter().find_map(|t| {
                    let name = t.get("name").and_then(Value::as_str)?;
                    name.eq_ignore_ascii_case(issue_type)
                        .then(|| t.get("id").and_then(id_string_ref))?
                });
                if found.is_some() {
                    break found;
                }
                if n == 0 || v.get("isLast").and_then(Value::as_bool).unwrap_or(true) {
                    break None;
                }
                start += n;
            }
        };
        // A type this project does not offer is `None` rather than an error: a
        // `create` naming an unknown type fails on the create itself, where the
        // message is about the type rather than about a lookup nobody asked for.
        let Some(type_id) = type_id else {
            return Ok(None);
        };

        let mut screen = ScreenMeta::default();
        let mut start = 0usize;
        loop {
            let v = self
                .get(
                    &routes::CREATE_META_FIELDS,
                    &[("projectIdOrKey", project), ("issueTypeId", &type_id)],
                    &[("startAt", &start.to_string()), ("maxResults", "50")],
                )
                .await?;
            // The paged form answers with `values`, a list of fields each
            // carrying its own `fieldId`; the unpaged shape is the same
            // `fields` object editmeta uses.
            let values = v
                .get("values")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            if values.is_empty() {
                let page = normalize::screen_from_json(&v);
                screen.fields.extend(page.fields);
                break;
            }
            let n = values.len();
            for f in &values {
                let Some(id) = f
                    .get("fieldId")
                    .and_then(Value::as_str)
                    .or_else(|| f.get("key").and_then(Value::as_str))
                else {
                    continue;
                };
                screen.fields.insert(
                    id.to_string(),
                    FieldSchema {
                        id: id.to_string(),
                        name: f
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or(id)
                            .to_string(),
                        required: f.get("required").and_then(Value::as_bool).unwrap_or(false),
                        operations: f
                            .get("operations")
                            .and_then(Value::as_array)
                            .map(|ops| {
                                ops.iter()
                                    .filter_map(Value::as_str)
                                    .map(str::to_string)
                                    .collect()
                            })
                            .unwrap_or_default(),
                    },
                );
            }
            if n == 0 || v.get("isLast").and_then(Value::as_bool).unwrap_or(true) {
                break;
            }
            start += n;
        }
        Ok(Some(screen))
    }

    async fn link_types(&self) -> Result<Vec<IssueLinkType>> {
        let v = self.get(&routes::ISSUE_LINK_TYPES, &[], &[]).await?;
        Ok(v.get("issueLinkTypes")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|t| {
                        Some(IssueLinkType {
                            id: id_string(t.get("id")?)?,
                            name: t.get("name")?.as_str()?.to_string(),
                            inward: t.get("inward")?.as_str()?.to_string(),
                            outward: t.get("outward")?.as_str()?.to_string(),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default())
    }

    async fn create_link(&self, type_name: &str, inward: &str, outward: &str) -> Result<()> {
        self.http
            .post_json(
                &self.url(&routes::CREATE_ISSUE_LINK, &[], &[]),
                json!({
                    "type": { "name": type_name },
                    "inwardIssue": { "key": inward },
                    "outwardIssue": { "key": outward },
                }),
            )
            .await
            .map(|_| ())
    }

    async fn delete_link(&self, link_id: &str) -> Result<()> {
        self.http
            .delete(&self.url(&routes::DELETE_ISSUE_LINK, &[("linkId", link_id)], &[]))
            .await
    }

    async fn remote_links(&self, key: &str) -> Result<Vec<Value>> {
        let v = self
            .get(&routes::REMOTE_LINKS, &[("issueIdOrKey", key)], &[])
            .await?;
        Ok(v.as_array().cloned().unwrap_or_default())
    }

    async fn create_remote_link(&self, key: &str, url: &str, title: &str) -> Result<()> {
        self.http
            .post_json(
                &self.url(&routes::CREATE_REMOTE_LINK, &[("issueIdOrKey", key)], &[]),
                remote_link_body(url, title),
            )
            .await
            .map(|_| ())
    }

    async fn delete_remote_link(&self, key: &str, link_id: &str) -> Result<()> {
        self.http
            .delete(&self.url(
                &routes::DELETE_REMOTE_LINK,
                &[("issueIdOrKey", key), ("linkId", link_id)],
                &[],
            ))
            .await
    }

    async fn set_issue_property(&self, key: &str, property: &str, value: Value) -> Result<()> {
        self.http
            .put_json(
                &self.url(
                    &routes::SET_ISSUE_PROPERTY,
                    &[("issueIdOrKey", key), ("propertyKey", property)],
                    &[],
                ),
                value,
            )
            .await?;
        Ok(())
    }

    async fn assign(&self, key: &str, user_id: Option<&str>) -> Result<()> {
        self.http
            .put_json(
                &self.url(&routes::ASSIGN, &[("issueIdOrKey", key)], &[]),
                crate::api::assignee_body(self.deployment, user_id),
            )
            .await?;
        Ok(())
    }

    /// Every comment, paged.
    ///
    /// Both deployments declare `startAt` and `maxResults` here — unlike the
    /// worklog route, where only Cloud does — so one loop serves both.
    async fn comments(&self, key: &str) -> Result<Vec<RemoteComment>> {
        let values = self.comment_values(key).await?;
        Ok(normalize::comments_from(Some(&json!(values)), self.markup()).0)
    }

    async fn add_comment(&self, key: &str, body: &RichText) -> Result<RemoteComment> {
        let v = self
            .http
            .post_json(
                &self.url(&routes::ADD_COMMENT, &[("issueIdOrKey", key)], &[]),
                json!({ "body": body.to_field_value() }),
            )
            .await?;
        one_comment(&v, self.markup())
    }

    async fn update_comment(&self, key: &str, id: &str, body: &RichText) -> Result<RemoteComment> {
        let v = self
            .http
            .put_json(
                &self.url(
                    &routes::UPDATE_COMMENT,
                    &[("issueIdOrKey", key), ("id", id)],
                    &[],
                ),
                json!({ "body": body.to_field_value() }),
            )
            .await?;
        one_comment(&v, self.markup())
    }

    async fn delete_comment(&self, key: &str, id: &str) -> Result<()> {
        self.http
            .delete(&self.url(
                &routes::DELETE_COMMENT,
                &[("issueIdOrKey", key), ("id", id)],
                &[],
            ))
            .await
    }

    async fn upload_attachment(
        &self,
        key: &str,
        filename: &str,
        bytes: Vec<u8>,
    ) -> Result<RemoteAttachment> {
        let v = self
            .http
            .post_multipart(
                &self.url(&routes::ADD_ATTACHMENT, &[("issueIdOrKey", key)], &[]),
                filename,
                bytes,
            )
            .await?;
        // The endpoint answers with a one-element array.
        let first = v.as_array().and_then(|a| a.first()).cloned().unwrap_or(v);
        normalize::attachments_from(Some(&json!([first])))
            .into_iter()
            .next()
            .ok_or_else(|| JiredError::generic(format!("{key}: upload returned no attachment")))
    }

    async fn download_attachment(&self, attachment: &RemoteAttachment) -> Result<Vec<u8>> {
        self.http.get_bytes(&attachment.content_url).await
    }

    /// The body is the identifier itself, as a bare JSON string — not an object
    /// with a field in it, which is what every other write here sends.
    async fn watch(&self, key: &str, user_id: &str) -> Result<()> {
        self.http
            .post_json_repeatable(
                &self.url(&routes::ADD_WATCHER, &[("issueIdOrKey", key)], &[]),
                json!(user_id),
            )
            .await?;
        Ok(())
    }

    async fn unwatch(&self, key: &str, user_id: &str) -> Result<()> {
        self.http
            .delete(&self.url(
                remove_watcher_route(self.deployment),
                &[("issueIdOrKey", key)],
                &[(watcher_param(self.deployment), user_id)],
            ))
            .await
    }
}

#[async_trait]
impl Agile for Rest {
    async fn boards(&self, project: Option<&str>) -> Result<Vec<Board>> {
        let mut out = Vec::new();
        let mut start = 0usize;
        loop {
            let mut query = vec![("maxResults", "50".to_string())];
            if let Some(p) = project {
                query.push(("projectKeyOrId", p.to_string()));
            }
            query.push(("startAt", start.to_string()));
            let query: Vec<(&str, &str)> = query.iter().map(|(n, v)| (*n, v.as_str())).collect();
            let v = self.get(&routes::BOARDS, &[], &query).await?;
            let values = v
                .get("values")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let n = values.len();
            out.extend(values.iter().filter_map(|b| {
                Some(Board {
                    id: id_string(b.get("id")?)?,
                    name: b.get("name")?.as_str()?.to_string(),
                    board_type: b
                        .get("type")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                    project_key: b
                        .get("location")
                        .and_then(|l| l.get("projectKey"))
                        .and_then(Value::as_str)
                        .map(str::to_string),
                })
            }));
            if v.get("isLast").and_then(Value::as_bool).unwrap_or(true) || n == 0 {
                break;
            }
            start += n;
        }
        Ok(out)
    }

    async fn board_columns(&self, board_id: &str) -> Result<Vec<BoardColumn>> {
        let v = self
            .get(&routes::BOARD_CONFIGURATION, &[("boardId", board_id)], &[])
            .await?;
        Ok(v.get("columnConfig")
            .and_then(|c| c.get("columns"))
            .and_then(Value::as_array)
            .map(|cols| {
                cols.iter()
                    .filter_map(|c| {
                        Some(BoardColumn {
                            name: c.get("name")?.as_str()?.to_string(),
                            status_ids: c
                                .get("statuses")
                                .and_then(Value::as_array)
                                .map(|s| s.iter().filter_map(|x| id_string(x.get("id")?)).collect())
                                .unwrap_or_default(),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default())
    }

    async fn board_project(&self, board_id: &str) -> Result<Option<String>> {
        if self.deployment == Deployment::DataCenter {
            return crate::api::datacenter::client::board_project(self, board_id).await;
        }
        // Two endpoints answer this, one per deployment, and neither answers on
        // both: `location` in the board configuration is a Cloud field that
        // Data Center omits entirely, so Cloud asks that one first and
        // `datacenter::client` reverses the order. Both still fall back to the
        // other, because which endpoint a given Jira version populates is not
        // something to bet a null on.
        if let Some(key) = self
            .board_project_from(board_id, Source::Configuration)
            .await?
        {
            return Ok(Some(key));
        }
        self.board_project_from(board_id, Source::Listing).await
    }

    async fn sprints(&self, board_id: &str) -> Result<Vec<Sprint>> {
        let mut out = Vec::new();
        let mut start = 0usize;
        loop {
            let v = self
                .get(
                    &routes::BOARD_SPRINTS,
                    &[("boardId", board_id)],
                    &[("startAt", &start.to_string()), ("maxResults", "50")],
                )
                .await?;
            let values = v
                .get("values")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let n = values.len();
            out.extend(values.iter().filter_map(|s| sprint_from_json(s, board_id)));
            if v.get("isLast").and_then(Value::as_bool).unwrap_or(true) || n == 0 {
                break;
            }
            start += n;
        }
        Ok(out)
    }

    async fn create_sprint(&self, input: &SprintInput) -> Result<Sprint> {
        let v = self
            .http
            .post_json(&self.url(&routes::CREATE_SPRINT, &[], &[]), input.to_body())
            .await?;
        // The response is a sprint like any other, so it is read like any
        // other; only its id is beyond invention if the server omits it.
        sprint_from_json(&v, &input.board_id).ok_or_else(|| {
            JiredError::generic("sprint creation returned nothing recognisable as a sprint")
                .with_detail("response", v)
        })
    }

    async fn sprint(&self, id: &str) -> Result<Sprint> {
        let v = self
            .get(&routes::GET_SPRINT, &[("sprintId", id)], &[])
            .await?;
        // No board to fall back on: this endpoint is asked by sprint id, and
        // the payload's own `originBoardId` is the only board there is.
        sprint_from_json(&v, "").ok_or_else(|| {
            JiredError::generic(format!(
                "sprint {id} came back in a shape jired cannot read"
            ))
            .with_detail("response", v)
        })
    }

    async fn update_sprint(&self, id: &str, update: &SprintUpdate) -> Result<Sprint> {
        let v = self
            .http
            .post_json(
                &self.url(&routes::UPDATE_SPRINT, &[("sprintId", id)], &[]),
                update.to_body(),
            )
            .await?;
        sprint_from_json(&v, "").ok_or_else(|| {
            JiredError::generic(format!(
                "sprint {id} came back in a shape jired cannot read"
            ))
            .with_detail("response", v)
        })
    }

    async fn move_to_sprint(&self, sprint: Option<&str>, keys: &[String]) -> Result<()> {
        let path = match sprint {
            Some(id) => self.url(&routes::SPRINT_ISSUES, &[("sprintId", id)], &[]),
            None => self.url(&routes::BACKLOG_ISSUES, &[], &[]),
        };
        self.http
            .post_json_repeatable(&path, json!({ "issues": keys }))
            .await
            .map(|_| ())
    }
}

#[async_trait]
impl Projects for Rest {
    async fn projects(&self) -> Result<Vec<Project>> {
        if self.deployment == Deployment::DataCenter {
            return crate::api::datacenter::client::projects(self).await;
        }
        // Cloud paginates `/project/search`, which Data Center does not
        // serve at all; there the whole listing is one array, and
        // `datacenter::client` reads it that way.
        let mut out = Vec::new();
        let mut start = 0usize;
        loop {
            let v = self
                .http
                .get_json(&crate::api::cloud::PROJECT_SEARCH.url(
                    self.api(),
                    &[],
                    &[("startAt", &start.to_string()), ("maxResults", "50")],
                ))
                .await?;
            let values = v
                .get("values")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let n = values.len();
            out.extend(values.iter().filter_map(project_from));
            let is_last = v.get("isLast").and_then(Value::as_bool).unwrap_or(true);
            if is_last || n == 0 {
                break;
            }
            start += n;
        }
        out.sort_by(|a, b| a.key.cmp(&b.key));
        Ok(out)
    }

    async fn project(&self, key: &str) -> Result<Project> {
        let v = self
            .get(&routes::GET_PROJECT, &[("projectIdOrKey", key)], &[])
            .await?;
        project_from(&v).ok_or_else(|| {
            JiredError::generic(format!(
                "project {key} came back in a shape jired cannot read"
            ))
            .with_detail("response", v)
        })
    }

    async fn versions(&self, project: &str) -> Result<Vec<Version>> {
        let v = self
            .get(
                &routes::PROJECT_VERSIONS,
                &[("projectIdOrKey", project)],
                &[],
            )
            .await?;
        Ok(v.as_array()
            .map(|a| a.iter().filter_map(version_from_json).collect())
            .unwrap_or_default())
    }

    async fn version(&self, id: &str) -> Result<Version> {
        let v = self.get(&routes::GET_VERSION, &[("id", id)], &[]).await?;
        version_from_json(&v).ok_or_else(|| unreadable_version(id, v))
    }

    async fn create_version(&self, input: &VersionInput) -> Result<Version> {
        let v = self
            .http
            .post_json(
                &self.url(&routes::CREATE_VERSION, &[], &[]),
                input.to_body(),
            )
            .await?;
        version_from_json(&v).ok_or_else(|| unreadable_version(&input.name, v))
    }

    async fn update_version(&self, id: &str, update: &VersionUpdate) -> Result<Version> {
        let v = self
            .http
            .put_json(
                &self.url(&routes::UPDATE_VERSION, &[("id", id)], &[]),
                update.to_body(),
            )
            .await?;
        version_from_json(&v).ok_or_else(|| unreadable_version(id, v))
    }
}

#[async_trait]
impl Filters for Rest {
    /// Cloud's `filter/search`, a page at a time, or its favourites.
    async fn filters(&self, listing: FilterListing<'_>) -> Result<Vec<Filter>> {
        if self.deployment == Deployment::DataCenter {
            return crate::api::datacenter::client::filters(self, listing).await;
        }
        let (by, value) = match listing {
            FilterListing::Favourites => {
                let v = self.get(&routes::FAVOURITE_FILTERS, &[], &[]).await?;
                return Ok(filter::filters_from_json(&v));
            }
            FilterListing::Owned(account) => ("accountId", account),
            FilterListing::Named(name) => ("filterName", name.trim()),
        };
        let mut all = Vec::new();
        loop {
            let start = all.len().to_string();
            let v = self
                .get(
                    &crate::api::cloud::FILTER_SEARCH,
                    &[],
                    &[
                        (by, value),
                        ("expand", filter::CLOUD_EXPAND),
                        ("startAt", start.as_str()),
                        ("maxResults", "100"),
                    ],
                )
                .await?;
            let page = filter::filters_from_json(&v);
            let last = page.is_empty() || v.get("isLast").and_then(Value::as_bool) != Some(false);
            all.extend(page);
            if last {
                break;
            }
        }
        // `filterName` matches a part of the name; the question was the whole.
        if let FilterListing::Named(name) = listing {
            all.retain(|f| f.name.eq_ignore_ascii_case(name.trim()));
        }
        Ok(all)
    }

    async fn filter(&self, id: &str) -> Result<Filter> {
        let v = self.get(&routes::GET_FILTER, &[("id", id)], &[]).await?;
        filter::filter_from_json(&v).ok_or_else(|| unreadable_filter(id, v))
    }

    async fn create_filter(&self, input: &FilterInput) -> Result<Filter> {
        let plan = filter::create(self.deployment, input, filter::NEW);
        let v = self.send(&plan[0]).await?;
        let made = filter::filter_from_json(&v).ok_or_else(|| unreadable_filter(&input.name, v))?;
        // Planned again with the id the server gave: the rest address it.
        let follow_ups = &filter::create(self.deployment, input, &made.id)[1..];
        if follow_ups.is_empty() {
            return Ok(made);
        }
        for call in follow_ups {
            self.send(call).await?;
        }
        self.filter(&made.id).await
    }

    async fn update_filter(&self, current: &Filter, update: &FilterUpdate) -> Result<Filter> {
        for call in filter::update(self.deployment, current, update) {
            self.send(&call).await?;
        }
        // Read back rather than taken from the PUT: on Cloud the star is a
        // second call the PUT's answer predates.
        self.filter(&current.id).await
    }

    async fn delete_filter(&self, id: &str) -> Result<()> {
        self.send(&filter::delete(self.deployment, id)).await?;
        Ok(())
    }

    async fn add_filter_share(&self, id: &str, share: &ShareInput) -> Result<()> {
        self.send(&filter::add_share(self.deployment, id, share))
            .await?;
        Ok(())
    }

    async fn delete_filter_share(&self, id: &str, permission_id: &str) -> Result<()> {
        self.send(&filter::remove_share(self.deployment, id, permission_id))
            .await?;
        Ok(())
    }
}

/// Which of the two `DELETE …/watchers` declarations to send, and what it calls
/// the user: the one place the deployments disagree about watching.
pub(crate) fn remove_watcher_route(deployment: Deployment) -> &'static Route {
    match deployment {
        Deployment::Cloud => &crate::api::cloud::REMOVE_WATCHER,
        Deployment::DataCenter => &routes::REMOVE_WATCHER,
    }
}

pub(crate) fn watcher_param(deployment: Deployment) -> &'static str {
    match deployment {
        Deployment::Cloud => "accountId",
        Deployment::DataCenter => "username",
    }
}

/// Cloud's user search: `query`, and the assignable route wherever the caller
/// named something to be assigned to.
///
/// Its own function so a test can read the URL without a server; Data Center's
/// counterpart is [`crate::api::datacenter::client::user_search_url`].
pub(crate) fn cloud_user_search(prefix: &str, query: &str, scope: UserScope<'_>) -> String {
    match scope {
        UserScope::Any => crate::api::cloud::FIND_USERS.url(
            prefix,
            &[],
            &[("query", query), ("maxResults", "20")],
        ),
        UserScope::Issue(key) => crate::api::cloud::FIND_ASSIGNABLE_USERS.url(
            prefix,
            &[],
            &[("query", query), ("issueKey", key), ("maxResults", "20")],
        ),
        UserScope::Project(project) => crate::api::cloud::FIND_ASSIGNABLE_USERS.url(
            prefix,
            &[],
            &[("query", query), ("project", project), ("maxResults", "20")],
        ),
    }
}

pub(crate) fn project_from(v: &Value) -> Option<Project> {
    Some(Project {
        id: id_string(v.get("id")?)?,
        key: v.get("key")?.as_str()?.to_string(),
        name: v
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        project_type: v
            .get("projectTypeKey")
            .and_then(Value::as_str)
            .map(str::to_string),
    })
}

/// One sprint out of an agile-API payload.
///
/// `board` is what the sprint belongs to when the payload does not say — a
/// board's own sprint listing repeats the board in the URL rather than in
/// every row.
fn sprint_from_json(s: &Value, board: &str) -> Option<Sprint> {
    Some(Sprint {
        id: id_string(s.get("id")?)?,
        name: s.get("name")?.as_str()?.to_string(),
        state: SprintState::parse(s.get("state").and_then(Value::as_str).unwrap_or("")),
        board_id: s
            .get("originBoardId")
            .and_then(id_string_ref)
            .or_else(|| Some(board.to_string()))
            .filter(|b| !b.is_empty()),
        start_date: s
            .get("startDate")
            .and_then(Value::as_str)
            .and_then(parse_timestamp),
        end_date: s
            .get("endDate")
            .and_then(Value::as_str)
            .and_then(parse_timestamp),
        goal: s
            .get("goal")
            .and_then(Value::as_str)
            .map(str::to_string)
            .filter(|g| !g.is_empty()),
    })
}

/// One version out of a platform-API payload.
///
/// Shared with the fixture client, which keeps its versions in the server's
/// own shape so a recording reads the way a live answer does.
pub fn version_from_json(v: &Value) -> Option<Version> {
    let text = |name: &str| {
        v.get(name)
            .and_then(Value::as_str)
            .map(str::to_string)
            .filter(|t| !t.is_empty())
    };
    Some(Version {
        id: id_string(v.get("id")?)?,
        name: v.get("name")?.as_str()?.to_string(),
        project_id: v.get("projectId").and_then(id_string_ref),
        description: text("description"),
        released: v.get("released").and_then(Value::as_bool).unwrap_or(false),
        archived: v.get("archived").and_then(Value::as_bool).unwrap_or(false),
        start_date: text("startDate").as_deref().and_then(parse_day),
        release_date: text("releaseDate").as_deref().and_then(parse_day),
    })
}

fn unreadable_filter(which: &str, response: Value) -> JiredError {
    JiredError::generic(format!(
        "filter {which} came back in a shape jired cannot read"
    ))
    .with_detail("response", response)
}

fn unreadable_version(which: &str, response: Value) -> JiredError {
    JiredError::generic(format!(
        "version {which} came back in a shape jired cannot read"
    ))
    .with_detail("response", response)
}

/// The one project a board's project listing names, or `None` when it names
/// none or several.
///
/// Shared with the fixture client so both read the payload the same way.
pub fn single_project(v: &Value) -> Option<String> {
    let mut keys: Vec<&str> = v
        .get("values")
        .and_then(Value::as_array)?
        .iter()
        .filter_map(|p| p.get("key").and_then(Value::as_str))
        .collect();
    keys.dedup();
    match keys.as_slice() {
        [only] => Some((*only).to_string()),
        _ => None,
    }
}

/// Jira is inconsistent about whether ids are numbers or strings.
fn id_string(v: &Value) -> Option<String> {
    id_string_ref(v)
}

fn id_string_ref(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

fn one_comment(v: &Value, markup: Markup) -> Result<RemoteComment> {
    normalize::comments_from(Some(&json!([v])), markup)
        .0
        .into_iter()
        .next()
        .ok_or_else(|| JiredError::generic("the server returned an unreadable comment"))
}

fn one_worklog(v: &Value, markup: Markup) -> Result<RemoteWorklog> {
    normalize::one_worklog(v, markup)
        .ok_or_else(|| JiredError::generic("the server returned an unreadable worklog"))
}

/// The body of a web link's create call. The server requires a title, so a
/// link written without one is titled with its URL, which is what Jira's own
/// dialog does with an empty one.
pub fn remote_link_body(url: &str, title: &str) -> Value {
    let title = match title.trim() {
        "" => url,
        t => t,
    };
    json!({"object": {"url": url, "title": title}})
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A listing asks for the fields its rows show and nothing else; the
    /// instance-specific ones only where this instance has them.
    #[test]
    fn a_listing_asks_for_its_fields_and_the_instances_sprint() {
        let bare = Rest::field_param(FieldSet::Listing, &FieldMap::default());
        assert!(bare.starts_with("key,summary,status,"), "{bare}");
        assert!(
            bare.contains(",parent,") && bare.contains(",fixVersions,"),
            "{bare}"
        );
        assert!(
            !bare.contains("*all") && !bare.contains("customfield"),
            "{bare}"
        );

        let map = FieldMap {
            sprint: Some("customfield_10020".into()),
            epic_link: Some("customfield_10014".into()),
            ..FieldMap::default()
        };
        let mapped = Rest::field_param(FieldSet::Listing, &map);
        assert!(
            mapped.ends_with(",customfield_10020,customfield_10014"),
            "{mapped}"
        );
    }

    #[test]
    fn ids_normalize_regardless_of_json_type() {
        assert_eq!(id_string(&json!(42)).as_deref(), Some("42"));
        assert_eq!(id_string(&json!("42")).as_deref(), Some("42"));
        assert_eq!(id_string(&json!(null)), None);
    }

    #[test]
    fn project_parsing_tolerates_a_missing_name() {
        let p = project_from(&json!({"id": 1, "key": "PROJ"})).unwrap();
        assert_eq!(p.key, "PROJ");
        assert_eq!(p.name, "");
    }

    /// Routing every path through the table was meant to change where the
    /// strings come from and not the strings themselves. These are the URLs
    /// Cloud has always been sent, pinned so a table edit made for Data
    /// Center's sake cannot quietly move a Cloud request.
    ///
    /// The end-to-end suite runs against the fixture client, which never builds
    /// a URL, so this is the only place the rendering is checked at all.
    #[test]
    fn cloud_paths_are_unchanged() {
        let api = "/rest/api/3";
        let agile = "/rest/agile/1.0";
        let issue = [("issueIdOrKey", "PROJ-1")];

        assert_eq!(
            routes::SERVER_INFO.url(api, &[], &[]),
            "/rest/api/3/serverInfo"
        );
        assert_eq!(routes::MYSELF.url(api, &[], &[]), "/rest/api/3/myself");
        assert_eq!(routes::FIELDS.url(api, &[], &[]), "/rest/api/3/field");
        assert_eq!(routes::SEARCH.url(api, &[], &[]), "/rest/api/3/search");
        assert_eq!(
            routes::GET_ISSUE.url(api, &issue, &[("fields", "*all")]),
            "/rest/api/3/issue/PROJ-1?fields=*all"
        );
        assert_eq!(
            routes::GET_ISSUE.url(api, &issue, &[("fields", "updated")]),
            "/rest/api/3/issue/PROJ-1?fields=updated"
        );
        assert_eq!(
            routes::GET_ISSUE.url(api, &issue, &[("expand", "changelog"), ("fields", "key")]),
            "/rest/api/3/issue/PROJ-1?expand=changelog&fields=key"
        );
        assert_eq!(
            routes::TRANSITIONS.url(api, &issue, &[("expand", "transitions.fields")]),
            "/rest/api/3/issue/PROJ-1/transitions?expand=transitions.fields"
        );
        assert_eq!(
            routes::ADD_COMMENT.url(api, &issue, &[]),
            "/rest/api/3/issue/PROJ-1/comment"
        );
        assert_eq!(
            routes::ADD_ATTACHMENT.url(api, &issue, &[]),
            "/rest/api/3/issue/PROJ-1/attachments"
        );
        assert_eq!(
            routes::BOARDS.url(
                agile,
                &[],
                &[
                    ("maxResults", "50"),
                    ("projectKeyOrId", "PROJ"),
                    ("startAt", "0")
                ]
            ),
            "/rest/agile/1.0/board?maxResults=50&projectKeyOrId=PROJ&startAt=0"
        );
        assert_eq!(
            routes::SPRINT_ISSUES.url(agile, &[("sprintId", "43")], &[]),
            "/rest/agile/1.0/sprint/43/issue"
        );
        assert_eq!(
            routes::BACKLOG_ISSUES.url(agile, &[], &[]),
            "/rest/agile/1.0/backlog/issue"
        );
        // And the routes only Cloud has.
        assert_eq!(
            crate::api::cloud::GET_WORKLOGS.url(
                api,
                &issue,
                &[("startAt", "0"), ("maxResults", "100")]
            ),
            "/rest/api/3/issue/PROJ-1/worklog?startAt=0&maxResults=100"
        );
    }

    use crate::api::loopback::{self, Reply};

    /// A Cloud site: an empty field list, and a search that answers in two
    /// token-paged pages.
    fn cloud_server() -> (String, loopback::Seen) {
        loopback::serve(|r| {
            let issue = |key: &str| {
                json!({"id": "1", "key": key,
                       "fields": {"summary": "s", "updated": "2026-01-01T00:00:00.000+0000"}})
            };
            Reply::Json(
                200,
                if r.path.ends_with("/field") {
                    json!([])
                } else if r.json().get("nextPageToken").is_none() {
                    json!({"issues": [issue("PROJ-1")], "nextPageToken": "p2", "isLast": false})
                } else {
                    json!({"issues": [issue("PROJ-2")], "isLast": true})
                },
            )
        })
    }

    /// Cloud's search is `search/jql`, paged by the token each page gives:
    /// the offset search it used to share with Data Center is going away
    /// there, and a token that was read and never sent back walked nothing.
    #[tokio::test]
    async fn a_cloud_search_pages_by_token() {
        let (url, seen) = cloud_server();
        let rest = Rest::new(loopback::client(&url, Deployment::Cloud));
        let all = crate::api::client::search_all(
            &rest,
            SearchQuery {
                jql: "project = PROJ".into(),
                ..Default::default()
            },
            |_| {},
        )
        .await
        .unwrap();
        assert_eq!(
            all.iter().map(|i| i.key.as_str()).collect::<Vec<_>>(),
            ["PROJ-1", "PROJ-2"]
        );
        let searches: Vec<_> = seen
            .lock()
            .unwrap()
            .iter()
            .filter(|r| !r.path.ends_with("/field"))
            .cloned()
            .collect();
        assert_eq!(searches.len(), 2, "{searches:?}");
        assert!(
            searches.iter().all(|r| r.path == "/rest/api/3/search/jql"),
            "{searches:?}"
        );
        assert_eq!(searches[1].json()["nextPageToken"], "p2");
        assert!(searches[0].json().get("startAt").is_none());
    }

    /// Data Center pages comments by offset, and the walk reads until the
    /// total the server reports, a page at a time.
    #[tokio::test]
    async fn comments_are_read_to_the_total_a_page_at_a_time() {
        let (url, seen) = loopback::serve(|r| {
            let start: usize = r
                .path
                .split(['?', '&'])
                .find_map(|q| q.strip_prefix("startAt="))
                .and_then(|n| n.parse().ok())
                .unwrap_or(0);
            // A server that hands out two at a time, whatever was asked.
            let page: Vec<_> = (start..(start + 2).min(5))
                .map(|n| json!({"id": n.to_string(), "body": "b"}))
                .collect();
            Reply::Json(200, json!({"startAt": start, "total": 5, "comments": page}))
        });
        let rest = Rest::new(loopback::client(&url, Deployment::DataCenter));
        let all = rest.comment_values("PROJ-1").await.unwrap();
        assert_eq!(all.len(), 5);
        assert_eq!(seen.lock().unwrap().len(), 3);
    }

    /// Data Center's worklog route declares no paging, and an issue whose
    /// embedded worklogs were cut short is completed with the one call that
    /// returns them all — not Cloud's paged walk, whose parameters the
    /// server's description says not to send.
    #[tokio::test]
    async fn data_center_completes_worklogs_in_one_unpaged_call() {
        let (url, seen) = loopback::serve(|r| {
            if r.path.ends_with("/field") {
                return Reply::Json(200, json!([]));
            }
            if r.path.contains("/worklog") {
                let all: Vec<_> = (1..=3)
                    .map(|n| {
                        json!({"id": n.to_string(), "timeSpentSeconds": 60,
                                    "started": "2026-01-01T00:00:00.000+0000"})
                    })
                    .collect();
                return Reply::Json(200, json!({"startAt": 0, "total": 3, "worklogs": all}));
            }
            Reply::Json(
                200,
                json!({"startAt": 0, "total": 1, "issues": [
                {"id": "1", "key": "DC-1", "fields": {
                 "summary": "s", "updated": "2026-01-01T00:00:00.000+0000",
                 "worklog": {"startAt": 0, "maxResults": 0, "total": 3, "worklogs": []}}}]}),
            )
        });
        let rest = Rest::new(loopback::client(&url, Deployment::DataCenter));
        let page = rest
            .search(&SearchQuery {
                jql: "project = DC".into(),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(page.issues[0].worklogs.len(), 3, "{:?}", page.issues);
        let calls: Vec<_> = seen
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r.path.contains("/worklog"))
            .map(|r| r.path.clone())
            .collect();
        assert_eq!(calls, ["/rest/api/2/issue/DC-1/worklog"]);
    }

    /// A page's issues have the rest of their comments read a few at a time:
    /// one after another, each waited its turn on the round trip before it.
    #[tokio::test]
    async fn a_pages_issues_are_completed_at_once() {
        let (url, _) = loopback::serve(|r| {
            if r.path.contains("/comment") {
                std::thread::sleep(std::time::Duration::from_millis(300));
                return Reply::Json(
                    200,
                    json!({"startAt": 0, "total": 1, "comments": [
                    {"id": "1", "body": "b"}]}),
                );
            }
            if r.path.ends_with("/field") {
                return Reply::Json(200, json!([]));
            }
            let issue = |n: u32| {
                json!({"id": n.to_string(), "key": format!("DC-{n}"), "fields": {
                "summary": "s", "updated": "2026-01-01T00:00:00.000+0000",
                "comment": {"startAt": 0, "maxResults": 0, "total": 1, "comments": []}}})
            };
            Reply::Json(
                200,
                json!({"startAt": 0, "total": 4,
                "issues": [issue(1), issue(2), issue(3), issue(4)]}),
            )
        });
        let rest = Rest::new(loopback::client(&url, Deployment::DataCenter));
        let started = std::time::Instant::now();
        let page = rest
            .search(&SearchQuery {
                jql: "project = DC".into(),
                ..Default::default()
            })
            .await
            .unwrap();
        assert!(
            page.issues.iter().all(|i| i.comments.len() == 1),
            "{:?}",
            page.issues
        );
        assert!(
            started.elapsed() < std::time::Duration::from_millis(1000),
            "four 300ms reads took {:?}",
            started.elapsed()
        );
    }
}
