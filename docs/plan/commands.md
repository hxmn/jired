# Command specifications

## Global contract

**Flags on every command:** `--json`, `--no-input`, `-y/--yes`, `-q/--quiet`,
`-v/-vv`, `--color auto|always|never`, `--no-color`, `--dir PATH`, `--config PATH`,
`--project KEY`, `--ca-cert PATH`, `--insecure`. They are grouped under their own
help heading and displayed after the command's own options.

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
JSON: `{"schema":"jired/fetch/1","fetched":123,"changed":17,"deleted":2}`.

### `jired pull`
`jired pull [ISSUES…] [--no-fetch] [--strategy abort|merge|ours|theirs] [--force] [-n/--dry-run] [fetch scopes…]`
Fetch (unless `--no-fetch`) + three-way merge ([sync-algorithm.md](sync-algorithm.md)).
Default: abort-on-conflict — exit 5, list conflicting files/fields, write nothing.
`merge` writes git-style markers, records conflicts, **and still exits 5**: a
non-zero status does not mean the working copy is unchanged. `--force` ≡ theirs
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
`jired push [ISSUE…] [-n/--dry-run|preview] [--force] [--allow-lossy] [--allow-comment-delete] [--allow-worklog-delete] [--resume]`
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
`sprint:` is a list — an issue that rolls over stays in the sprint it came from
— written as a plain string when there is only one. Its last entry is where the
issue is now, and the only part a push can act on: a change to it is a board call
(`POST /rest/agile/1.0/sprint/{id}/issue`, or `…/backlog/issue`), not a field
update, because the Sprint field is read-only on most instances. The name is
resolved against the cached sprints, preferring an open one; a name no board
knows is reported and not guessed at, and an edit to the earlier entries is
reported as history that cannot be rewritten.
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

### `jired status [--all]`
One-screen overview: dirty files, new local issues, staged transitions (incl. board
moves), conflicts, last fetch age, pending push journal, and a git-style trailer
saying what to run next. `--all` additionally lists the unchanged issues. JSON
mirrors it. Offline. Visible alias `st`.

### `jired log [ISSUE] [--limit N] [--refresh]`
Changelog entries newest first, each tagged with its issue. With no `ISSUE` it
covers the whole workspace, the way `git log` does; naming one narrows to it.

Changelogs are fetched per issue (`expand=changelog`) and cached, so the first
log over a workspace fetches what it is missing and every run after it is
offline. Freshness is the issue's own `updated` at the time its changelog was
cached, not "is the cache empty": an issue with no history is a real answer and
must not be asked about twice, and one the server has moved on from is asked
about exactly once. `--refresh` overrides both. In workspace mode a single
unreadable issue warns and stops the fetch rather than losing the rest of the
log.

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

### `jired report time [--days N] [--show-issues]`
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

### `jired create [--type Story] [--template T] [--summary S] [-i]`
Writes `NEW-<6hex>. Summary.md` (D14) from template; real key assigned on push
(file renamed; JSON reports the mapping).

With `--summary` nothing is asked, so a scripted `create` behaves the same with or
without a terminal. Without one — or with `-i`, which asks anyway — it opens the
new-issue form: summary, type, priority, labels, parent and project, with the
options taken from what the workspace has already pulled (ranked by use) rather
than from the server. It is offline like the rest of the command. The form draws
on the alternate screen over stderr, so `--json` stdout stays a single document.

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

### `jired transition ISSUE --to "In Review" [--list] [--field k=v…] [--push] [--dry-run]`
Validates against cached/live transitions; missing required fields → exit 2 with a
machine-readable list. The flag is `--field`, repeated (`--field k=v --field j=w`).
Without `--to` and without `--list` it is a usage error and emits nothing on stdout.
`--list` alone prints the transitions available from the current status, falling
back to the cached list (with its age) when the network is down.

