# M6 — Data Center against its own spec (size: L)

**Goal:** every REST call jired makes to Jira Data Center is an entry in a
route table that a test checks against the server's own API description (the
WADLs a Jira DC 10.7.4 publishes for `api/2`, `agile/1.0` and `auth/1`), plus
the four behaviours that description makes possible — assignable-user search, a
complete comment list, a dedicated assignee route, and screen checks before a
push — and the support floor raised from 9.0 to 10.0.

Depends on: M5's contract tests (schema drift, agent-docs drift, no-secrets),
which are the gates every task here runs through.

## Status

Planned. Ships as **0.6.0** (Breaking: the floor, the `assign` op, the screen
refusal). M6.1–M6.5 change no command surface and could go first as 0.5.1;
M6.6–M6.9 ship together.

Where the code stands today: the Data Center calls live in the *shared*
`Rest` (`src/api/rest.rs`) behind four `match self.deployment` branches
(`projects`, `board_project`, `move_subtask_parent`, `search_users`); every
path is a `format!` literal; `src/api/datacenter.rs` holds only the Move-wizard
scrape, the WADL module list and two payload helpers. The only WADLs in the
tree are hand-made Jira 9.12.4 stubs (nine methods, Jersey 1 shape) that
exercise `fetch-spec`; nothing parses them.

Auditing every path against the 10.7.4 description found:

- every route jired uses exists, and nothing it uses was removed between 9.12
  and 10.7 (the only declaration gone is `serverInfo?doHealthCheck`, never sent);
- two calls send parameters the server does not declare: `expand=transitions.fields`
  on `GET issue/{k}/transitions` (a Jira-framework-wide parameter Jersey never
  lists per method, and one the server honours) and `startAt`/`maxResults` on
  `GET issue/{k}/worklog`, which on Data Center declares no paging and returns
  every worklog in one answer;
- the description offers routes jired ignores: `user/assignable/search` (the
  users who *can* be assigned, where `user/search` returns everyone),
  `PUT issue/{k}/assignee`, a paged `GET issue/{k}/comment`, `GET issue/{k}/editmeta`
  and `GET issue/createmeta/{project}/issuetypes/{type}`;
- `issue/{k}/subtask/move` is sub-task *reordering*, not re-parenting, so the
  Move wizard stays ([JRASERVER-68763] is still open), together with the
  credential gate 0.5.1 put in front of it (`AuthKind::opens_web_session`: a
  PAT authenticates the REST API and not Jira's web pages). The `auth/1`
  description's `POST session` takes a username and password, so it opens no
  door a PAT holder is not already refused at, and stays uncalled;
- the WADL is Jersey's *simplified* listing ("user and core resources only";
  `?detail=true` adds the generated `OPTIONS`/`HEAD` methods and the
  `application.wadl` meta-resource itself) and carries no grammars and no
  `element=` attributes — it is a route inventory with no payload schema, which
  is why the table below is written by hand and checked, rather than generated.

[JRASERVER-68763]: https://jira.atlassian.com/browse/JRASERVER-68763

## Acceptance criteria

- [ ] `tests/dc_spec_contract.rs` parses the vendored 10.7.4 WADLs and passes; a route with a wrong method or an undeclared query parameter fails it naming the route (verified once by breaking the table by hand)
- [ ] `src/api/rest.rs` has no `match self.deployment` on control flow; every Data Center path is rendered by `Route::url` from `src/api/datacenter/routes.rs`
- [ ] `jired assign --user` searches `user/assignable/search`, scoped to the issue, on both deployments
- [ ] An issue whose payload pages its comments comes back complete on both deployments
- [ ] An `assignee:` edit is its own `assign` op on `PUT /issue/{key}/assignee`, verified on the re-read like `parent`
- [ ] A field the edit or create screen lacks is refused at plan time on Data Center, exit 2, naming the field
- [ ] `MIN_DC_MAJOR == 10`; a 9.x server is refused at `init` with exit 10
- [ ] Data Center worklogs are fetched in one call, with no paging parameters
- [ ] README, `docs/plan/commands.md`, `docs/plan/architecture.md`, `docs/plan/README.md`, the `init-agents` prose, `schemas/push.json`, CHANGELOG 0.6.0 and `Cargo.toml`/`Cargo.lock` updated
- [ ] `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test --all-targets` and `cargo deny check` clean on Linux, macOS and Windows

