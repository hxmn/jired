//! Shared REST implementation for both deployments.
//!
//! Cloud and Data Center differ in API version, rich-text format, user
//! identifiers and search pagination — all of which are data, not control flow.
//! Keeping one implementation parameterized by those facts means a fix lands on
//! both products at once.

use crate::api::client::{FieldSet, JiraApi, SearchPage, SearchQuery};
use crate::api::http::HttpClient;
use crate::api::model::*;
use crate::api::normalize::{self, FieldMap};
use crate::api::{Deployment, Markup};
use crate::error::{JiredError, Result};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::{json, Value};
use tokio::sync::OnceCell;

pub struct Rest {
    http: HttpClient,
    deployment: Deployment,
    /// `/field` is instance-wide and stable within a run.
    field_map: OnceCell<FieldMap>,
}

impl Rest {
    pub fn new(http: HttpClient) -> Self {
        let deployment = http.deployment();
        Self {
            http,
            deployment,
            field_map: OnceCell::new(),
        }
    }

    fn api(&self) -> &'static str {
        match self.deployment {
            Deployment::Cloud => "/rest/api/3",
            Deployment::DataCenter => "/rest/api/2",
        }
    }

    fn markup(&self) -> Markup {
        self.deployment.markup()
    }

    async fn field_map(&self) -> Result<&FieldMap> {
        self.field_map
            .get_or_try_init(|| async {
                let fields = self.fields().await?;
                Ok(FieldMap::from_meta(&fields))
            })
            .await
    }

    fn field_param(&self, set: FieldSet) -> &'static str {
        match set {
            FieldSet::Summary => "key,summary,status,assignee,updated,issuetype",
            FieldSet::Full => "*all",
        }
    }
}

#[async_trait]
impl JiraApi for Rest {
    fn deployment(&self) -> Deployment {
        self.deployment
    }

    fn base_url(&self) -> &str {
        self.http.base_url()
    }

    async fn server_info(&self) -> Result<ServerInfo> {
        let v = self
            .http
            .get_json(&format!("{}/serverInfo", self.api()))
            .await?;
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

    async fn myself(&self) -> Result<Myself> {
        let v = self
            .http
            .get_json(&format!("{}/myself", self.api()))
            .await?;
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

    async fn projects(&self) -> Result<Vec<Project>> {
        // Cloud paginates `/project/search`; Data Center returns `/project`
        // as a bare array.
        let mut out = Vec::new();
        match self.deployment {
            Deployment::Cloud => {
                let mut start = 0usize;
                loop {
                    let v = self
                        .http
                        .get_json(&format!(
                            "{}/project/search?startAt={start}&maxResults=50",
                            self.api()
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
            }
            Deployment::DataCenter => {
                let v = self
                    .http
                    .get_json(&format!("{}/project", self.api()))
                    .await?;
                out.extend(
                    v.as_array()
                        .unwrap_or(&vec![])
                        .iter()
                        .filter_map(project_from),
                );
            }
        }
        out.sort_by(|a, b| a.key.cmp(&b.key));
        Ok(out)
    }

    async fn fields(&self) -> Result<Vec<FieldMeta>> {
        let v = self.http.get_json(&format!("{}/field", self.api())).await?;
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
                })
            })
            .collect())
    }

