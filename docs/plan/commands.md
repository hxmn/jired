# Command specifications

## Global contract

**Flags on every command:** `--json`, `--no-input`, `--yes`, `--quiet`, `-v/-vv`,
`--dir PATH`, `--config PATH`, `--url`, `--token`, `--email`, `--project`,
`--insecure`, `--ca-cert PATH`, `--profile NAME`.

**Env vars:** `JIRED_URL`, `JIRED_EMAIL`, `JIRED_TOKEN`, `JIRED_PROJECT`,
`JIRED_PROFILE`, `JIRED_NO_INPUT`, `JIRED_SECRET_KEY`, `JIRED_CA_CERT`,
`JIRED_INSECURE`, plus standard `HTTPS_PROXY`/`NO_PROXY`.

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
Prints change summary (added/modified/deleted issues, changed fields).
JSON: `{"schema":"jired/pull/1","updated":[…],"created":[…],"deleted":[…],"conflicts":[{"key":"PROJ-1","fields":["description"]}]}`.
Agent example: `jired pull --no-input --json || test $? -eq 5`

### `jired push`
`jired push [ISSUE…] [--dry-run] [--force] [--allow-lossy] [--allow-comment-delete] [--resume]`
Plans ops (D23 order), `--dry-run` prints exact API calls + diffs (human and JSON).
Per-issue concurrency check (D8): stale → that issue fails (exit-5 contribution,
"pull first"), others proceed. Partial failure → exit 1 with per-issue results; base
advances only for confirmed issues (durable `pending_op` journal). Lossy-region edit
without `--allow-lossy` → exit 6.
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

### `jired create [--type Story] [--template T] [--summary S]`
Writes `NEW-<6hex>. Summary.md` (D14) from template; real key assigned on push
(file renamed; JSON reports the mapping).

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
missing config defaults, readable credentials, filenames that predate a naming
convention (this is the migration path for zero-padded issue numbers), database rows
pointing at the wrong file, the `.gitignore` block, and the generated agent docs.
Renaming files re-links the grouping folders automatically; `--regroup` rebuilds them
even when nothing was renamed. Idempotent. Exit 1 when something needs a human.
JSON: `{"schema":"jired/doctor/1","checks":[{"name":"filenames","status":"fixed","detail":"…","changes":[{"from":"PROJ-9. x.md","to":"PROJ-0009. x.md"}]}],"fixed":1,"pending":0,"problems":0}`.

### `jired config get|set|list [--global]`
TOML round-trip preserving comments (`toml_edit`); refuses to store secrets
(points at `init`).

### `jired whoami` · `jired projects|boards|sprints list [--refresh]`
Cached + `--refresh`; table / JSON.

### `jired completion <shell>`
Via `clap_complete`.

### `jired agent-docs`
Regenerates `CLAUDE.md`/`AGENTS.md` (overwrites only between jired sentinel markers).

### `jired schema [name]`
Prints JSON Schemas: frontmatter, every `jired/*/1` output, the error shape.