## Design

### D27 — a hand-written route table, checked against the server's WADL

`src/api/datacenter/routes.rs`:

```rust
/// Which REST module a route is mounted on; the prefix differs per deployment.
pub enum Module { Api, Agile }

pub struct Route {
    /// The WADL `method id` — documentation, and the contract's secondary check.
    pub id: &'static str,
    pub method: &'static str,
    pub module: Module,
    /// Module-relative, with the WADL's own template names:
    /// `issue/{issueIdOrKey}/comment/{id}`.
    pub path: &'static str,
    /// Query parameters the WADL declares — plus `expand` where the contract
    /// test allowlists it.
    pub query: &'static [&'static str],
}

impl Route {
    /// `prefix` is `/rest/api/2`, `/rest/api/3` or `/rest/agile/1.0`. Every
    /// `{name}` in `path` is replaced by the matching, percent-encoded arg.
    /// A missing arg or an undeclared query name is a programming error —
    /// `debug_assert!` — because the set of names jired sends is static.
    /// Query pairs are emitted in the order given, never sorted.
    pub fn url(&self, prefix: &str, args: &[(&str, &str)], query: &[(&str, &str)]) -> String
}

pub const ALL: &[&Route] = &[ /* every const below */ ];
```

The table (id · method · path · declared query):

| Module | id | method | path | query |
|---|---|---|---|---|
| api | `getServerInfo` | GET | `serverInfo` | — |
| api | `getUser` | GET | `myself` | — |
| api | `getFields` | GET | `field` | — |
| api | `getAllProjects` | GET | `project` | `expand, recent, includeArchived, browseArchive` |
| api | `searchUsingSearchRequest` | POST | `search` | — |
| api | `getIssue` | GET | `issue/{issueIdOrKey}` | `fields, expand, properties, updateHistory` |
| api | `editIssue` | PUT | `issue/{issueIdOrKey}` | `notifyUsers` |
| api | `createIssue` | POST | `issue` | `updateHistory` |
| api | `getEditIssueMeta` | GET | `issue/{issueIdOrKey}/editmeta` | — |
| api | `getCreateIssueMetaProjectIssueTypes` | GET | `issue/createmeta/{projectIdOrKey}/issuetypes` | `startAt, maxResults` |
| api | `getCreateIssueMetaFields` | GET | `issue/createmeta/{projectIdOrKey}/issuetypes/{issueTypeId}` | `startAt, maxResults` |
| api | `assign` | PUT | `issue/{issueIdOrKey}/assignee` | — |
| api | `getTransitions` | GET | `issue/{issueIdOrKey}/transitions` | `transitionId` (+ `expand`, allowlisted) |
| api | `doTransition` | POST | `issue/{issueIdOrKey}/transitions` | — |
| api | `getComments` | GET | `issue/{issueIdOrKey}/comment` | `startAt, maxResults, orderBy, expand` |
| api | `addComment` | POST | `issue/{issueIdOrKey}/comment` | `expand` |
| api | `updateComment` | PUT | `issue/{issueIdOrKey}/comment/{id}` | `expand` |
| api | `deleteComment` | DELETE | `issue/{issueIdOrKey}/comment/{id}` | — |
| api | `getIssueWorklog` | GET | `issue/{issueIdOrKey}/worklog` | — |
| api | `addWorklog` | POST | `issue/{issueIdOrKey}/worklog` | `adjustEstimate, newEstimate, reduceBy` |
| api | `updateWorklog` | PUT | `issue/{issueIdOrKey}/worklog/{id}` | `adjustEstimate, newEstimate` |
| api | `deleteWorklog` | DELETE | `issue/{issueIdOrKey}/worklog/{id}` | `adjustEstimate, newEstimate, increaseBy` |
| api | `addAttachment` | POST | `issue/{issueIdOrKey}/attachments` | — (multipart) |
| api | `findUsers` | GET | `user/search` | `username, startAt, maxResults, includeActive, includeInactive` |
| api | `findAssignableUsers` | GET | `user/assignable/search` | `username, project, issueKey, maxResults, actionDescriptorId` |
| agile | `getAllBoards` | GET | `board` | `startAt, maxResults, type, name, projectKeyOrId` |
| agile | `getConfiguration` | GET | `board/{boardId}/configuration` | — |
| agile | `getProjects` | GET | `board/{boardId}/project` | `startAt, maxResults` |
| agile | `getAllSprints` | GET | `board/{boardId}/sprint` | `startAt, maxResults, state` |
| agile | `createSprint` | POST | `sprint` | — |
| agile | `moveIssuesToSprint` | POST | `sprint/{sprintId}/issue` | — |
| agile | `moveIssuesToBacklog` | POST | `backlog/issue` | — |

