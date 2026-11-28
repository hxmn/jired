# M7 — Jira Cloud, verified (size: L)

**Goal:** every claim jired makes about Jira Cloud is backed by something a real
Cloud site said: each Cloud route checked against Atlassian's published OpenAPI
description, a fixture set recorded from a live site and replayed by the e2e
suite, a live suite that drives the whole `init → pull → edit → push` cycle
against a sandbox, and a scheduled job that notices when Cloud moves.

Depends on: nothing new. M6's route table (`src/api/datacenter/routes.rs`) and
its contract test (`tests/dc_spec_contract.rs`) are the pattern M7.2 copies;
`jired-record` (`src/bin/record.rs`) is the tool M7.4 extends.

## Status

**Backlog — not started.**

jired has never sent a request to Jira Cloud. Every Cloud test runs against
`tests/fixtures/cloud/`, which was written by hand from Atlassian's
documentation, so it proves only that jired agrees with itself. M1's "Cloud and
Data Center are both exercised end to end" means *against those fixtures*;
M1.12's "Baselines recorded from real instances" is still unticked, and so is
M1.6's "Done when: `whoami` works live against Cloud and DC". Data Center at
least has M6: every route checked against the server's own description, and
real field reports behind the fixes since. Cloud has neither.

What planning turned up, each one a task below:

- **Nothing checks Cloud's routes.** The ten Cloud-only `Route` consts in
  `src/api/cloud.rs` (`search/jql`, `search/approximate-count`,
  `project/search`, paged `worklog`, `user/search?query=`,
  `user/assignable/search?query=`, `watchers?accountId=`, `filter/search`,
  `filter/{id}/favourite`) say so themselves. So do the shared routes
  Cloud is served from, which were checked against the Data Center WADL alone.
  Atlassian publishes an OpenAPI document for Cloud (`cloud::PUBLISHED_OPENAPI`),
  and a second one for the Agile API, so a contract test needs no credentials
  (M7.2).
- **The incremental fetch may skip updates for anyone west of UTC.**
  `fetch::build_jql` writes the watermark as `updated >= "YYYY-MM-DD HH:MM"`
  through `value::jql_timestamp`, which formats the UTC instant. Jira reads a
  JQL date in the *searching user's profile timezone*. At UTC−8 a watermark of
  `12:00Z` reads as `20:00Z`, and an issue updated in between is never fetched.
  The five-minute back-date in `skew_adjusted` covers index lag, not an offset.
  At UTC+N the query fetches more than it needs, which is harmless. This
  affects Data Center too, and nobody has observed it yet (M7.3).
- **`jired-record` captures 7 of the ~28 fixture files `FixtureClient` reads.**
  It writes `serverInfo`, `myself`, `projects`, `versions`, `fields`, one search
  page and one issue's transitions. Boards, sprints, board configuration,
  changelog, comment and worklog pages, editmeta, createmeta, users, filters and
  link types are not recorded (M7.4).
- **Scoped API tokens are not handled.** Atlassian's newer scoped tokens
  authenticate only through the `api.atlassian.com/ex/jira/{cloudId}` gateway,
  not through `<site>.atlassian.net`. jired builds every URL from the site URL
  and has no notion of a cloud id (M7.9).
- **Cloud's screens are not checked** (`sync/push.rs`, the M6.9 gate), because
  no Cloud editmeta has ever been seen. Lifting the gate is one line once one
  has been (M7.8).
- **The sub-task re-parent on Cloud is an assumption.** `move_subtask_parent`
  sends `{"parent": {"key": …}}` as an ordinary field edit, on the strength of
  Atlassian's changelog. Nothing has ever checked the re-read (M7.7).
- **Attachment downloads on Cloud redirect to the media host.** `get_bytes`
  follows reqwest's default redirect policy. Nobody has checked that the bytes
  arrive, or that the `Authorization` header stays behind on the other host
  (M7.7, M7.10).
