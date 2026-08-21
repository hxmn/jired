# Command specifications

## Global contract

**Flags on every command:** `--json`, `--no-input`, `--yes`, `--quiet`, `-v/-vv`,
`--dir PATH`, `--config PATH`, `--url`, `--token`, `--email`, `--project`,
`--insecure`, `--ca-cert PATH`, `--profile NAME`.

**Env vars:** `JIRED_URL`, `JIRED_EMAIL`, `JIRED_TOKEN`, `JIRED_PROJECT`,
`JIRED_PROFILE`, `JIRED_NO_INPUT`, `JIRED_SECRET_KEY`, `JIRED_CREDS_STORAGE`,
`JIRED_CA_CERT`, `JIRED_INSECURE`, plus standard `HTTPS_PROXY`/`NO_PROXY`.

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
`jired init [--url U] [--auth api-token|pat|basic] [--project KEY] [--no-pull] [--force]`
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
`CLAUDE.md`/`AGENTS.md` → optional initial pull.
Errors: 3 bad creds; 7 missing url/token with `--no-input`.
JSON: `{"schema":"jired/init/1","account":{…,"tls":{…}},"workspace":{…},"pulled":{…}}`.
Agent example:
`jired init --url https://x.atlassian.net --email a@b.c --token "$JIRED_TOKEN" --project PROJ --no-input --json`

### `jired fetch`
`jired fetch [--jql Q] [--sprint S] [--board B] [--issues K1,K2] [--updated-since TS] [--full] [--slim]`
Incremental by default (`updated >= last_fetch − 5min` skew buffer); paginated
(100/page, 4 parallel); 429/`Retry-After` aware (D26). Updates remote snapshot only —
never touches files. Resumable after network failure.
JSON: `{"schema":"jired/fetch/1","fetched":123,"changed":17,"deleted":2}`.

### `jired pull`
`jired pull [--no-fetch] [--strategy theirs|ours|merge] [--force] [fetch scopes…]`
Fetch (unless `--no-fetch`) + three-way merge ([sync-algorithm.md](sync-algorithm.md)).
Default: abort-on-conflict — exit 5, list conflicting files/fields, write nothing.
`merge` writes git-style markers and records conflicts. `--force` ≡ theirs everywhere.
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
`jired push [ISSUE…] [--dry-run] [--force] [--allow-lossy] [--allow-comment-delete] [--resume]`
Plans ops (D23 order), `--dry-run` prints exact API calls + diffs (human and JSON).
Per-issue concurrency check (D8): stale → that issue fails (exit-5 contribution,
"pull first"), others proceed. Partial failure → exit 1 with per-issue results; base
advances only for confirmed issues (durable `pending_op` journal). Lossy-region edit
without `--allow-lossy` → exit 6.
A sprint change is a board call (`POST /rest/agile/1.0/sprint/{id}/issue`, or
`…/backlog/issue`), not a field update: the Sprint field is read-only on most
instances. The name is resolved against the cached sprints, preferring an open
one; a name no board knows is reported and not guessed at.
JSON: `{"schema":"jired/push/1","results":[{"key":"PROJ-1","ok":true,"ops":[…]},{"key":"NEW-x7f3a2","ok":true,"new_key":"PROJ-201"}]}`.

### `jired diff`
`jired diff [ISSUE…] [--remote] [--all] [--stat] [--exit-code]`
Modes: working↔base (default), base↔remote (`--remote`), working↔remote (`--all`).
Unified diff for bodies (`similar`), field-level table for metadata; `--stat` summary.
Exit 0 even with differences unless `--exit-code` (git-style). Fully offline.

### `jired status`
One-screen overview: dirty files, new local issues, staged transitions (incl. board
moves), conflicts, last fetch age, pending push journal. JSON mirrors it. Offline.

### `jired log ISSUE-KEY [--limit N]`
Cached changelog (fetched with `expand=changelog`). Offline.

### `jired comment ISSUE-KEY -m "text" [--push]`
Appends `### [new]` section to the file; `--push` sends immediately.

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
is refused rather than sent. The sprint field offers the sprint running now, the
ones queued behind it, and the backlog; whichever sprint the issue is already in
is always among them, even if it has closed. Options come from
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