`auth/1` is vendored so `fetch-spec` has all three modules to compare against;
nothing calls it, so it has no entry.

Three things jired reaches that are outside the description, by design, and are
listed as such in the contract test so a reader finds them next to the table:
the Move wizard (`/secure/MoveSubTaskParent!default.jspa`, a web action —
there is no REST route that re-parents a sub-task), attachment bytes (the
payload's `content` URL under `/secure/attachment/`; `GET attachment/{id}` is
metadata only), and `/rest/*/application.wadl` itself (an "extended resource"
the simplified listing leaves out).

**The contract test** — `tests/dc_spec_contract.rs`, on a dev-dependency
`roxmltree` (pure Rust, no dependencies, MIT/Apache; its `has_tag_name` matches
on the local name, so the Jersey 2 default-namespace files and the Jersey 1
`ns2:`-prefixed style parse alike). The parser finds `<resources>`, walks the
nested `<resource path>` elements joining segments with `/` (trimming the
leading slash some carry), keys each `<method>` by *(METHOD, module-relative
path)* — method ids are not unique, `getIssue` is in both modules — and
collects `<request><param style="query" name>`. Files are found from
`env!("CARGO_MANIFEST_DIR")` so Windows agrees. Five tests:

1. `every_route_jired_uses_is_declared_by_the_server_with_the_same_method` —
   the failure lists near-misses (same path, another method).
2. `no_route_sends_a_query_parameter_the_server_does_not_declare` — with
   `EXPAND_ALLOWLIST = ["getTransitions"]` and the reason beside it. The test
   also asserts each allowlisted parameter is *actually* undeclared, so an
   entry cannot outlive its reason.
3. `the_route_ids_match_the_servers_own_names`.
4. `what_jired_reaches_outside_the_rest_api_is_listed_and_absent_from_it` —
   none of the three paths above is a prefix of any route in `ALL` or of any
   declared path.
5. `the_fixture_replays_the_vendored_spec` — the fixture's `spec-api-2.wadl`
   and `spec-agile-1.0.wadl` are byte-equal to the vendored files, and
   `spec-auth-1.wadl` is absent (the e2e for "this server does not serve one").

### Layout — `Rest` stays shared, a real `DataCenter` type overrides

```
src/api/datacenter/
  mod.rs      client(http) -> DataCenter; spec_modules(), WADL_ACCEPT,
              user_field_value(), assignee_body(), mention_markup()
  wizard.rs   move_subtask_parent(), Form, controls, attr, unescape,
              complaint, strip_tags — moved verbatim, tests included
  routes.rs   the table above
  client.rs   pub struct DataCenter { rest: Rest } + impl JiraApi
```

`Rest` keeps the two data lookups (`api()` prefix, `markup()`) and loses its
four control-flow branches: it implements `projects`, `board_project`,
`move_subtask_parent` and `search_users` the Cloud way. `DataCenter` delegates
the unchanged methods as one-liners and overrides `projects` (a bare array),
`board_project` (the board's project listing first, then its configuration —
the order and the comment at `rest.rs:365` survive), `move_subtask_parent`
(`rest.issue_id()` then the wizard), `search_users`, `worklogs` (one call),
`assign`, `edit_meta` and `create_meta`. `Rest::issue_id`, `field_map`,
`board_project_from` and `http` become `pub(crate)`; the JSON→model helpers
(`project_from`, `sprint_from_json`, `single_project`, `id_string`,
`one_comment`, `one_worklog`) stay where they are.

Every path in `Rest` is rendered by `Route::url` with the deployment's prefix,
so the code cannot reach a Data Center route except through the table. Cloud
strings come out byte-identical to today's, and a unit test pins a handful. The
three Cloud-only routes (`project/search`, `user/search?query=`,
`user/assignable/search?query=`) are `Route` consts in `cloud.rs`, outside
`ALL`, and not contract-tested — there is no Cloud WADL to test them against.

