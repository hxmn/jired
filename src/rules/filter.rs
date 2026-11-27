//! The rules of a saved filter: how one is found by name or id, and what a
//! JQL has to be before it is saved.
//!
//! Kept apart from the command, which parses its flags, calls these and
//! prints what came back, so the rules can be read and tested without one.

use crate::api::{Deployment, Filter, FilterListing, Filters, Issues, JiraApi};
use crate::api::{ShareInput, UserScope};
use crate::error::{JiredError, Result};
use serde_json::{json, Value};

/// The filter an id or a name points at.
///
/// Matched the way `--sprint` is: `id:10400` is the id and nothing else, and a
/// bare number is taken only when every reading of it agrees — a filter called
/// `2026` and a filter whose id is 2026 are two filters, and picking either is
/// a guess. A name that more than one filter carries, which happens as soon as
/// two people save the same query under the same name, is refused with the ids.
pub(crate) fn find(client: &dyn JiraApi, wanted: &str) -> Result<Filter> {
    let wanted = wanted.trim();
    if wanted.is_empty() {
        return Err(JiredError::usage("no filter named")
            .with_hint("pass its id or its name — `jired filters` lists them"));
    }
    if let Some(id) = wanted.strip_prefix("id:").map(str::trim) {
        return by_id(client, id)?.ok_or_else(|| not_found(client, wanted));
    }

    let mut candidates: Vec<Filter> = Vec::new();
    if is_id(wanted) {
        candidates.extend(by_id(client, wanted)?);
    }
    for f in crate::api::block_on(client.filters(FilterListing::Named(wanted)))? {
        if !candidates.iter().any(|c| c.id == f.id) {
            candidates.push(f);
        }
    }
    match candidates.len() {
        0 => Err(not_found(client, wanted)),
        1 => Ok(candidates.remove(0)),
        _ => {
            let named: Vec<String> = candidates
                .iter()
                .map(|f| {
                    let by = f
                        .owner
                        .as_ref()
                        .map(|o| format!(", owned by {}", o.display_name))
                        .unwrap_or_default();
                    format!("id:{} ({}{by})", f.id, f.name)
                })
                .collect();
            Err(JiredError::usage(format!(
                "`{wanted}` could be any of {} filters",
                named.len()
            ))
            .with_detail("filter", wanted.to_string())
            .with_detail("ambiguous", true)
            .with_detail(
                "ids",
                json!(candidates.iter().map(|f| f.id.clone()).collect::<Vec<_>>()),
            )
            .with_detail("known", json!(named))
            .with_hint(format!("name one by its id: {}", named.join(", "))))
        }
    }
}

/// The filter with this id, or `None` when the server says there is none.
///
/// Jira answers a filter that does not exist and one the account may not see
/// with the same 400, and both mean "not this one" to a lookup.
pub(crate) fn by_id(client: &dyn Filters, id: &str) -> Result<Option<Filter>> {
    if !is_id(id) {
        return Ok(None);
    }
    match crate::api::block_on(client.filter(id)) {
        Ok(f) => Ok(Some(f)),
        Err(e) if matches!(status(&e), Some(400 | 404)) => Ok(None),
        Err(e) => Err(e),
    }
}

pub(crate) fn not_found(client: &dyn JiraApi, wanted: &str) -> JiredError {
    let err = JiredError::usage(format!("`{wanted}` is not a filter this account can see"))
        .with_detail("filter", wanted.to_string());
    match client.deployment() {
        Deployment::DataCenter => err.with_hint(
            "Data Center finds a filter by name only among the account's favourites — \
             pass its id, the number in `/issues/?filter=ID`",
        ),
        Deployment::Cloud => {
            err.with_hint("`jired filters` lists the ones you own or have starred")
        }
    }
}

/// How many issues a JQL matches, asked before it is saved.
///
/// One count: the server parses the whole query, and names an unknown field
/// or value, at the cost of a number. A search with room for no issues used
/// to stand in for it, and Cloud's search reports no total.
pub(crate) fn check_jql(client: &dyn Issues, jql: &str) -> Result<usize> {
    match crate::api::block_on(client.count(jql)) {
        Ok(total) => Ok(total),
        // A 400 from search is the query, and the query is the user's input:
        // exit 2, with nothing saved.
        Err(e) if status(&e) == Some(400) => Err(JiredError::usage(format!(
            "Jira refused the JQL — {}",
            e.message
        ))
        .with_detail("jql", jql.to_string())
        .with_hint("nothing was saved; `jired search --count '…'` tries a query on its own")),
        Err(e) => Err(e),
    }
}

