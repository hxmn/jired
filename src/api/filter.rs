//! Saved filters: the calls that make and change them, and their payloads.
//!
//! The calls are planned here, as data, rather than inside each client:
//! `--dry-run` has to print what a write will send, and a dry run that renders
//! its own idea of the request is one that drifts from the request. So the
//! command prints a plan from these functions, and every client — the REST one
//! on both deployments, and the fixture client — sends exactly that plan.
//!
//! The two deployments differ in three places, all visible below: Cloud stars
//! a filter with a call of its own where Data Center takes `favourite` in the
//! body; Cloud names a user to share with by account id, Data Center by
//! username; and Data Center can list only the account's favourites.

use crate::api::cloud;
use crate::api::datacenter::routes::{self, Route};
use crate::api::model::{Filter, FilterInput, FilterListing, FilterUpdate, ShareInput};
use crate::api::model::{SharePermission, UserRef};
use crate::api::{normalize, Deployment};
use serde_json::{json, Map, Value};

/// What a dry run writes for the id a filter has not been given yet.
pub const NEW: &str = "{id}";

/// Asked of every Cloud listing: `filter/search` answers with an id and a name
/// alone unless told otherwise.
pub const CLOUD_EXPAND: &str = "description,owner,jql,favourite,sharePermissions";

/// One request, as it will be sent.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Call {
    pub method: &'static str,
    pub path: String,
    /// `null` for the calls that carry no body.
    pub body: Value,
}

impl Call {
    fn new(route: &Route, d: Deployment, id: &str, more: &[(&str, &str)], body: Value) -> Self {
        let mut args = vec![("id", id)];
        args.extend_from_slice(more);
        let path = route
            .url(d.api_prefix(), &args, &[])
            // The stand-in id stays readable: `%7Bid%7D` in a dry run reads as
            // a bug in the request rather than as "the id it will be given".
            .replace("%7Bid%7D", NEW);
        Self {
            method: route.method,
            path,
            body,
        }
    }
}

/// Making a filter: the create, then on Cloud the star.
///
/// `id` is what the follow-up calls address — [`NEW`] in a dry run, the id the
/// server gave once the create has been sent.
pub fn create(d: Deployment, input: &FilterInput, id: &str) -> Vec<Call> {
    let mut body = json!({ "name": input.name, "jql": input.jql });
    if let Some(text) = input
        .description
        .as_deref()
        .filter(|t| !t.trim().is_empty())
    {
        body["description"] = json!(text);
    }
    let mut calls = Vec::new();
    match d {
        // Sent either way: Data Center stars a filter made over REST only when
        // asked, where its own "Save as" stars it without asking.
        Deployment::DataCenter => body["favourite"] = json!(input.favourite),
        Deployment::Cloud if input.favourite => calls.push(favourite(d, id, true)),
        Deployment::Cloud => {}
    }
    calls.insert(0, Call::new(&routes::CREATE_FILTER, d, id, &[], body));
    calls
}

/// Changing a filter: one `PUT` for what its body carries, and on Cloud a call
/// of its own for the star.
pub fn update(d: Deployment, filter: &Filter, update: &FilterUpdate) -> Vec<Call> {
    let star_in_body = d == Deployment::DataCenter && update.favourite.is_some();
    let mut calls = Vec::new();
    if update.name.is_some() || update.jql.is_some() || update.description.is_some() || star_in_body
    {
        let mut body = Map::new();
        // Always sent: Cloud refuses an edit without a name, even one that
        // changes nothing but the JQL. The rest is merged by the server, so a
        // field left out is one the filter keeps.
        body.insert(
            "name".into(),
            json!(update.name.as_deref().unwrap_or(&filter.name)),
        );
        if let Some(jql) = &update.jql {
            body.insert("jql".into(), json!(jql));
        }
        if let Some(text) = &update.description {
            body.insert("description".into(), json!(text));
        }
        if star_in_body {
            body.insert("favourite".into(), json!(update.favourite));
        }
        calls.push(Call::new(
            &routes::EDIT_FILTER,
            d,
            &filter.id,
            &[],
            Value::Object(body),
        ));
    }
    if let (Deployment::Cloud, Some(on)) = (d, update.favourite) {
        calls.push(favourite(d, &filter.id, on));
    }
    calls
}

/// Cloud's star, on or off. Data Center has no such call; see [`update`].
fn favourite(d: Deployment, id: &str, on: bool) -> Call {
    let route = match on {
        true => &cloud::SET_FAVOURITE_FILTER,
        false => &cloud::DELETE_FAVOURITE_FILTER,
    };
    Call::new(route, d, id, &[], Value::Null)
}