Why not a Cloud-only `Rest` and a standalone Data Center client: the Agile API
and some twenty-five of the trait's methods are identical on both deployments,
and two copies would drift. Why not a generic `Rest<Flavour>`: the four
overrides are the whole difference, and a type a reader can `grep` for beats a
type parameter.

### `search_users` gets a scope

```rust
pub enum UserScope<'a> { Any, Issue(&'a str), Project(&'a str) }
async fn search_users(&self, query: &str, scope: UserScope<'_>) -> Result<Vec<UserRef>>;
```

Data Center: `Any` → `findUsers?username=`; `Issue(k)` →
`findAssignableUsers?username=&issueKey=`; `Project(p)` → `…&project=`;
`maxResults=20` throughout. Cloud: the `query=` spellings of the same routes.
The one caller, `jired assign` (`src/cli/cmd/assign.rs`), passes
`UserScope::Issue(&key)`; the TUI's assignee completion reads the people seen
in fetched issues (`tui/catalog.rs`) and is untouched. The fixture reads
`assignable-{key}.json`, then `assignable-{project}.json`, then `users.json`,
the way `sprints-{board}.json` falls back to `sprints.json`.

### The assignee route (Breaking)

`JiraApi::assign(key, user_id: Option<&str>)`. Bodies differ per deployment —
`{"name": id | null}` and `{"accountId": id | null}` — and an unassign wants
an object with a null id, not the bare `null` `user_field_value` returns, so a
second helper `assignee_body(deployment, id)` sits beside it.

In `push`: `Action::Assign { to: Option<String> }`, op kind `assign`, request
`{"method": "PUT", "path": "/issue/{key}/assignee", "body": …}`. `plan()`
takes `assignee` out of the field payload on both deployments (push stays
deployment-neutral) and orders the op after `update_fields` and before
`move_parent`; `unapplied()` compares the re-read assignee id
(`eq_ignore_ascii_case`, since Data Center usernames are case-insensitive) and
fails the push with exit 10 — "the assignee is still …" — on the same evidence
rule as `parent`. `create_issue()` keeps `assignee` in the create payload; the
create screen carries it. The fixture records `PUT /issue/{key}/assignee`,
honours `ignored.json`/`faults.json`, and applies it. `schemas/push.json`'s
`kind` enum gains `assign`, in the same commit, or the schema contract fails.

### Screen checks before a push

```rust
pub struct FieldSchema { pub id: String, pub name: String, pub required: bool, pub operations: Vec<String> }
/// The fields a screen offers — an issue's edit screen, or a project and type's create screen.
pub struct ScreenMeta { pub fields: BTreeMap<String, FieldSchema> }

async fn edit_meta(&self, key: &str) -> Result<Option<ScreenMeta>>;
async fn create_meta(&self, project: &str, issue_type: &str) -> Result<Option<ScreenMeta>>;
```

`None` means the server describes no screen, and the check is skipped rather
than failed. Data Center: one `GET` for editmeta; createmeta pages
`…/issuetypes` to resolve the type *name* to an id (case-insensitive), then
pages `…/issuetypes/{id}` (`startAt`, `maxResults=50`). Cloud: the same paths
under v3, in `Rest`. Fixture: `editmeta-{key}.json` and
`createmeta-{project}-{type}.json`, absent → `Ok(None)`, so no existing test
changes. `allowed_values` is deliberately not modelled: nothing reads it yet.

A new `src/sync/screen.rs` owns the rule: `refused_by_screen(meta, sent, names)`
names the payload keys the screen lacks; `refusal(key, screen, refused)` is a
`JiredError::usage` (exit 2, the class the existing plan-time refusals use)
with `details.issue`, `details.field`, `details.name`, `details.screen` and the
hint "the field is not on the {screen} screen for this issue type — change it
in Jira, or ask an admin to add the field to the screen". Push raises it in the
per-issue `prepared` closure for the `update_fields` keys plus `description`
when an `update_description` op is planned (both travel on `PUT issue/{k}`),
and in `create_issue()` before journaling, for the create payload minus
`project` and `issuetype`. `--dry-run` shows it; the issue fails as its own
`IssueResult`; others proceed; the edit stays in the file. Not checked:
`assign` (its own route — the reason it exists), `move_parent`, transitions,
sprint, comments, worklogs, attachments. A metadata call that *fails* is a
`tracing::warn!` and the push proceeds — the server's own 400 remains the
backstop, and a lookup must not turn a working push into a failing one (the
`complete_worklogs` rule). Createmeta is cached per `(project, type)` for the
run.

