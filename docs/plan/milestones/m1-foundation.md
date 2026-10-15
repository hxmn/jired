# M1 — Foundation: init, fetch, read-only pull (size: L)

**Goal:** `jired init && jired pull` produces a correct read-only checkout of a
1k-issue project on both Cloud and DC; re-pull with no changes is a no-op.
Local edits are *not yet* merged or pushed (that is M2) — M1 pull requires a
clean working copy.

## Status

Delivered. Cloud and Data Center are both exercised end to end: the same
workflow suite runs against each, covering wiki markup, `[~user]` mentions,
username-based identities, stringified sprint values and `{panel}` macros.

Still open: the fixtures are hand-built rather than recorded from real
instances. `jired-record` exists and is ready to use, but running it needs
credentials for a Cloud sandbox and a DC container, so no code path that only a
real server can trigger (pagination past one page, a live 429, an unusual
custom-field schema) has been observed yet.

## Acceptance criteria

- [x] `init --no-input` with flags/env authenticates against Cloud (API token) and DC (PAT) and persists the workspace
- [ ] `fetch` is incremental, paginated, rate-limit-aware, and resumable
- [x] `pull` writes correct Markdown files (frontmatter + description + raw blocks) for both deployments
- [x] Second `pull` with no remote changes touches zero files
- [ ] All commands honor exit codes 0–4, 7–10 and `--json` error shape
- [ ] CI green on Linux/macOS/Windows

## Tasks

### M1.1 Project scaffolding
- [x] `cargo init` single binary crate; module layout (`cli/`, `config/`, `auth/`, `api/`, `store/`, `convert/`, `sync/`, `project/`)
- [x] Dependencies pinned: clap, tokio, reqwest(rustls-ring), rusqlite(bundled), serde/serde_json/serde-yaml-ng/toml_edit, comrak, keyring, similar, tracing, secrecy, thiserror, chrono, sha2 (std file locking replaced `fs4`; rustls uses the ring provider to avoid an aws-lc C build)
- [x] rustfmt.toml, clippy config, `cargo deny` license/advisory check
- [x] GitHub Actions: {ubuntu, macos, windows} × stable, fmt + clippy -D warnings + test
- **Done when:** empty `jired --help` builds and CI is green on all three OSes.

### M1.2 Error & exit-code framework
- [x] `JiredError` enum with `kind` + mapped exit code per table (D15)
- [x] stderr JSON error emitter (`jired/error/1`) active in `--json` mode
- [x] Human error rendering (message + hint) in non-JSON mode
- [x] Unit tests: each error kind → correct exit code and JSON shape
- **Done when:** a forced auth failure exits 3 with valid `jired/error/1` on stderr.

### M1.3 Config resolver
- [x] Precedence chain: flag → `JIRED_*` env → `.jired/config.toml` → XDG global → prompt hook
- [x] `--no-input` + non-TTY autodetection disables prompt rung; missing value → exit 7 naming the value
- [x] Documented env-var list generated from one source of truth (used later by init-agents)
- [x] Unit test matrix covering all 5×levels precedence combinations
- **Done when:** the precedence matrix test passes and exit-7 errors name the missing key.

### M1.4 Workspace layout, lock, gitignore
- [x] `.jired/` creation (0700) with `config.toml`, `logs/`, `lock`
- [x] Advisory file lock (fs4) + PID/timestamp; stale-lock breaking; live lock → exit 8
- [x] Idempotent `.gitignore` management between sentinel comments (`.jired/`)
- [x] Tests: concurrent invocation blocked; dead-PID lock auto-broken
- **Done when:** two parallel `jired fetch` runs — one proceeds, one exits 8.

### M1.5 session.db & secret store
- [x] `session.db` schema + `user_version` migration runner (shared with state.db)
- [x] File-mode enforcement (0600, warn+fix)
- [x] Keyring backend (`secret_ref`); `secrecy::SecretString` everywhere
- [x] Encrypted-blob fallback: scrypt(`JIRED_SECRET_KEY`) + XChaCha20-Poly1305; refusal without key unless `--insecure-store` (D11)
- [x] `creds.storage` picks the backend: `keychain`, `local_db` (`.jired/session.db`), or `user_config` (`credentials.toml` in the user config directory, shared by every workspace and keyed by host + identity)
- [x] An explicit choice is honoured or fails — never silently downgraded — and `jired doctor` moves the credential when the setting changes
- [x] `init` reuses an account-scoped credential this machine already holds instead of asking for it again (not under `--force`)
- [x] Tests: round-trip via mock keyring; blob encrypt/decrypt; refusal path; relocation between all three stores; a second workspace reusing a shared credential
- **Done when:** credentials survive restart via keyring and via blob fallback, and never appear in logs (grep test).