- **Custom-domain Cloud sites are guessed as Data Center.**
  `Deployment::guess_from_url` knows only `*.atlassian.net` and `*.jira.com`.
  `serverInfo.deploymentType` corrects the guess, but whether `init` then picks
  the right credential prompt has not been tried (M7.9).

## Acceptance criteria

- [ ] Every route jired sends on Cloud, shared and Cloud-only, is checked in CI against vendored copies of Atlassian's platform v3 and Agile OpenAPI documents; a deprecated operation fails the test unless it is allowlisted with a reason
- [ ] `tests/fixtures/cloud-recorded/` holds a scrubbed, reviewed recording from a real Cloud site covering every file `FixtureClient` reads, and the Cloud e2e workflow suite passes against it
- [ ] An ADF corpus authored in Cloud's own editor, one issue per node and mark family, round-trips: pull renders it as `format-conversion.md` says, an untouched push sends nothing, and an edit beside a node leaves that node byte-identical on re-read
- [ ] `tests/live_cloud.rs` (ignored by default, enabled by env) passes against the sandbox: every push op is applied and verified by re-read, and the stale-push, partial-failure and resume paths behave as `sync-algorithm.md` says
- [ ] The incremental-fetch timezone question is settled by a live observation, and fixed with a regression test if it is real
- [ ] `init` and `whoami` work with a classic API token; a scoped token, a custom domain and a hidden-email account each either work or fail with an exit code and hint that say what to do
- [ ] A weekly CI job runs the live suite and a shape diff of a fresh recording, and turns drift into a failing run with the diff attached
- [ ] No document still says a Cloud behaviour is unverified unless it is, and each fix found along the way shipped as its own patch release

## The sandbox

Everything live runs against **one dedicated Cloud site, never a real
workspace**. M7.1 creates it, and this section says what it must contain.

| Thing | Why |
|---|---|
| A site on the Free plan, with Jira Software (`<site>.atlassian.net`) | Boards, sprints and the Agile API. The free plan allows ten users |
| A bot account `jired-bot` with a classic API token | The credential every run uses. Its email is hidden in profile visibility, to exercise the privacy path |
| A second account `jired-peer` | Assignee, mention, watcher and `assign --user` targets that are not the caller |
| Company-managed Scrum project `PROJ` | Epic → `parent`, sprints on a board, sub-tasks, components, versions, a resolution screen on Done |
| Team-managed Kanban project `OPS` | A different field id per project, no Epic Link, per-project issue types, a board with no sprints |
| Custom fields on `PROJ`: single select, multi select, number, date, datetime, user picker, multi-user picker, labels, cascading select, URL, paragraph (ADF), story points | Every `custom.rs` expand/simplify branch against a real schema |
| A transition that requires a field (resolution, comment) | `transition` with fields; `expand=transitions.fields` |
| An issue in `PROJ` the bot cannot browse | The 404-instead-of-403 path |
| Smart Checklist for Jira (Forge), if the free tier allows it | The Cloud checklist field route; skipped with a note if not |
| ≥ 250 issues in `PROJ`, created by M7.10's seeding script | `search/jql` token paging past two pages |

Secrets live in a password manager and in the CI secrets `JIRED_CLOUD_URL`,
`JIRED_CLOUD_EMAIL` and `JIRED_CLOUD_TOKEN`, plus the same three for the peer.
The site name is treated as a secret as well. It never appears in a commit,
a fixture or a log. Recordings are scrubbed to `fixture.invalid`, and tests
read the URL from the environment.

## Layers of evidence

| Layer | Needs credentials | Runs | Catches |
|---|---|---|---|
| OpenAPI contract (M7.2) | no | every CI run | a path, method or query parameter Cloud does not declare; a deprecated route |
| Recorded fixtures (M7.4–M7.6) | once, to record | every CI run | a payload shape jired misreads; ADF it cannot round-trip |
| Live suite (M7.7–M7.11) | yes | by hand, then weekly | behaviour no description states: re-reads, ordering, permissions, rate limits, redirects |
| Scheduled drift job (M7.12) | yes | weekly, `main` only | Cloud changing under a release that used to work |