The check is **gated to Data Center** (`deployment == DataCenter`, the
precedent being the `parent` handling in `plan()`). Cloud's editmeta is
unrecorded, and a Cloud sub-task's re-parenting is an ordinary `parent` field
edit: if a company-managed edit screen omits `parent` while the edit works, the
check would refuse a move that succeeds today, and a false refusal is worse
than the 400 it pre-empts. Both clients implement the methods; lifting the gate
is a one-line change once a Cloud recording exists.

### Other corrections the description drives

- Data Center `worklogs` is one `GET issue/{k}/worklog` with no paging
  parameters: the route declares none and answers with every worklog. Cloud
  keeps its offset loop.
- `GET issue/{k}/transitions?expand=transitions.fields` stays, allowlisted.
- Search stays `POST search` (`searchUsingSearchRequest`): a JQL with many
  keys does not fit a query string.
- The floor moves to 10.0. Nothing used was removed since 9.12, but 10.7.4 is
  the description the routes are checked against, and a floor is a promise that
  has been tested.

## Tasks

Each task lands green: `cargo test --all-targets`, `cargo fmt --check`,
`cargo clippy --all-targets -- -D warnings`.

### M6.1 Vendor the 10.7.4 description, retire the 9.12.4 stubs
- [x] Copy the three WADLs to `specs/datacenter/10.7.4/jira.corp.example-datacenter-10.7.4-{api-2,agile-1.0,auth-1}.wadl`, replacing the recording host with `jira.corp.example` (two occurrences per file: `base=` and the `?detail=true` hint). No other edit — the files are the server's words
- [x] Replace `tests/fixtures/datacenter/spec-api-2.wadl` and `spec-agile-1.0.wadl` with byte-identical copies; keep `spec-auth-1.wadl` absent (the e2e for a module the server does not serve)
- [x] `tests/fixtures/datacenter/serverInfo.json` → `10.7.4` / `[10, 7, 4]` — before M6.6, or every Data Center e2e fails at `init`
- [x] `tests/e2e.rs::fetch_spec_writes_one_file_per_described_rest_module`: `9.12.4` → `10.7.4` in the report and both file names; the content check `path="api/2/search"` → `id="searchUsingSearchRequest"` (Jersey 2 puts the module in `base` and nests the paths, so the old string does not occur)
- [x] `tests/no_secrets.rs`: `walk` collects `.wadl` as well as `.json`; both fixture scans also walk `specs/`; `fixture_hosts_are_reserved_names` accepts the XML namespace identifiers (`wadl.dev.java.net`, `www.w3.org`, `jersey.java.net`) with the comment that a parser never contacts them
- [x] `.gitattributes`: `*.wadl -text`, so a Windows checkout keeps the bytes the byte-equality test compares
- [x] `docs/plan/commands.md` `fetch-spec` sample file names → 10.7.4; open `## [0.6.0] — <date>` in `CHANGELOG.md` (never `Unreleased`)
- **Done when:** `cargo test --test e2e fetch_spec` and `cargo test --test no_secrets` pass with the real WADLs in place.

### M6.2 Split `datacenter.rs` into a module
- [x] `git mv src/api/datacenter.rs src/api/datacenter/mod.rs`; the wizard and its tests move to `wizard.rs` as `pub(crate)`; `mod.rs` keeps `client()`, `spec_modules()`, `WADL_ACCEPT`, `user_field_value`, `mention_markup` and their tests
- [x] Fix the call in `rest.rs` and the doc link in `push.rs`
- **Done when:** `cargo test api::datacenter` runs the same tests; no behaviour change.

### M6.3 The route table and the contract test
- [x] `routes.rs` as designed, with unit tests `a_url_substitutes_and_encodes_its_arguments`, `an_undeclared_query_parameter_is_a_programming_error` (`#[should_panic]`) and `every_route_is_in_the_table`
- [x] `Cargo.toml` `[dev-dependencies] roxmltree`; `cargo build` so `Cargo.lock` records it; `cargo deny check`
- [x] `tests/dc_spec_contract.rs` with the five tests; its module doc says the vendored WADL is Jersey's simplified listing and `…/application.wadl?detail=true` is where to look before calling a route absent
- **Done when:** the contract passes, and changing `ASSIGN.method` to `POST` or adding `"foo"` to `GET_ISSUE.query` fails it with a message naming the route.

