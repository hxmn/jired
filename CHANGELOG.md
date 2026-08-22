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

- **`jired report time`** — who booked how much time, totalled off the
  `## Worklogs` sections already in the working copy, so it answers offline.
  `--days N` sets the window (whole days, today included, default 7) and
  `--show-issues` opens each person into the issues their time went to. In a
  terminal it is a screen; a pipe, `--json` or `--no-input` gets the same figures
  as a table and as a `jired/report-time/1` document.

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