### `jired transition ISSUE-KEY --to "In Review" [--fields k=v…]`
Validates against cached/live transitions; missing required fields → exit 2 with a
machine-readable list.

### `jired assign ISSUE-KEY [--me|--user Q]`
`--user` resolves via user search; ambiguity → TUI picker or exit 7.

### `jired attach ISSUE-KEY FILE…` / `jired attachments pull [KEY…] [--max-size N]`
`attach` copies into the dot-folder and stages upload (cap 50 MiB default).
`attachments pull` replaces stubs with real files (streamed, sha256-verified).
Policy: lazy by default (D5).

### `jired open ISSUE-KEY`
Browser via `open` crate; prints URL in `--json`/non-TTY.

### `jired search --jql "…" [--pull] [--limit N]`
Table / JSON rows; `--pull` pulls matches into workspace scope.

### `jired resolve [KEY] [--ours|--theirs] [--field F]`
TUI conflict list, per-field pick, `$EDITOR` for body hunks. Non-interactive
requires explicit flags, else exit 7.

### `jired clean [--regroup] [--dry-run] [--force]`
Removes orphaned files/symlinks/stubs (never dirty files without `--force`);
`--regroup` rebuilds all grouping folders from `state.db` (they are never the
source of truth).

### `jired doctor [--dry-run] [--regroup]`
Checks a workspace over and repairs what it can: `.jired/` layout and permissions,
missing config defaults, settings that changed name (`checklist.done_transition` →
`children.done_transition`), credentials that are unreadable or no longer where
`creds.storage` says they belong (it moves them), filenames that predate a naming
convention (this is the migration path for zero-padded issue numbers), issue files
whose sections predate one — subtasks written under `## Checklist` move to
`## Children`, and the checklist comes back from the last snapshot — custom fields
that disagree with `ignored_custom_fields` (dropped from every file; restored from
the last snapshot for a file that is still in sync, since after un-ignoring one it
is the file that would read as having cleared the field), database rows pointing at
the wrong file, the `.gitignore` block, and the generated agent docs.
Renaming files re-links the grouping folders automatically; `--regroup` rebuilds them
even when nothing was renamed. Idempotent. Exit 1 when something needs a human.
JSON: `{"schema":"jired/doctor/1","checks":[{"name":"filenames","status":"fixed","detail":"…","changes":[{"from":"PROJ-9. x.md","to":"PROJ-0009. x.md"}]}],"fixed":1,"pending":0,"problems":0}`.

### `jired config get|set|add|remove|unset|list [--global]`
TOML round-trip preserving comments (`toml_edit`); refuses to store secrets
(points at `init`). Values with a fixed vocabulary (`creds.storage`,
`server.deployment`, `auth.kind`, `links.mode`) are validated and canonicalised
at set time; `list --all` prints each setting's documentation. `add`/`remove`
edit one value of a setting that holds a list (only `ignored_custom_fields`
today), written as a TOML array and read back — and given in an environment
variable — comma-separated; they build the new list from the file being edited,
never from the resolved value, so a workspace list never silently absorbs the
global one. `unset` drops the key, and the table it leaves empty with it.

### `jired whoami` · `jired projects|boards|sprints list [--refresh]`
Cached + `--refresh`; table / JSON.

### `jired --version` / `-V`
`-V` is one line — `jired 0.1.0 (9212430 2026-08-21T10:49:39Z)`; `--version` adds
the compiler and target. The commit carries a `-dirty` suffix when the tree had
uncommitted changes, and reads `unknown` when built from a tarball with no git.
`SOURCE_DATE_EPOCH` fixes the timestamp for reproducible builds.

### `jired completion <shell>`
Via `clap_complete`.

### `jired agent-docs`
Regenerates `CLAUDE.md`/`AGENTS.md` (overwrites only between jired sentinel markers).

### `jired schema [name]`
Prints JSON Schemas: frontmatter, every `jired/*/1` output, the error shape.
