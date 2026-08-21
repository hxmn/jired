# Command specifications

## Global contract

**Flags on every command:** `--json`, `--no-input`, `-y/--yes`, `-q/--quiet`,
`-v/-vv`, `--color auto|always|never`, `--dir PATH`, `--config PATH`,
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

## Commands

### `jired init`
`jired init [--url U] [--token T] [--email E] [--auth api-token|pat|basic] [--deployment cloud|datacenter] [--insecure-store] [--project KEY] [--no-pull] [--force] [--no-agent-docs]`
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
`CLAUDE.md`/`AGENTS.md` (unless `--no-agent-docs`) → optional initial pull. A
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
JSON: `{"schema":"jired/push/1","results":[{"key":"PROJ-1","ok":true,"ops":[…]},{"key":"NEW-x7f3a2","ok":true,"new_key":"PROJ-201"}]}`.

### `jired diff`
`jired diff [ISSUE…] [--against base|remote] [--remote] [--stat] [--exit-code]`
Modes: working↔base (`--against base`, the default: what `push` would send),
working↔remote (`--against remote`: what `pull` would bring in), base↔remote
(`--remote`). `--all` was the old spelling of `--against remote` and is hidden and
deprecated — on every other command `--all` widens the scope, and here it selected
a comparison.
Unified diff for bodies (`similar`), field-level table for metadata; `--stat` summary.
Exit 0 even with differences unless `--exit-code` (git-style). Fully offline.

### `jired status [--all]`
One-screen overview: dirty files, new local issues, staged transitions (incl. board
moves), conflicts, last fetch age, pending push journal, and a git-style trailer
saying what to run next. `--all` additionally lists the unchanged issues. JSON
mirrors it. Offline. Visible alias `st`.

### `jired log ISSUE [--limit N] [--refresh]`
Cached changelog (fetched with `expand=changelog`). Offline unless `--refresh`.

### `jired comment ISSUE [-m "text"] [--push] [--dry-run]`
Appends a `### [new]` section to the file; `--push` sends immediately. Without
`-m` it opens `$VISUAL`/`$EDITOR` on a scratch file (an empty buffer aborts), or
reads the body from stdin when one is piped.

### `jired logwork [ISSUE] [TIME] [-m "text"] [--started TS] [-i] [--push]`
Books time against the issue by appending a `### [new] 2h 30m — <when>` section
under `## Worklogs`; `--push` sends immediately. `--started` defaults to now, so
the entry is dated when the work happened rather than when the push does.

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
With no issue named, a filtered list of everything in the working copy: type a
subsequence of the key or the summary (`pl142`, `sso ro`), ↑↓ to move, ⏎ to open.
Most recently touched first. The issue can also be named by key in any case, by
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

### `jired open ISSUE [--print]`
Browser via `open` crate; `--print` (and `--json`/non-TTY) prints the URL instead.

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
the wrong file, the `.gitignore` block, and the generated agent docs.
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

### `jired --version` / `-V`
`-V` is one line — `jired 0.1.0 (9212430 2026-08-21T10:49:39Z)`; `--version` adds
the compiler and target. The commit carries a `-dirty` suffix when the tree had
uncommitted changes, and reads `unknown` when built from a tarball with no git.
`SOURCE_DATE_EPOCH` fixes the timestamp for reproducible builds.

### `jired completion <shell>`
Via `clap_complete`, for bash/zsh/fish/powershell/elvish; `--help` says where each
shell wants the script. Config keys, schema names and enum values complete; issue
keys do not, because completion must not open the database.

### `jired agent-docs`
Regenerates `CLAUDE.md`/`AGENTS.md` (overwrites only between jired sentinel markers).

### `jired schema [name]`
Prints JSON Schemas: frontmatter, every `jired/*/1` output, the error shape.
