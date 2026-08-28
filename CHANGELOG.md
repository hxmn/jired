# Changelog

Notable user-facing changes. Anything that changes a command's spelling, its exit
code, or what it does without asking is called out as **Breaking**; internal
refactors are not listed.

Versions follow [semantic versioning](https://semver.org). Before 1.0 a minor
bump is where breaking changes go, so read the **Breaking** section of every
version between the one you have and the one you are moving to.

## [0.2.0] — 2026-08-28

### Added

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
