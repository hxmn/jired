# Usability review and plan

Review date: 2026-08-21, against `main` at 9212430. Method: built the binary and
exercised it in an empty directory, read every command's `--help`, and audited the
source along four axes — error experience, command flow and output, CLI surface
consistency, and docs-versus-code. Line numbers refer to that commit.

> **Status: implemented.** Phases 0–5 all landed on 2026-08-21; the decisions
> below were taken as recommended. See `CHANGELOG.md` for the user-facing list
> and "What shipped" at the end of this document for the mapping back to each
> phase. What remains open is listed under "Still open".

## What is already good

Keep these patterns; several fixes below are "do what X already does".

- `Resolver::require` (`src/config/mod.rs:114`) — names the config key, env var and
  flag, exits 7. The best error in the tree; the template for the others.
- `Projector::read` (`src/project/mod.rs:88`) re-wraps a parse error with the filename
  and forwards the inner hint.
- `init` recovery loop: rejected token becomes a menu, URL validated before any round
  trip, server corrects the cloud/DC guess, cert inspect-and-pin flow, and a "next:"
  block at the end.
- Staging vs sending is always stated (`staged a comment on PROJ-1 … run jired push`).
- `--json` discipline: one versioned document on stdout, logs to stderr, exit-code table
  locked by a test.
- `config list` shows the *source* of every value; `config set` canonicalises enums and
  writes each key's doc as a comment; secrets never echo.
- `pull --strategy` is a proper `ValueEnum` with per-variant docs; `diff --exit-code`
  mirrors git; `resolve` re-runs the merge instead of editing files a second way.
- Ambiguity is never resolved silently (`assign` lists candidates, bare `resolve` stops).

## The weak points, grouped

### A. Things that are broken or lie to the user

| # | Problem | Where |
|---|---|---|
| A1 | `push --dry-run` can **panic**: `&s[..240]` slices a `String` on a byte boundary; any non-ASCII summary trips it | `src/sync/push.rs:118` |
| A2 | `jired status \| head` panics on EPIPE (`println!`, no SIGPIPE handling) | `src/json.rs:58`, `src/main.rs` |
| A3 | Blanket `From<io/rusqlite/serde>` interpolate `{e}` *and* set it as source, so `Display` prints the cause twice | `src/error.rs:215-237`, `:194` |
| A4 | `--insecure` / `--insecure-store` warnings use raw `eprintln!`, breaking the "stderr is one JSON document" contract in `--json` mode | `src/cli/mod.rs:163`, `src/auth/mod.rs:253` |
| A5 | `transition` without `--to` emits a result document on stdout *and* an error on stderr | `src/cli/cmd/transition.rs:38-56` |
| A6 | `--dir <non-workspace>` creates `.jired/session.db` there, then errors "no stored credentials" | `src/cli/context.rs:20`, `src/store/mod.rs:54` |
| A7 | `status --all` parses and does nothing | `src/cli/cmd/status.rs:11`, `src/sync/status.rs:180` |
| A8 | `projects/boards/sprints --refresh` is a no-op (and undocumented on two of three) | `src/cli/cmd/list.rs` |
| A9 | `--profile` / `JIRED_PROFILE` / `workspace.profile` are inert — session store hardcodes `profile = 'default'` | `src/store/session.rs:172` |
| A10 | `--url/--token/--email/--auth/--deployment` are global flags but only `init` reads them; `jired pull --url other` silently uses the stored account | `src/cli/mod.rs:60-78`, `src/api/mod.rs:247` |
| A11 | `JIRED_NO_INPUT` / `ui.no_input` never disable prompting; only the clap flag is consulted | `src/cli/context.rs:38` |
| A12 | `search --pull` and `resolve --field` are advertised in `--help` and always error (`resolve --field X --ours` fails only after filtering) | `src/cli/cmd/search.rs:24`, `src/cli/cmd/resolve.rs:91` |
| A13 | `init` against an unreachable/wrong URL says *"the issue may not exist"* — the 404 hint for issues is applied to `serverInfo` | `src/api/http.rs:321` (observed live) |
| A14 | `jired projects` outside any workspace says *"this workspace has no stored credentials"* | `src/store/session.rs:213` |
| A15 | README/agent docs say exit 5 means "nothing was written", but `pull --strategy merge` writes conflict markers and exits 5 | `README.md:105`, `src/cli/cmd/agent_docs.rs:73,166` |
| A16 | `clean --regroup` returns before looking for orphans — the flag disables the command's own purpose; duplicates `doctor --regroup` | `src/cli/cmd/clean.rs:38-53` |