    async fn boards(&self, project: Option<&str>) -> Result<Vec<Board>> {
        let mut path = "/rest/agile/1.0/board?maxResults=50".to_string();
        if let Some(p) = project {
            path.push_str(&format!("&projectKeyOrId={}", urlencoding::encode(p)));
        }
        let mut out = Vec::new();
        let mut start = 0usize;
        loop {
            let v = self
                .http
                .get_json(&format!("{path}&startAt={start}"))
                .await?;
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
            .http
            .get_json(&format!("/rest/agile/1.0/board/{board_id}/configuration"))
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

    async fn sprints(&self, board_id: &str) -> Result<Vec<Sprint>> {
        let mut out = Vec::new();
        let mut start = 0usize;
        loop {
            let v = self
                .http
                .get_json(&format!(
                    "/rest/agile/1.0/board/{board_id}/sprint?startAt={start}&maxResults=50"
                ))
                .await?;
            let values = v
                .get("values")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let n = values.len();
            out.extend(values.iter().filter_map(|s| {
                Some(Sprint {
                    id: id_string(s.get("id")?)?,
                    name: s.get("name")?.as_str()?.to_string(),
                    state: s
                        .get("state")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_ascii_lowercase(),
                    board_id: Some(board_id.to_string()),
                    start_date: s
                        .get("startDate")
                        .and_then(Value::as_str)
                        .and_then(parse_timestamp),
                    end_date: s
                        .get("endDate")
                        .and_then(Value::as_str)
                        .and_then(parse_timestamp),
                })
            }));
            if v.get("isLast").and_then(Value::as_bool).unwrap_or(true) || n == 0 {
                break;
            }
            start += n;
        }
        Ok(out)
    }

    async fn search(&self, query: &SearchQuery) -> Result<SearchPage> {
        let map = self.field_map().await?.clone();
        let markup = self.markup();
        let mut body = json!({
            "jql": query.jql,
            "maxResults": query.max_results,
            "fields": self.field_param(query.fields)
                .split(',')
                .collect::<Vec<_>>(),
        });
        if query.expand_changelog {
            body["expand"] = json!(["changelog"]);
        }
        body["startAt"] = json!(query.start_at);

        let v = self
            .http
            .post_json(&format!("{}/search", self.api()), body)
            .await?;

        let issues = v
            .get("issues")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .map(|i| normalize::issue_from_json(i, markup, &map))
                    .collect::<Result<Vec<_>>>()
            })
            .transpose()?
            .unwrap_or_default();

        Ok(SearchPage {
            start_at: v.get("startAt").and_then(Value::as_u64).unwrap_or(0) as usize,
            total: v
                .get("total")
                .and_then(Value::as_u64)
                .unwrap_or(issues.len() as u64) as usize,
            next_page_token: v
                .get("nextPageToken")
                .and_then(Value::as_str)
                .map(str::to_string),
            issues,
        })
    }

    async fn issue(&self, key: &str) -> Result<RemoteIssue> {
        let map = self.field_map().await?.clone();
        let v = self
            .http
            .get_json(&format!("{}/issue/{key}?fields=*all", self.api()))
            .await?;
        normalize::issue_from_json(&v, self.markup(), &map)
    }

    async fn issue_updated(&self, key: &str) -> Result<DateTime<Utc>> {
        let v = self
            .http
            .get_json(&format!("{}/issue/{key}?fields=updated", self.api()))
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
            .http
            .get_json(&format!(
                "{}/issue/{key}?expand=changelog&fields=key",
                self.api()
            ))
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
            .http
            .get_json(&format!(
                "{}/issue/{key}/transitions?expand=transitions.fields",
                self.api()
            ))
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
                &format!("{}/issue/{key}", self.api()),
                json!({ "fields": fields }),
            )
            .await?;
        Ok(())
    }

    async fn create_issue(&self, fields: Value) -> Result<(String, String)> {
        let v = self
            .http
            .post_json(
                &format!("{}/issue", self.api()),
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
            .post_json(&format!("{}/issue/{key}/transitions", self.api()), body)
            .await?;
        Ok(())
    }

    async fn add_comment(&self, key: &str, body: &RichText) -> Result<RemoteComment> {
        let v = self
            .http
            .post_json(
                &format!("{}/issue/{key}/comment", self.api()),
                json!({ "body": body.to_field_value() }),
            )
            .await?;
        one_comment(&v, self.markup())
    }

    async fn update_comment(&self, key: &str, id: &str, body: &RichText) -> Result<RemoteComment> {
        let v = self
            .http
            .put_json(
                &format!("{}/issue/{key}/comment/{id}", self.api()),
                json!({ "body": body.to_field_value() }),
            )
            .await?;
        one_comment(&v, self.markup())
    }

    async fn delete_comment(&self, key: &str, id: &str) -> Result<()> {
        self.http
            .delete(&format!("{}/issue/{key}/comment/{id}", self.api()))
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
                &format!("{}/issue/{key}/attachments", self.api()),
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

    async fn search_users(&self, query: &str) -> Result<Vec<UserRef>> {
        // Cloud searches by `query`; Data Center by `username`.
        let param = match self.deployment {
            Deployment::Cloud => "query",
            Deployment::DataCenter => "username",
        };
        let v = self
            .http
            .get_json(&format!(
                "{}/user/search?{param}={}&maxResults=20",
                self.api(),
                urlencoding::encode(query)
            ))
            .await?;
        Ok(v.as_array()
            .unwrap_or(&vec![])
            .iter()
            .filter_map(|u| normalize::user_ref(Some(u)))
            .collect())
    }
}

fn project_from(v: &Value) -> Option<Project> {
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
        .into_iter()
        .next()
        .ok_or_else(|| JiredError::generic("the server returned an unreadable comment"))
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
