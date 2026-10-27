# Command specifications

## Global contract

**Flags on every command:** `--json`, `--no-input`, `-y/--yes`, `-q/--quiet`,
`--silent`, `-v/-vv`, `--color auto|always|never`, `--no-color`, `--dir PATH`,
`--config PATH`, `--project KEY`, `--ca-cert PATH`, `--insecure`. They are
grouped under their own help heading and displayed after the command's own
options.

`-q/--quiet` drops the narration and the progress bars. `--silent` is the whole
of that plus the two things it leaves behind: the warnings, and the `warn`-level
log where the HTTP retries live — a `jired pull --silent` against a slow server
writes nothing where `--quiet` writes a line per attempt. It is the flag for a
cron job or a CI step that should speak only when something went wrong, so the
error and the exit code survive it: an exit code with no reason is not something
anyone can act on. `JIRED_LOG` still wins over the log floor, somebody who set it
having asked for those lines by name, and `--silent -v` is refused rather than
reconciled. Under `--json` the result document is untouched, warnings included:
silence is about the streams a person reads, and dropping a warning out of the
document a machine reads would be losing it rather than quieting it.

**Connection flags** — `--url`, `--token`, `--email`, `--auth`, `--deployment`,
`--insecure-store` — belong to `init` and `doctor`, the only commands that act on
them. Everywhere else they were accepted and ignored, which read like switching
servers; every other command uses the account `init` stored.

**Env vars:** `JIRED_URL`, `JIRED_EMAIL`, `JIRED_TOKEN`, `JIRED_AUTH`,
`JIRED_DEPLOYMENT`, `JIRED_PROJECT`, `JIRED_NO_INPUT`, `JIRED_SECRET_KEY`,
`JIRED_NO_KEYRING`, `JIRED_CREDS_STORAGE`, `JIRED_CA_CERT`, `JIRED_INSECURE`,
`JIRED_LOG`, `JIRED_FIXTURES`, plus standard `HTTPS_PROXY`/`NO_PROXY`.
`config::keys::KEYS` is the full list of settings and `keys::ENV_ONLY` the
variables that are not settings; both feed the help text and the agent docs.

`--profile`/`JIRED_PROFILE`/`workspace.profile` were removed: nothing read them
(the session store hardcodes the `default` profile). They come back with
multi-profile support, not before.

**Colour:** on when the stream is a terminal, off when it is a pipe, decided per
stream (stdout can be redirected while stderr is not). `--no-color` is the
shorthand for `--color never` and conflicts with `--color`, so naming both is a
usage error rather than a precedence rule to remember. `NO_COLOR` and `TERM=dumb`
turn `auto` off; an explicit `--color always` outranks them, because it is a
preference someone just typed rather than one a profile set months ago. The
vocabulary is `style.rs`: issue keys cyan, headings bold, meta and hints dim,
what was there red and what replaced it green, warnings yellow. Statuses go
through `style::status`, which colours by Jira's category — `new` dim,
`indeterminate` yellow, `done` green — and leaves a category it does not
recognise uncoloured rather than guessing. Every column is padded *before* it is
painted: escape codes count towards a format width and would knock the following
columns out of line.

**Timestamps:** what jired *keeps* is UTC — `state.db`, issue files, API payloads
and every `--json` document — because a record has to mean the same thing on
every machine that reads it. What jired *prints* is the reader's own timezone:
`2026-08-16 14:03:00`, no zone marker, because on a screen UTC is a subtraction
the reader has to do and the offset is one they already know. The pair is
`model::format_timestamp` and `model::format_local`; anything going to a terminal
or a TUI uses the second. Where a printed value is also a record — a worklog's
start time, which the form writes into the issue file — `format_local_iso` keeps
the offset, so it reads as the author's clock and still names one instant.
`parse_local_timestamp` is the other direction: it fills in the reader's zone
when they leave it out, which they will, having just been shown a time without
one, and never second-guesses an explicit zone.

**Addressing an issue:** any command that names an issue accepts its key (any
case) or the path to its file — `jired comment "PROJ-0142. Fix login flow.md"`.
`fetch`, `pull`, `push`, `diff`, `resolve` and `attachments pull|list` take a
comma- or space-separated positional list; `--issues` survives on `fetch`/`pull`
as the same thing spelled as a flag.

**Resolution chain (highest first):** CLI flag → `JIRED_*` env → `.jired/config.toml`
→ `$XDG_CONFIG_HOME/jired/config.toml` → built-in default → prompt. `--no-input` (or
non-TTY) disables the last rung; a missing required value then exits 7 with a JSON
error on stderr. Only `server.url`, `auth.email`, `auth.token` and
`workspace.project` are ever prompted for: a setting with a default is answered by
that default rather than by a question, so no command interrogates the user about
TLS, link style or attachment limits.

**Exit codes:** `0` ok · `1` generic · `2` usage · `3` auth · `4` network ·
`5` conflict · `6` lossy blocked · `7` missing value (non-interactive) · `8` locked ·
`9` corrupt state · `10` unsupported server.

**`--json` mode:** stdout carries exactly one versioned result document
(`"schema":"jired/<name>/1"`); stderr carries the structured error and nothing
else:
`{"schema":"jired/error/1","code":5,"kind":"conflict","message":"…","details":{}}`.
Narration and warnings are suppressed — mixing prose into stderr would make the
error document unparseable, so commands that model warnings carry them inside
their result document instead. Ordering is deterministic in all outputs.

`--fields PATHS` keeps only the named paths and `--json-compact` drops nulls and
empty collections. Both are opt-in: dropping keys by default would break every
schema's `required` list, and the bytes matter to a caller reading three values,
not to everyone. A path walks objects and maps over arrays — `results.ok` keeps
the `ok` of every result — two paths under one head merge, a path nobody has is
left out rather than being an error, and `schema` survives both, because a
document that cannot say what it is has stopped being one. A shaped document is
deliberately no longer schema-valid.

## Commands

### `jired init`
`jired init [--url U] [--token T] [--email E] [--auth api-token|pat|basic] [--deployment cloud|datacenter] [--insecure-store] [--project KEY] [--no-pull] [--force] [--no-agents]`
Interactively a three-question wizard — URL, identity, project — and nothing else:

* the URL decides the deployment (`*.atlassian.net`/`*.jira.com` → Cloud, anything
  else → Data Center) and the deployment decides the credential (Cloud: email + API
  token; Data Center: personal access token). The server's own `serverInfo` corrects
  the guess before anything is written.
* the token prompt links to the page that mints one, and offers to open it.
* a failed sign-in re-asks rather than aborting: re-enter the token, switch
  deployment, change the URL, or stop.
* an untrusted TLS certificate is fetched and shown — subject, issuer, validity,
  SHA-256 fingerprint — and pinned to `.jired/trusted-ca.pem` if the user accepts it
  (trust-on-first-use). An expired or wrong-hostname certificate is reported as
  unpinnable rather than half-trusted. Later runs reuse the pinned file silently.
* the project picker lists keys, filters on typing, and accepts an empty answer as
  "every project I can see".

* `creds.storage` decides where the credential goes — `keychain`, `local_db`
  (`.jired/session.db`), or `user_config` (`credentials.toml` in the user config
  directory, shared by every workspace). Unset means keychain-if-available. An
  existing credential for the same host and identity in a shared store is reused
  instead of asked for, except under `--force`.

Then: `GET /myself` verify → `.jired/`, a fully documented `config.toml` (every
setting, including the ones at their default), `.gitignore` entry,
`CLAUDE.md`/`AGENTS.md` and the agent permission files (unless `--no-agents`) →
optional initial pull. A
failed initial pull is a warning, not a failed init: the account is stored and the
"next:" block still prints.
Errors: 3 bad creds; 7 missing url/token with `--no-input`.
JSON: `{"schema":"jired/init/1","account":{…,"tls":{…}},"workspace":{…},"pulled":{…}}`.
Agent example:
`jired init --url https://x.atlassian.net --email a@b.c --token "$JIRED_TOKEN" --project PROJ --no-input --json`

### `jired fetch`
`jired fetch [ISSUES…] [--jql Q] [--sprint S] [--board B] [--issues K1,K2] [--updated-since TS] [--full] [--slim]`
Incremental by default (`updated >= last_fetch − 5min` skew buffer); paginated
(100/page, 4 parallel); 429/`Retry-After` aware (D26). Updates remote snapshot only —
never touches files. Resumable after network failure.
A parent's sub-task list — what `## Children` is rendered from — is then brought
in line with the sub-tasks stored on their own: Jira does not move a parent's
`updated` when a sub-task is created under it, moved, renamed or resolved, so an
incremental fetch never brings the parent back. Of a parent and a sub-task, the
snapshot fetched later is believed; a sub-task the store has no copy of stays as
the parent listed it. A parent whose list changed is *behind* in `status` and is
written by the next `pull`, merged like any server-side edit.
JSON: `{"schema":"jired/fetch/1","fetched":123,"changed":17,"deleted":2}`.

### `jired pull`
`jired pull [ISSUES…] [--no-fetch] [--strategy abort|merge|ours|theirs] [--force] [-n/--dry-run] [fetch scopes…]`
Fetch (unless `--no-fetch`) + three-way merge ([sync-algorithm.md](sync-algorithm.md)).
Default: abort-on-conflict — exit 5, list conflicting files/fields, write nothing.
`merge` writes git-style markers, records conflicts, **and still exits 5**: a
non-zero status does not mean the working copy is unchanged. `ours` and `theirs`
settle each conflict by taking that side: it is written, reported under
`decided` rather than `conflicts`, and the pull exits 0. `links:` is the
exception to field-level merging: it merges as a set and never conflicts (see
`push`). A recorded conflict carries the base, local and remote values of a
frontmatter field, a comment or the description, so `resolve --list` shows what
there is to decide between. `--force` ≡ theirs
everywhere, and both it and `--strategy theirs` list the issues whose local edits
would be lost and ask before discarding them (`-y`/non-interactive proceeds).
`--dry-run` renders the plan without writing.
Prints a change summary: one line per issue, then one line per field with its
before and after — body text and comments as counts rather than quoted content,
custom fields named individually (by the instance's own field name where it has
one). `jired diff` remains the way to see the full text.

```
  updated   PROJ-142
              status             In Progress → In Review
              labels             auth, backend → auth, backend, sso
              custom.Team        Platform → Engineering
              description        +3 -1
              comments           +1
```

JSON: `{"schema":"jired/pull/1","updated":[{"key":"PROJ-1","fields":["status"],
"changes":[{"field":"status","from":"In Progress","to":"In Review"}]}],…}`.
`fields` stays at merge granularity (`custom` covers every custom field);
`changes` is the per-field detail.
Agent example: `jired pull --no-input --json || test $? -eq 5`

Custom fields are written into the file in their editable form — a select list
as `Platform`, a multi-select as a YAML list, a user as `@[Name](id)` — and the
Jira wrapper (`{"value": …}`, `{"name": …}`, `{"accountId": …}`) is rebuilt on
push from the shape the server last sent, falling back to the field's schema.
Values that do not collapse unambiguously (cascading selects, rich text, plugin
structures) stay in their raw shape and are sent back verbatim. Deleting an entry
under `custom:` clears that field.

