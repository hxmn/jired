//! Every REST route jired calls, named as the server names it [M6.3, D27].
//!
//! Atlassian publishes no OpenAPI document for Data Center, but a Jira
//! describes itself: Jersey serves a WADL beside each REST module it mounts,
//! listing every resource, method and query parameter. `jired fetch-spec`
//! fetches those; `specs/datacenter/10.7.4/` carries a recorded set; and
//! `tests/dc_spec_contract.rs` checks this table against it, so a route that
//! does not exist, a method spelled wrong, or a parameter the server never
//! declared fails the build rather than a user's push.
//!
//! Written by hand rather than generated, because there is nothing to generate
//! from: the WADL carries no grammars and no `element=` attributes, so it
//! describes paths and parameters and not one payload. A table a reader can
//! search is worth more than a build step that produces the same strings.
//!
//! Two things to know before concluding the server lacks a route. The WADL
//! Jersey serves by default is the *simplified* one — "user and core resources
//! only" — which omits the generated `OPTIONS` and `HEAD` methods and the
//! `application.wadl` resource itself; `…/application.wadl?detail=true` is the
//! full listing. And `expand` is a Jira-wide parameter that Jersey does not
//! declare per method even where the server honours it, which is why the
//! contract test carries a small, justified allowlist for it.
//!
//! Cloud is served from the same table wherever the two agree, which is
//! everywhere but a handful of routes; those live in [`crate::api::cloud`] and
//! are not contract-tested, there being no Cloud description to test against.

/// Which REST module a route is mounted on. The prefix differs per deployment
/// — `/rest/api/2` on Data Center, `/rest/api/3` on Cloud — while the Agile
/// API is `/rest/agile/1.0` on both.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Module {
    Api,
    Agile,
}

/// One route, as the server's own description declares it.
#[derive(Debug, Clone, Copy)]
pub struct Route {
    /// The WADL `method id` — `editIssue`, `findAssignableUsers`. Documentation
    /// for a reader, and the contract test's secondary check: a path that still
    /// exists under a method that has been renamed is a change worth seeing.
    pub id: &'static str,
    pub method: &'static str,
    pub module: Module,
    /// Module-relative, spelled with the WADL's own template names so the two
    /// can be compared without translation: `issue/{issueIdOrKey}/comment/{id}`.
    pub path: &'static str,
    /// The query parameters the server declares for this method. Sending one
    /// that is not here is a bug in jired, and [`Route::url`] says so.
    pub query: &'static [&'static str],
}

impl Route {
    /// The path to request: `prefix` (`/rest/api/2`), the template with every
    /// `{name}` replaced by its argument, and the query string.
    ///
    /// Arguments and query values are percent-encoded, since an issue key is
    /// whatever the project's administrator made it and a JQL query is
    /// whatever the user typed.
    ///
    /// A missing argument or an undeclared query name is a programming error
    /// rather than a condition a caller could handle — the names are fixed at
    /// compile time — so it trips a `debug_assert!` and shows up under `cargo
    /// test`, instead of putting a `?` on every call site for an error no user
    /// can act on.
    ///
    /// Query pairs are emitted in the order given, never sorted: the tests that
    /// pin Cloud's URLs read them as strings.
    pub fn url(&self, prefix: &str, args: &[(&str, &str)], query: &[(&str, &str)]) -> String {
        let mut path = String::with_capacity(prefix.len() + self.path.len() + 16);
        path.push_str(prefix);
        for segment in self.path.split('/') {
            path.push('/');
            match segment.strip_prefix('{').and_then(|s| s.strip_suffix('}')) {
                Some(name) => {
                    let value = args.iter().find(|(n, _)| *n == name).map(|(_, v)| *v);
                    debug_assert!(
                        value.is_some(),
                        "{}: no argument for {{{name}}} in {}",
                        self.id,
                        self.path
                    );
                    path.push_str(&escape(value.unwrap_or_default()));
                }
                None => path.push_str(segment),
            }
        }
        for (i, (name, value)) in query.iter().enumerate() {
            debug_assert!(
                self.query.contains(name),
                "{}: the server does not declare a `{name}` parameter on {} {} \
                 (it declares {:?})",
                self.id,
                self.method,
                self.path,
                self.query
            );
            path.push(if i == 0 { '?' } else { '&' });
            path.push_str(name);
            path.push('=');
            path.push_str(&escape(value));
        }
        path
    }
}