### `jired assign ISSUE --me|--user Q|--none [--push] [--dry-run]`
Exactly one of `--me`/`--user`/`--none` is required — clap enforces it, so the
error arrives before any work. `--user` resolves via user search; an ambiguous
query lists the candidates and exits 2 rather than picking one. There is no
picker: the TUI is M4 work and nothing in the tree draws one today.

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

### `jired search "project = PROJ AND status = Open" [--limit N]`
JQL is positional; `--jql` is the same value spelled as a flag, and the two
conflict. Table / JSON rows, `showing N of TOTAL`, an explicit line when nothing
matches, and a warning when `--limit` is clamped to the server's page ceiling of
100. `--pull` is hidden and unimplemented: it parses and then exits 2 saying so.

### `jired resolve [ISSUES…] [--list] [--ours|--theirs|--done]`
Lists the recorded conflicts (issue key and field). A bare `resolve` prints that
list and then exits 7 rather than choosing for the user; `--list` prints it and
exits 0. `--ours`/`--theirs` re-run the merge under that strategy so they cannot
drift from `pull --strategy`; `--theirs` names what it would discard and asks
first. `--done` accepts files edited by hand and refuses (exit 5) any that still
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

### `jired show ISSUE [--raw]`
Everything the workspace knows about one issue, from the file when there is one
and from the snapshot when there is not — the preference `create --like` uses
too, and the reason both live on `cmd::lookup`. The default is the issue as jired
models it; `--raw` is the server's own payload.

`--raw` is the point of the command. An unset field is not written into a `.md`
file, so the file cannot answer "is this empty, or does this instance not have
that field?" — and the answer was `sqlite3 .jired/state.db` with `json_extract`
over `remote_raw`, an agent reverse-engineering a private schema to ask
something the tool already knows.
JSON: `{"schema":"jired/show/1","key":"PROJ-142","source":"file","frontmatter":{…},"description":"…"}`.

### `jired fields [TEXT] [--custom]`
What this instance's fields are called, out of the definitions the last fetch
cached. `custom:` keys a value by its Jira id, which is the only name stable
across instances and the only one nobody can read; finding out what
`customfield_10015` is meant opening `state.db` in `sqlite3`. `TEXT` filters on
the name or the id, `--custom` narrows to the instance's own.
JSON: `{"schema":"jired/fields/1","count":42,"fields":[{"id","name","custom","schema_type","items"}]}`.

### `jired list [--summary-contains T] [--label L] [--status S] [--sprint S] [--assignee U] [--type T] [--keys]`
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
because a status half-named is a different status. `--assignee` is a substring of
the file's `@[Name](id)`, so either half finds the person. `--sprint` asks where
the issue *is*, which is the last entry of its sprint history.

`--keys` prints the keys alone, one per line, with no heading, count or colour,
so the output is an argument list:
`jired list --keys --label regular | xargs jired sprint add --sprint "Sprint 44"`.
JSON: `{"schema":"jired/list/1","count":2,"issues":[{"key","file","summary","status","type","labels","assignee","sprint"}]}`.

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
what gets written, whatever case was typed. A name no board knows is exit 2 here,
listing the open ones — the same condition `push` could only report afterwards as
a warning, having already skipped the issue.

The list is a history, so a move appends and drops any earlier entry for the same
sprint rather than leaving the name in twice. An issue already in the sprint is
reported and not staged: staging a change that does nothing would only give `push`
something to explain. `remove` empties the list, which is what the backlog looks
like in a file. Issues are named as keys or file paths, positionally or
comma-separated; one that the workspace does not have is a warning and the others
still move, and exit 2 only when none of them are here.
JSON: `{"schema":"jired/sprint-move/1","sprint":{…}|null,"issues":[{"key","file","changed"}],"pushed":false}`.

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
JSON: `{"schema":"jired/init-agents/1","files":[…],"permissions":[…]}`.

### `jired schema [name]`
Prints JSON Schemas: frontmatter, every `jired/*/1` output, the error shape.
