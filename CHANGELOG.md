# Changelog

Notable user-facing changes. Anything that changes a command's spelling, its exit
code, or what it does without asking is called out as **Breaking**; internal
refactors are not listed.

Versions follow [semantic versioning](https://semver.org). Before 1.0 a minor
bump is where breaking changes go, so read the **Breaking** section of every
version between the one you have and the one you are moving to.

## [0.8.0] — 2026-08-30

### Breaking

- **`jired transition` and `jired assign` take several issues, and their JSON
  is per-issue.** Both now take `<ISSUES>...` the way `label`, `set` and
  `sprint add` always have — a bulk status change was a shell loop, and the two
  commands most worth piping `jired list --keys` into were the two that could
  not accept a list. The result documents changed shape with it:
  `jired/transition/1` and `jired/assign/1` now carry an `issues` array (one
  `{key, file, changed}` entry per file written; transition's entries also
  carry `to` and `transition_id`, which differ per issue even under one
  `--to`) where they used to carry one top-level `key`/`file`. A script
  reading `.key` reads `.issues[0].key` now. Each issue is validated against
  its own workflow: one that cannot move is a warning beside the ones staged,
  and only every issue failing fails the command. `transition --list` reads
  one issue at a time.

### Added

- **`jired list --category`, `--not-status`, and a repeatable `--status`.**
  "Everything in this sprint that isn't finished yet" had no expression:
  `--status` matched one exact name, and spelling the negation by hand breaks
  the day a workflow gains a status. `--category new|indeterminate|done`
  selects by Jira's own three buckets — the same vocabulary the picker's
  `ui.issues.filter` takes — read off the snapshots' `statusCategory`, so it
  works across differently-named workflows. `--status` repeats to mean "any
  of", and `--not-status` (repeatable) excludes by exact name.

- **`push` warns about work an earlier push left unfinished.** A push whose
  summary read `failed: 0` while failed operations sat in the journal waiting
  for `--resume` was telling the truth about what it attempted and hiding the
  queue: the only way to learn of the stuck operations was `jired status`.
  They are named in the push report's `warnings` now — for issues the push did
  not touch, since an issue in `results` already tells its own story.

### Fixed

- **`diff` agrees with `status` and `push` about an untouched file.** A file
  whose bytes are exactly what the last pull wrote holds no edit, but base
  re-rendered under today's rules can differ from it anyway — a field the
  renderer has learned since the file was written (`due_date` in 0.3.0), or
  `ignored_custom_fields` changed underneath it. `status` and `push` skipped
  such files on the recorded hash; `diff` re-rendered and reported the drift
  as a `"sendable": true` change no push would ever send, and telling which
  answer was true took a dry run. `diff` now skips on the same hash in its
  default mode, and the next `pull` rewrites the file in today's rendering.

- **A retried failing push no longer duplicates its journal.** Every attempt
  journaled a fresh set of `pending_op` rows and only success cleared them, so
  each retry of a failing operation added another byte-identical `failed` row
  for `status` to report. A re-plan now replaces the failed attempt's rows for
  that issue: two failed attempts at one operation are one unfinished
  operation.

## [0.7.0] — 2026-08-30

### Added

- **The generated agent instructions say which jired wrote them, and jired says
  when that is not this one.** The block in `CLAUDE.md` and `AGENTS.md` now opens
  `<!-- BEGIN jired v0.7.0 -->`. `jired status` reports every file whose block
  another version wrote in a new `stale_agent_docs` array — the file, the version
  stamped in it (`null` for a block written before the stamp existed), and the
  version running — and says so on its way past; `doctor`'s `agents` check names
  that version instead of only reporting that something differs. A stale agent
  doc is worse than none: it reads as authoritative and it is what an agent plans
  against before it runs anything, and until now nothing reported one. The marker
  is *found* by its `<!-- BEGIN jired` prefix, so a block an older jired stamped
  is replaced where it stands rather than left with a second one appended under
  it, and the closing marker is looked for after the opening one so prose above
  the block cannot truncate it.

- **The generated docs carry the flags a scripted caller actually reaches for.**
  Each of these was in `--help` and in no generated table, which left the long way
  round looking like the only way through:

  - `--fields` and `--json-compact` join the agent-flags table, with the dotted-path
    rules and worked examples. Asking for `results.key,results.ops.summary` is the
    difference between three values and parsing a whole `push --dry-run` document.
  - `diff --stat` and `--exit-code` join the safe-workflow block. Confirming only
    the intended changes is what `--stat` is for, and a script branching on whether
    there is anything to send wants a status code, not an empty array.
  - `jired list`'s filters — `--sprint`, `--status`, `--assignee`, `--label`,
    `--issue-type`, `--summary-contains`, `--keys` — get a section and a table,
    with `--keys` shown feeding the commands that take several issues. A test
    renders the flag list from `ListArgs`, so a filter added later cannot go
    undocumented.
  - `transition`, `assign`, `set`, `comment`, `label` and `logwork` get full
    signatures, the way `sprint` already had them. The status goes after `--to`;
    `jired transition KEY Done` is exit 2, and guessing it was two wasted
    round-trips on the most common write in the tool.
  - `pending_ops`, `push --resume` and `push --refresh-stale` are described: what
    the queue is, that the edit is still in the file, and how to clear it.

## [0.6.0] — 2026-08-30

### Breaking

- **An `assignee:` edit is sent as its own operation, not as a field.**
  `push --dry-run` now reports it as an `assign` op against
  `/issue/{key}/assignee`, where it used to appear inside `update_fields` as
  `body.fields.assignee` — a script reading the assignee out of the field
  payload must read the `assign` op instead. Assigning is not editing: Jira
  gives it a route because it is a permission of its own, and an instance whose
  edit screen does not carry `assignee` — an ordinary configuration — refuses
  the field update while accepting the route. Clearing puts the null on the id
  (`{"accountId": null}`) rather than on the field. Verified on the re-read like
  every other write that can be accepted and not performed.

- **Jira Data Center older than 10.0 is refused, where 9.0 was the floor
  before.** jired's Data Center routes are now checked in CI against the API
  description a Jira 10.7.4 publishes of itself, and that is the version the
  promise is made about. Nothing jired uses was removed between 9.12 and 10.7,
  so a 9.x instance would very likely still work — but "very likely" is not a
  support floor. `init` refuses with exit 10 before writing anything, naming
  the version it found and the one it needs.

### Added

- **A field the screen does not carry is refused before the server sees it
  (Data Center).** Jira configures fields per project, per issue type and per
  screen, so an issue can have a priority while the screen it is edited through
  offers no way to set one — and that write comes back a 400 naming a screen,
  after the rest of the push has already gone out. `push` and `create` now ask
  the edit and create screens first: a field they do not carry is exit 2 at plan
  time, visible in `push --dry-run`, naming the field, with the other issues in
  the push unaffected and the edit still in the file. A server that describes no
  screen refuses nothing. Note that a push which used to fail on the server's
  400 (exit 1) now fails at plan time with exit 2.

- **`jired assign --user` searches only the people who can be assigned that
  issue.** Assignment is a permission, held per project and per issue-security
  level, so the assignable users are a smaller set than the instance's
  directory — and jired had been searching the directory, offering names the
  server would then refuse on a push that had already got as far as sending.
  It now asks `user/assignable/search`, scoped to the issue in hand, on both
  deployments. A name that no longer resolves is a name that could not have
  been assigned.

- **Every Data Center route is checked against the server's own description in
  CI.** `src/api/datacenter/routes.rs` names the thirty-two REST routes jired
  calls by the `method id` Jira itself gives each one, and a contract test reads
  the vendored WADLs and fails the build on a route that does not exist, a
  method spelled wrong, or a query parameter the server never declared. The
  three things jired reaches that are not REST at all — the Move wizard, the
  attachment bytes, and the description endpoint itself — are listed beside the
  table with a reason each, and the test asserts none of them has quietly become
  a route.

- **jired carries the API description its Data Center routes are checked
  against.** `specs/datacenter/10.7.4/` holds the three WADLs a Jira Data
  Center 10.7.4 publishes for itself, recorded with the host replaced by a
  reserved name and otherwise byte for byte as the server sent them. The
  recorded Data Center fixtures replay the same files, so the description a
  test reads and the description `fetch-spec` writes are one document.

### Fixed

- **`pull` writes every comment, and every worklog, not the first page.** An
  issue payload carries the first 50 comments and 20 worklogs and states how
  many there are; jired read the page and dropped the count, so a long
  discussion produced a file showing fifty comments and no sign that seventy
  were missing. The worklog half had looked correct since it was written and
  never worked: the completed page was set on the parsed issue while `fetch`
  stored, and `pull` re-read, the untouched server payload — so everything it
  fetched was discarded on the way to disk. Both are now completed into the
  payload itself.

- **Data Center worklogs are fetched in one call, as the API declares.** The
  description declares no paging on `issue/{key}/worklog` and the server answers
  with all of them; jired had been sending `startAt` and `maxResults` and looping
  on a total. Cloud's route does page, and keeps its loop.

- **The scale benchmark runs again.** It looked for issue files by an unpadded
  name and had found none since filenames started zero-padding the issue
  number, so the suite CI runs on every push was failing on its own expectation
  rather than on anything it measures. It now names files through the same
  function the projector does. The numbers it records are unchanged.

- **`move_parent` and `assign` are in the published `push` schema.** 0.4.0 added
  the `move_parent` op without adding it to `schemas/push.json`, so a push that
  actually moved a sub-task emitted `--json` that failed validation against
  jired's own schema. Both op kinds are now in the enum.

## [0.5.1] — 2026-08-29

### Fixed

- **The sub-task move is refused under a credential that cannot reach it.**
  0.4.0 shipped the Move wizard without noticing what authenticates one. Data
  Center's web pages go through Seraph, which knows about basic auth and about
  a session cookie and not about a bearer token, so a personal access token —
  `init`'s own recommendation there, and fine for everything else jired does —
  is redirected to `SessionTimeoutMessage.jspa` the moment it asks for the
  wizard. Jira serves that page as HTTP 200, so nothing in the reply says the
  credential was refused: 0.4.0 read the page as the form, posted against it,
  and only the re-read afterwards caught that the parent had not moved.

  The credential is known before anything is sent, so the refusal is now made
  in the plan — exit 2, visible in `push --dry-run`, naming `auth.kind = basic`
  as the way out — instead of after every other operation in the push has
  already gone. `jired parent` warns while staging, for sub-tasks only; the
  staged line is still what the file should say once the move is made in Jira.

- **A wizard that never appeared says where the request landed.** The first
  reply now has to *be* the wizard: 0.4.0 treated a page with no form as the
  wizard having finished, which is true after a step has been posted and false
  before one has. When something else answers, the error carries the URL the
  request came to rest at — a status line reading 200 explains nothing, and
  `…/SessionTimeoutMessage.jspa` explains all of it. Reported with the debug log
  that made the cause findable at all; without it the failure named no reason.

## [0.5.0] — 2026-08-29

### Added

- **`jired fetch-spec` writes the server's own description of its REST API.**
  Atlassian publishes no OpenAPI document for Jira Data Center — the request for
  one has been open for years — and for the Agile API, where `boards`, `sprints`
  and every sprint move live, it publishes nothing at all. The instance itself
  serves both, for the version you actually run, and this fetches them.

  ```sh
  jired fetch-spec --output ./specs
  ```

  ```
  Corp Jira 9.12.4 at https://jira.corp.example/jira
    jira.corp.example-datacenter-9.12.4-api-2.wadl      1.2 MiB
    jira.corp.example-datacenter-9.12.4-agile-1.0.wadl  86.4 KiB
    auth-1 — this server does not serve one
  2 files into /home/you/specs
  ```

  One file per REST module the server describes, named for the server and the
  version that answered: a bare `application.wadl` in a directory is a file
  nobody can place later, and no two Jiras answer the same. A module a given
  Jira does not mount is reported rather than failing the run, and so is a module
  the credential may not read — a token without permission for the Agile plugin
  still gets the platform API. A 200 that is not a description is skipped with a
  warning instead of written, because an instance behind an SSO proxy answers its
  login page for every path, and an HTML login page saved as a `.wadl` lies for
  as long as it sits there. `--json` reports it as `jired/fetch-spec/1`.

  Jira Cloud has nothing to fetch and says so with exit 10: an Atlassian-hosted
  site serves no description of itself, and the OpenAPI document that covers
  Cloud is published by Atlassian rather than by the site. The hint names that
  URL rather than jired fetching it — the credential belongs to your Jira site
  and has no business travelling to `developer.atlassian.com`.

## [0.4.0] — 2026-08-29

### Breaking

- **A sub-task's `parent:` is sent on Data Center instead of refused.** 0.3.3
  made the change exit 10 at plan time, which was the honest answer while jired
  had no route for it; it now has one, and a push that used to fail applies the
  move. A workspace scripted around the refusal — treating exit 10 on a
  `parent:` edit as "leave it staged" — will find the edit gone through instead.
  The two moves Jira genuinely cannot make are still refused, now at exit 2: a
  sub-task with no parent at all, and a parent in another project.

### Added

- **Sub-tasks move between parents on Data Center.** Jira exposes this nowhere
  in its REST API — `PUT /issue` takes a sub-task's `parent`, answers success
  and leaves it where it was, which is [JRASERVER-68763], open since 2018 — so
  `push` drives the same Move wizard the UI does: it fetches the form Jira
  serves, posts back every field that form declared with the new parent filled
  in, and follows the confirm step. Reading the form rather than assuming its
  shape is what keeps it working across versions, and `push --dry-run` shows it
  as `move_parent` against `/secure/MoveSubTaskParent.jspa` rather than
  pretending it is a REST call. It is a web action, so success is never taken
  on its word: the re-read that already follows every push checks the parent
  actually moved, and one that did not is a failed operation with the edit
  still in the file. Cloud is unchanged — there the field update works, and it
  is still what goes out.

- **`jired parent ISSUES… --to ISSUE`** stages the same change without opening
  the files, across as many sub-tasks as you name — the case it exists for is
  the four sub-tasks filed under the wrong story. Staged like `label` and
  `sprint add`: `status` and `diff` show it, `push` sends it, `--push` sends it
  now. An issue already under that parent is reported and not staged; an issue
  named as its own parent is exit 2.

[JRASERVER-68763]: https://jira.atlassian.com/browse/JRASERVER-68763

## [0.3.3] — 2026-08-29

### Fixed

- **A push no longer believes the server.** Jira answers success to payloads it
  has no intention of applying, and `push` read that answer as the work being
  done: it marked the operation `done`, exited 0, rewrote the file from the
  server — and the edit that was never sent anywhere was gone, with the report,
  the exit code and stdout all saying it had worked. The re-read that rebuilds
  the file is now asked first whether the change is in it. An operation the
  server did not act on is `failed`, the issue is `ok: false` and the push exits
  10, base does not advance and the file is left exactly as it was written.
  Checked are `parent` and the sprint — the two whose payload and whose
  rendering can be compared without guessing, and the two Jira is known to
  swallow.

- **A sub-task's sprint and parent are refused before they are sent.** A
  sub-task is not a board item: it takes its sprint from its parent, and the
  board call for one is accepted and ignored. That is now exit 2 at plan time,
  the hint naming the parent to move instead. Its `parent:` is the same on Data
  Center, where Jira re-parents a sub-task only through the Move operation,
  which the REST API does not offer: exit 10. Both are decided in the plan, so
  `push --dry-run` shows them — a clean green plan was what made the silence
  worth trusting — and both leave the edit in the file.

- **A failed operation says what was sent.** `push --json` set `request` to
  `null` on anything but a dry run, so a failure carried the server's message
  and no way to see the call it answered. A failed op now carries its request;
  the error already names the HTTP status.

## [0.3.2] — 2026-08-28

### Fixed

- **The indent a region is written at is still not an edit — this time past the
  first line.** 0.3.1 took the container's prefix off the marker's own line,
  which is only where the indent is when the region starts one. A region that
  opens mid-line — a `{color}` macro inside a numbered item — runs onto lines
  the list indents and the store never saw, so 0.3.1 still refused those
  issues for anything at all. What comes off now is the indent the region's
  lines *share*, off both sides of the comparison, so indentation belonging to
  the region itself survives and a change to it still reads as one.

- **A read-only region in a comment or a worklog note is covered by the same
  gate.** Only the description was checked, so an edit inside a region anywhere
  else was dropped on the way out: the payload is rebuilt from the stored
  original either way, and `push` reported success having sent the original
  text back over a comment somebody else wrote. It now fails with exit 6 like
  the documentation always said it would.

- **`--allow-lossy` sends the edit it was reached for.** It skipped the gate and
  nothing else, so the conversion still rebuilt the region from the store: the
  edit was discarded and the original markup — the thing the flag says it
  replaces — went back to the server unchanged. The markers now come out of the
  text, and what the file says is what is sent, as plain Markdown.

- **`status` reports an edit to a comment that is already on the server.** It
  counted only new comments, so editing an existing one left the workspace
  reported clean while `push` went on planning the `update_comment` for it.
  `status` and `push` now ask the same question about regions too, instead of
  `status` looking at the description alone.

## [0.3.1] — 2026-08-28

### Fixed

- **A read-only region that is not at the left margin no longer blocks the
  whole issue.** A region inside a quote or a list is written with its
  container's indent on every line, the `<!--jired:raw:…-->` markers included.
  The comparison that decides whether the region was edited did not know about
  that indent, so it read it as an edit that had never happened — and `push`
  then refused the issue for anything at all, booking time against it or
  putting a label on it. jired contradicted itself in the process: `status`
  said `fields: ["worklogs"]` and `lossy_touched: true` in the same breath, and
  `diff` showed a description that had not moved. The indent is stripped before
  the comparison now; a real edit inside the region still stops the push.

- **`diff` no longer reports the entry above an appended one as changed.** Each
  section is joined with newlines, so its last line ended without one and
  growing a line ending counted as an edit. Booking time therefore accused you
  of rewriting the previous worklog's note, byte-identical text on both sides
  of the `-`/`+`, in exactly the place you read a diff to check that.

## [0.3.0] — 2026-08-28

### Added

- **`jired create --like PROJ-142`.** "Create a task like that one" had no
  expression, so it meant reading the model issue and typing six fields back.
  It copies type, priority, labels, assignee, components and custom fields —
  what describes the *kind* of issue — and never identity, status, summary,
  description, comments or worklogs, which belong to the issue that already
  exists. Flags and `--set` override it.

- **`jired show ISSUE [--raw]`.** An unset field is not written into a `.md`
  file, so the file cannot answer "is this empty, or does this instance not have
  that field?" — the answer was `sqlite3 .jired/state.db` and `json_extract`
  over `remote_raw`. `--raw` is the server's own payload; the default is the
  issue as jired models it, read from the file when there is one and the
  snapshot when there is not.

- **`jired fields [TEXT] [--custom]`.** `custom:` keys a value by its Jira id,
  the only name stable across instances and the only one nobody can read.
  Finding out what `customfield_10015` is meant reading `field_meta` out of the
  state database; it is now a command, filterable by name or id.

### Fixed

- **`due_date` is a field of its own, and can finally be read.** Jira's
  `duedate` is a system field, and `custom:` only ever collected
  `customfield_*` — so a due date set on the server reached no file, and one
  written into `custom:` by hand could be sent but never seen again. It is now
  named in the frontmatter, read from the server, sent back, and checked as a
  date before a push rather than after. `custom:` still accepts any field id,
  including a system one jired does not model, which the docs now say.

## [0.2.0] — 2026-08-28

### Added

- **`jired push --refresh-stale` recovers from a stale push in one step.**
  Sending a label to six issues and having one refused because somebody logged
  work on it meanwhile took three more commands — `pull --dry-run` to check the
  incoming change was not a conflict, `pull`, `push` — and a full pull of the
  workspace to send one label.

  The flag re-fetches the refused issues, merges them, and pushes again the ones
  that still merge cleanly. That is the right test for "the staleness does not
  touch what I am sending", and it is one that already existed: a three-way
  merge conflicts exactly when the incoming change and the local one touch the
  same field. A real conflict is still refused and now says why. Detection is
  unchanged — this is only the recovery path.

- **`jired create` takes the fields an issue is raised with.** It accepted only
  `--type`, `--summary`, `--parent`, `--description` and `--template`, so
  everything else meant creating the file, editing the YAML by hand and pushing:
  three steps and a hand-edit for one API call. It now takes `--assignee`,
  `--sprint`, `--labels`, `--priority`, `--component`, `--epic`, and
  `--set field=value` for anything else including custom fields by id.

  ```sh
  jired create -s 'Ship SSO' --assignee nilsson.m --sprint 'Sprint 44' \
               --labels regular --push
  ```

  `--template` now also takes a path, so a skeleton can live in the repository
  being worked on rather than being copied into every workspace, and a template
  no longer has to carry a placeholder `key:` to get past the parser — which the
  README had always said it did not.

- **`jired list` picks issues out of the working copy, offline.** `search` is
  JQL on the server; asking the local files "which issues say X" had no answer,
  so it was answered with `grep -l '^summary:.*\[ALL\]' *.md` — the caller
  writing regexes against jired's own file format, and getting it wrong whenever
  a filename and a summary disagree.

  ```sh
  jired list --summary-contains '[ALL]' --status 'In Progress'
  jired list --keys --label regular | xargs jired sprint add --sprint 'Sprint 44'
  ```

  Filters for summary, label, status, sprint, assignee and type, all
  case-insensitive and all conjunctive; `--keys` prints keys alone so the output
  is an argument list for the commands that take issues. It reads the files
  rather than the snapshot, so what it lists and what a following `label add`
  edits are the same set.

- **`--fields` and `--json-compact` cut a result document down.** A full
  document is a complete record, which is the wrong size when the caller wanted
  three values: reading them meant piping jired's own JSON through a script that
  re-printed a subset of it.

  ```sh
  jired status --json --fields dirty,new_issues
  jired push --json --fields summary,results.key,results.ok
  jired push --json --json-compact     # no nulls, no empty lists
  ```

  A `--fields` path walks objects and maps over arrays, so `results.ok` keeps
  the `ok` of every result rather than needing an index nobody knows in advance;
  `schema` always survives. Both are opt-in, because dropping keys by default
  would break every schema's `required` list for consumers who are not counting
  bytes — and a shaped document is deliberately no longer schema-valid.

- **`jired push --json` leads with a summary.** `{"ok": 5, "failed": 0,
  "stale": ["PROJ-194"]}`, first in the document, so checking whether a push
  went through is one read rather than a walk over every operation of every
  issue.

- **`jired label` and `jired set` edit fields across many issues.** Putting one
  label on six issues meant a script that rewrote `labels:` in the YAML — the
  caller parsing jired's own file format, which is the thing the format exists
  to spare people, and which breaks the first time the format moves.

  ```sh
  jired label add regular PROJ-60 PROJ-65 PROJ-193
  jired label remove regular PROJ-60
  jired label set a,b PROJ-60 --push

  jired set --field priority --value High PROJ-60 PROJ-65
  jired set --field components --value Auth,API PROJ-60
  jired set --field customfield_10456 --value Platform PROJ-60
  ```

  `label` has three verbs because labels are a list: `add` and `remove` touch
  one and leave the rest, `set` replaces them. `set` writes any name in the
  editable frontmatter list, or a `customfield_*` id, with commas separating a
  list and an empty value clearing the field. Both stage like `assign` does —
  the file is written, `status` and `diff` show it, `push` sends it, `--push`
  sends it now — and both report an issue that already had the value instead of
  staging a change that does nothing. `--json` reports them as `jired/label/1`
  and `jired/set/1`.

### Fixed

- **`push` no longer drops fields when it creates an issue.** A locally created
  issue sent `project`, `summary`, `issuetype`, `description`, `labels`,
  `parent` and `priority` and silently discarded the rest — `assignee` most
  visibly, but `components`, `fix_versions`, `affects_versions` and
  `story_points` went the same way. `diff` had promised to send them, `push`
  reported clean success, and the values were gone from the rewritten file with
  nothing saying so; the only way to notice was to re-read the file after every
  create.

  The cause was two independent lists of what a frontmatter field becomes on the
  wire: a field handled for updates was simply absent from the create path.
  There is now one mapping, read by both, and a test asserts every editable
  field is answered by it — sent, carried by another operation, or refused out
  loud.

- **A field push cannot send now says so instead of reporting success.** `epic`
  and `links` are in the file format and `diff` calls them sendable, but push
  has no call for either and never mentioned it. Both now produce a warning
  naming the field and the issue. They are still not sent — that is a feature,
  not a fix — but a push no longer claims to have done something it did not.

- **`push --dry-run` shows the operations that follow a create.** Moving the new
  issue into its sprint, and transitioning it to a status the workflow does not
  start in, happen after the create call and were invisible until the file came
  back. The plan now lists them, and a real run reports each one, so the plan
  and the outcome line up operation for operation.

## [0.1.0] — 2026-08-28

The first release: the whole offline-first cycle — `init`, `fetch`, `pull`,
`diff`, `push` — plus a usability pass over the CLI surface, covering what the
flags are called, what happens before something destructive, what an error says,
and what the docs claim.

### Breaking

- **`jired agent-docs` is now `jired init-agents`.** It no longer only writes
  docs: it also writes the permission files that let an agent run the commands
  that cannot change anything, and a name saying "docs" hid half of what it
  does. `jired init --no-agent-docs` is likewise `--no-agents`, the JSON field
  `workspace.agent_docs` is `workspace.agent_files`, the schema is
  `jired/init-agents/1`, and `doctor`'s `agent-docs` check is `agents`.

- **Connection flags moved to the commands that read them.** `--url`, `--token`,
  `--email`, `--auth`, `--deployment` and `--insecure-store` are now options of
  `jired init` and `jired doctor` instead of global flags. Elsewhere they were
  accepted and silently ignored, so `jired pull --url other` looked like it
  switched servers while using the stored account.
- **`--profile`, `JIRED_PROFILE` and `workspace.profile` are gone.** Nothing read
  them — the session store hardcodes the `default` profile — so they promised a
  feature that did not exist. They return with multi-profile support.
- **`clean --regroup` removed.** It returned before ever looking for orphans,
  disabling the command's own purpose, and duplicated `jired doctor --regroup`,
  which is now the way to rebuild grouping folders.
- **`diff --all` replaced by `diff --against base|remote`.** `--all` widens the
  scope on every other command; on `diff` it selected a comparison. The old
  spelling still parses but is hidden and deprecated.
- **`jired push` now asks before sending.** An interactive push prints the plan —
  the same lines `--dry-run` produces — and asks `send N operation(s) across M
  issue(s)?`. `-y` and non-interactive runs skip the question, so scripts are
  unaffected.
- `jired doctor` previews its repair list and asks before renaming files,
  rewriting config or moving credentials. `-y`, `--no-input` and `--dry-run`
  behave as before.
- `projects`, `boards` and `sprints` lost `--refresh`, which was a no-op, and
  `boards --project` is now `boards --for-project` (the old name shadowed the
  global `--project`).
- `search --pull` and `resolve --field` are hidden and now fail immediately with
  exit 2 instead of exit 1 after doing part of the work; neither is implemented.

### Fixed

- **A subtask whose summary ends in a space is no longer a permanent
  difference.** `## Children` is line-based, so reading a file back trims the
  title the snapshot still held: `diff` reported every issue with such a child as
  modified while `status` called the workspace clean and `push` had nothing to
  send — with no user edit involved, on a section a push cannot act on anyway.
  The title is now trimmed where it is built from the server, which is what the
  checklist already did to every title of its own, and again where the line is
  written, so what jired writes it reads back unchanged. No file is rewritten:
  the difference stops being reported at once.

- **`-v` shows the REST paths it always claimed to.** The flag is documented as
  revealing "REST paths, pids, causes" and no request was ever logged, so the way
  to find out whether a call had even been made was to read the source — which is
  the question a debug flag exists to answer. Every request now logs its method
  and path at `-v`, with the status and elapsed time when it returns, and the
  request body at `-vv`.

- **`push` no longer refuses an issue the server has not touched.** Jira reports
  `updated` to the millisecond and the snapshot in `state.db` keeps whole
  seconds, so the same instant reached the staleness check spelled two ways and
  every push against an issue whose `updated` had a non-zero millisecond was
  refused as a conflict — with a message printing what looked like two identical
  timestamps. `pull` could not clear it, because snapshot and server really did
  agree; only `--force` got through. The check now compares whole seconds, which
  is the precision both sides have.

  Its message also spells the offset on both timestamps (`2026-08-28T08:42:10+02:00`)
  rather than printing bare local time, so it can be checked against the UTC
  `jired.base_updated` in the file without doing the subtraction in your head.

### Added

- **`jired version --changelog` prints the changelog the binary was built
  from.** It is compiled in, so the question that follows "which version is
  this" no longer needs a checkout to answer — and answers it for *that* binary,
  rather than for wherever a `CHANGELOG.md` beside it has got to. `jired version`
  on its own is what `--version` prints; `--json` reports both as
  `jired/version/1`, with `changelog` null unless it was asked for.

- **`jired sprint add` and `jired sprint remove` move issues between sprints.**
  The scriptable single-field commands for an issue's sprint, alongside
  `transition`, `assign` and `comment`, and staging the same way — the
  frontmatter is written, `status` and `diff` show it, `push` sends it, `--push`
  sends it now:

  ```sh
  jired sprint add PROJ-142 PROJ-150 --sprint "Sprint 44"
  jired sprint remove PROJ-142            # back to the backlog
  ```

  `--sprint` takes a name or an id, and a name no board knows is an error while
  the typo is on screen rather than a warning after the push has skipped the
  issue. An issue already in the sprint is said so, not staged. `--json` reports
  it as `jired/sprint-move/1`.

  `jired sprint create --issues K1,K2 [--push]` does both in one command, since
  creating a sprint is almost always followed by filling it.

- **`jired init` grants agents the commands that only read.** Setting a
  workspace up now writes `.claude/settings.json` and
  `.codex/rules/default.rules` alongside `CLAUDE.md` and `AGENTS.md`, so Claude
  Code and Codex can run `jired status`, `jired diff`, `jired log`,
  `jired report`, `jired search`, `jired fetch`, the `--dry-run` form of `pull`,
  `push`, `reset`, `clean` and `doctor`, and the rest of the read-only surface
  without stopping to ask. An agent that has to be approved before every
  `jired status` is an agent nobody lets near the workspace, and approving each
  one by hand is how a blanket approval ends up granted instead.

  The line is drawn at "sends nothing to Jira and writes nothing into the
  working copy" — a refresh of the read-only snapshot under `.jired/` is
  allowed, since no file you edit depends on it — and every writing command is
  left out. One table in the source produces the allow-list, the Codex rules and
  the table in the generated docs, so the three cannot disagree; a test proves
  every granted command still exists and that no writer is on the list.

  Claude's file is merged rather than rewritten: entries jired did not put there
  survive, including ones for commands it no longer generates. Codex's rules
  live between `# BEGIN jired` and `# END jired`, so hand-written rules in the
  same file survive too, and Codex applies them once the project's `.codex/`
  layer is trusted. `jired doctor` refreshes both when they fall behind the
  binary, and `jired init-agents --no-docs` / `--no-permissions` rewrite one
  half on its own.

- **`jired sprint create` makes a sprint.** A form when a terminal can draw one,
  and `jired sprint create "Sprint 44" --board 12 --start 2026-09-01 --end
  2026-09-15 --goal "…"` when it cannot; `--json` reports it as
  `jired/sprint-create/1`. A sprint has no file to stage it in, so this is the
  one write that goes straight to the server rather than waiting for `push` —
  which is why it has a `-n/--dry-run` of its own that prints the exact call.
  Jira takes both dates or neither, so an end without a start is refused rather
  than sent and silently dropped, and the new sprint is always `future`.

  The form prefills the name the board is due — a board whose last sprint is
  `Sprint 42` is offered `Sprint 43`, renumbered when you change the board and
  left alone once you type over it — and offers the dates a sprint usually runs
  between. It says when a name is already taken on that board rather than
  refusing it, because Jira allows it.

- **`sprints --board` takes a name, and a project with one board needs no
  `--board` at all.** `jired sprints --board "Platform Board"` used to ask the
  server for a board by that literal id and fail; `jired sprints` with nothing
  named used to exit 7 even when there was only one board it could have meant.
  An id still goes straight through, so a board outside the workspace project is
  still addressable.

- **Colour across the read commands, and a `--no-color` to turn it off.**
  `log`, `search`, `projects`, `boards`, `sprints`, `whoami`, `transition --list`,
  `resolve --list`, `clean` and `doctor` printed plain text; they now follow the
  vocabulary `status`, `diff` and `push` already used — keys cyan, headings bold,
  meta dim, what was there red and what replaced it green, warnings yellow.
  Statuses and sprint states are coloured by Jira's own category rather than
  their names (`new` dim, `indeterminate` yellow, `done` green), so an instance
  that renamed its workflow still reads correctly and a category jired does not
  recognise is left uncoloured rather than guessed at. `jired search --json`
  gains `status_category` alongside `status`.

  Colour was already on for terminals and off for pipes, decided per stream;
  `--no-color` is the spelling every other tool uses, equivalent to the
  `--color never` this one always had. Naming both is a usage error rather than a
  precedence rule to remember, and an explicit `--color always` still outranks
  `NO_COLOR`.

- **Times print in your own timezone.** `jired log`, `jired status`,
  `jired report time` and every message jired writes to a terminal now show
  `2026-08-16 14:03:00` — your clock, no zone marker — instead of the UTC the
  server happened to record. (`report time` still counts whole UTC days, so its
  `2026-08-16 → 2026-08-22` span is unchanged; only the `last fetch` line moved.) What jired
  *keeps* is unchanged and still UTC: `--json` documents, issue files and their
  frontmatter, `.jired/state.db`, and everything sent to Jira, because a record
  has to mean the same thing on every machine that reads it. Where the two meet —
  a worklog's start time, which the `logwork` form shows you and then writes into
  the file — the offset is spelled out, so it reads as the clock you were looking
  at and still names one instant. `--started` and a hand-written `### [new]`
  worklog heading now accept a time with no zone (`2026-08-21 09:00`), reading it
  as yours; one that names its zone is kept exactly as written.

- **`jired log` with no issue reads like `git log`.** It merges every issue's
  history into one newest-first list, each entry tagged with the issue it belongs
  to; naming an issue still narrows to it. The first run fetches the changelogs it
  is missing, and every run after it is offline: freshness is now the issue's own
  `updated` at the time its changelog was cached, so an issue with no history —
  a real answer, not an empty cache — is no longer asked about on every run, and
  one the server has moved on from is refetched exactly once. `jired/log/1` gains
  `scope` and a per-entry `key`, and its top-level `key` is null in workspace mode.

- **The issue picker is a sortable table, and `jired comment` opens it too.**
  `edit`, `comment` and `open` all ask "which issue?" the same way now, on a
  screen showing key, summary, status, priority, assignee, reporter and the ages
  of `updated`, `created` and `viewed`. `^s` moves the sort to the next column
  (pointing the way that column is usually read — newest first for a clock,
  most urgent first for priority), `^r` reverses it, and `^f` narrows the list to
  one status or to a whole status category. Typing still filters on the key and
  the summary alone: an age or an assignee that happens to contain the letters
  typed must not pull a row in. Columns that do not fit are dropped from the
  right, keeping the key, the summary and whichever column is sorted, and rows
  with nothing in the sorted column go last in either direction. `edit` and
  `comment` list only the issues with a file, because that is where they stage
  the change; `open` lists every issue the workspace has a snapshot of, since a
  browser URL needs no file, and leaves out locally created `NEW-*` issues, which
  have no server key to browse. `comment`'s and `open`'s issue arguments are
  optional as a result, so a scripted `jired comment` with nothing after it now
  exits 7 naming what is missing rather than 2 from clap.
- **`ui.issues.sort` and `ui.issues.filter`**, which the picker writes back to
  the workspace config itself, so the list opens the way it was left — set them
  by hand (`jired config set ui.issues.sort priority:asc`) or with `^s`/`^r`/`^f`,
  which are the same thing spelled differently. `config set` rejects a value
  neither can read, naming the columns and the filters that exist.
- A `viewed` column, from a `viewed_at` stamp `state.db` now keeps: when *this
  machine* last went to an issue, written by `open` and by every command that
  stages a change. It is the one thing in that database a `fetch` cannot rebuild,
  which is deliberate — the server does not know it and never will, and losing it
  with the database costs one sort order and nothing else.

- **`jired reset [ISSUES…]`** drops local changes: every dirty file is re-rendered
  from its base snapshot, byte-identical to what the last pull wrote, so the issue
  has nothing left for `push` to send. The mirror of `pull --force` pointing the
  other way — backwards to the last sync rather than forwards to the server. It is
  offline and never reads the working copy to do its job, which makes it the way
  out of an edit that left a file unparseable. Recorded conflicts and the
  pending-op journal go with the edits that caused them, and a card dragged
  between board columns is put back. Staged attachment uploads and locally created
  `NEW-*` issues are reported rather than deleted, because base cannot bring
  either back; `--include-new` opts into deleting the second. `-n/--dry-run` lists
  what would go, field by field; the listing precedes the confirmation, and a
  non-interactive run needs `-y` or exits 7.

- **`jired report time`** — who booked how much time, as a grid of a row per
  person and a column per day, totalled off the `## Worklogs` sections already in
  the working copy, so it answers offline. `--days N` sets the window (whole
  days, today included, default 7) and `--show-issues` opens each person into the
  issues their time went to. In a terminal it is a screen whose keys re-ask the
  question without re-running the command: `i` for the issue level, `+`/`-` for
  the width of the window, `<`/`>` to walk it back over earlier weeks, `t` to
  return to today, `?` for the rest. A pipe, `--json` or `--no-input` gets the
  same grid as a table and as a `jired/report-time/1` document.

- **Install and licence.** `LICENSE-MIT` and `LICENSE-APACHE`, and a README
  install section: `cargo install --git … --locked --bin jired`, Rust ≥ 1.89, and
  what a Linux machine without a Secret Service provider needs.
- `make install` now writes the bash and zsh completions as well as the binary,
  into the XDG user directories, so an upgrade cannot leave a script completing
  flags the binary no longer takes. `make install-completions` refreshes them on
  their own, `make uninstall` removes them, and `BASH_COMPLETION_DIR` /
  `ZSH_COMPLETION_DIR` move them.
- **README sections** for the file format (editable vs read-only vs identity
  frontmatter, `### [new]` comments, checklist semantics, `links:`, `lossy: true`,
  conflict markers), for the ~17 commands it never mentioned, for grouping folders
  and board moves, and a "when something goes wrong" table from exit code to
  cause to command.
- Issue arguments accept a **file path** anywhere a key is accepted:
  `jired comment "PROJ-0142. Fix login flow.md" -m "on it"`.
- Positional `[ISSUES]…` on `fetch` and `pull`, so `jired pull PROJ-1` works like
  `jired push PROJ-1` always did. `--issues` remains as the flag spelling.
- Positional JQL on `search` (`jired search "project = PROJ"`), a
  `showing N of TOTAL` line, an explicit "no issues match", and a warning when
  `--limit` hits the server's 100-result ceiling.
- `jired config path` and `jired config edit`; `config list` names the file each
  value came from; `config set` validates every key against its declared type and
  suggests a near spelling for an unknown key. None of the `config` subcommands
  needs an initialized workspace any more, so the README's own
  `config set creds.storage user_config --global` recipe works before `init`.
- `pull` gained `-n`/`--dry-run`. `push --dry-run` gained `-n` and a visible
  `preview` alias.
- `$VISUAL`/`$EDITOR` support: `comment` without `-m` at a terminal opens an
  editor (empty buffer aborts), and `config edit` opens the config file.
- `create` prompts for what it needs interactively and takes `--push`.
- `--color auto|always|never` (honouring `NO_COLOR`), progress reporting during
  fetch/pull/push, git-style "what to run next" trailers in `status`, and a
  counted summary from `push`.
- `jired help <command>`, root and per-command examples, `st` as an alias for
  `status`, and `next_help_heading` so a command's own flags come before the
  global ones.
- Agent docs gained a conflict-recovery section, the `NEW-<hex>`/templates rules,
  and the `JIRED_SECRET_KEY`/`JIRED_NO_KEYRING`/`JIRED_LOG`/`JIRED_FIXTURES` rows
  that appeared in no table before. The editable/read-only/identity field lists
  are now rendered from the constants the parser enforces, with a test that they
  cannot drift.

### Changed

- `push` exit codes: when every per-issue failure shares one kind, the run exits
  with that kind's own code (all-lossy → 6, all-auth → 3) instead of collapsing to
  1. Mixed causes still exit 1.
- A lossy block is now a failed result for that issue rather than an error that
  aborts the run, so the other issues still push and the report of what already
  succeeded is never discarded.
- `pull --force` and `pull --strategy theirs`, and `resolve --theirs`, list the
  issues whose local edits would be lost and ask before discarding them.
- Errors name the thing that failed and carry a hint: HTTP failures say
  `assignee of PROJ-7: <what the server said> (HTTP 400)` and keep the REST path
  for `-v`, network errors show the innermost cause instead of the whole reqwest
  chain, database errors say *which* database, the lock error names the holding
  pid and when it started, and frontmatter parse errors give a line and column.
- `status --all` lists unchanged issues instead of parsing the flag and ignoring it.
- `--no-input` is honoured from `JIRED_NO_INPUT` and `ui.no_input`, not only from
  the flag.
- `AuthKind` and `Deployment` are proper enums in the CLI, so bad values are
  rejected with the accepted list, and `config <KEY>`/`schema <NAME>` complete.

### Fixed

- **`jired diff` no longer reports sprint history as a change.** `sprint:` is a
  list of the sprints an issue has been through, and only the last of them —
  where the issue is now — is something a push can act on. `status` and `push`
  already knew that; `diff` did not, so a workspace whose files were written
  before jired kept the whole list saw every one of them reported as modified,
  burying the changes that were real under changes nobody could send. The rule
  now lives in one place and all three follow it: an issue that differs only in
  sprint history is not listed, and on an issue that has real changes too the
  row is kept for context, marked `"sendable": false` in `--json` and
  `(history, not sendable)` on screen, and left out of the `--stat` counts.
- **A push that moves an issue rebuilds the grouping folders.** `sprints/`,
  `boards/` and `by-parent/` are views over base, and a push moves base — but
  only `pull` rebuilt them, and only when it wrote a file. Moving an issue into
  a sprint and pushing therefore left `sprints/` describing where the issue used
  to be, and the sprint just created had no folder at all until the next pull
  that happened to write something. The rebuild now runs at the end of a push
  that created, transitioned, re-parented or re-sprinted an issue, and reports
  as `groups` in `--json`. A push that only adds a comment, a worklog or an
  attachment leaves the tree alone, so a card dragged between board columns and
  not yet pushed still survives it.
- `sprint create` says where the sprint's folder will appear and when — a sprint
  with nothing in it has no folder, which reads like the sprint never happened.
- **`jired sprints` stops demanding `--board` when the boards all say the same
  thing.** Several boards over one project's backlog commonly share a sprint
  pool, and every answer to "which board?" was the same answer; the command asked
  anyway and exited 7. It now asks the boards in turn and prints what they agree
  on, stopping at the first one that disagrees — where there really is a choice,
  the error lists the choices exactly as before.
- **`jired boards` fills in `project_key` on Data Center.** The board listing
  leaves the project out there, so `--json` reported `null` for every board and a
  script had no way to tell which board belonged to the project it was working
  in. `boards` now asks the board itself for any that arrived unlabelled — one
  request each, in this command only. It is a column on screen too, which is what
  tells two similarly named boards apart.

  Two endpoints answer this and neither answers on both deployments:
  `location.projectKey` in the board's configuration is a Cloud field that Data
  Center omits from that payload entirely, so reading only it left every DC board
  `null` exactly as before. Each deployment now asks the one that answers it —
  the project listing on Data Center — and falls back to the other. A board whose
  filter spans several projects still reports `null`: one project is an answer,
  several is not one, and naming the first would be a guess a script would act
  on.
- `push --dry-run` no longer panics on a summary containing non-ASCII text (it
  sliced a `String` on a byte boundary).
- `jired status | head` exits 0 instead of panicking on a closed pipe.
- Error messages no longer print their cause twice.
- The `--insecure` and `--insecure-store` warnings go through the normal output
  channel, so `--json` stderr stays a single parseable document.
- `transition` without `--to` no longer emits a result document on stdout *and*
  an error on stderr.
- `--dir` pointing at a non-workspace says so instead of creating
  `.jired/session.db` there and then reporting missing credentials.
- `jired init` against a URL with no Jira behind it says so, instead of "the issue
  may not exist".
- `jired projects` outside a workspace no longer claims "this workspace has no
  stored credentials".
- The exit-5 documentation said "nothing was written"; `pull --strategy merge`
  writes conflict markers *and* exits 5. Corrected in the README and in the
  generated agent docs.
- A failed initial pull no longer turns a completed `init` into an error, and the
  "next:" block still prints. `init` gained `--no-agent-docs`.