### B. Help and discoverability

- 15 global flags appear on all 27 `--help` screens and **interleave** with the
  command's own flags (`transition --to` sits between `--json` and `--no-input`).
  No `next_help_heading`, no `after_help`, no `long_about` anywhere.
- `--auth <KIND>`, `--deployment <KIND>`, `config get/set <KEY>`, `schema <NAME>` are
  free strings: no listed values in help, no completion, typos caught late.
  `AuthKind::parse` also silently accepts `cookie`/`oauth2` that no doc mentions.
- Positional `<ISSUE>` has no description on `transition`, `assign`, `attach`; `config`
  positionals undocumented.
- `jired help <cmd>` is disabled (`disable_help_subcommand = true`); no aliases (`st`);
  no `sync`.
- Issue addressing is inconsistent: `fetch/pull --issues A,B` vs `push/diff A B` vs
  single positional elsewhere. `jired pull PROJ-1` errors while `jired push PROJ-1`
  works. **No command accepts a file path** even though issues *are* files and
  `naming::key_from_file_name` already exists.
- `--all` means four different things; on `diff` it is a comparison *mode*, not a scope.
- `search --jql` is a required flag (natural form `jired search "project = X"` fails);
  `--limit` silently clamped to 100; zero results print nothing.
- `config` requires a workspace even for `--global` and `list`, so the README's own
  pre-init recipe (`config set creds.storage user_config --global`) fails.
- No `config edit`/`config path`; `config list` shows source *kind* but not the file.
- `[env: JIRED_URL]` suffixes are hand-typed; `--no-input`/`--insecure` have env vars
  the help never mentions. `JIRED_SECRET_KEY`, `JIRED_NO_KEYRING`, `JIRED_LOG` are in
  no table at all.
- No `value_hint` on path args; completion is static (no config keys, no schema names).

### C. Feedback during operations

- **`push` never confirms.** The only `confirm()` calls in the tree are in `init`.
  `jired push` plans and sends every dirty issue — including comment deletes under
  `--allow-comment-delete` — with no preview and no y/N; `-y` is meaningless for it.
- Push output has no summary line (`3 of 5 pushed, 2 failed`) and never mentions
  `--resume`; per-issue failures keep `e.message` and drop the hint
  (`src/sync/push.rs:398`); `PushReport::exit_code()` collapses every non-stale failure
  to exit 1; a lossy abort mid-loop `return Err`s and throws away the results of
  issues already pushed (`:318-334`).
- `comment/transition/assign --push` failures print only "the push failed" — the report
  is never rendered (`src/cli/cmd/comment.rs:47`).
- **No progress indication**: fetch pagination is `tracing::debug!` only; pull/push loops
  are silent until done. A 500-issue first pull looks hung. The spinner exists
  (`prompt.rs:316`) but `Ui` is built only in `init`.
- No colour in `status`/`diff`; fixed-width columns (`{:<14}`) misalign on long keys.
- `status`/`diff` don't say what to do next for the common states (git's
  `(use "jired push" to send)`); `pull` never reports how many issues were fetched.
- `init`: initial pull failure turns a completed setup into an error and skips the
  "next:" block; `CLAUDE.md`/`AGENTS.md` are written with no opt-out; `.jired/` is
  created before auth succeeds; Enter at the project picker means "all projects" after
  which plain `pull` fails with "no project or query to fetch".

### D. Errors