### M6.4 The Data Center client, and `Rest` without deployment branches
- [x] `src/api/datacenter/client.rs`: `DataCenter { rest: Rest }` delegating the unchanged methods; overriding `projects`, `board_project`, `move_subtask_parent`, `search_users` (unscoped `findUsers` until M6.7) and `worklogs` (one call)
- [x] `Rest`: delete the `Source`-based branching; add `prefix(Module)` and `url(&Route, args, query)`; replace every `format!("{}/…", self.api())` and `/rest/agile/1.0/…` literal — `issue_id`, `issue_updated`, `changelog`, `transitions`, `boards`, `sprints`, `board_project_from` included
- [x] Unit tests: `worklogs_are_asked_for_in_one_call`, `projects_are_read_as_a_bare_array`, `cloud_paths_are_unchanged` (pins `/rest/api/3/issue/PROJ-1?fields=*all` and `/rest/agile/1.0/board?maxResults=50&projectKeyOrId=PROJ&startAt=0`, among others)
- [x] `src/api/mod.rs` header: "one trait, two implementations" is true again
- **Done when:** green, and `grep -n 'match self.deployment' src/api/rest.rs` finds only `api()` and `markup()`.

### M6.5 Complete paged comments
- [x] `RemoteIssue.comment_total`; `normalize::comments_from` returns `(Vec<RemoteComment>, usize)` like `worklogs_from`; every call site and `RemoteIssue` literal updated (`sync/attachments.rs`, `sync/render.rs`, `store/state.rs`, `push.rs` tests — the compiler finds them)
- [x] `JiraApi::comments(key)`: both deployments page `getComments` (`startAt`, `maxResults=100`) to `total`; fixture reads `comments-{key}.json`, else the payload's list
- [x] `Rest::complete_comments` beside `complete_worklogs`, called from `search` and `issue`; `DataCenter` inherits it through delegation
- [x] Unit `a_paged_comment_field_reports_the_full_total`; e2e `every_comment_is_pulled_when_the_issue_payload_pages_them` on both deployments (payload `comment.total = 3` with one inline; `comments-{key}.json` with three; `pull`; three comment headings)
- **Done when:** the e2e fails without `complete_comments` and passes with it.

### M6.6 The floor: Data Center 10.0 (Breaking)
- [x] `MIN_DC_MAJOR = 10` in `src/api/mod.rs`; the hint says the routes are checked against the 10.x description; `old_data_center_is_refused_with_exit_10` refuses 9.4.0 and accepts 10.3.2
- [x] e2e `a_data_center_below_ten_is_refused_before_anything_is_stored`: `serverInfo.json` at 9.12.4 → `init --no-pull` exits 10, `kind == "unsupported_server"`, `details.minimum == "10.0"`, no `session.db`
- [x] README exit-10 row; `docs/plan/README.md` D20 and the open item; `grep -rn '9\.0' README.md docs src` names no floor
- **Done when:** unit and e2e pass and no document names 9.0 as the floor.

### M6.7 Assignable-user search
- [x] `UserScope` and the new `search_users` signature; `Rest` Cloud-shaped; `DataCenter` on `findUsers`/`findAssignableUsers`; a pure `user_search_url()` with unit `assignable_search_is_scoped_by_issue_or_project`
- [x] `assign.rs` passes `UserScope::Issue(&key)` into `resolve_assignee`
- [x] Fixture fallback chain; e2e `assign_searches_the_users_who_can_be_assigned_the_issue` (Data Center: `users.json` = jane, bob; `assignable-DC-77.json` = jane; `--user bo` exits 2 "no user matches"; `--user ja` stages)
- [x] Docs: `commands.md` `jired assign`, README, the `init-agents` prose; CHANGELOG **Added** — Cloud now finds only assignable users too, which is Added rather than Breaking since no command surface changes
- **Done when:** the e2e passes, and fails when the scope is ignored (bob found).

