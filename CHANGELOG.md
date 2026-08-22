# Changelog

Notable user-facing changes. Anything that changes a command's spelling, its exit
code, or what it does without asking is called out as **Breaking**; internal
refactors are not listed.

## Unreleased

A usability pass over the whole CLI: what the flags are called, what happens
before something destructive, what an error says, and what the docs claim.

### Breaking

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

### Added

- **`jired log` with no issue reads like `git log`.** It merges every issue's
  history into one newest-first list, each entry tagged with the issue it belongs
  to; naming an issue still narrows to it. The first run fetches the changelogs it
  is missing, and every run after it is offline: freshness is now the issue's own
  `updated` at the time its changelog was cached, so an issue with no history —
  a real answer, not an empty cache — is no longer asked about on every run, and
  one the server has moved on from is refetched exactly once. `jired/log/1` gains
  `scope` and a per-entry `key`, and its top-level `key` is null in workspace mode.

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
- `jired open` with no issue asks which one, on the filtered list `edit` searches.
  It offers every issue the workspace has a snapshot of rather than only the ones
  with a file, because a browser URL needs no file — and leaves out locally
  created `NEW-*` issues, which have no server key to browse. Without a terminal
  to draw the list on it is exit 7 naming what to type, in place of clap's
  "required arguments were not provided".
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