/// Percent-encode a path segment or a query value.
///
/// RFC 3986's unreserved set, plus `*`. The asterisk is exempt because
/// `fields=*all` is how every Jira client asks for the whole issue and how
/// jired has always asked: it is a sub-delimiter, legal unencoded in a query,
/// and sending `%2A` instead would be a change to the wire format for no gain.
/// Everything else is escaped, because the values reaching here include issue
/// keys an administrator chose and search terms a user typed.
fn escape(value: &str) -> std::borrow::Cow<'_, str> {
    if value
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~' | b'*'))
    {
        return std::borrow::Cow::Borrowed(value);
    }
    let mut out = String::with_capacity(value.len() + 8);
    for byte in value.bytes() {
        match byte {
            b if b.is_ascii_alphanumeric() => out.push(byte as char),
            b'-' | b'.' | b'_' | b'~' | b'*' => out.push(byte as char),
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    std::borrow::Cow::Owned(out)
}

/// Shorthand for a table entry. The field order is the order a reader wants to
/// read them in: what it is called, how it is called, and what it takes.
macro_rules! route {
    ($(#[$doc:meta])* $name:ident, $id:literal, $method:literal, $module:ident, $path:literal $(, $query:literal)* $(,)?) => {
        $(#[$doc])*
        pub const $name: Route = Route {
            id: $id,
            method: $method,
            module: Module::$module,
            path: $path,
            query: &[$($query),*],
        };
    };
}

// ---- The platform API ----------------------------------------------------

route!(SERVER_INFO, "getServerInfo", "GET", Api, "serverInfo");
route!(MYSELF, "getUser", "GET", Api, "myself");
route!(FIELDS, "getFields", "GET", Api, "field");
route!(
    PROJECTS,
    "getAllProjects",
    "GET",
    Api,
    "project",
    "expand",
    "recent",
    "includeArchived",
    "browseArchive",
);

route!(
    /// Search is a POST because a JQL naming a few dozen issue keys does not fit in
    /// a query string, and `fetch` builds exactly that.
    SEARCH, "searchUsingSearchRequest", "POST", Api, "search");

route!(
    GET_ISSUE,
    "getIssue",
    "GET",
    Api,
    "issue/{issueIdOrKey}",
    "fields",
    "expand",
    "properties",
    "updateHistory",
);
route!(
    EDIT_ISSUE,
    "editIssue",
    "PUT",
    Api,
    "issue/{issueIdOrKey}",
    "notifyUsers",
);
route!(
    CREATE_ISSUE,
    "createIssue",
    "POST",
    Api,
    "issue",
    "updateHistory"
);

route!(
    /// The fields an issue's edit screen carries, which is not the same as the
    /// fields the issue has.
    EDIT_META,
    "getEditIssueMeta",
    "GET",
    Api,
    "issue/{issueIdOrKey}/editmeta",
);
route!(
    CREATE_META_TYPES,
    "getCreateIssueMetaProjectIssueTypes",
    "GET",
    Api,
    "issue/createmeta/{projectIdOrKey}/issuetypes",
    "startAt",
    "maxResults",
);
route!(
    CREATE_META_FIELDS,
    "getCreateIssueMetaFields",
    "GET",
    Api,
    "issue/createmeta/{projectIdOrKey}/issuetypes/{issueTypeId}",
    "startAt",
    "maxResults",
);

route!(
    /// Assigning has a route of its own, and it is not the field edit: an assignee
    /// missing from the edit screen is still assignable through this one.
    ASSIGN, "assign", "PUT", Api, "issue/{issueIdOrKey}/assignee");

route!(
    /// `expand` is not declared here — see the module doc and the contract test's
    /// allowlist — but `expand=transitions.fields` is what carries which fields a
    /// transition demands, and the server honours it.
    TRANSITIONS,
    "getTransitions",
    "GET",
    Api,
    "issue/{issueIdOrKey}/transitions",
    "transitionId",
    "expand",
);
route!(
    DO_TRANSITION,
    "doTransition",
    "POST",
    Api,
    "issue/{issueIdOrKey}/transitions",
);

route!(
    GET_COMMENTS,
    "getComments",
    "GET",
    Api,
    "issue/{issueIdOrKey}/comment",
    "startAt",
    "maxResults",
    "orderBy",
    "expand",
);
route!(
    ADD_COMMENT,
    "addComment",
    "POST",
    Api,
    "issue/{issueIdOrKey}/comment",
    "expand",
);
route!(
    UPDATE_COMMENT,
    "updateComment",
    "PUT",
    Api,
    "issue/{issueIdOrKey}/comment/{id}",
    "expand",
);
route!(
    DELETE_COMMENT,
    "deleteComment",
    "DELETE",
    Api,
    "issue/{issueIdOrKey}/comment/{id}",
);

route!(
    /// Data Center declares no paging on this one and answers with every worklog,
    /// which is why the Data Center client asks once and Cloud's loops.
    GET_WORKLOGS,
    "getIssueWorklog",
    "GET",
    Api,
    "issue/{issueIdOrKey}/worklog",
);
route!(
    ADD_WORKLOG,
    "addWorklog",
    "POST",
    Api,
    "issue/{issueIdOrKey}/worklog",
    "adjustEstimate",
    "newEstimate",
    "reduceBy",
);
route!(
    UPDATE_WORKLOG,
    "updateWorklog",
    "PUT",
    Api,
    "issue/{issueIdOrKey}/worklog/{id}",
    "adjustEstimate",
    "newEstimate",
);
route!(
    DELETE_WORKLOG,
    "deleteWorklog",
    "DELETE",
    Api,
    "issue/{issueIdOrKey}/worklog/{id}",
    "adjustEstimate",
    "newEstimate",
    "increaseBy",
);

route!(
    /// The instance's link types, asked for once per push and only when a
    /// `links:` line changed: the directed descriptions a file spells a link
    /// with are per instance, not a fixed vocabulary.
    ISSUE_LINK_TYPES,
    "getIssueLinkTypes",
    "GET",
    Api,
    "issueLinkType",
);
route!(CREATE_ISSUE_LINK, "linkIssues", "POST", Api, "issueLink");
route!(
    /// By the link's own id — the one each `issuelinks` entry carries — which
    /// is why deleting needs the snapshot the link was last seen in.
    DELETE_ISSUE_LINK,
    "deleteIssueLink",
    "DELETE",
    Api,
    "issueLink/{linkId}",
);

route!(
    ADD_ATTACHMENT,
    "addAttachment",
    "POST",
    Api,
    "issue/{issueIdOrKey}/attachments",
);

route!(
    /// Adding a watcher is a POST whose whole body is the user's identifier as
    /// a bare JSON string — `"jane"` on Data Center, an account id on Cloud.
    ADD_WATCHER,
    "addWatcher",
    "POST",
    Api,
    "issue/{issueIdOrKey}/watchers",
);
route!(
    /// Removing one names the user in the query string, and this is where the
    /// two deployments part: Data Center declares `username`, Cloud takes
    /// `accountId` — see [`crate::api::cloud::REMOVE_WATCHER`].
    REMOVE_WATCHER,
    "removeWatcher",
    "DELETE",
    Api,
    "issue/{issueIdOrKey}/watchers",
    "username",
);

route!(
    FIND_USERS,
    "findUsers",
    "GET",
    Api,
    "user/search",
    "username",
    "startAt",
    "maxResults",
    "includeActive",
    "includeInactive",
);
route!(
    /// Who can be assigned this issue, which is a smaller set than who exists.
    FIND_ASSIGNABLE_USERS,
    "findAssignableUsers",
    "GET",
    Api,
    "user/assignable/search",
    "username",
    "project",
    "issueKey",
    "maxResults",
    "actionDescriptorId",
);

// ---- The Agile API -------------------------------------------------------

route!(
    BOARDS,
    "getAllBoards",
    "GET",
    Agile,
    "board",
    "startAt",
    "maxResults",
    "type",
    "name",
    "projectKeyOrId",
);
route!(
    BOARD_CONFIGURATION,
    "getConfiguration",
    "GET",
    Agile,
    "board/{boardId}/configuration",
);
route!(
    BOARD_PROJECTS,
    "getProjects",
    "GET",
    Agile,
    "board/{boardId}/project",
    "startAt",
    "maxResults",
);
route!(
    BOARD_SPRINTS,
    "getAllSprints",
    "GET",
    Agile,
    "board/{boardId}/sprint",
    "startAt",
    "maxResults",
    "state",
);
route!(CREATE_SPRINT, "createSprint", "POST", Agile, "sprint");
route!(GET_SPRINT, "getSprint", "GET", Agile, "sprint/{sprintId}");
route!(
    /// Starting, closing, renaming and re-goaling a sprint are all this one
    /// call: the endpoint merges, so what is not sent is what the sprint keeps.
    /// `PUT updateSprint` beside it replaces instead, which would silently drop
    /// the fields a caller did not know to resend.
    UPDATE_SPRINT,
    "partiallyUpdateSprint",
    "POST",
    Agile,
    "sprint/{sprintId}",
);
route!(
    /// Moving an issue into a sprint is a board call, not a field edit: the Sprint
    /// field is read-only on most instances.
    SPRINT_ISSUES,
    "moveIssuesToSprint",
    "POST",
    Agile,
    "sprint/{sprintId}/issue",
);
route!(
    BACKLOG_ISSUES,
    "moveIssuesToBacklog",
    "POST",
    Agile,
    "backlog/issue",
);

/// Every route above, for the contract test to check and for a reader to count.
pub const ALL: &[&Route] = &[
    &SERVER_INFO,
    &MYSELF,
    &FIELDS,
    &PROJECTS,
    &SEARCH,
    &GET_ISSUE,
    &EDIT_ISSUE,
    &CREATE_ISSUE,
    &EDIT_META,
    &CREATE_META_TYPES,
    &CREATE_META_FIELDS,
    &ASSIGN,
    &TRANSITIONS,
    &DO_TRANSITION,
    &GET_COMMENTS,
    &ADD_COMMENT,
    &UPDATE_COMMENT,
    &DELETE_COMMENT,
    &GET_WORKLOGS,
    &ADD_WORKLOG,
    &UPDATE_WORKLOG,
    &DELETE_WORKLOG,
    &ISSUE_LINK_TYPES,
    &CREATE_ISSUE_LINK,
    &DELETE_ISSUE_LINK,
    &ADD_ATTACHMENT,
    &ADD_WATCHER,
    &REMOVE_WATCHER,
    &FIND_USERS,
    &FIND_ASSIGNABLE_USERS,
    &BOARDS,
    &BOARD_CONFIGURATION,
    &BOARD_PROJECTS,
    &BOARD_SPRINTS,
    &CREATE_SPRINT,
    &GET_SPRINT,
    &UPDATE_SPRINT,
    &SPRINT_ISSUES,
    &BACKLOG_ISSUES,
];

/// What jired reaches that is not in the REST API, and why each one is.
///
/// Listed here rather than only in the code that calls them, so that a reader
/// comparing this table against the server's description finds the exceptions
/// in the same place as the rule. The contract test asserts none of these is
/// a route — if Atlassian ever gives one a REST equivalent, that test is where
/// the news arrives.
pub const OUTSIDE_THE_SPEC: &[(&str, &str)] = &[
    (
        "/secure/MoveSubTaskParent!default.jspa",
        "re-parenting a sub-task: a web action, because REST has never offered \
         one (JRASERVER-68763)",
    ),
    (
        "/secure/attachment/",
        "attachment bytes: the payload's own `content` URL, since \
         `GET attachment/{id}` returns metadata only",
    ),
    (
        "/rest/api/2/application.wadl",
        "the description itself: an extended resource the simplified WADL \
         Jersey serves by default leaves out",
    ),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_url_substitutes_and_encodes_its_arguments() {
        assert_eq!(
            GET_ISSUE.url(
                "/rest/api/2",
                &[("issueIdOrKey", "DC-77")],
                &[("fields", "*all")]
            ),
            "/rest/api/2/issue/DC-77?fields=*all"
        );
        assert_eq!(
            UPDATE_COMMENT.url(
                "/rest/api/2",
                &[("issueIdOrKey", "DC-77"), ("id", "10500")],
                &[],
            ),
            "/rest/api/2/issue/DC-77/comment/10500"
        );
        // Several parameters keep the order they were given, because the tests
        // that pin these URLs read them as strings.
        assert_eq!(
            BOARD_SPRINTS.url(
                "/rest/agile/1.0",
                &[("boardId", "12")],
                &[("startAt", "0"), ("maxResults", "50")],
            ),
            "/rest/agile/1.0/board/12/sprint?startAt=0&maxResults=50"
        );
        // A key is whatever an administrator made it, and a JQL is whatever the
        // user typed; neither may end the path early or start a parameter.
        assert_eq!(
            GET_ISSUE.url("/rest/api/2", &[("issueIdOrKey", "A B/C")], &[]),
            "/rest/api/2/issue/A%20B%2FC"
        );
        assert_eq!(
            FIND_USERS.url("/rest/api/2", &[], &[("username", "a&b=c")]),
            "/rest/api/2/user/search?username=a%26b%3Dc"
        );
    }

    #[test]
    #[should_panic(expected = "the server does not declare")]
    fn an_undeclared_query_parameter_is_a_programming_error() {
        // `getIssue` takes `fields`; `field` is the mistake this catches.
        GET_ISSUE.url(
            "/rest/api/2",
            &[("issueIdOrKey", "DC-77")],
            &[("field", "*all")],
        );
    }

    #[test]
    #[should_panic(expected = "no argument for")]
    fn a_missing_path_argument_is_a_programming_error() {
        GET_ISSUE.url("/rest/api/2", &[], &[]);
    }

    /// A route nothing lists is a route the contract never checks, which is the
    /// one way this table could quietly stop describing what jired sends.
    #[test]
    fn every_route_is_in_the_table() {
        let listed: Vec<&str> = ALL.iter().map(|r| r.id).collect();
        for id in [
            "getServerInfo",
            "getUser",
            "getFields",
            "getAllProjects",
            "searchUsingSearchRequest",
            "getIssue",
            "editIssue",
            "createIssue",
            "getEditIssueMeta",
            "getCreateIssueMetaProjectIssueTypes",
            "getCreateIssueMetaFields",
            "assign",
            "getTransitions",
            "doTransition",
            "getComments",
            "addComment",
            "updateComment",
            "deleteComment",
            "getIssueWorklog",
            "addWorklog",
            "updateWorklog",
            "deleteWorklog",
            "getIssueLinkTypes",
            "linkIssues",
            "deleteIssueLink",
            "addAttachment",
            "addWatcher",
            "removeWatcher",
            "findUsers",
            "findAssignableUsers",
            "getAllBoards",
            "getConfiguration",
            "getProjects",
            "getAllSprints",
            "createSprint",
            "getSprint",
            "partiallyUpdateSprint",
            "moveIssuesToSprint",
            "moveIssuesToBacklog",
        ] {
            assert!(listed.contains(&id), "{id} is missing from ALL");
        }
        assert_eq!(ALL.len(), 39, "a route was added without a test naming it");

        // `getIssue` is a method id in both modules, so nothing may key on the
        // id alone — the contract test keys on (method, path).
        assert_eq!(
            ALL.iter().filter(|r| r.module == Module::Agile).count(),
            9,
            "the Agile routes: boards, configuration, projects, sprints, \
             create, get one, update one, sprint issues, backlog issues"
        );
    }
}