### M1.6 Jira API abstraction
- [x] `JiraApi` trait: myself, server_info, projects, search (paged), issue, transitions, comments, attachments (download later)
- [x] Shared HTTP core: TLS (`--ca-cert`, `--insecure`), proxy env vars, custom base paths
- [x] Rate limiter (token bucket, D26) + 429/`Retry-After` + jittered backoff (max 5)
- [x] Cloud (v3, ADF) and Data Center (v2, wiki) clients; capability probe via `serverInfo`, floor check (D20 → exit 10). Implemented as one `Rest` core parameterized by deployment, since every difference is data rather than control flow
- [x] Normalization layer: v2/v3 payload differences, TZ→UTC (D22), custom-field discovery into `field_meta`
- [x] Fixture player implementing `JiraApi` from recorded JSON (see M1.12)
- **Done when:** `whoami` works live against Cloud and DC, and all trait methods run against fixtures in tests.

### M1.7 state.db
- [x] Full schema from [architecture.md](../architecture.md) + WAL mode
- [x] Migration scripts + `user_version` gating (refuse newer, migrate older)
- [x] Store API layer (typed accessors, no SQL outside `store/`)
- [x] Test: delete `state.db` → refetch reconstructs identical snapshot
- **Done when:** schema tests pass and `rm .jired/state.db && jired fetch` recovers cleanly.

### M1.8 `jired fetch`
- [x] Scopes: `--jql`, `--sprint`, `--board`, `--issues`, `--updated-since`, `--full`, `--slim`
- [ ] Incremental JQL (`updated >= last_fetch − 5min`), pagination (100/page, 4-way parallel)
- [x] Upsert remote snapshot columns; mark `deleted_remote` for issues gone from scope
- [ ] Partial-progress persistence → resumable after network failure (exit 4)
- [x] `--json` output `jired/fetch/1` with counts
- **Done when:** fixture test fetches 1k issues across pages with an injected 429 mid-stream and completes.

### M1.9 JiredAst + ADF → Markdown (one-way)
- [x] `JiredAst` node model (all bidirectional node types + `Raw(id)`)
- [x] ADF parser → JiredAst; unknown/lossy nodes captured into `raw_block` with approx rendering
- [x] Markdown renderer incl. `<!--jired:raw:ID-->` markers, mentions `@[Name](id)`, issue-key links
- [x] ~~Heading demotion (+2)~~ — replaced: issue-body headings keep their levels, and the file reader tells them from the `##` skeleton by section name (0.18.1)
- [ ] Snapshot tests (insta) over an ADF corpus: panels, media, mentions, tables, task lists, code blocks, expands
- **Done when:** every corpus document renders deterministically and every lossy node has a raw_block row.

### M1.10 wiki markup → Markdown (one-way)
- [x] Wiki parser → JiredAst (headings, lists, tables, code, quotes, links, mentions `[~user]`, macros→Raw)
- [ ] Shares renderer with M1.9; snapshot corpus for DC
- **Done when:** DC corpus renders with macros preserved as raw blocks.

### M1.11 Read-only pull (clean checkout) + projector v1
- [x] Filename sanitizer (strip `/\:*?"<>|`, collapse ws, trim trailing dots, 120-char cap) + unit tests incl. Windows reserved names
- [x] Frontmatter serialization (exact schema, stable key order)
- [x] File writer: full body assembly (Description/Checklist/Children/Comments/Attachments sections)
- [x] Base advancement (`base_* := remote_*`, `base_md_hash`)
- [x] Dirty-file guard: pull refuses to overwrite files whose hash ≠ `base_md_hash` (real merge is M2)
- [x] Remote rename handling by KEY prefix (file + attachment folder)
- [x] No-op guarantee: unchanged issue → file untouched (mtime preserved)
- **Done when:** acceptance-criteria pulls pass on Cloud+DC fixtures; double-pull touches zero files.

### M1.12 Fixture recording harness
- [x] `jired-record` dev-binary: drives `JiraApi` against a live server and writes scrubbed fixtures
- [x] Secret scrubbing (identities pseudonymised, host/token literals redacted, signed URLs dropped) with a leak test that is itself verified to fail on an injected leak
- [x] Hand-built baseline sets for Cloud (ADF) and Data Center 9.x (wiki markup), both exercised end to end
- [ ] Baselines *recorded from real instances* (needs credentials for a Cloud sandbox and a DC container)
- [ ] Pagination and 429 fixtures
- **Done when:** the M1 test suite runs entirely from fixtures with no network.

### M1.13 `jired init`, `whoami`, `projects list`
- [x] `init` flags-only flow: auth → verify → `--project` → persist → optional pull (`--no-pull`)
- [x] Line-based interactive wizard: URL → deployment and credential inferred → token page linked → project picker with filtering; a failed sign-in re-asks instead of aborting
- [x] Trust-on-first-use for an unknown TLS certificate: shown in full, pinned to `.jired/trusted-ca.pem` only if accepted, never offered when pinning could not help
- [x] Writes every setting into `.jired/config.toml` with its documentation, so only URL, email, token and project are ever prompted for anywhere in the CLI
- [x] `jired doctor` repairs layout, config, filenames, file sections, database paths, gitignore and agent docs, and is the migration path when a convention changes
- [x] Generates `CLAUDE.md`/`AGENTS.md` stubs (full content in M5)
- [x] `whoami`, `projects list [--refresh]` with `--json`
- **Done when:** `init --no-input --json` on a fresh dir + `pull` yields the acceptance checkout.