A field listed in `ignored_custom_fields` — by id or by the name the instance
gives it — is left out of the file altogether rather than written empty, which
would read as a clearing edit. Nothing is discarded: the value stays in the
payload and in `state.db`, so the setting is reversible. `push` skips (and reports)
an ignored field a file still carries, and `jired doctor` is what brings the files
into line in either direction.

### `jired push`
`jired push [ISSUE…] [-n/--dry-run|preview] [--force] [--allow-lossy] [--allow-comment-delete] [--allow-worklog-delete] [--resume] [--refresh-stale] [--verify]`
Plans ops (D23 order), `--dry-run` prints exact API calls + diffs (human and JSON).
Interactively it always plans first: the plan lines are printed and the run asks
`send N operation(s) across M issue(s)?` before anything is sent. `-y` and
`--no-input` skip the question; a dry run never asks.
Per-issue concurrency check (D8): stale → that issue fails (exit-5 contribution,
"pull first"), others proceed. Every per-issue failure — including an edited lossy
region — is an `IssueResult` rather than an abort, so one bad issue never discards
the results of the ones already pushed; base advances only for confirmed issues
(durable `pending_op` journal). The exit code is the failing kind's own code when
every failure shares one kind (all-lossy → 6, all-auth → 3) and 1 when the causes
are mixed.
A retry re-plans the same work from the same working copy — base never advanced
for a failure — so its journal *replaces* the failed attempt's rows for that
issue rather than piling a second identical set beside them. And work an earlier
push left unfinished is named in `warnings` when this push did not touch its
issue: `summary.failed: 0` is true of what was attempted now and silent about
the queue, and a caller reading only the push report used to have no way to know
`--resume` still had something to retry. An issue in `results` is not warned
about — its row already tells the story.
A sub-task this push created, moved, renamed or resolved rewrites its parents'
files before the push returns — old parent and new — through the same offline
merge `pull --no-fetch` makes, so a parent with edits of its own keeps them, and
one whose edits would conflict is left alone and named in `warnings`.
`sprint:` is a list — an issue that rolls over stays in the sprint it came from
— written as a plain string when there is only one. Its last entry is where the
issue is now, and the only part a push can act on: a change to it is a board call
(`POST /rest/agile/1.0/sprint/{id}/issue`, or `…/backlog/issue`), not a field
update, because the Sprint field is read-only on most instances. The name is
resolved against the cached sprints, preferring an open one; a name no board
knows is reported and not guessed at, and an edit to the earlier entries is
reported as history that cannot be rewritten.
A sub-task is not a board item — it takes its sprint from its parent — so a
`sprint:` on one is refused at plan time (exit 2, the hint naming the parent to
move instead) rather than sent to a board call that answers 204 and does
nothing. The refusal is made in `plan`, so `--dry-run` shows it before anything
is sent, and it leaves the edit in the file: the issue fails, base does not
advance, and nothing is overwritten.
On Data Center, the fields a `PUT` would carry are checked against the issue's
edit screen (`GET /issue/{key}/editmeta`) before anything is sent, and a
`create` against the create screen for its project and issue type
(`GET /issue/createmeta/{project}/issuetypes/{type}`). Jira's field
configuration is per project, per issue type and per screen, so an issue can
have a priority while the screen it is edited through offers no way to set one;
that write is a 400 naming a screen, arriving after everything else in the push
has gone. Asked first, it is exit 2 at plan time, visible in `--dry-run`, naming
the field as the workspace knows it, with the other issues in the push
unaffected and the edit still in the file. A server that describes no screen —
an older version, a credential that may not read it — refuses nothing: unknown
is not "no fields", and the server's own answer remains the backstop. Cloud is
not checked, deliberately: both endpoints exist there and both clients implement
them, but no Cloud screen has been recorded to verify the reading against, and a
false refusal is worse than the 400 it pre-empts.

`assignee:` is an operation of its own on both deployments, an `assign` op
against `PUT /issue/{key}/assignee`, never a key inside a `fields` payload.
Assigning is not editing: Jira gives it a route because it is its own
permission, and an instance whose edit screen does not carry `assignee` — a
normal configuration — refuses the field update while accepting the route.
Clearing it puts the null on the id (`{"accountId": null}`, `{"name": null}`)
rather than on the field, which is the spelling this route takes. Like every
write that can be accepted and not performed, it is verified on the re-read: an
assignee that did not change fails the operation at exit 10 with the edit left
in the file.

A `links:` line is an operation of its own on both deployments: an added line
is a `create_link` op (`POST /issueLink`), a removed one a `delete_link`
(`DELETE /issueLink/{id}`, the id read out of the base snapshot's `issuelinks`
entry — the file spells a link as a direction and a key, and only the snapshot
still knows the id). The direction the file spells — `is blocked by` — is
resolved against the instance's link types (`GET /issueLinkType`), fetched once
per push and only when a link actually changed. Jira's convention reads
backwards and is absorbed in `plan`: the *inward* issue of the create call is
the one that bears the type's *outward* description, so `TSK-303 is blocked by
TSK-266` posts `{inwardIssue: TSK-266, outwardIssue: TSK-303}`. A direction
this Jira does not define is refused at plan time (exit 2, visible in
`--dry-run`), the hint naming the directions it does offer. A removed link is
deleted without the opt-in a comment or worklog deletion takes: those are
unrecoverable content someone else may have written, while a link is exactly
the pair on the deleted line, and an ordinary revert puts it back. On a `NEW-*`
issue the links are follow-ups after the create — the create call has no field
for them — planned before it so the dry run lists them and an unknown
direction refuses the issue while nothing has been sent.

Jira records a link on both issues and moves both `updated` stamps, so the
other end goes stale the moment a link to it is made or removed. After every
`create_link`/`delete_link` that went through, push re-reads the other end and
merges it into its file through a scoped `pull` (under `abort`, so a file whose
own edits conflict is left alone and named in `warnings`). Within one push that
is what lets a chain go out together: in `push A B` with `A blocks B` and
`B blocks C`, B takes in `is blocked by: A` before its own turn, is no longer
stale, and sends only `blocks C`. Links merge as a set in every pull — the
server's links, less the ones the file removed since base, plus the ones it
added — so they never conflict, and a link the server gained after the last sync
is never in a position to be read as a deletion. The same-second case is covered
too: an issue counts as behind the server when its links differ from base, not
only when its stamp moved, because a stamp is kept to the second and a create
followed by a link to it can land inside one.

A web (remote) link is a `links:` line under the key `web`, its value the URL
and, when it says more than the URL does, the title in quotes:
`- web: https://wiki.example/display/TSK/Release "Release 1.0.0"`. Jira keeps
them apart from issue links, in a resource of their own
(`GET /issue/{key}/remotelink`), so `fetch` asks for them separately — only for
an issue whose stamp moved, and not under `--slim` — and keeps them in the
snapshot under `remotelinks`, beside `fields`. Otherwise, and when that read
fails (a warning, not a failed fetch), the last snapshot's are carried forward:
dropping them would read as every web link deleted on the server. Push sends an
added line as `create_remote_link` (`POST /issue/{key}/remotelink`
`{"object": {"url", "title"}}`, the title defaulting to the URL, which the
server requires) and a removed one as `delete_remote_link`
(`DELETE /issue/{key}/remotelink/{id}`, the id out of base). A changed title is
both. On a `NEW-*` issue they are follow-ups, like issue links. After a push
the web links are read back with the issue, so the rewritten file keeps them.
They merge as a set, like issue links.

`--verify` reads every issue the push changed back from the server and compares
what its file asked for, as it was sent, with what the server now holds: every
editable frontmatter field, `custom:` by id, the description and the checklist.
Each field is compared the way it means — links and the list fields as sets,
`sprint` as the one the issue is in now, a user by account id, names without
regard to case — and a created issue only on what its file set, so a blank
status is not held against the workflow's first one. Every difference is listed
under `verify` as `{key, field, expected, actual}` and printed, and any one makes
the exit code 10, the code for a write the server accepted and did not apply. An
issue refused as stale, or whose every op failed, is not read back: it already
says it failed. Without the flag `verify` is absent; with it and nothing amiss,
it is `[]`.

`sprint:` is resolved against the sprints the workspace has fetched, for an
update and for a create's follow-up alike, and a name no board knows refuses the
issue at plan time (exit 2, the hint listing the open sprints). `epic` remains the
one editable field with no call behind it; a change to it is warned about
rather than sent.

`fix_versions:` and `affects_versions:` go out by name in the field update, and
Jira answers a name the project does not have with a 400 — after the issues
ahead of it in the push have already gone. So a name an edit *adds* is checked at
plan time against the project's versions (exit 2, visible in `--dry-run`, the
hint listing the versions there are and the `jired release create` that would
make the missing one). Only added names: the ones already on the issue came from
the server. The list is the one `fetch`, `jired releases` and every `jired
release` write leave in the cache, so a version created a moment ago passes with
no fetch in between. A workspace that has never listed the project's versions
checks nothing — it has no grounds to call a name unknown. Matching ignores case,
as the server's does.

An `ISSUE` that is not in the workspace is a failed result of its own (exit 2),
not an empty push: `jired push TSK-578` answering "nothing to push" read as the
issue being clean. And one argument holding several keys — `"TSK-487 TSK-556"`,
which is what zsh hands over for an unsplit `$keys` — is refused on every command
that takes an issue, naming the keys: it used to read as a file name, a key and
then a summary, and push the first issue alone.

`## Checklist` goes out by a different route on each deployment. On Cloud it is
a field update like any other — the checklist plugin's text field, carried in
the same `PUT /issue` as the rest. On Data Center it is an `update_checklist`
op of its own, `PUT /issue/{key}/properties/com.railsware.SmartChecklist.checklist`
with the text as the property's whole JSON document. The text field is an
ordinary custom field, which `PUT /issue` takes only where the issue type's
edit screen carries it, and Smart Checklist does not need it there: an instance
with the field on no screen at all refused every checklist with a 400, and the
screen check refused it at plan time. The property is not a field, so no screen
governs it. It is the route the vendor documents for Data Center automation,
and since 5.0, when the plugin moved its primary storage into the database, one
of the storages it keeps in sync; Cloud's Forge app no longer reads it, which is
why Cloud stays on the field. A property takes any document whether or not
anything reads it — another checklist app behind the same field name, the
plugin switched off for the project — so the re-read is the proof. The plugin
takes the property up on its own schedule, a minute later on the instance this
was reported against, so push re-reads until the checklist shows: quickly at
first, backing off to every ten seconds, for up to `checklist.apply_wait`
seconds (default 90; 0 reads once). A checklist that still does not read back as
what was sent fails the op at exit 10, with the edit left in the file. If the
plugin applies it after that, the next `pull` finds the same checklist in the
file and on the server — an agreed change, not a conflict — and a pull that
leaves an issue level with the server clears whatever the push journal still
held for it. On a `NEW-*` issue either route is a follow-up after the
create, because a plugin's field is rarely on the create screen. What goes out is
the plugin's own syntax rebuilt from the section: `# ` for a `### ` group, `-`
`~` `+` `x` for the four statuses the checkbox and its backticked name spell,
and a note on the line below its item. The derived fields are never written: the
progress string is the plugin's, and the object dump is not a document anything
could write back.
The dialect is learned from the field rather than assumed. jired writes the
edit only once rendering the *unedited* checklist reproduces the text the server
holds, compared line for line with the plugin's own trailing spaces and blank
lines discarded; the note marker is whichever of `>>` and `>` that text uses,
and `>` — the plugin's own, in every current vendor guide — where it has no note
to learn from. `>>` is a nested quote to the plugin: `>> note` is stored as
`> > note`, a note whose text begins with `> `.
Where the two do not match there is something in the field this jired does not
model — a mandatory item, a status the instance configured itself, a syntax this
plugin version spells differently — and sending the field would drop it, so the
section is refused in `warnings` and left alone while the rest of the issue goes.
The plugin's fields are found by display name, the schema deciding which of the
pair is the writable text one (`string`) and which the object (`any`);
`checklist.field` names it outright where an instance calls them something else.
An instance that exposes no text field keeps the section read-only, and says so
when it is edited.