pub fn add_share(d: Deployment, id: &str, share: &ShareInput) -> Call {
    let body = match share {
        // A string, as both deployments document it.
        ShareInput::Project { id, .. } => json!({ "type": "project", "projectId": id }),
        ShareInput::Group(name) => json!({ "type": "group", "groupname": name }),
        ShareInput::User(user) => match d {
            Deployment::Cloud => json!({ "type": "user", "accountId": user }),
            Deployment::DataCenter => json!({ "type": "user", "username": user }),
        },
        // `authenticated` going in, `loggedin` coming back, on both.
        ShareInput::Authenticated => json!({ "type": "authenticated" }),
        ShareInput::Global => json!({ "type": "global" }),
    };
    Call::new(&routes::ADD_FILTER_SHARE, d, id, &[], body)
}

pub fn remove_share(d: Deployment, id: &str, permission_id: &str) -> Call {
    Call::new(
        &routes::DELETE_FILTER_SHARE,
        d,
        id,
        &[("permission-id", permission_id)],
        Value::Null,
    )
}

pub fn delete(d: Deployment, id: &str) -> Call {
    Call::new(&routes::DELETE_FILTER, d, id, &[], Value::Null)
}

/// Where a person opens the filter: the issue navigator with it applied, which
/// is the same path on both deployments.
pub fn browse_url(base: &str, id: &str) -> String {
    format!("{}/issues/?filter={id}", base.trim_end_matches('/'))
}

/// Data Center's answer to every listing, out of the one it serves.
///
/// Its description has no `filter/my` and no `filter/search`, so the account's
/// favourites are the whole of what can be asked; "mine" and "named X" are read
/// out of those, and a filter nobody starred cannot be found by name at all.
pub fn narrow(listing: FilterListing<'_>, favourites: Vec<Filter>) -> Vec<Filter> {
    favourites
        .into_iter()
        .filter(|f| match listing {
            FilterListing::Favourites => true,
            FilterListing::Owned(me) => f.owner.as_ref().is_some_and(|o| o.id == me),
            FilterListing::Named(name) => f.name.eq_ignore_ascii_case(name.trim()),
        })
        .collect()
}

/// One filter out of a platform-API payload, on either deployment.
pub fn filter_from_json(v: &Value) -> Option<Filter> {
    let text = |name: &str| {
        v.get(name)
            .and_then(Value::as_str)
            .map(str::to_string)
            .filter(|t| !t.trim().is_empty())
    };
    Some(Filter {
        id: id_of(v.get("id")?)?,
        name: v.get("name")?.as_str()?.to_string(),
        description: text("description"),
        owner: normalize::user_ref(v.get("owner")),
        jql: text("jql").unwrap_or_default(),
        favourite: v.get("favourite").and_then(Value::as_bool).unwrap_or(false),
        share_permissions: v
            .get("sharePermissions")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(share_from_json).collect())
            .unwrap_or_default(),
    })
}

/// Every filter in a listing, which is a bare array (`filter/favourite`) or a
/// page with the rows under `values` (`filter/search`).
pub fn filters_from_json(v: &Value) -> Vec<Filter> {
    v.as_array()
        .or_else(|| v.get("values").and_then(Value::as_array))
        .map(|a| a.iter().filter_map(filter_from_json).collect())
        .unwrap_or_default()
}

fn share_from_json(v: &Value) -> Option<SharePermission> {
    let name_of = |field: &str, key: &str| {
        v.get(field)
            .and_then(|o| o.get(key))
            .and_then(Value::as_str)
            .map(str::to_string)
    };
    let project = name_of("project", "key");
    let role = name_of("role", "name");
    let group = name_of("group", "name");
    let user: Option<UserRef> = normalize::user_ref(v.get("user"));
    // Data Center calls a role share `project` with a `role` beside it; Cloud
    // calls it `projectRole`. Logged-in users are `loggedin` on the way out of
    // both and `authenticated` on the way in, and the way in is what a reader
    // types back.
    let kind = match v.get("type")?.as_str()? {
        "project" | "projectRole" if role.is_some() => "project_role",
        "loggedin" => "authenticated",
        other => other,
    }
    .to_string();
    let spec = match kind.as_str() {
        "project" => format!("project:{}", project.clone().unwrap_or_default()),
        "project_role" => format!(
            "project:{}/{}",
            project.clone().unwrap_or_default(),
            role.clone().unwrap_or_default()
        ),
        "group" => format!("group:{}", group.clone().unwrap_or_default()),
        "user" => format!(
            "user:{}",
            user.as_ref().map(|u| u.id.clone()).unwrap_or_default()
        ),
        other => other.to_string(),
    };
    Some(SharePermission {
        id: v.get("id").and_then(id_of),
        kind,
        spec,
        project,
        role,
        group,
        user,
    })
}