- Human renderer (`src/json.rs:76-87`): no colour, no exit code, never prints `details`
  (so the lock error's pid is invisible), no "re-run with -v" line.
- `From<rusqlite::Error>` maps *everything* to CorruptState/exit 9 — busy, read-only,
  disk-full included — and never says which DB (`state.db` is disposable, `session.db`
  is not). No `rm .jired/state.db && jired fetch` hint anywhere in the store layer.
- Bare `?` on `std::fs` leaks pathless "No such file" (`project/mod.rs:101,127`,
  `config/file.rs:150`, `attach.rs:63`…).
- HTTP errors are REST dumps (`PUT /rest/api/3/issue/10432/assignee failed with 400`)
  — path noise, numeric id instead of key. Network errors interpolate the whole reqwest
  chain; only TLS gets a hint; `api/tls.rs` errors have none.
- Frontmatter parse errors are Generic/exit 1, name no file or line; hand-edited-file
  mistakes should be Usage.
- `config set` with a bad value doesn't name the key; no did-you-mean though
  `keys::KEYS` is a static table.
- `not_yet()` leaks "planned for milestone M3.7" and exits 1 not 2.
- Warnings go through three channels (`Out::warn`, `tracing::warn!`, `eprintln!`); a
  failed attachment download is a `tracing::warn!` invisible at default verbosity.

### E. Safety

- `pull --force` and `resolve --theirs` overwrite local edits without listing what is
  lost or confirming; `clean` (least destructive) is the only one that asks.
- `doctor` repairs by default (renames files, rewrites config, moves credentials).
- `Ui::confirm` treats unrecognised input as the default instead of re-asking.

### F. Editing flows

- No `$EDITOR` anywhere: `comment` without `-m` on a TTY errors; `create` refuses to run
  interactively without `--summary` and has no `--push` (every other staging command
  has one).
- `--dry-run` missing from `pull`, `create`, `resolve`, `attachments pull`, and the
  `--push` shortcuts (`push_one` hardcodes `PushArgs::default()`). `push --dry-run`
  has a hidden `preview` alias no other command has.
- `transition` and `assign` hit the network even when only staging, so neither works
  offline despite `edit.rs:1-6`.

### G. Docs and onboarding

- **No install section, no LICENSE files** (Cargo.toml says MIT OR Apache-2.0), MSRV
  1.89 unstated. Only `make install` works today; no release job.
- README names 9 of 26 commands. Grouping folders, board-column moves, templates,
  completions, `fetch --slim` are undocumented for humans.
- File-format rules (RO vs editable fields, `### [new]` comments, checkbox semantics,
  `links:` shape, `lossy: true`, `--allow-lossy`, conflict markers, `custom:`) live only
  in the generated CLAUDE.md or nowhere.
- No troubleshooting section; the good hints in code (401→`init --force`,
  `--ca-cert`, proxies, `JIRED_SECRET_KEY` for CI, exit 8/9 recovery) are never in prose.
- Agent docs: no conflict-recovery section; claim grouping folders must never be edited
  while the code treats a board move as a staged transition; editable-field list is a
  hand-copied string with no drift test. `commands.md` documents `--fields`, TUI pickers
  and a `list` group that don't exist.

## Plan

Ordered by user impact per hour. Each phase is independently shippable; file refs are
the entry points. "Accept" lines are what a reviewer checks.

### Phase 0 — Stop the bleeding (≈1 day)

1. `compact()` → `chars().take(240)` (`sync/push.rs:118`); add a non-ASCII dry-run test.
2. Handle EPIPE: lock stdout in `Out::say`/`emit`, map `BrokenPipe` to exit 0
   (`json.rs:58`).
3. Fix double-printed causes: blanket `From`s keep a short message, `Display` appends
   source (`error.rs:194-237`).
4. Route the two `eprintln!` warnings through `Out::warn`; in `--json` carry them in the
   result document (`cli/mod.rs:163`, `auth/mod.rs:253`).
5. `transition` without `--to`: emit only on `--list`, otherwise error only
   (`transition.rs:38`).
6. `Context::open` with `--dir`: check `is_initialized()`, reuse the "no jired
   workspace" usage error; delete dead `require_workspace` in `clean.rs`
   (`context.rs:20`).
7. `hide = true` on `search --pull` and `resolve --field`; `not_yet` → Usage/exit 2
   with a user-facing hint (no milestone ids) (`cli/mod.rs:230`).
8. Remove `--refresh` from `projects/boards/sprints` (or implement a cache later)
   (`list.rs`).
9. Implement `status --all` or delete the flag (`sync/status.rs:180`).
10. `init` serverInfo 404 → "no Jira found at URL; check server.url" (`http.rs:321`:
    only attach the issue hint when the path is an issue path).
11. Fix exit-5 wording in README and `agent_docs.rs` (`--strategy merge` writes markers).
12. `clean --regroup` → remove, point at `doctor --regroup`; fix the `about`.

Accept: `cargo test` green; `jired push --dry-run` on a fixture with "é" in the summary;
`jired status | head -1` exits 0; `jired --json --insecure status` stderr parses.

### Phase 1 — Help and CLI surface (≈2 days)

1. `#[command(next_help_heading = "Global options")]` on `GlobalArgs` and
   `next_display_order` so command flags list first (`cli/mod.rs:30`).
2. Move `--email/--auth/--deployment/--insecure-store` (and `--url/--token`) to
   `InitArgs` (+ `doctor` where it reads them). If they stay global, `build_client`
   must honour them or error when they disagree with the stored account.
   **Decision needed** — recommended: move them.
3. Decide `--profile`: implement (`SessionStore::open(profile)`) or remove flag, env,
   key and docs until it exists. **Decision needed** — recommended: remove for now.
4. Wire `ui.no_input` into `interactive` (two-phase `Context` build) and the `--insecure`
   warning to the resolved value, not the flag (`context.rs:38`).
5. `ValueEnum` for `AuthKind`/`Deployment`; `PossibleValuesParser` over `keys::KEYS` for
   `config <KEY>` and over `SCHEMAS` for `schema <NAME>`. Document `cookie`/`oauth2` or
   stop accepting them.
6. Doc comments on every positional; `ValueHint::FilePath/DirPath` on `--dir`,
   `--config`, `--ca-cert`, `attach <FILES>`.
7. Generate the `[env: …] [config: …]` help suffix from `keys::lookup` instead of typing
   it; add `JIRED_SECRET_KEY`, `JIRED_NO_KEYRING`, `JIRED_LOG` as env-only rows so
   `config list --all` and agent docs list them.
8. Drop `disable_help_subcommand`; `visible_alias = "st"` on status; add
   `after_help` examples on the root and on `push`/`pull`/`init` (lift from README).
9. Unify issue addressing: positional `[ISSUES]...` on `fetch`/`pull` (keep `--issues`
   as hidden alias), `value_delimiter = ','` everywhere, and a shared
   `resolve_issue_ref()` that accepts keys (any case) **or file paths** via
   `naming::key_from_file_name`.
10. `search`: positional JQL, print `showing N of TOTAL` / `no issues match`, warn when
    clamped.
11. `config`: use `open_uninitialized` so `--global`/`list`/`get` work before `init`; add
    `config path` and `config edit`; show the file path in `config list`; add a
    `kind: Bool|Int|Enum|Str` to `KeyDef` so `set` validates every key.
12. Rename `diff --all` → `diff --against working|base|remote` (ValueEnum); keep `--all`
    as scope only.
13. `ArgGroup(required = true)` for `assign --me|--user|--none`, `resolve
    --ours|--theirs|--done`; `create --summary` required unless interactive (Phase 4).

Accept: `jired transition --help` shows `--to`/`--list` first under "Options" and
globals in their own section; `jired pull PROJ-1`, `jired comment ./PROJ-0142*.md -m x`,
`jired config set --global links.mode stub` in an empty dir, `jired help push` all work.

### Phase 2 — Feedback during pull/push (≈2–3 days)

1. **Push confirmation**: when interactive and not `-y`, print the plan (dry-run lines
   without raw JSON) and ask `send N operations across M issues? [y/N]`; `--no-input`
   without `-y` → exit 7 (`cli/cmd/push.rs:35`).
2. Push report: counted summary line; `--resume` hint on failure; per-issue `hint`
   carried from the error; `exit_code()` returns the shared kind when all failures
   agree (`sync/push.rs:50,61,398`).
3. Lossy/oversize/parse failures inside the push loop become failed `IssueResult`s and
   `continue`; the loop never discards completed results (`sync/push.rs:295-336`).
4. Render the report on `--push` failures in comment/transition/assign.
5. Progress: build `Ui` in fetch/pull/push; spinner + `page N (K issues)` from the fetch
   callback, counted bar over the write/execute loops; stderr only, off in `--json`/`-q`.
6. Colour module (move `paint` out of `Ui`; honour `NO_COLOR`, `TERM=dumb`, non-TTY,
   add `--color auto|always|never`); apply to diff `+/-`, conflict lines, issue keys,
   `error:`/`warning:`/`hint:` prefixes.
7. Git-style trailers in `status` (`(use "jired push" to send)`, `(use "jired pull" to
   update 3 behind)`) and a clean-state next step; `no differences` → include what was
   compared; `pull` reports `fetched N, wrote M, unchanged K`; compute column widths.
8. `init` tail: catch the initial pull error, warn, still print "next:"; add
   `--no-agent-docs`; create `.jired/` after auth succeeds; project picker default is
   the first project, "all" is an explicit choice.

Accept: interactive `jired push` shows the plan and waits; `jired push -y` and
`--no-input -y` don't; partial failure prints `2 of 5 failed — run jired push --resume`
and exits with the failures' kind; a 300-issue fixture pull shows progress.

### Phase 3 — Error quality (≈2 days)

1. Human renderer: coloured `error:`/`hint:`, `details` printed under `-v`, trailing
   `(exit 5 · conflict; re-run with -v for details)` (`json.rs:76`).
2. `io_ctx(path)` helper; replace every bare `?` on `std::fs` (list in review §D).
3. `From<rusqlite::Error>`: map Busy/Locked → Locked with pid hint, CantOpen/ReadOnly →
   Generic with path, true corruption → CorruptState. Store-layer errors name the DB and
   carry `rm .jired/state.db && jired fetch` (state) or `jired init --force` (session)
   (`store/mod.rs:43,59`, `store/state.rs:946`).
4. HTTP: pass an operation label + issue key into `request()`; message becomes
   `PROJ-7: cannot assign — <server text> (HTTP 400)`, REST path under `-v`. 404 on a
   user-supplied key → Usage. Network: innermost cause only, hints for DNS/connect/
   timeout (`check server.url … jired doctor`), TLS hint copied into `api/tls.rs`.
5. Lock error: put pid/since in the message; if the pid is dead, say so and offer to
   clear (or have `doctor` do it) (`workspace.rs:181`).
6. `IssueDoc::parse(label)` with YAML line/column; Usage kind for hand-edited files;
   hint on "no `key`" (`doc/mod.rs:233`, `doc/frontmatter.rs:111`).
7. `config set`: prefix message with the key, did-you-mean over `keys::KEYS` (also for
   unknown subcommand-free typos like `jired config get nope`).
8. One warning channel: user-actionable warnings go through `Out::warn` (and into the
   JSON `warnings` array); `tracing::warn!` reserved for diagnostics. Fix attachment
   download failure and the comment-delete notice.
9. Hints for the hintless: `unsupported_server`, crypt errors (`init --force`),
   `no user config directory` (`XDG_CONFIG_HOME`), `cannot find an unused temporary key`.
10. Gate `JIRED_FIXTURES` behind `cfg(debug_assertions)` or make its errors Usage with
    the fixture dir named.

Accept: a test per error family asserting message names the thing + has a hint + right
exit code; no message contains `/rest/api/` at default verbosity.

### Phase 4 — Safety and editing flows (≈2 days)

1. Confirm before discarding: `pull --force`, `pull --strategy theirs`, `resolve
   --theirs` list the issues whose local changes will be lost and ask; `-y` bypasses.
2. `doctor`: report by default, `--fix` to repair (or confirm the repair set). Keep
   `--dry-run` as alias of the default for compatibility.
3. `Ui::confirm` re-asks on unrecognised input; honour global `-y`.
4. `$VISUAL`/`$EDITOR` (git-style temp file, empty = abort) for `comment` without `-m`,
   `create` without `--description`, and `config edit`.
5. `create`: prompt for summary/type/parent when interactive; add `--push`.
6. Thread `--dry-run` through `push_one` so `comment/transition/assign --push --dry-run`
   work; add `--dry-run` to `pull` (render the plan without writing) and `resolve`.
7. `transition`/`assign` offline: stage from `transition_cache`/known users when the
   network is down, warn that validation is deferred to push.
8. Drop the hidden `preview` alias or make it visible everywhere.

### Phase 5 — Docs and onboarding (≈1–2 days, can run in parallel with any phase)

1. README "Install": `cargo install --git … --locked`, Rust ≥ 1.89, Linux keychain needs
   a Secret Service provider or `JIRED_SECRET_KEY`. Add `LICENSE-MIT`/`LICENSE-APACHE`.
2. README "The file format": RO vs editable table (rendered from
   `frontmatter::EDITABLE` — same source as agent docs), `### [new]` comments,
   `--allow-comment-delete`, checkbox semantics, `links:` shape, `lossy: true` +
   `--allow-lossy`, conflict markers + `jired resolve`, `custom:` rules, section order.
3. README "When something goes wrong": exit code → cause → command, covering 3/4/5/6/8/
   9/10, `--ca-cert`/`--insecure`/pinning, proxies (`HTTPS_PROXY`), CI recipe with
   `JIRED_SECRET_KEY`, cloud/DC override.
4. README command overview for the 17 unmentioned commands, grouping folders,
   board-column moves, templates, completions (where to put the script), `fetch --slim`.
5. `agent_docs.rs`: add the conflict-recovery section; fix the grouping-folder
   sentence; render field lists from `frontmatter::*` with a drift test; mention
   templates and `NEW-<hex>` keys.
6. `docs/plan/commands.md`: align with code (`--field`, no TUI, `config unset`, flat
   `projects/boards/sprints`, `assign` exit 2) or mark divergences as planned.
7. `CHANGELOG.md` and a release job (cargo-dist) — M5 items, listed here because
   "how do I get it" is the first usability question.

### Decisions taken before Phase 1

| Decision | Taken |
|---|---|
| Global auth flags (`--url/--token/--email/--auth/--deployment`) | Moved to `init` (+`doctor`) via `ConnectionArgs`; `--ca-cert/--insecure` stayed global |
| `--profile` | Removed — flag, env var and config key — until multi-profile exists |
| `clean --regroup` | Removed; `doctor --regroup` owns it |
| `diff --all` | Renamed to `--against base\|remote`; `--all` hidden for compatibility |
| `projects/boards/sprints` | Kept flat; only the dead `--refresh` went, and `boards --project` became `--for-project` so it stops shadowing the global |
| `doctor` default | **Not** inverted to `--fix`. `doctor` surveys first and asks before repairing when interactive; `-y` and non-interactive runs proceed. Inverting the default would have broken the documented `jired doctor` recipe and every scripted caller for no extra safety — the confirmation already stops the "I only wanted to look" case |
| `resolve` requiring one of `--ours/--theirs/--done` | **Not** made a required `ArgGroup`. Bare `jired resolve` listing the conflicts and stopping is more useful than a parse error, and it already exits 7 naming the three ways out. `assign` did get the group, since it has no such listing |

## What shipped

- **Phase 0** — `compact()` char-boundary panic, EPIPE handling, doubled error
  causes, `eprintln!` warnings routed through `Out`, `transition`'s two
  documents, the `--dir` workspace guard, `hide`+usage-exit for the two stubs,
  dead `--refresh`, `status --all`, the init 404 hint, the exit-5 wording, and
  `clean --regroup`.
- **Phase 1** — help headings and `after_help` examples, `ConnectionArgs`,
  `--profile` removal, `ui.no_input` wired, `ValueEnum`/`PossibleValuesParser`
  for auth/deployment/config keys/schema names, positional issues everywhere,
  `resolve_issue_ref` accepting file paths, `search` positional JQL, `config`
  before `init` plus `path`/`edit`/per-key validation/source files,
  `diff --against`, and `jired help` + `st`.
- **Phase 2** — push confirmation with a real plan, counted summary and
  `--resume` hint, per-issue hints, kind-aware exit codes, per-issue failures
  that no longer discard completed work, progress bars, a `style` module with
  `--color`, git-style trailers, and the init tail fixes.
- **Phase 3** — the error renderer (colour, `details` under `-v`, exit code),
  `io_ctx`, SQLite kind mapping, HTTP/network messages without REST paths,
  the lock message, frontmatter line/column, did-you-mean, one warning channel,
  and hints for the hintless.
- **Phase 4** — confirmations before `pull --force`/`--strategy theirs`,
  `resolve --theirs` and `doctor`, `Ui::confirm` re-asking and honouring `-y`
  (except the certificate-trust question), `$EDITOR`, interactive `create` with
  `--push`, and `--dry-run` threaded through the `--push` shortcuts and `pull`.
- **Phase 5** — README install/file-format/troubleshooting/commands, LICENSE
  files, CHANGELOG, agent-doc conflict recovery, field lists generated from
  `frontmatter::*` with a drift test, and `commands.md` aligned with the code.

Published JSON schemas were updated for every new field (`push` per-issue
`hint`, `pull` `dry_run`/`warnings`, `status` `unchanged`, `doctor`
`checks[].regrouped`, `config-list` `file`/`env_only`, `create` `new_key`).

## Still open

- No dynamic completion of issue keys — it would have to read `state.db` from
  the completion hook. Config keys, schema names and every enum do complete.
- `search --pull` and `resolve --field` are hidden rather than built; both are
  M3 items.
- `--profile` is gone rather than implemented; multi-profile needs
  `SessionStore::open` to take one.
- The M4 TUI is untouched. Every item above is a prerequisite for its
  `--no-input` degradation table and stays useful without it.
- `jired sync` (pull then push) was considered and left out: the two halves fail
  differently and a combined exit code would hide which one did.

## Out of scope here

The M4 TUI (pickers, conflict resolver, pull/push preview screens) is the planned
answer to several items above (project picker, conflict resolution, confirmation
previews). This plan deliberately fixes the plain-terminal path first: every item is a
prerequisite for the TUI's `--no-input` degradation table and stays useful without it.