What a `NEW-*` issue's create call cannot carry — the checklist, the sprint, a
status the workflow does not start in, its links — is a follow-up op, planned
before the create so `--dry-run` lists it, and sent once there is a key to aim
it at. A follow-up that fails, or that the re-read shows was not applied, does
not undo the create; it makes the result `"ok": false` with `new_key` set and
the failure's `error_kind`, counted in `summary.failed`. The file is renamed to
the real key and rebuilt from the server, except for the part the failed
follow-up owns, which stays as the file said it. Base is the server's rendering,
so `status` reports that part as an edit to an issue that exists, `pending_ops`
carries the op under the real key, and `push --resume` (or a plain `push`)
re-plans it as an ordinary update. The issue is not sent again in the push that
created it. No follow-up is checked against an edit screen: the edit screen is
described per issue, and before the create there is none to ask about. None
needs to be — Data Center's only follow-up write is the checklist property, and
Cloud's screens are not checked.

A sub-task's `parent:` is an operation of its own on Data Center, not a field
update: `PUT /issue` accepts the payload, answers success and ignores it, and
the REST API offers no route at all (JRASERVER-68763). The change is taken out
of the field payload in `plan` and becomes a `move_parent` op, which drives the
web action behind Jira's Move dialog — `GET
/secure/MoveSubTaskParent!default.jspa?id=…`, then the form that page serves,
posted back with `parentIssue` filled in and every field it declared (the
`atl_token` its XSRF check wants included), following its redirects to the
confirm step. Reading the form rather than assuming it is what survives a
version change; the answer is verified by re-reading the issue, because a page
that renders is not a move that happened. Cloud sends the field update, which
is the route Atlassian added there and never on Data Center.
Three things are refused at plan time (exit 2), where `--dry-run` shows them
rather than a green plan that fails on the way out. A sub-task with no parent at
all, which is a conversion rather than a move, and a parent in another project,
which would have to change the sub-task's key — neither has a wizard page. And a
credential that cannot open a web session: Seraph authenticates `/secure/*.jspa`
from basic auth or a session cookie, never from a bearer token, so a personal
access token — `init`'s own recommendation on Data Center — is redirected to
`SessionTimeoutMessage.jspa`, which Jira serves as HTTP 200. `AuthKind::
opens_web_session` is the one place that judgement is made. Where the wizard is
still not what answers, the error names the URL the request landed on: a status
line that says 200 explains nothing, and the landing URL explains all of it.
A 2xx is not evidence, so the re-read that rebuilds the file is asked first
whether the change is in it. `parent` and the sprint are checked — the two whose
payload and whose rendering can be compared without guessing, and the two Jira
is known to swallow; a field whose value comes back as an object with an id and
a self link is not, because a comparison that had to guess would invent
failures. An op the server did not act on is `failed`, carrying the request that
was sent, the issue is `ok: false` with kind `unsupported_server`, and the file
is left exactly as the user wrote it.
A push that moved an issue — created, transitioned, re-parented or re-sprinted
it — rebuilds the grouping folders on the way out and reports them as `groups`,
because `sprints/`, `boards/` and `by-parent/` are views over base and the push
has just moved base. The sprint someone just created gets its folder from the
push that fills it, rather than waiting for a pull that writes a file. A push
that only adds a comment, a worklog or an attachment leaves the tree alone, so a
card dragged between board columns and not yet pushed survives it.
JSON: `{"schema":"jired/push/1","results":[{"key":"PROJ-1","ok":true,"ops":[…]},{"key":"NEW-x7f3a2","ok":true,"new_key":"PROJ-201"}]}`.

`--refresh-stale` is the recovery path for exit 5. Every issue the concurrency
check refused is re-fetched and merged (abort strategy, scoped to just those),
and the ones that merge cleanly are pushed again. "Merges cleanly" is the right
test for "the staleness does not touch what I am sending", and it is a test that
already exists: a three-way merge conflicts exactly when the incoming change and
the local one touch the same field. A worklog logged while you worked has nothing
to do with the labels you are sending; a priority changed under a priority you
changed is a real conflict and stays refused, named in the warnings. The retry
runs with the flag off, so an issue that goes stale twice in one push reports
rather than looping. It conflicts with `--force`, which answers the same question
by not asking it.

### `jired diff`
`jired diff [ISSUE…] [--against base|remote] [--remote] [--stat] [--exit-code]`
Modes: working↔base (`--against base`, the default: what `push` would send),
working↔remote (`--against remote`: what `pull` would bring in), base↔remote
(`--remote`). `--all` was the old spelling of `--against remote` and is hidden and
deprecated — on every other command `--all` widens the scope, and here it selected
a comparison.
Unified diff for bodies (`similar`), field-level table for metadata; `--stat` summary.
Exit 0 even with differences unless `--exit-code` (git-style). Fully offline.
Against the working copy, `diff` shows what a push could send and nothing else:
a `sprint:` difference confined to the earlier entries is history the board API
cannot rewrite (see `push` above), so an issue that differs only there is not
listed, and on an issue with real changes the row rides along marked
`"sendable": false` — shown, not counted in `--stat`, never on its own. Between
two server snapshots (`--remote`) every difference is sendable: nothing is being
pushed, and the history is exactly what a pull would bring in.
In the default mode a file whose bytes are exactly what the last pull wrote is
skipped outright, whatever base re-rendered under today's rules would say: the
rendering changes between versions (a field the renderer has since learned) and
between configurations (`ignored_custom_fields`), and that drift is not an edit
anyone made. `status` and `push` skip on the same hash, and the three have to
agree about one working copy — `diff` reporting a `"sendable": true` change
that `status` calls clean and `push` will never send left callers dry-running a
push to learn which answer was true. The next `jired pull` rewrites the file in
today's rendering.

### `jired status [--all]`
One-screen overview: dirty files, new local issues, staged transitions (incl. board
moves), conflicts, last fetch age, pending push journal, and a git-style trailer
saying what to run next. `--all` additionally lists the unchanged issues. JSON
mirrors it. Offline. Visible alias `st`.

`stale_agent_docs` names any `CLAUDE.md`/`AGENTS.md` whose generated block was
written by a different jired than the one running, with the version stamped in
the file (`null` for a block predating the stamp) and the version expected. Only
a file that already carries the block is reported: a workspace that never asked
for one is not nagged about it. It does not make the workspace unclean — nothing
is at risk — but a stale doc is what an agent plans against, so it is said out
loud rather than left to be discovered.

### `jired log [ISSUE] [--limit N|SPAN] [--field NAME… [--reverse]] [--links] [--refresh]`
Everything that happened, newest first, each entry tagged with its issue. With
no `ISSUE` it covers the whole workspace, the way `git log` does; naming one
narrows to it.

Wider than Jira's changelog, which records field changes and nothing else: a
comment posted and time booked are as much of an issue's history, so they are
merged into the same timeline out of the snapshot `fetch` already stores — no
second round of requests, and nothing a `--json` consumer has to stitch together
itself. A worklog is dated by when it was booked (`created`, falling back to
`started`) rather than by when the work started: a log answers "what happened
today", and time booked this morning for last Tuesday happened this morning.
`report time` is the command that reads a worklog the other way round.

Neighbouring events that share an issue and an author print under one heading.
Jira records a transition, the comment explaining it and the time booked against
it as three separate things, and three identical headings make one person's
afternoon look like three. Only neighbours: the timeline is already in the order
it will be printed, so a block is a run of rows whose headings would have been
identical — never a gathering that moves an event away from where its timestamp
put it. Somebody else's edit in the middle of a run therefore ends the block, and
the same hand returning afterwards starts a new one. An author nobody recorded
joins another, since the heading it would have printed is the same heading; that
is a claim about the row and not about the person.

Each block heads with when, the key, the issue's summary and the author, in
columns measured from the data, and one blank line separates it from the next —
a block is as many rows as it changed things, and without the gap a three-field
edit and three one-field edits read the same. A blank line therefore means only
one thing, which is why quoted text (a prose diff, a comment, a worklog note)
prints under a `│` gutter: a description with a blank paragraph in it would
otherwise split its own block in two. The key is bold as well as cyan, being
what the eye lands on to find where a block starts. The timestamp prints only the part the reader
does not already know — `14:03` inside a day, `08-16 14:03` beyond one,
`2025-11-02 16:20` in another year — in local time, like everything jired puts
on a screen. A block whose events did not all happen at once is headed with the
span from its oldest to its newest rather than with the newest alone, which
would be the heading claiming an issue was touched once when it was touched
three times: `08-16 11:05–14:03` within a day, where the date is already on the
left of the dash, and `08-14 11:00–08-15 13:30` across one. The summary is the
snapshot's current one, not the summary as it read at the time; a rename is its
own row anyway.

Under the heading, one row per thing that changed, in the order the block's
events happened:

* `field: old → new` when both sides have a value;
* `+field: value` when there was none before, `-field: value` when there is none
  now — the sign says in one character what `(none) →` said in seven;
* `field: +3 -1` and a unified diff under it when the values are prose. Jira's
  changelog carries the *whole* old and new text of a description, and a page of
  it does not fit through a column layout. Which fields those are is decided by
  the values and not by a list of names, since an instance's own "Acceptance
  Criteria" is as much prose as `description` is: a value with a newline in it,
  or one too long for the row, is diffed. The text is the server's own, not
  jired's markdown rendering of it, and one field's diff stops at 30 lines so a
  rewritten description cannot bury the rest of the week;
* `+comment:` and `+worklog:` with the text quoted under the same gutter, capped
  at six lines — a comment is worth recognising in a log and worth reading in
  the issue file.

`--links` turns each key into a link to `/browse/KEY`. On a terminal it is an
OSC 8 hyperlink on the key itself, so the rows stay the width they were; into a
pipe or a file, where the escape sequence is no use to anyone, the URL is
printed at the end of the heading row instead.

`--json` is the record rather than the rendering: one document per event, in the
same order, with the blocks left out — a block is how a screen reads a run of
events, and a consumer that wants one can group `key` and `author` itself.
Every entry carries `kind` —
`change`, `comment` or `worklog` — plus `key`, `summary`, `author` and `created`;
a change carries `items` with both texts of every field it touched, whole and
undiffed, and a comment or worklog carries `text` (and `time_spent`). Comments
and worklogs are read from the stored snapshot, which is completed page by page
when an issue is fetched; an issue whose snapshot is nonetheless short of what
the server said it had is named in a trailer rather than quietly under-reported.

`--limit` says how much of the log to show, as either a count of entries (`20`,
the default) or a span back from now — `1d`, `2w`, `36h`, `90m`, `1d12h`. Which
was meant is unambiguous from the spelling: a bare number is a count, a unit
makes a span. A span is calendar time, where a day is twenty-four hours and a
week is seven days; this is deliberately *not* the working time `logwork "1d"`
books, where the instance's day may be eight hours. An entry the server dated
nothing cannot be shown to fall inside a window, so a span leaves it out where a
count keeps it. History that exists but is older than the window says so — and
offers twice the span — rather than printing the nothing-recorded answer.

Changelogs are fetched per issue (`expand=changelog`) and cached, so the first
log over a workspace fetches what it is missing and every run after it is
offline. Freshness is the issue's own `updated` at the time its changelog was
cached, not "is the cache empty": an issue with no history is a real answer and
must not be asked about twice, and one the server has moved on from is asked
about exactly once. `--refresh` overrides both. In workspace mode a single
unreadable issue warns and stops the fetch rather than losing the rest of the
log.

`--field NAME` (repeatable, or comma-separated) is the history of named fields
instead of the timeline: one flat row per change, matched against the
changelog's own field name ignoring case — `Sprint`, `status`, `Fix Version`.
Each row carries its issue, its time and its author beside `from` and `to`, so
"which sprints did this go through, and when" is one command rather than a
recursive `jq` over the timeline's nested items — the one an agent wrote kept the
values and dropped the dates. No comments or worklogs, no grouping, and no
default cap: `--limit` still cuts from the newest end when given. Newest first,
oldest first with `--reverse`. The text form is one line per change,
`KEY  2026-10-20 14:03  Author  Sprint: Sprint 16 → Sprint 17`, with a full date
because a field's history spans months; a long or multi-line value is flattened
and cut there and travels whole in the JSON. Fetching and caching are the
timeline's, `--refresh` included.
JSON: `{"schema":"jired/log-field/1","key":"PROJ-142"|null,"scope":"issue"|"workspace","fields":["Sprint"],"total":3,"changes":[{"key","created","author","field","from","to"}]}`.

### `jired comment [ISSUE] [-m "text"] [--push] [--dry-run]`
Appends a `### [new]` section to the file; `--push` sends immediately. Without
`-m` it opens `$VISUAL`/`$EDITOR` on a scratch file (an empty buffer aborts), or
reads the body from stdin when one is piped.

With no issue named, the shared picker (see `jired edit`), scoped to the issues
with a file — the comment is staged into one. Which issue is asked first and the
body second, because the editor the body may open wants the terminal the picker
is still holding, and because it is the order the questions come in anyway.

### `jired logwork [ISSUE] [TIME] [-m "text"] [--started TS] [-i] [--push]`
Books time against the issue by appending a `### [new] 2h 30m — <when>` section
under `## Worklogs`; `--push` sends immediately. `--started` defaults to now, so
the entry is dated when the work happened rather than when the push does.

`--started` reads a timestamp that names its zone as exactly that and keeps it in
the words it was written in; one that does not — `2026-08-21 09:00`, which is
what someone shown a local time types back — is read in the machine's timezone
and written down with the offset spelled out, so the file still names one
instant. The form's presets and its prefilled "now" are local for the same
reason. Scripts should name the zone.

The duration is Jira's own notation — `2h 30m`, `1d`, `45m` — validated here for
syntax only and sent as the text it was written as: how long a day is is an
instance setting, and converting to seconds would answer that question wrongly.
A bare number is refused rather than read as minutes.

With both the issue and the time given nothing is asked, so a scripted
`logwork` behaves the same with or without a terminal; `-i` opens the form
anyway. Without them there are three rungs, as `create` has: the full-screen
form, line prompts, then exit 7 naming what is missing.

The form asks for four things — issue, time, start, note — and checks the first
three while they are being typed rather than after it closes, since every
complaint it can make is one the command would otherwise make with the
half-filled form already gone. The issue field completes as you type: type any
part of a key or summary, tab takes the suggestion shown beside it, and only the
issues that have a file in the working copy are offered, because the file is
where the entry is staged. Time and Start each open a list of the values people
actually type — a quarter of an hour through a day, and start times counted back
from when the screen opened — and the start time reads back in words (`4 hours
ago`) so an ISO string is never the only thing on screen.

`## Worklogs` holds what the server has, oldest first, one heading each:
`### [w-10500] 2h 30m — Jane Doe — 2026-08-15T09:00:00Z`, with the note below
it. An issue payload carries only the first page of them, so `fetch` asks the
worklog endpoint for the rest whenever the server says there are more — a file
that showed 20 of 45 would be a file that lies. Editing a heading or its note is
an update on push; removing a section is reported and ignored unless
`--allow-worklog-delete` is passed, since deleting somebody's booked time cannot
be undone from the working copy.

### `jired report issues --by FIELD[,FIELD] [--jql Q] [--format text|md]`
How many issues, broken down by one field, or by two as a matrix: rows are the
first field's values, columns the second's, with a total column down the right
and a total row along the bottom. The chart that asked for this — issues,
stories and bugs per sprint — was answered with a JQL count per cell, 75
searches and `jq` to read each total; every figure in it is already on the
issues, so one pass answers the lot:
`jired report issues --by sprint,type --jql "project = TSK" --format md`.

Fields: `sprint`, `type`, `status`, `category` (Jira's `new` / `indeterminate`
/ `done`), `assignee`, `label`, `fix_version`, `priority`, `parent`, `epic`,
`component`. An issue with several values for a field counts once under each —
the sprints it has been in, its labels, its fix versions — which is how JQL's
`sprint = X` matches: an issue carried from sprint 41 into 42 is one of each
sprint's issues. Every total counts each issue once, so a row's total can be less
than the sum of its cells. An issue with no value counts under `(none)`, last.
Values are in natural order, `Sprint 9` before `Sprint 10`.

The working copy by default, offline, from the files' frontmatter as `list`
reads it (`category` from the snapshots, like `list --category`); with `--jql`,
the issues the server finds, fetched as a listing a page at a time — the way to
count sprints nobody in this workspace pulled. `--format md` prints a Markdown
pipe table, names left and counts right, ready to paste into a page.
JSON: `{"schema":"jired/report-issues/1","by":["sprint","type"],"source":"workspace"|"server","jql","total","columns":["Bug","Story"],"rows":[{"value":"Sprint 41","counts":{"Bug":2,"Story":5},"total":7}],"totals":{"Bug":2,"Story":5}}`.

### `jired report time [--days N | --since DATE [--until DATE]] [--rollup parent] [--columns days|people] [--format text|md] [--show-issues]`
Who booked how much time, totalled off the `## Worklogs` sections of the files
already on disk. A read, so it works offline and costs one pass over the working
copy; what it cannot see is time booked since the last `pull`, which is why
every rendering carries the age of the snapshot.

The shape is a grid: a row per person, a column per day, a total down the right
and along the bottom. `--days N` (default 7) is how many columns, whole UTC days
ending with today — not a rolling `N × 24h`, because the question is asked in
days and an entry booked at 09:00 seven days ago should not drop out of a
seven-day report because it is now 10:00. Every entry therefore falls in exactly
one column, and `dates`/`by_day` in the JSON are that grid in the order it is
drawn. The columns are the one printed thing that does not move with the reader:
a date range is what the window counts, not a clock reading, while the `last
fetch` line beside it is a clock reading and is shown as their own.

Totals are shown in hours and minutes and never in days, for the same reason
`logwork` sends the duration as written: how long a working day is is an
instance setting. A cell says the same thing in the two or three characters a
column has — `2h30`, `45m`, `·` for a day nothing was booked on. Entries staged
locally count towards whoever will push them — a `### [new]` heading names no
author because the author is decided by the push — and are reported separately
(`staged_seconds`) so "on the server" and "on my disk" never look like the same
claim. A worklog whose start or duration cannot be read is counted under
`skipped` rather than dropped silently.

In a terminal it opens as a screen, and the keys are the reason it is one rather
than a printed table: the window can be re-asked without re-running the command.

| Key | What it does |
|---|---|
| `↑` `↓` | move between rows |
| `→` `⏎` | open a person into the issues their time went to, biggest first |
| `←` | close them again, from the person or from inside them |
| `i` | open or shut that level for everybody — the same state `--show-issues` starts in |
| `+` `-` | one day more or less in the window |
| `<` `>` | the window before this one, or after it — a whole width at a time, so a week steps to the week before |
| `t` | back to a window ending today |
| `?` | the key card |
| `q` `esc` | leave |

Each of those re-aggregates from the files already in memory, which is cheap
enough that no pre-computed grid has to be kept in step with a moving window.
A window too wide for a figure per column falls back to a heat map — the shape
of a month is readable when the figures in it are not — and one too wide even
for that keeps its most recent columns and says in the frame how many earlier
days are not drawn, because the totals still cover all of them.

Everywhere a screen cannot run — a pipe, `--json`, `--quiet`, `--no-input`, a
dumb terminal — the same grid prints as a table, with the day columns dropped
(not wrapped) once the line would pass 100 characters; `--show-issues` is then
the only way to the second level. The JSON document (`jired schema report-time`)
carries the whole grid either way.

`--since DATE --until DATE` names the window by its dates, both counted —
`--since 2026-10-19 --until 2026-10-23` is that Monday to Friday whatever day it
is asked on. `--until` defaults to today; `--since` with `--days N` counts N days
forward from it, and `--until` with `--days` is refused as saying the same thing
twice. Dates that run backwards, or a window over 3650 days, are exit 2. The
screen opens on that window when it ends today or earlier.

A person is their account, not their display name. A file names a worklog's
author as the server spelled it when the file was written, so somebody renamed
in Jira appeared under the old name in files pulled before the rename and the
new one after, and was counted twice. The snapshots in `state.db` carry the
account id behind each worklog id (`accountId` on Cloud, the user name on Data
Center), so time is grouped by that: a person is named by the display name on
their most recent entry, the others listed in `also_known_as`, and `account_id`
is in the JSON. Staged entries belong to the account running the report and
join its booked time. Only an entry no snapshot attributes falls back to the
name.

`--rollup parent` files each sub-task's time under the issue at the top of its
`parent:` chain, the row saying which issues it folded in (`rolled_up`). A
parent with no file in the workspace is still the row, named from the snapshot
when one has it. Every issue row carries its `type` and `parent` either way, and
the document's top-level `issues` is the same time the other way up — a row per
issue, each listing who booked it — so a table of issues against people is read
off it rather than pivoted by hand.

`--columns people` draws that table: issues down the side, a column per person,
a total column and a total row. `--format md` prints the table as a Markdown pipe
table, ready to paste into a page — empty cells where nothing was booked, `2h
30m` where something was, the total row last — for either shape (`--columns days`
is people against days). Neither opens the screen. The parent-level worklog
table for a week is one command:
`jired report time --since 2026-10-19 --until 2026-10-23 --rollup parent --columns people --format md`.

### `jired create [--type Story] [--template T] [--summary S] [-i]`
Writes `NEW-<6hex>. Summary.md` (D14) from template; real key assigned on push
(file renamed; JSON reports the mapping).

With `--summary` nothing is asked, so a scripted `create` behaves the same with or
without a terminal. Without one — or with `-i`, which asks anyway — it opens the
new-issue form: summary, type, priority, labels, parent and project, with the
options taken from what the workspace has already pulled (ranked by use) rather
than from the server. It is offline like the rest of the command. The form draws
on the alternate screen over stderr, so `--json` stdout stays a single document.

`--sprint` is resolved before anything is written, the way `sprint add`
resolves it, and the file carries the sprint's own name: an unknown one is exit
2 while nothing exists. `--push --dry-run` plans from a draft and removes it
afterwards, so a dry run leaves no `NEW-*` issue for a later push to create;
`staged` in the JSON says whether the file is still a draft waiting for push.
`--push --verify` reads the new issue back and names any field the server does
not hold as the file asked, the way `push --verify` does.

Degradation: full-screen form → line prompts for summary, type and project (a
terminal that cannot do raw mode, `TERM=dumb`, or `JIRED_NO_TUI=1`) → exit 7
naming the missing summary (`--no-input`, or no terminal at all). Backing out of
the form writes nothing and exits 1.

### `jired edit [ISSUE] [--push]`
With no issue named, the issue picker: a table of everything in the working copy,
narrowed by typing a subsequence of the key or the summary (`pl142`, `sso ro`),
↑↓ to move, ⏎ to open. Columns are key, summary, status, priority, assignee,
reporter and the ages of updated/created/viewed; `^s` moves the sort to the next
column, `^r` reverses it, `^f` narrows to a status or a whole status category.
The filter box reads the key and the summary only — an age or an assignee that
happens to contain the letters typed must not pull a row in.

Columns that do not fit are dropped from the right, keeping the key, the summary
and whichever column is in force; sorting by a column that is not on screen would
leave the order looking arbitrary. Rows with nothing in the sorted column go last
in either direction. `viewed` is stamped locally — by `open`, and by every command
that stages a change through `edit_issue` — and lives in `state.db`, the one thing
in there a fetch cannot rebuild (D17); losing it costs a sort order.

Sort and filter are `ui.issues.sort` and `ui.issues.filter`, written back to the
workspace config by the screen itself, so the list opens the way it was left. An
unreadable value is exit 2 naming the key, raised before the screen is drawn.

The issue can also be named by key in any case, by
file name, or by a path — which is what shell completion produces, and the
likeliest thing to be typed for an issue whose summary is hard to spell. A
locally created `NEW-8bf7b6` keeps the case its hex suffix was generated with.

Opens the issue in a form: summary, status, type, priority, assignee, labels,
components, sprint, story points, parent, description — the fields a push can
actually send, and no others, so nothing offered is silently dropped. The
assignee field completes as you type: a name becomes the `@[Name](id)` mention
the file wants, tab takes the suggestion, and saving a name that matches nobody
is refused rather than sent. The sprint field is a ticked list — space toggles, or tab while
filtering — offering every sprint the workspace knows: the one running now, the
ones queued behind it, then the closed ones newest first, each row saying which
it is. Closed sprints are on the list because `sprint:` records where an issue
has been as well as where it is; only the last entry is a move a push can make.
A sprint the issue is in that the cache has never seen is offered too, rather
than dropped. Ticking none of them is the backlog. Options come from
the workspace's own snapshots (statuses and types it uses, people it has seen,
its issues as parents), never from the network. Saving writes the file, renaming
it when the summary changed; the change then behaves like any other local edit —
visible in `status` and `diff`, sent by `push`, or immediately with `--push`.
A status change is staged as a transition, as it is when edited by hand.

Everything the form does not show — comments, custom fields, links, sprint — is
left exactly as it was. Without a full-screen terminal the file is handed to
`$VISUAL`/`$EDITOR` instead, and is re-parsed afterwards so a stray edit is
reported here rather than in the next command. With no terminal at all: exit 7,
pointing at `comment`, `transition`, `assign` and the file itself.
JSON: `{"schema":"jired/edit/1","key":"PROJ-142","file":"…","changed":["status"]}`.

### `jired transition ISSUES… --to "In Review" [--list] [--field k=v…] [--push] [--dry-run]`
Takes several issues for the reason `label` and `set` do: closing out a sprint
is a dozen tickets moving to one status, and `list --keys` exists to be piped
into exactly this. Each issue is validated against cached/live transitions for
*its* workflow position — the same `--to` can be a different transition id, or
no transition at all, per issue — and one that cannot move is a warning beside
the ones that were staged, not a reason none of them were; only every issue
failing fails the command, with the first issue's error. Missing required
fields → exit 2 with a machine-readable list. The flag is `--field`, repeated
(`--field k=v --field j=w`). Without `--to` and without `--list` it is a usage
error and emits nothing on stdout. `--list` reads one issue at a time — the
listing is one issue's position in its workflow — and prints the transitions
available from its current status, falling back to the cached list (with its
age) when the network is down.
JSON: `{"schema":"jired/transition/1","to":"Done","issues":[{"key","file","changed","to","transition_id"}],"pushed":false,"push":null}` —
per-issue `to` as the workflow spells it, per-issue `transition_id` because one
`--to` matches a different transition on each issue.

### `jired assign ISSUES… --me|--user Q|--none [--push] [--dry-run]`
Exactly one of `--me`/`--user`/`--none` is required — clap enforces it, so the
error arrives before any work. `--user` resolves against the users who can be
assigned the *first* issue named (`user/assignable/search`, scoped by
`issueKey`), which is a smaller set than the instance's directory: assignment is
a permission held per project and per issue-security level, and searching
everyone offered names the server would then refuse — a failure that arrives one
push later. One person is resolved once and written everywhere; an issue that
person cannot hold gets the server's own refusal at push time. An ambiguous
query lists the candidates and exits 2 rather than picking one. There is no
picker: the TUI is M4 work and nothing in the tree draws one today. Staged like
`label` across the lot, and the JSON is the same shape:
`{"schema":"jired/assign/1","assignee":"@[Jane D](jane)","issues":[{"key","file","changed"}],"pushed":false,"push":null}`.

### `jired attach ISSUE FILE…` / `jired attachments pull|list [ISSUES…] [--max-size N]`
`attach` copies into the dot-folder and stages upload (cap 50 MiB default).
`attachments pull` replaces stubs with real files (streamed, sha256-verified);
`attachments list` reports which contents are present. Policy: lazy by default (D5).

### `jired open [ISSUE] [--print]`
Browser via `open` crate; `--print` (and `--json`/non-TTY) prints the URL instead.
With no issue, the same picker `edit` uses, over every issue the state store
holds rather than only the ones with a file — `open` browses the server, so a
snapshot is enough. Locally created `NEW-*` issues are left out: they have no
server key to browse. No terminal for the picker → exit 7 naming what to type.
Going to an issue stamps `viewed_at`, this being the plainest thing viewing an
issue can mean.

### `jired watch [--all] [--limit N|SPAN]`
Who watches an issue is a per-issue toggle in Jira's UI, and the question worth
asking is about a list, so this is a list: the workspace's issues, newest first,
with a box ticked where this account is a watcher. `a` ticks or unticks them all
— the one key the screen exists for — `space` toggles the row, `⏎` sends, `esc`
throws the plan away. Nothing leaves the machine while the screen is open, which
is what makes a key that ticks everything safe to offer; the line under the title
says what `⏎` would send, before it is sent.

The ticks start from the *server*, asked as one `watcher = currentUser()` search
scoped to the workspace's project. Not from the snapshot: somebody else can add
you as a watcher, and a screen about watching that shows a week-old answer is
worse than no screen. `FieldSet::Summary` on that search, so it does not drag
every comment page behind it. When the search cannot be run, the snapshot's own
`watches.isWatching` stands in and a warning says which answer this is.

`--limit` is `jired log`'s, sharing its parser: a count (`50`, the default) or a
span back from now (`2w`), applied to the list the screen offers. `--all` skips
the screen and watches every issue in the workspace — the scope `fetch` brought
in, which for an `init` workspace is the project — and is what a script and a
terminal with no screen use. Issues already watched are not sent again, watching
being a set; `jired watch` with neither a terminal nor `--all` is exit 7 naming
the flag.

Adding a watcher is `POST issue/{key}/watchers` whose whole body is the user's
identifier as a bare JSON string; removing one names them in the query, and that
is where the deployments part — Data Center declares `username`, Cloud takes
`accountId`, which is exactly the difference `myself` already carries. One issue
the server refuses does not stop the rest: it is recorded against its own key,
the run finishes, and the exit code is 1 with the failures named.

### `jired search "project = PROJ AND status = Open" [--limit N] [--count] [--columns C,…] [--keys]`
JQL is positional; `--jql` is the same value spelled as a flag, and the two
conflict. Table / JSON rows, `showing N of TOTAL`, and an explicit line when
nothing matches. `--limit` (default 50) pages through the server's 100-issue
pages up to 1000, and says so when it stops there. `--pull` is hidden and
unimplemented: it parses and then exits 2 saying so.

Every row carries what the next question needs — `type`, `parent`, `epic`,
`sprint` (the whole history, oldest first, as `list --json` has it and as JQL's
`sprint = X` matches), `fix_versions`, `labels`, `priority`, `components`,
`created`, `updated`, and the assignee's id beside the name — asked for as a
listing (`FieldSet::Listing`: those fields and the instance's own sprint,
epic-link and story-point ids) rather than `*all`, so a page costs no
descriptions, comments or worklogs. A listing that named only key, summary and
status sent every check after it back to the server one issue at a time.

`--columns` prints any of those fields, in the order named, with the layout
rules `list --columns` has: padded on a terminal, tab-separated into a pipe, an
empty cell for an empty value, `sprint` the one it is in now. `--keys` prints
the keys alone. `--count` prints only how many issues match: one request with
no issues in it, whatever the size of the answer.
JSON: `{"schema":"jired/search/1","jql","total","issues":[{"key","summary","status","status_category","type","assignee","assignee_id","parent","epic","sprint":[…],"fix_versions":[…],"labels":[…],"priority","components":[…],"created","updated"}]}`;
with `--count`, `{"schema":"jired/search-count/1","jql","total"}`.

### `jired resolve [ISSUES…] [--list] [--ours|--theirs|--done]`
Lists the recorded conflicts (issue key and field). A bare `resolve` prints that
list and then exits 7 rather than choosing for the user; `--list` prints it and
exits 0. `--ours`/`--theirs` re-run the merge under that strategy so they cannot
drift from `pull --strategy`; `--theirs` names what it would discard and asks
first. An issue a `pull --strategy merge` already wrote is level with the server
— base moved to the server's copy — so there is nothing left to re-merge
against. Its recorded fields are settled in the file instead: `--theirs` takes
each from base, which is the server's side, and collapses the description's
markers to the remote lines; `--ours` keeps the file's values and collapses the
markers to the local lines. Either way every resolved record is cleared. `--done` accepts files edited by hand and refuses (exit 5) any that still
contain conflict markers. `--field` is hidden and unimplemented — it narrows the
listing, and combining it with a resolution flag fails immediately with exit 2.
There is no TUI and no `$EDITOR` hunk picker; both are M4 work.

### `jired reset [ISSUES…] [-n/--dry-run] [--include-new]`
Drops local changes: every dirty file is re-rendered from its base snapshot,
through the renderer `pull` uses, so the result is byte-identical to what the
last pull wrote and the issue has nothing left for `push` to send. Offline —
base is already on disk, which is also why this is the recovery path for a file
that no longer parses: it never needs to read one. An edited summary renamed the
file, so the name is put back too.

The mirror of `pull --force`, pointing the other way: a forced pull moves a file
*forward* to what the server holds now, a reset moves it *back* to what jired
last wrote. Recorded conflicts and the pending-op journal go with the edits that
caused them, and a card dragged between board columns is put back by the regroup
that follows the writes.

Two kinds of local work are reported rather than dropped, because base cannot
bring either back: a staged attachment, whose only copy is the file sitting in
the folder, and a locally created `NEW-*` issue, which has no earlier state at
all — `--include-new` opts into deleting the second. Naming an issue narrows the
run; a key the workspace does not have is exit 2, not a licence to reset the
rest. The listing comes before the question, `-n` prints it and stops, and a
non-interactive run needs `-y` (otherwise exit 7).
JSON: `{"schema":"jired/reset/1","restored":[{"key":"PROJ-1","file":"…","dropped":["summary","description"]}],"removed":[],"kept":[],"conflicts_cleared":0,"pending_ops_cleared":0}`.

### `jired clean [--dry-run] [--force]`
Removes orphaned files/symlinks/stubs (never dirty files without `--force`) and
asks before deleting. Rebuilding the grouping folders is `jired doctor --regroup`:
`clean --regroup` existed, returned before ever looking for orphans, and has been
removed.

### `jired doctor [--dry-run] [--regroup] [connection flags]`
Checks a workspace over and repairs what it can: `.jired/` layout and permissions,
missing config defaults, settings that changed name (`checklist.done_transition` →
`children.done_transition`), credentials that are unreadable or no longer where
`creds.storage` says they belong (it moves them), filenames that predate a naming
convention (this is the migration path for zero-padded issue numbers), issue files
whose sections predate one — subtasks written under `## Checklist` move to
`## Children`, and the checklist and the worklogs come back from the last
snapshot — custom fields
that disagree with `ignored_custom_fields` (dropped from every file; restored from
the last snapshot for a file that is still in sync, since after un-ignoring one it
is the file that would read as having cleared the field), database rows pointing at
the wrong file, the `.gitignore` block, the generated agent docs and the agent
permission files.
Renaming files re-links the grouping folders automatically; `--regroup` rebuilds them
even when nothing was renamed and reports what it did under the `groups` check
(`checks[].regrouped`). Idempotent. Exit 1 when something needs a human.
It always surveys first: interactively the repair list is printed and
`apply N repair(s)?` asked before anything is renamed, rewritten or moved; `-y`
and non-interactive runs proceed, `--dry-run` reports and stops. Declining prints
`no changes made` and emits the survey with `dry_run: true`. It takes the
connection flags so a workspace whose stored URL is wrong can still be examined.
JSON: `{"schema":"jired/doctor/1","checks":[{"name":"filenames","status":"fixed","detail":"…","changes":[{"from":"PROJ-9. x.md","to":"PROJ-0009. x.md"}]}],"fixed":1,"pending":0,"problems":0}`.

### `jired config get|set|add|remove|unset|list|path|edit [--global]`
TOML round-trip preserving comments (`toml_edit`); refuses to store secrets
(points at `init`). Every key declares a `Kind` (`Text`/`Bool`/`Bytes`/`Enum`), so
`set` validates and canonicalises the value while the typo is still on screen
rather than at the next command that reads it; an unknown key gets a did-you-mean
over `keys::KEYS`. `add`/`remove` edit one value of a setting that holds a list
(only `ignored_custom_fields` today), written as a TOML array and read back — and
given in an environment variable — comma-separated; they build the new list from
the file being edited, never from the resolved value, so a workspace list never
silently absorbs the global one. `unset` drops the key, and the table it leaves
empty with it. `list` names the file each value came from and `--all` includes
unset settings with their documentation. `path` prints the file a write would go
to and `edit` opens it in `$VISUAL`/`$EDITOR`. None of them requires an
initialized workspace, so `jired config set creds.storage user_config --global`
works in an empty directory before `init`.

### `jired whoami [--refresh]` · `jired projects` · `jired boards [--for-project KEY]` · `jired sprints [--board B] [--all]`
Flat commands, not a `list` group. Table / JSON. `whoami --refresh` asks the
server instead of reporting the cached identity; `boards --for-project` is spelled
that way because a `--project` here would shadow the global flag. The listings had
a `--refresh` that did nothing and it is gone; they always ask the server.

`sprints --board` takes a name as well as an id. An id goes straight to the
sprint endpoint — it is what the API wants, and it may name a board outside this
project — so only a name, or no board at all, costs the extra `boards` request
that resolves it. With nothing named, a project with one board uses it, because
there is no choice to make. With several, they are asked in turn: boards over one
project's backlog commonly share a sprint pool and answer identically, and then
there is still no choice to make, so the shared answer is printed. The first
board that disagrees ends the walk and the error lists them all — so genuinely
different boards cost two requests and then say `one of: sprints (12), TSK (13),
TSK board (11)`.

`boards` fills in `project_key` for any board the listing left unlabelled, which
on Data Center is all of them — a board nobody can place cannot be selected by a
script. Two endpoints answer this and neither answers on both deployments:
`location.projectKey` in `/board/{id}/configuration` is a Cloud field that Data
Center omits from that payload entirely, and `/board/{id}/project` lists the
projects the board's filter covers. Each deployment asks the one that answers it
first and falls back to the other, since which of them a given Jira version
populates is not something to bet a `null` on.

One request per unlabelled board, in this command only, and best-effort: a board
that will not answer stays `null` rather than failing a listing that is otherwise
right. So does a board whose filter spans several projects — one project is an
answer, several is genuinely not one, and naming the first would be a guess a
script would then act on.

### `jired fetch-spec [-o/--output DIR]`
Writes the server's own description of the REST API it serves into `DIR`
(default `.`), one file per described module, named for the server and the
version that answered — `jira.corp.example-datacenter-10.7.4-api-2.wadl`. The
name is the point: a bare `application.wadl` in a directory is a file nobody can
place six months later, and two Jiras — or two versions of one — do not answer
the same. The version and the host keep their dots; everything else in a name is
flattened to a dash, so a server that reports something odd as its version still
cannot produce a path.

There is no single document to fetch. Jira mounts its REST modules on Jersey,
which serves a WADL beside each one, so the platform API (`/rest/api/2`), the
Agile API (`/rest/agile/1.0`) and the session endpoints (`/rest/auth/1`) are
three separate requests, and a module a given Jira does not mount is reported
under `skipped` rather than failing the run. Asking takes an XML `Accept`: the
`application/json` every other jired request sends comes back a 406, so the
module carries the header it needs and the transport stays deployment-agnostic.

The Agile half is why this is a command and not a link in the docs. Atlassian
publishes the platform WADL per release on `docs.atlassian.com` and has never
published anything for the Agile API — which is where `boards`, `sprints` and
every sprint move live. The instance itself serves both.

Cloud is refused with exit 10. An Atlassian-hosted site serves no WADL at all,
and the OpenAPI document that covers it is published by Atlassian rather than by
the site, so the hint names that URL instead of jired fetching it: the credential
this client carries belongs to the Jira site and has no business travelling to
`developer.atlassian.com`.

A 200 that is not a description is skipped with a warning rather than written.
An instance behind an SSO proxy answers its login page for every path including
this one, and an HTML login page saved as a `.wadl` is a file that lies for as
long as it sits there. Likewise one module refusing does not fail the run — a
token without permission for the Agile plugin still gets the platform API — and
the run fails only when nothing was written, and then with what the server
actually said rather than a summary that hides it.

Not in the read-only set `init-agents` grants: it writes files to a path the
caller names, which is exactly what that list exists to keep out. Note the name
shares a prefix with the granted `jired fetch`, and the two permission systems
draw that line differently — Codex matches argv tokens, so `["jired", "fetch"]`
does not cover `fetch-spec`, while a Claude Code `Bash(jired fetch:*)` entry
matches on text and may. What it could do with that is write a WADL under a name
it does not choose into a directory it does; worth knowing rather than worth a
different spelling.
JSON: `{"schema":"jired/fetch-spec/1","base_url":…,"deployment":"datacenter","version":"10.7.4","title":…,"output_dir":…,"specs":[{"module","path","file","media_type","bytes"}],"skipped":[{"module","path","reason"}]}`.

### `jired sprint create [NAME] [--board B] [--start TS] [--end TS] [--goal TEXT] [-i] [-n]`
The one write that does not go through `push`: a sprint has no file to stage it
in, so this posts `/rest/agile/1.0/sprint` there and then, and `-n/--dry-run`
prints the exact call instead — the same escape hatch `push` offers, for the same
reason. The new sprint is written into the local sprint cache on the way out, so
a `sprint:` naming it resolves before the next fetch has run.

Three rungs, as everywhere: the full-screen form, line prompts, then exit 7
naming what is missing. A name and a board are all it needs, and the board is
only missing when the project has more than one. `--start`/`--end` are read in
the reader's own timezone unless they name one, and go out in UTC; Jira takes
both or neither, so an end on its own is refused rather than sent and silently
dropped, and an end before its start is refused too. State is not settable: a
sprint nobody has started is `future`.

The form prefills the name the board is due — the highest number the board's
sprints have used, plus one, in the same words and the same width, so `Sprint 09`
is followed by `Sprint 10`. Changing the board renumbers it until someone types
over it. A board whose sprints are not numbered gets an empty field rather than a
guess at a naming scheme. A name the board already uses is pointed out beside the
field and not refused: Jira allows it, though it does make an issue's `sprint:`
ambiguous to resolve.

`--issues K1,K2` moves those issues into the sprint the moment it exists, exactly
as `sprint add` would — creating one is almost always followed by filling it. The
moves are ordinary file edits and stage like any other; `--push` sends them in the
same command. `-n` names the issues it would move and creates nothing.

### `jired sprint start SPRINT [--goal T] [--start TS] [--end TS] [-n]` · `jired sprint edit SPRINT [--goal T] [--name T] [--start TS] [--end TS] [-n]`
The other two writes onto a sprint. `create` set a goal only at creation and
nothing moved a sprint out of `future`, so both were the one step in starting a
sprint that had to go through the web UI: `set` writes issue frontmatter, and a
sprint has no file to stage an edit in. These have none either, so like `create`
they send there and then and `-n` prints the call.

One endpoint under both, `POST /rest/agile/1.0/sprint/{id}`, which *merges* —
what is not sent is what the sprint keeps. So only the flags that were given go
into the body, and `--goal ''` is how a goal is taken back off, an empty string
being a different request from an absent key. `--name` renames; issue files spell
a sprint by name, so a rename ends with a hint to `jired pull` and bring `sprint:`
in line. `edit` with no flags at all is exit 7 before anything is fetched.

`SPRINT` is a name or an id, matched the way `sprint add` matches it — against the
cached sprints, preferring an open one where a name repeats. A bare number that
the cache has never seen is still taken as an id, because the sprint is fetched
from the server either way and it is the server that says whether the id is real.

Two things Jira refuses with a 400 that names nothing, refused here at plan time
as exit 2 instead. A sprint with no dates cannot be started — the message names
which half is missing, and passing `--start`/`--end` sets them and starts it in
the one call. And a board runs one sprint at a time, so a second is refused with
the name of the one in the way. A sprint already `active`, or `closed`, says so
rather than sending. The dates are read in the reader's own timezone unless they
name one, and are checked against what the sprint *would* have rather than only
against each other, so a `--start` after an end that is already set is caught.
JSON: `{"schema":"jired/sprint-update/1","dry_run":false,"sprint":{…}|null,"request":{"method","path","body"}}`.

### `jired sprint close SPRINT [--move-incomplete-to SPRINT] [-n]`
The other half of a sprint's life, and the half that strands issues. Closing one
in the UI leaves everything unfinished pointing at a closed sprint with no
carry-over, and jired had no view of that beyond reading `jired list --sprint` and
comparing it against the board by eye. So this always names them — `9 issue(s) are
not done and stay in the closed sprint: TSK-181, TSK-198, …` — and
`--move-incomplete-to` carries them into the next sprint first.

"Not done" is the status *category* the last fetch recorded, not a status name: a
workflow's `Done` is `Готово` on the next instance, and the category is Jira's own
three-way reading of any of them. A status jired has never seen counts as
unfinished, because carrying an issue that was already done is a move somebody can
undo and stranding one is what this exists to prevent. The set is read off the
working copy, like `list` reads it, so the issues that move are the issues listed.

The carry-over goes first and the close is abandoned if it fails: an issue cannot
be rescued out of a sprint that is already closed, so the order that leaves a
recoverable state is moves, then close. The moves themselves are ordinary
`sprint add` edits, pushed in the same command rather than staged — a close that
left them staged would be the stranding again, one step removed. A target that is
itself closed, or is the sprint being closed, is exit 2; so is closing a `future`
sprint, or one already closed.
JSON: `{"schema":"jired/sprint-close/1","dry_run":false,"closed":true,"sprint":{…}|null,"request":{…},"incomplete":[{"key","file","status"}],"carried_to":{"id","name"}|null,"issues":[{"key","file","changed"}],"push":{…}|null}`.

### `jired releases [--all]`
The project's versions — what `fix_versions:` and `affects_versions:` name — as
the server holds them now (`GET …/project/{key}/versions`): id, state, name, and
the start and release dates. Released and archived versions are hidden without
`--all`, the way `sprints` hides closed sprints; `--json` always carries all of
them. The listing replaces the cached one, which is what `push` checks version
names against. The project is `--project`, else `workspace.project`.
JSON: `{"schema":"jired/releases/1","project":"TSK","versions":[{"id","name","project_id","description","released","archived","start_date","release_date"}]}`.

### `jired release create NAME [--start D] [--release-date D] [--description T] [--issues K1,K2] [--push] [-n]`
Versions had no command at all, so a sprint whose tasks were scripted still
needed the web UI for the version the release task points at. The group is
`release` because `jired version` already says which binary this is.

Like `sprint create`, it posts there and then (`POST …/version`) — a version has
no file to stage it in — and `-n/--dry-run` prints the call. The body names the
project by `projectId`, the spelling both deployments take; Cloud documents the
key as deprecated. The id comes from any version the project already has, or
from `GET …/project/{key}` for its first. A name the project already has is exit
2 naming it, rather than Jira's 400. Dates are days (`2026-10-29`), because that
is all Jira keeps of a version's; a release date before the start is exit 2.

The new version goes into the cache on the way out, so `fix_versions:` can name
it straight away. `--issues` adds it to those issues' `fix_versions:` — appended,
the rest of the list kept — as staged edits, and `--push` sends them.
JSON: `{"schema":"jired/release-create/1","dry_run":false,"project":"TSK","version":{…}|null,"request":{"method","path","body"},"issues":[{"key","file","changed"}],"push":{…}|null}`.

### `jired release edit VERSION [--name T] [--description T] [--start D] [--release-date D] [-n]` · `jired release archive VERSION [-n]`
One endpoint under both, `PUT …/version/{id}`, which merges: only what was
given is in the body, and `--description ''` is how a description comes off.
`VERSION` is a name or an id, matched against the project's listing as the server
returns it — the id first, then the name without regard to case. A miss is exit
2 listing the unreleased versions. `edit` with no flags is exit 7 before anything
is asked; renaming onto a name that exists is exit 2. Issue files spell a version
by name, and renaming one does not touch the issues, so an ordinary fetch does
not see it: a rename ends with a hint to `jired fetch --full` and then `pull`.
`archive` sets `archived`, which takes the version out of Jira's pickers; one
already archived says so.
JSON: `{"schema":"jired/release-update/1","dry_run":false,"project":"TSK","version":{…}|null,"request":{…}}`.

### `jired release ship VERSION [--date D] [--move-unfixed-to VERSION] [-n]`
Marks the version released, and does for it what `sprint close` does for a
sprint: names the issues in the working copy whose `fix_versions:` hold it and
whose status *category* is not `done` — `2 issue(s) are not done and stay in the
released version: TSK-181, TSK-198` — and with `--move-unfixed-to` moves them on
first. The move swaps the one version for the other in each list, keeping any
other version the issue is in, and is pushed in the same command; if it fails,
the version is left unreleased. Releasing goes after the move because a version
that shipped with work still pointing at it is the state the flag exists to
prevent.

This is jired's own move, not Jira's `moveUnfixedIssuesTo`: that one moves what
is *unresolved* — a different question from "not done" on a workflow that never
sets a resolution — and moves it out of sight of the working copy. Here the set
that moves is the set that was listed.

The release date is `--date`, else the date the version was planned for, else
today, and is only sent when it differs from what the version holds. A version
already released, or archived, is exit 2; so is moving to itself, or to one that
has shipped.
JSON: `{"schema":"jired/release-ship/1","dry_run":false,"released":true,"project":"TSK","version":{…}|null,"request":{…},"unfinished":[{"key","file","status"}],"moved_to":{"id","name"}|null,"issues":[{"key","file","changed"}],"push":{…}|null}`.

### `jired create` — the field flags
`--assignee`, `--sprint`, `--labels`, `--priority`, `--component` (repeatable),
`--epic`, and `--set field=value` (repeatable) for everything else, including an
instance's custom fields by id. They go through `Frontmatter::set_field`, the
same setter `jired set` uses, so the two cannot disagree about what a field is
called or how a value is spelled. Precedence is template, then flag, then
`--set`, which is what lets one skeleton stamp out variations.

`--template` takes a name under `.jired/templates` or a path, preferring the
named one when both exist. A template is a partial file: the identity fields
(`key`, `id`, `jired`) are supplied if it does not carry them, so a skeleton with
frontmatter no longer needs a placeholder key to get past the parser.

### `jired show ISSUE [--raw|--path]`
Everything the workspace knows about one issue, from the file when there is one
and from the snapshot when there is not — the preference `create --like` uses
too, and the reason both live on `cmd::lookup`. The default is the issue as jired
models it; `--raw` is the server's own payload.

`--path` prints the issue's file and nothing else, relative to the workspace, so
`$EDITOR "$(jired show TSK-710 --path)"` works. File names carry a zero-padded
number — `TSK-0710. Summary.md` — so `ls TSK-710*` finds nothing, and guessing
the padding is not something a caller should have to do. `ISSUE` is a key in any
case and with or without the padding. An issue the workspace knows but has no
file for is exit 2, as is one it does not know at all. Every form of the JSON
carries `file`, null when there is none.

`--raw` is the point of the command. An unset field is not written into a `.md`
file, so the file cannot answer "is this empty, or does this instance not have
that field?" — and the answer was `sqlite3 .jired/state.db` with `json_extract`
over `remote_raw`, an agent reverse-engineering a private schema to ask
something the tool already knows.
JSON: `{"schema":"jired/show/1","key":"PROJ-142","file":"PROJ-0142. Fix login flow.md","source":"file","frontmatter":{…},"description":"…"}`.

### `jired fields [TEXT] [--custom]`
What this instance's fields are called, out of the definitions the last fetch
cached. `custom:` keys a value by its Jira id, which is the only name stable
across instances and the only one nobody can read; finding out what
`customfield_10015` is meant opening `state.db` in `sqlite3`. `TEXT` filters on
the name or the id, `--custom` narrows to the instance's own.
JSON: `{"schema":"jired/fields/1","count":42,"fields":[{"id","name","custom","schema_type","items"}]}`.

### `jired users [TEXT]`
The people this workspace knows and the mention that names each, offline: the
assignees, reporters, comment and worklog authors in the snapshots, and every
`@[Name](id)` in the files. The id is the half of a mention nobody can type — an
account id on Cloud, a username on Data Center — and finding one was a `grep`
over the working copy that also listed a renamed person once per name. One entry
per id: named by the name seen most recently (the issue's last update, the
comment, the worklog — a mention in a file carries no date and never outranks
one that does), the others under `also_known_as`. Most mentioned first. `TEXT`
filters on any of the names or the id, ignoring case.
JSON: `{"schema":"jired/users/1","count":1,"users":[{"display_name","account_id","mention":"@[Name](id)","also_known_as":[],"seen":12}]}`.

### `jired list [--summary-contains T] [--label L] [--status S…] [--not-status S…] [--category new|indeterminate|done] [--sprint S] [--assignee U] [--issue-type T] [--columns COLS] [--keys]`
`search` is JQL on the server; this is its offline half, and the other half of
any bulk edit — choosing what to edit. Without it the choice was a `grep` over
the `.md` files, which means the caller writing regexes against jired's own
format, and is wrong the moment a filename and a summary disagree (the filename
is cosmetic).

It reads the **files**, not the snapshot: the files are what a following
`label add` or `set` will edit, so what this lists and what those change are the
same set even when the working copy is ahead of the last fetch. A file that will
not parse is skipped rather than failing the listing — `doctor` and `diff` are
where a broken file is reported.

Every filter is a conjunction, and every comparison ignores case: these are
values read off a screen and typed back. `--summary-contains` is a substring,
because that is what looking for `[ALL]` means; the rest match a whole value,
because a status half-named is a different status. `--issue-type` is one of
those: an instance whose nine type names differ by a prefix — `Task`, `Sub-task`,
`Analytics sub-task`, `Sub-task bug` — would answer a different question under a
substring, so `--issue-type sub-task` is `Sub-task` and nothing else.
`--assignee` is the exception it looks like it is not: a substring of the file's
`@[Name](id)`, so either half finds the person. `--sprint` asks where the issue
*is*, which is the last entry of its sprint history.

A repeated `--status` is the one disjunction — an issue is in one status, so
several can only mean "any of" — and `--not-status` (also repeatable) excludes
by exact name. `--category` selects by Jira's own three buckets
(`new`/`indeterminate`/`done`), which is what "everything not finished yet"
means across differently-named workflows and is the same vocabulary the picker's
`ui.issues.filter` takes; naming the negation status by status breaks the day a
workflow gains one. The bucket behind a status name is the server's
classification, read off the snapshots — every status a workflow uses is on some
pulled issue — so a status jired has never seen (typed into a file by hand) has
no category and `--category` does not claim it.

Bare, it prints a line per matching issue: key, status, summary. `--keys` prints
the keys alone, one per line, with no heading, count or colour, so the output is
an argument list:
`jired list --keys --label regular | xargs jired sprint add --sprint "Sprint 44"`.

`--columns` names the fields instead, in the order it names them, out of `key`,
`type`, `status`, `assignee`, `summary`, `sprint`, `labels`, `parent`, `epic`,
`priority`, `fix_versions` and `file` — the
fields of the JSON document and nothing besides, so the two cannot disagree
about what an issue is. It answers the question the default line cannot: which of
these are `Task`s and who has them, on an instance where the type names differ by
a prefix. The layout follows the stream rather than a flag, because the caller
should not have to say which of the two it is: columns are padded to line up when
stdout is a terminal and separated by a single tab when it is not, so
`jired list --columns key,type,assignee | cut -f2` works without a JSON parser.
An issue with nothing in a column gets an empty cell, never a placeholder, and a
tab or newline inside a value is flattened to a space rather than being allowed
to forge a column boundary. `assignee` is the name out of `@[Name](id)` and
`sprint` is where the issue is *now*, the last of the ones it has been in; the
id and the whole history are in `--json`, which is also where anything no column
offers lives.

`sprint` in the JSON is that whole history, oldest first, exactly as the file's
`sprint:` has it — and not the last of it, the way the filter and the column read
it. The difference between the two is what splits carry-over from new work, and
answering that used to mean parsing the `.md` files instead of using `--json`.
JSON: `{"schema":"jired/list/1","count":2,"issues":[{"key","file","summary","status","type","labels","assignee","sprint":["Sprint 42","Sprint 43"]}]}`.

### `jired label add|remove|set LABEL ISSUES… [--push] [--dry-run]`
Labels are a list, so there are three verbs: `add` and `remove` touch one label
and leave the rest, `set` replaces the lot and takes several comma-separated.
Staged like `assign` — the file is written, `status` and `diff` show it, `push`
sends it, `--push` sends it now. An issue that already has the label (or already
lacks it, for `remove`) is reported and not staged: staging a change that does
nothing only gives `push` something to explain.

A label containing whitespace is exit 2 — Jira labels have no spaces, so it is a
list somebody separated the wrong way — and so is passing several to `add` or
`remove`, which act on one at a time.
JSON: `{"schema":"jired/label/1","operation":"add","labels":["regular"],"issues":[…],"pushed":false}`.

### `jired link ISSUES DIRECTION OTHERS [--push] [--dry-run]` · `jired link ISSUES --web URL [--title T]` · `jired unlink ISSUES [DIRECTION] OTHERS` · `jired unlink ISSUES --web URL`
Writes the `links:` line push turns into a link, without touching the YAML:
`jired link TSK-740 is blocked by TSK-739`. The direction is the words between
the two keys, so it needs no quotes, and either end may be several keys
comma-separated. It is checked against the link types the last `fetch` listed
and written in the instance's own spelling, so `Blocks` becomes `blocks`; a
direction the instance does not have is exit 2 naming the ones it does. With no
list cached nothing is checked here, and push refuses an unknown one at plan
time as it always has. Linking an issue to itself is exit 2. `--web URL
[--title T]` writes a `- web:` line instead; the same URL with a new title
replaces the line rather than adding a second link to the page.

`unlink` takes the lines out: with a direction, that link; without one, every
link between the two issues; with `--web URL`, the web link to that page.
Staged like `label` — `status` and `diff` show it, `push` sends it, `--push`
sends it now — with `staged` and `next` in the JSON. An issue line goes before
the first web line, where a pull puts it. A line already there (for `unlink`,
already absent) is reported and not staged.
JSON: `{"schema":"jired/link/1","operation":"link","links":[{"direction":"blocks","key":"TSK-741"}],"issues":[…],"staged":true,"next":"jired push TSK-740","pushed":false}`.

### `jired parent ISSUES… --to ISSUE [--push] [--dry-run]`
Writes `parent:` across several sub-tasks at once, staged like `label` and
`sprint add`: the same edit a text editor makes, without opening four files to
correct one misfiling. `--to` takes a key or a path, and something that is not
an issue key is exit 2 rather than a value pushed at the server. An issue
already under that parent is reported and not staged, and an issue named as its
own parent is exit 2 — the shape a mistyped `--to` usually takes.

What `push` does with the staged line differs by deployment and neither this
command nor the file carries that difference; see `push` above for the Move
wizard Data Center needs and the verification that follows it. Where the
workspace's credential cannot open a web session, staging a *sub-task's* parent
warns that `push` will refuse it — a warning and not a refusal, because the line
is what the file should say once the move is made in Jira, and an ordinary
issue's `parent` is a field update the same credential sends without trouble.
JSON: `{"schema":"jired/parent/1","parent":"PROJ-150","issues":[…],"pushed":false}`.

### `jired set --field F --value V ISSUES… [--push] [--dry-run]`
The general form of the three above, for the fields that do not earn their own
spelling: `components`, `story_points`, `priority`, and an instance's own custom
fields by id. Without it, setting one of those across six issues meant a script
rewriting the YAML — the caller parsing jired's file format, which breaks the
first time the format moves.

`--field` takes any name in `frontmatter::EDITABLE` or a `customfield_*` id;
`--value` takes the text a person would type, with commas separating a list and
an empty string clearing the field (what a push sends as `null`). A custom value
that parses as JSON is stored as JSON, which is how a multi-select is written in
the file. `links` is refused — it is a list of maps, not a value — and so are
`key` and `id`, which identify the issue.

Both checks happen before a file is opened: a field nobody can write is a
mistake about the command, not about one of the issues, and finding out on the
fourth would leave three already edited.
JSON: `{"schema":"jired/set/1","field":"priority","value":"Low","issues":[…],"pushed":false}`.

### `jired sprint add ISSUES… --sprint S [--push] [--dry-run]` · `jired sprint remove ISSUES… [--push] [--dry-run]`
The scriptable single-field commands for an issue's sprint, alongside
`transition`, `assign` and `comment`, and behaving like them: the frontmatter is
written, the change shows up in `status` and `diff`, and `push` sends it — or
`--push` does, straight away. What they save is knowing where `sprint:` sits in
the key order and that its last entry is the one a move can act on.

`--sprint` takes a name or an id, matched against the sprints the workspace has
fetched, preferring an open one where a name repeats; the board's own spelling is
what gets written, whatever case was typed. A bare number that is no sprint's id
is taken as the number that ends a name — `17` for `TSK Sprint 17` — when exactly
one sprint answers to it, and is exit 2 naming them when several do. A name no board knows is exit 2 here,
listing the open ones — the same condition `push` could only report afterwards as
a warning, having already skipped the issue.

The list is a history, so a move appends and drops any earlier entry for the same
sprint rather than leaving the name in twice. An issue already in the sprint is
reported and not staged: staging a change that does nothing would only give `push`
something to explain. `remove` empties the list, which is what the backlog looks
like in a file. Issues are named as keys or file paths, positionally or
comma-separated; one that the workspace does not have is a warning and the others
still move, and exit 2 only when none of them are here.
Without `--push` nothing reaches the server, and the JSON says so where an agent
reads it rather than only in a stderr hint: `staged: true` and `next`, the push
that sends it. `label`, `parent`, `assign` and `set` carry the same two fields.
JSON: `{"schema":"jired/sprint-move/1","sprint":{…}|null,"issues":[{"key","file","changed"}],"staged":true,"next":"jired push PROJ-142","pushed":false}`.

### `jired version [--changelog]`
The same lines `--version` prints, as a command — and, with `--changelog`, the
`CHANGELOG.md` the binary was compiled from. Compiled in rather than read from
disk on purpose: the copy that describes the binary in your hand is the one it
was built with, a checkout beside it has moved on, and an installed binary has no
checkout at all. It costs ~19 KB of `.rodata`, which is the whole price of being
able to answer "what changed" without the source.
JSON: `{"schema":"jired/version/1","version":"0.3.0","commit":"9212430","built":…,"rustc":…,"target":…,"changelog":null}`.
`changelog` is null unless `--changelog` was given: every agent that never reads
it would otherwise pay for it in every document.

### `jired --version` / `-V`
`-V` is one line — `jired 0.3.0 (9212430 2026-08-21T10:49:39Z)`; `--version` adds
the compiler and target. The commit carries a `-dirty` suffix when the tree had
uncommitted changes, and reads `unknown` when built from a tarball with no git.
`SOURCE_DATE_EPOCH` fixes the timestamp for reproducible builds.

### `jired completion <shell>`
Via `clap_complete`, for bash/zsh/fish/powershell/elvish; `--help` says where each
shell wants the script. Config keys, schema names and enum values complete; issue
keys do not, because completion must not open the database.

### `jired init-agents`
`jired init-agents [--print] [--no-docs] [--no-permissions]`
Rewrites `CLAUDE.md`/`AGENTS.md` and the two permission files that let an agent run
the read-only commands without asking: `.claude/settings.json` (`permissions.allow`
entries, merged into whatever else the file holds) and `.codex/rules/default.rules`
(execpolicy `prefix_rule`s). The Markdown and the rules file are overwritten only
between jired sentinel markers; the JSON is merged, never pruned, because an entry
someone added by hand is a decision.

The granted set is one table in `cli/cmd/init_agents.rs`, so the docs, the allow-list
and the rules cannot disagree; the line it draws is "sends nothing to Jira and writes
nothing into the working copy", which lets `fetch` and `--dry-run` in and keeps every
writing command out. Both systems match a command-line prefix, so global flags have to
follow the subcommand.

The Markdown block opens `<!-- BEGIN jired v0.7.0 -->`: the version that wrote it.
The marker is *found* by its `<!-- BEGIN jired` prefix, so a block an older jired
stamped is replaced in place rather than left with a second one appended under it,
and the closing marker is looked for after the opening one so prose above the block
cannot truncate it. A block whose stamp or body is not this build's is what
`jired status` reports as `stale_agent_docs` and what `doctor`'s `agents` check
repairs, naming the version that wrote it.
JSON: `{"schema":"jired/init-agents/1","files":[…],"permissions":[…]}`.

### `jired schema [name]`
Prints JSON Schemas: frontmatter, every `jired/*/1` output, the error shape.