pub(crate) fn checked_jql(jql: &str) -> Result<String> {
    let jql = jql.trim();
    if jql.is_empty() {
        return Err(JiredError::missing_value("a filter needs a JQL")
            .with_hint("`--jql 'project = TSK AND fixVersion = \"1.0.0\"'`"));
    }
    Ok(jql.to_string())
}

pub(crate) fn status(e: &JiredError) -> Option<u64> {
    e.details.get("status").and_then(Value::as_u64)
}

pub(crate) fn is_id(text: &str) -> bool {
    !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit())
}

/// A spec with the ids the server takes in place of the names people use.
pub(crate) fn resolve(client: &dyn JiraApi, spec: &Spec) -> Result<ShareInput> {
    Ok(match spec {
        Spec::Project(key) => {
            let p = crate::api::block_on(client.project(key)).map_err(|e| {
                JiredError::usage(format!("`{key}` is not a project this account can see"))
                    .with_detail("project", key.clone())
                    .with_source(e)
            })?;
            ShareInput::Project {
                id: p.id,
                key: p.key,
            }
        }
        Spec::Group(name) => ShareInput::Group(name.clone()),
        Spec::User(query) => {
            let found = crate::api::block_on(client.search_users(query, UserScope::Any))?;
            // The id or the full name, exactly, before anything looser: `ann`
            // is a prefix of `anna`, and sharing with the wrong one is a leak.
            let exact: Vec<_> = found
                .iter()
                .filter(|u| u.id == *query || u.display_name.eq_ignore_ascii_case(query))
                .collect();
            let pick = match (exact.as_slice(), found.as_slice()) {
                ([one], _) => *one,
                ([], [one]) => one,
                ([], []) => {
                    return Err(JiredError::usage(format!("no user matches `{query}`"))
                        .with_detail("query", query.clone()))
                }
                _ => {
                    let names: Vec<String> = found
                        .iter()
                        .map(|u| format!("{} ({})", u.display_name, u.id))
                        .collect();
                    return Err(JiredError::usage(format!(
                        "`{query}` matches {} users",
                        found.len()
                    ))
                    .with_detail("matches", json!(names))
                    .with_hint(format!("pass the id — `user:{}`", found[0].id)));
                }
            };
            ShareInput::User(pick.id.clone())
        }
        Spec::Authenticated => ShareInput::Authenticated,
        Spec::Global => ShareInput::Global,
    })
}

/// A `--share` or `--add` value, before anything is looked up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Spec {
    Project(String),
    Group(String),
    User(String),
    Authenticated,
    Global,
}

impl Spec {
    pub(crate) fn parse(text: &str) -> Result<Self> {
        let text = text.trim();
        let (kind, value) = match text.split_once(':') {
            Some((k, v)) => (k.trim().to_ascii_lowercase(), v.trim()),
            None => (text.to_ascii_lowercase(), ""),
        };
        let needs = |what: &str| {
            JiredError::usage(format!("`{text}` names no {what}"))
                .with_hint(format!("`{kind}:{}`", what.to_ascii_uppercase()))
        };
        Ok(match kind.as_str() {
            "project" if value.contains('/') => {
                return Err(JiredError::usage(format!(
                    "`{text}` is a project role, which jired cannot share with"
                ))
                .with_hint(
                    "share with the whole project — `project:KEY` — or set the role in Jira",
                ))
            }
            "project" if value.is_empty() => return Err(needs("project")),
            "project" => Spec::Project(value.to_ascii_uppercase()),
            "group" if value.is_empty() => return Err(needs("group")),
            "group" => Spec::Group(value.to_string()),
            "user" if value.is_empty() => return Err(needs("user")),
            "user" => Spec::User(value.to_string()),
            // Jira's own name for it, on the way out of the API, is accepted
            // too: it is what a payload read elsewhere would say.
            "authenticated" | "loggedin" if value.is_empty() => Spec::Authenticated,
            "global" if value.is_empty() => Spec::Global,
            _ => return Err(
                JiredError::usage(format!("`{text}` is not a way to share a filter")).with_hint(
                    "one of `project:KEY`, `group:NAME`, `user:NAME`, `authenticated`, `global`",
                ),
            ),
        })
    }
}