## Tasks

Each task lands green: `cargo test --all-targets`, `cargo fmt --check` and
`cargo clippy --all-targets -- -D warnings`. A bug found by a task is fixed in
its own commit, with a test that fails without the fix, and ships as its own
patch release (`CLAUDE.md`). It does not wait for the milestone.

### M7.1 Sandbox and runbook
- [ ] Create the site, both accounts, both projects, the custom fields and the restricted issue as the sandbox table lists them; record each id needed later (field ids, board ids, transition ids) in the password manager beside the credentials, not in the tree
- [ ] Write `docs/plan/cloud-sandbox.md`: how to rebuild the sandbox from nothing, which settings matter (bot's profile timezone, email visibility) and the cleanup rule (every live-suite issue carries the label `jired-live`, and anything older than 7 days is deleted)
- [ ] By hand: `jired init --url … --email … --no-input` then `jired whoami --json`. Record what `serverInfo` returns (`version`, `deploymentType`, `serverTitle`) and the shape of `myself` with a hidden email
- **Done when:** `whoami` succeeds live with a classic token, and M1.6's "Done when" can be ticked for Cloud.

### M7.2 Cloud's OpenAPI description and a contract test
- [ ] Vendor `swagger-v3.v3.json` (platform) and the Jira Software `swagger.v3.json` (Agile) into `specs/cloud/<YYYY-MM-DD>/`, unmodified and dated by download day, since Cloud has no version number to name them by; `.gitattributes` `*.json -text` for that directory
- [ ] Build a route list for Cloud: the shared table's `ALL` minus the routes `DataCenter` overrides, plus every const in `cloud.rs`. Put it in `cloud::ALL` so a new Cloud route cannot skip the test
- [ ] `tests/cloud_spec_contract.rs`, mirroring `dc_spec_contract.rs`: each route's path template exists in the module's document, with the method; each query parameter it sends is declared; `operationId` matches `Route::id` (rename the ids in `cloud.rs` that are WADL names to the OpenAPI ones where they differ); an operation marked `deprecated: true` fails unless it is allowlisted with a sentence saying why
- [ ] Check every route Cloud *receives through the shared table*, not only the ten Cloud-only ones. Look first at `filter/favourite`, `issue/createmeta/{p}/issuetypes`, `GET issue/{k}/comment` paging names and `expand` on `search/jql` (a string, not a list)
- [ ] Where the description disagrees with the code, fix the code in its own commit and patch release, or allowlist the parameter with a reason (`expand=transitions.fields`, as M6 did)
- [ ] `tests/no_secrets.rs` walks `specs/cloud/` and allowlists the hosts the documents name (`developer.atlassian.com`, `your-domain.atlassian.net` and the like)
- [ ] `cloud.rs` and `routes.rs` module docs: "not contract-tested" becomes the name of the test that checks them
- **Done when:** the contract passes, and changing `SEARCH_JQL.method` to `GET` or adding `"foo"` to `PROJECT_SEARCH.query` fails it with a message naming the route.

### M7.3 The incremental-fetch timezone
- [ ] Unit test first: `build_jql` with a watermark and a profile timezone of `America/Los_Angeles` must produce a clause that matches an issue updated one hour after the watermark. It should fail today
- [ ] Live: set `jired-bot`'s profile timezone to UTC−8, `fetch`, update an issue in the UI, wait 10 minutes, `fetch` again. Does the update arrive? Repeat with UTC+3 to confirm that direction only over-fetches. Run the same on the Data Center container if one is to hand
- [ ] If it reproduces, the fix: format the watermark in the timezone `myself.timeZone` names (stored in `session.db` at `init`, re-read when `whoami` reports a change). The fallback when the timezone is unknown is to back-date by 14 hours, the widest offset, rather than guess. One commit, one patch release, a CHANGELOG **Fixed** entry naming who was affected
- [ ] If it does not reproduce, write down why next to `jql_timestamp`, so nobody re-opens it
- **Done when:** the live observation is recorded here, and either the regression test passes with the fix or the reason is in the code.

### M7.4 Record everything `FixtureClient` reads
- [ ] `jired-record` writes every file `api/fixture.rs` reads, in its layout: `boards.json`, `board-{id}-config.json`, `board-{id}-projects.json`, `board-{id}-columns.json`, `sprints-{board}.json`, `filters.json`, `linkTypes.json`, `users.json`, `assignable-{project}.json`, `assignable-{key}.json`, `changelog-{key}.json`, `comments-{key}.json`, `worklogs-{key}.json`, `transitions-{key}.json`, `editmeta-{key}.json` and `createmeta-{project}-{type}.json` for every issue type. It also records `search-page.json`: two pages of `search/jql`, at `--limit` deliberately small, so that `nextPageToken` and `isLast` are real
- [ ] It records raw payloads, not jired's normalised view, as it already does for issues, so replay exercises `normalize.rs`
- [ ] Scrubber coverage for what Cloud adds: `self` and `avatarUrls` hosts, media ids and collection names in ADF `media` nodes, `accountId` inside ADF `mention.attrs.id` (not only in user objects), `cloudId`-shaped UUIDs, the site title in `serverInfo`, attachment `content`/`thumbnail` URLs, and display names inside comment text. Add a unit test for each in `scrub.rs`
- [ ] `--project` may be given twice (`PROJ` and `OPS`), so one recording holds both project styles
- [ ] Record into `tests/fixtures/cloud-recorded/`, read every file by hand before committing, and run `no_secrets` over it. Keep the hand-built `tests/fixtures/cloud/` as it is: it carries crafted edge cases a real site will not produce on demand
- **Done when:** a recording from the sandbox replays with `JIRED_FIXTURES=tests/fixtures/cloud-recorded` through `init → pull → status`, and `no_secrets` passes over it.

### M7.5 The read path against the recording
- [ ] Run the existing Cloud e2e workflow tests a second time against `cloud-recorded/`, as a parameterised fixture root rather than copied tests. Where an assertion depends on hand-built data, split it into a crafted-data test and a recorded-data test
- [ ] Check each by reading the pulled files, with a test for each finding:
  - [ ] `sprint:` from Cloud's sprint field (an array of objects, never the Data Center string)
  - [ ] `epic:` from `parent` on a company-managed and on a team-managed project
  - [ ] Every custom field type in the sandbox table, through `custom.rs` expand → simplify → expand
  - [ ] Status categories, resolution, priority, components and fix versions
  - [ ] Assignee and reporter as mentions, with a hidden-email account
  - [ ] `updated` and `created` parse with Cloud's `+0000` offset spelling
  - [ ] Comments and worklogs with ADF bodies
  - [ ] Board columns and grouping folders
- [ ] The second `pull` touches zero files (M1's acceptance criterion, never yet met against real data)
- **Done when:** the workflow suite is green on both fixture roots, and every bullet above has a test.

### M7.6 The ADF corpus
- [ ] In the sandbox, author one `PROJ` issue per family, **in Cloud's editor** rather than through the API, so the ADF is what the editor really emits (`localId`s, marks in the editor's order, empty paragraphs):
  - [ ] Headings 1–6, paragraphs, hard breaks, rules
  - [ ] Bullet and ordered lists nested three deep, an ordered list starting at 3
  - [ ] Task lists with `localId`s and both states; decision lists
  - [ ] Code blocks with and without a language; blockquotes
  - [ ] Panels (info, note, warning, success, error, custom); expand and nested expand
  - [ ] Tables: header row, header column, merged cells, column widths, a table inside a panel
  - [ ] Mentions, emoji, dates, status lozenges, `inlineCard`, `blockCard`, `embedCard`
  - [ ] Media: `mediaSingle` of an attached image, `mediaGroup` of a file, inline media
  - [ ] Marks: strong, em, code, strike, underline, link, `textColor`, `backgroundColor`, sub/sup, and overlapping marks
  - [ ] An extension node an app left behind
  - [ ] Non-Latin text (Cyrillic, CJK, RTL), emoji ZWJ sequences, very long lines
- [ ] Record the corpus into `tests/fixtures/cloud-recorded/adf/` and add `tests/roundtrip.rs` cases: ADF → MD matches the table in `format-conversion.md`; MD → ADF of an untouched file reassembles the stored original byte for byte; editing one paragraph changes that paragraph alone
- [ ] Live, once: for each corpus issue, `pull`, edit an adjacent paragraph, `push`, re-read the issue through the API, and compare its ADF with the original modulo the edit. Then open it in the browser to check that it renders. A diff a test cannot see (a panel the editor refuses to open) is a bug too
- [ ] Every lossy region the corpus finds is either converted or protected by a raw block, and `format-conversion.md` says which
- **Done when:** the offline round-trip cases pass, and the live pass is recorded here with any mismatch either fixed or listed in `format-conversion.md` as known-lossy.

### M7.7 Every write, live
- [ ] `tests/live_cloud.rs`, every test `#[ignore]`, enabled by `JIRED_LIVE_CLOUD=1` plus the credentials. Each test makes its own issues labelled `jired-live` and a per-run id, works in a temporary workspace, and deletes what it made (deletes are best-effort, with the M7.1 sweep as the backstop). Tests run one at a time, since the bot's rate budget is shared
- [ ] One test per push op, each verified by re-reading the issue and **not** by jired's own report:
  - [ ] `update_fields`: summary (and the file rename), labels, priority, components, fix versions, each sandbox custom field type, and clearing each one
  - [ ] Description: an ADF edit; a description added to an issue that had none; a description cleared
  - [ ] `assign`: by `--user` prefix through the assignable search, to `none`, and to someone who cannot be assigned (refused at plan time)
  - [ ] Transitions: a plain one, one that requires a resolution and a comment, and one through a board-column symlink move (D4)
  - [ ] Comments: add, edit and delete (gated) with ADF bodies containing a mention
  - [ ] Worklogs: add, edit and delete; `started` round-trips in the bot's timezone; `timeSpent` notations
  - [ ] Issue links and remote links
  - [ ] Attachments: upload (with `X-Atlassian-Token: no-check`), lazy and eager download. The download follows the redirect to the media host and returns the right bytes
  - [ ] Sprint: move into the active and a future sprint; move to the backlog (`backlog/issue`)
  - [ ] Parent: set an epic on `PROJ` and `OPS`, and clear it. Re-parent a sub-task through the `parent` field, the unverified path. If Cloud refuses or ignores it, the fix is a plan-time refusal like Data Center's, plus a patch release
  - [ ] `create`: a task, a sub-task under a fresh parent, one in `OPS`, and one with a checklist. Follow-ups in D23 order. The `pending` journal row recovers after a killed push
  - [ ] `sprint create`, `release create`/`update`, `filter create`/`update`/star/share/delete, watchers add/remove (`accountId`)
  - [ ] Checklist: the Cloud field route, if the app is installed
- [ ] Concurrency (`sync-algorithm.md`): edit in the UI between `fetch` and `push` → exit 5 with a conflict; edit a different field → three-way merge; delete the issue between `fetch` and `push` → the error the spec names
- [ ] Partial failure: a push of three issues where the middle one is refused (the restricted issue). The other two apply, the journal records it, and the next push resumes
- **Done when:** every test above passes live in one run, and every failure it found is fixed and released.

### M7.8 Screen checks on Cloud
- [ ] Record `editmeta` and `createmeta` for every issue type in `PROJ` and `OPS` (M7.4 does this), and look at them. Does `parent` appear on a sub-task's edit screen? Do team-managed screens describe fields the same way?
- [ ] Add e2e cases on the recorded payloads: a field missing from the screen is refused at plan time, and a field present is allowed
- [ ] Lift the Cloud gate in `push.rs` only if every live write in M7.7 still passes with it lifted. Otherwise keep it, and replace "no Cloud screen has been recorded" in `commands.md` with what was actually seen
- **Done when:** the gate is either lifted with tests or kept with the recorded reason.

### M7.9 Credentials, URLs, identity
- [ ] Classic API token: `init`, `whoami`, a wrong token → exit 3 with `jired/error/1`, a revoked token mid-push → exit 3 and the journal intact
- [ ] Scoped API token on the site URL: record what Cloud answers. Then decide between supporting the gateway (find the cloud id from `https://<site>/_edge/tenant_info`, and prefix `https://api.atlassian.com/ex/jira/{cloudId}`) and refusing at `init` with a hint. Either way it is a new surface, so a minor bump
- [ ] A URL with a path (`https://<site>.atlassian.net/jira/software/projects/PROJ/boards/1`) pasted into `init` is cut to the site
- [ ] A custom domain: `--deployment cloud`, and the guess corrected by `serverInfo.deploymentType`, both reach the email + API token prompt. If the sandbox cannot have a custom domain, use a loopback test that serves Cloud's `serverInfo` from a non-Atlassian host
- [ ] A hidden email: `whoami`, mentions, `assign` and `log` all work by `accountId` alone, and nothing prints an empty `<>`
- [ ] The token reaches no log, trace (`-vv`), journal or recording. Run `no_secrets` patterns over a live run's `.jired/logs/`
- **Done when:** each bullet has a live observation and a test that holds it.

### M7.10 Paging, limits, failures
- [ ] A seeding script, `examples/seed-cloud.sh` or a `jired-record --seed N` mode, creates N `jired-live` issues with varied fields, and a sweep deletes them
- [ ] Search past two `nextPageToken` pages: `total` behaves as `commands.md` says, `--count` matches `approximate-count` roughly, and a resumed fetch (killed mid-walk) finishes the rest
- [ ] An issue with > 100 comments and > 100 worklogs: both pages are complete (`complete_comments`, `complete_worklogs`)
- [ ] Rate limiting: record the headers Cloud sends on a 429 (`Retry-After`, `X-RateLimit-*`, `Beta-Retry-After`) in a short, deliberate sandbox-only burst, and replay them in `http.rs`'s loopback tests. Check the 8 rps default (D26) against what Cloud actually allows
- [ ] Permission failures: Cloud answers 404 where Data Center answers 403. Check that the exit code and hint match the README table for the restricted issue, a project the bot cannot see, and an archived project
- [ ] Redirect safety: a loopback test where an attachment URL redirects to another host, showing that `Authorization` is not forwarded. The same check against Cloud's media host
- [ ] Timeouts and a dropped connection mid-push: covered by the existing loopback tests. Confirm the messages read correctly against a real site
- **Done when:** each bullet has a test, and every live surprise is fixed and released.

### M7.11 Scale
- [ ] M1's goal, for real: a 1,000-issue `PROJ` (seeded), a first `pull`, a no-op re-pull, and an incremental pull after 20 UI edits. Record wall time, request count and peak rate in this file
- [ ] Compare against `tests/scale.rs`'s numbers. Where they disagree by more than 2×, find out why
- **Done when:** the numbers are recorded here, and nothing is quadratic.

### M7.12 The scheduled job
- [ ] `.github/workflows/cloud-live.yml`: weekly cron plus `workflow_dispatch`; `if: github.repository == 'hxmn/jired'` and never on `pull_request`, so a fork's PR cannot reach the secrets; one `concurrency` group; runs `cargo test --test live_cloud -- --ignored --test-threads=1`, then the cleanup sweep
- [ ] Drift check: re-record into a temp directory, then compare *shapes* (keys and value types, with values ignored) against `cloud-recorded/`. A new key is a notice, and a missing or retyped key fails the run with the diff attached
- [ ] Re-download both OpenAPI documents, and fail the run if a route in `cloud::ALL` has gone or been deprecated since the vendored copy
- [ ] `security-testing.md`'s "weekly job re-verifies recorded fixtures against a live Cloud sandbox" becomes true, with the workflow named
- **Done when:** the job has run green twice on `main`, and once red on a deliberately broken route.

### M7.13 Docs, plan, version
- [ ] `commands.md`: replace each "not recorded / not checked" Cloud sentence (the screen-check paragraph, the `create` follow-ups paragraph and the sub-task `parent` paragraph) with what was observed
- [ ] `m1-foundation.md`: tick M1.6's live `whoami` and M1.12's recorded baselines; update its Status
- [ ] `docs/plan/README.md`: this milestone's row, "Where the project stands" and the "Not built" line
- [ ] `src/cli/cmd/init_agents.rs`: any Cloud claim the generated `CLAUDE.md`/`AGENTS.md` makes is one the live suite checked
- [ ] README: a "Tested against" note naming the date of the last green live run's spec snapshot, and Cloud's known-lossy ADF list from M7.6
- [ ] CHANGELOG: the fixes shipped as their own patch releases as they were found. The milestone's last release lists the contract test, the recorded fixtures and the live suite under **Added**
- **Done when:** the verification list below passes, and no sentence in `docs/` or the README calls a Cloud behaviour unverified when it has been verified.

## Verification

Offline, every CI run:

```sh
cargo test --test cloud_spec_contract
JIRED_FIXTURES=tests/fixtures/cloud-recorded cargo test --test e2e
cargo test --test roundtrip
cargo test --test no_secrets
cargo test --all-targets
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
```

Live, by hand once, then weekly:

```sh
JIRED_LIVE_CLOUD=1 JIRED_CLOUD_URL=… JIRED_CLOUD_EMAIL=… JIRED_CLOUD_TOKEN=… \
  cargo test --test live_cloud -- --ignored --test-threads=1
```

By hand, once: break `SEARCH_JQL.method` and watch the contract name it; set
the bot's timezone to UTC−8 and watch M7.3's live test; remove
`complete_comments` and watch the > 100-comments test fail live.

## Risks

| Risk | Handled by |
|---|---|
| A recording leaks a real name, host or account id | The scrubber additions in M7.4 with a unit test each, `no_secrets` over the recording, a by-hand read before commit, and a sandbox that holds no real people |
| A live test damages data that matters | One dedicated site, no real workspace; every test works only on issues it created under `jired-live` |
| The live suite is flaky against a shared, eventually-consistent service | Re-reads poll with backoff, as the checklist re-read does; tests run one at a time; a failure is retried once in the scheduled job before it is reported |
| Atlassian changes the OpenAPI document daily, and a vendored copy goes stale | The copy is dated; the weekly job compares against a fresh download and fails only on a route jired uses |
| The free plan lacks something (Smart Checklist, a custom domain, Advanced Roadmaps fields) | Each such bullet says what replaces it: skipped with a note, or a loopback test |
| Rate-limit probing gets the site throttled | One short burst, sandbox only, recorded once and replayed offline afterwards |
| A fork's PR reaching the secrets | The workflow never runs on `pull_request` and is pinned to the upstream repository |
| Fixing the timezone bug changes what an existing workspace re-fetches | It only ever widens the window: the next fetch reads more, never less, and the CHANGELOG says so |