fn id_of(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn filter(id: &str, name: &str, owner: &str, favourite: bool) -> Filter {
        Filter {
            id: id.into(),
            name: name.into(),
            description: None,
            owner: Some(UserRef {
                id: owner.into(),
                display_name: owner.into(),
            }),
            jql: "project = TSK".into(),
            favourite,
            share_permissions: vec![],
        }
    }

    fn input(favourite: bool) -> FilterInput {
        FilterInput {
            name: "TSK 1.0.0".into(),
            jql: "project = TSK".into(),
            description: None,
            favourite,
        }
    }

    /// The star is the one part of making a filter the two deployments spell
    /// differently.
    #[test]
    fn data_center_stars_a_filter_in_its_body_and_cloud_with_a_call() {
        let dc = create(Deployment::DataCenter, &input(true), NEW);
        assert_eq!(dc.len(), 1);
        assert_eq!(dc[0].path, "/rest/api/2/filter");
        assert_eq!(dc[0].body["favourite"], true);

        let cloud = create(Deployment::Cloud, &input(true), NEW);
        assert_eq!(cloud.len(), 2);
        assert!(cloud[0].body.get("favourite").is_none());
        assert_eq!(cloud[1].method, "PUT");
        assert_eq!(cloud[1].path, "/rest/api/3/filter/{id}/favourite");

        // Not starred is not a call at all on Cloud.
        assert_eq!(create(Deployment::Cloud, &input(false), NEW).len(), 1);
    }

    #[test]
    fn an_edit_sends_the_name_and_only_what_changes() {
        let f = filter("10400", "TSK 1.0.0", "ilya", false);
        let jql_only = FilterUpdate {
            jql: Some("project = TSK AND type = Bug".into()),
            ..Default::default()
        };
        let calls = update(Deployment::Cloud, &f, &jql_only);
        assert_eq!(calls.len(), 1);
        assert_eq!(
            calls[0].body,
            json!({ "name": "TSK 1.0.0", "jql": "project = TSK AND type = Bug" })
        );

        // A star alone is no PUT of the filter on Cloud.
        let star = FilterUpdate {
            favourite: Some(false),
            ..Default::default()
        };
        let calls = update(Deployment::Cloud, &f, &star);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].method, "DELETE");
        assert_eq!(calls[0].path, "/rest/api/3/filter/10400/favourite");
        let calls = update(Deployment::DataCenter, &f, &star);
        assert_eq!(
            calls[0].body,
            json!({ "name": "TSK 1.0.0", "favourite": false })
        );
    }

    #[test]
    fn shares_read_back_in_the_spelling_they_are_given_in() {
        let v = json!({
            "id": "10400",
            "name": "TSK 1.0.0",
            "jql": "project = TSK",
            "owner": { "name": "ilya", "displayName": "Ilya A" },
            "sharePermissions": [
                { "id": 1, "type": "project", "project": { "id": "10200", "key": "TSK" } },
                { "id": 2, "type": "project", "project": { "key": "TSK" }, "role": { "name": "Developers" } },
                { "id": 3, "type": "projectRole", "project": { "key": "TSK" }, "role": { "name": "QA" } },
                { "id": 4, "type": "group", "group": { "name": "jira-users" } },
                { "id": 5, "type": "loggedin" },
                { "id": 6, "type": "global" },
                { "id": 7, "type": "user", "user": { "accountId": "5b10", "displayName": "Ann" } },
            ]
        });
        let f = filter_from_json(&v).unwrap();
        let specs: Vec<&str> = f
            .share_permissions
            .iter()
            .map(|s| s.spec.as_str())
            .collect();
        assert_eq!(
            specs,
            [
                "project:TSK",
                "project:TSK/Developers",
                "project:TSK/QA",
                "group:jira-users",
                "authenticated",
                "global",
                "user:5b10"
            ]
        );
        assert_eq!(f.share_permissions[0].id.as_deref(), Some("1"));
        assert_eq!(f.owner.unwrap().id, "ilya");
    }

    #[test]
    fn data_center_reads_mine_and_named_out_of_the_favourites() {
        let all = || {
            vec![
                filter("1", "Bugs", "ilya", true),
                filter("2", "bugs", "ann", true),
                filter("3", "Ready", "ann", true),
            ]
        };
        let ids = |v: Vec<Filter>| v.into_iter().map(|f| f.id).collect::<Vec<_>>();
        assert_eq!(ids(narrow(FilterListing::Owned("ilya"), all())), ["1"]);
        assert_eq!(ids(narrow(FilterListing::Named("BUGS"), all())), ["1", "2"]);
        assert_eq!(ids(narrow(FilterListing::Favourites, all())).len(), 3);
    }
}