### M6.8 The assignee route (Breaking)
- [x] `JiraApi::assign`, `assignee_body`, `Action::Assign`, the `assign` op, `perform`, `unapplied`, the fixture, `schemas/push.json`
- [x] e2e: `assign_none_clears_the_field_and_pushes_a_null` now reads `ops[0].kind == "assign"`, `request.path == "/issue/{key}/assignee"`, `request.body == {"accountId": null}`; new `an_assignee_edit_is_its_own_call_on_the_assignee_route` (both deployments; the journal shows `PUT /issue/{key}/assignee` and no `PUT` carrying `body.fields.assignee`), `an_assignee_the_server_did_not_apply_fails_the_push` (`ignored.json` → exit 10, "the assignee is still"), `a_mixed_edit_sends_the_fields_then_the_assignee` (kinds `["update_fields", "assign"]`)
- [x] Docs: `commands.md` push section after the `parent` paragraph; README push `--dry-run` paragraph and `assign` row; `init-agents` push prose; CHANGELOG **Breaking** — a script reading `--dry-run` for the assignee under `body.fields` must read the `assign` op
- **Done when:** schema contract, e2e and unit tests are green and every `"assignee"` assertion in `tests/e2e.rs` points at the new op.

### M6.9 Screen checks
- [x] Model types, trait methods, `Rest` (v3) and `DataCenter` implementations, `normalize::screen_from_json` with a unit test on a trimmed real editmeta payload, fixture files, `src/sync/screen.rs` with units `a_payload_field_missing_from_the_screen_is_named` and `a_screen_nobody_described_refuses_nothing`, push wiring (gated; `run()` owns the createmeta cache)
- [x] e2e on Data Center: `a_field_the_edit_screen_lacks_is_refused_before_the_server_sees_it` (editmeta without `priority`; edit `priority:`; dry-run exits 2 naming it; the journal shows no `PUT /issue/DC-77`; the edit stays in the file), `a_field_the_create_screen_lacks_is_refused_before_the_issue_is_made` (`createmeta-DC-Task.json`; `create … --priority High`; no `POST /issue`), `a_description_edit_is_checked_against_the_edit_screen`
- [x] Docs: `commands.md` push and `create --push`; README push section; `init-agents` prose; CHANGELOG **Added**, with a **Breaking** line that a push which used to fail on the server's 400 (exit 1) now exits 2 at plan time
- **Done when:** the three Data Center e2e tests pass and every Cloud test is unaffected.

### M6.10 Docs, plan, version
- [ ] This file ticked; `docs/plan/README.md` status; `architecture.md`'s component graph names the route table and the vendored description, with a short "Specs" paragraph; README gets a "Specs" note under the `fetch-spec` section
- [ ] `Cargo.toml` 0.6.0, `cargo build` for the lock; CHANGELOG 0.6.0 ordered **Breaking** (floor 10.0; the `assign` op; the screen refusal) → **Added** (assignable search; complete comments; the route table and its contract test; the vendored description) → **Fixed** (Data Center worklogs fetched in one call, as the API declares)
- **Done when:** the verification list below passes on Linux, macOS and Windows CI.

## Verification

```sh
cargo build
cargo test --test dc_spec_contract
cargo test --test no_secrets
cargo test --test schema_contract
cargo test --test e2e -- fetch_spec assign assignee screen comment below_ten
cargo test --all-targets
cargo test --test scale -- --ignored --nocapture
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo deny check
```

By hand, once: break `ASSIGN.method` and watch the contract name it; remove
the `complete_comments` call and watch the comments e2e fail;
`grep -n 'match self.deployment' src/api/rest.rs` shows only the two data lookups.

## Risks

| Risk | Handled by |
|---|---|
| e2e tests asserting `update_fields` for an assignee edit | Only `assign_none_clears_the_field_and_pushes_a_null` does; the `ops[0].kind == "update_fields"` tests edit summary or labels |
| `schemas/push.json` missing `assign` | Same commit as the op (M6.8), or `schema_contract` is red |
| `no_secrets` scanning `.wadl` trips on XML namespace URIs | The namespace allowlist in M6.1 |
| Windows line endings corrupting the byte-equality test | `.gitattributes` `*.wadl -text` |
| The simplified WADL omits a route jired needs one day | `routes.rs` documents `?detail=true`; today every route used is listed |
| A Cloud screen check refusing a `parent` edit that works | The check is gated to Data Center |
| `comments_from`'s new signature | Touches five `RemoteIssue` literals; the compiler finds each |
| `Route::url` reordering query pairs | It emits them as given; the pinned Cloud strings would catch a sort |
