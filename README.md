# jired

Work with Jira issues as Markdown files, offline-first.

`jired` fetches issues into the current directory as `.md` files and gives you a
git-like cycle — `fetch`, `pull`, `diff`, `push` — to sync them back. All reads
and edits work without a network; only `fetch`, `pull`, and `push` need one.

Supports **Jira Cloud** (REST v3, ADF) and **Jira Data Center** (REST v2, wiki
markup) behind one interface.

## Install

Building from source is the only route today: there is no published crate and no
release job, so there are no prebuilt binaries or package-manager recipes.

```sh
cargo install --git https://github.com/hxmn/jired --locked --bin jired
```

Rust 1.89 or newer (`File::try_lock`, which the workspace lock uses). `--locked`
builds against the versions the tree was tested with; drop it and you are on your
own with a fresh resolver.

From a clone, `make install` does the same thing and then rewrites the bash and
zsh completions, so a new command or flag starts completing with the upgrade
rather than a step later. See [Shell completions](#shell-completions).

On Linux the default credential store is the OS keychain, which means a running
Secret Service provider — GNOME Keyring, KWallet, `keepassxc` with Secret Service
enabled. Headless machines and containers usually have none; set `JIRED_SECRET_KEY`
and jired encrypts the token into a file instead. See
[When something goes wrong](#when-something-goes-wrong) for the CI recipe.

## Quickstart

```sh
mkdir my-sprint && cd my-sprint

jired init               # asks for the URL, who you are, and which project

jired status            # what's dirty, behind, or conflicted
jired pull              # write issue files, and say what changed
$EDITOR "PROJ-0142. Fix login flow.md"
jired create            # fill in a new issue on a form, or pass --summary
jired edit              # or search for it and change it in a form
jired diff              # confirm only the intended changes
jired push --dry-run    # exact API calls, nothing sent
jired push              # apply
```

## For agents

`jired init` writes `CLAUDE.md` and `AGENTS.md` into the workspace documenting
the file format, the frontmatter schema, editability rules, and the exit-code
table. Every command takes `--json` and `--no-input`. In JSON mode both streams
are machine-readable: stdout carries one versioned result document, stderr
carries the structured error and nothing else. `--no-input` never prompts and
exits 7 naming any missing value.

```sh
jired pull --json --no-input || test $? -eq 5   # 5 means conflicts; the default strategy wrote nothing
jired schema pull                               # JSON Schema for that output
```

A full document is a complete record, which is the wrong size when you wanted
three values. Two flags cut it down, both opt-in so the default keeps the shape
its schema promises:

```sh
jired status --json --fields dirty,new_issues       # only these paths
jired push --json --fields summary,results.key,results.ok
jired push --json --json-compact                    # drop nulls and empty lists
```

`--fields` takes dotted paths, repeatable or comma-separated. A path walks
objects and *maps over* arrays, so `results.ok` keeps the `ok` of every result
rather than needing an index. `schema` always survives; a projected or compacted
document does not validate against its schema, which is the point of asking.

`jired push --json` leads with `summary` — `{"ok": 5, "failed": 0, "stale":
["PROJ-194"]}` — so the outcome is one read rather than a walk over every
operation of every issue.

### Permissions

`jired init` also writes an allow-list for the commands that only read: they send
nothing to Jira and write nothing into the working copy, so an agent running one
has nothing to ask about. Some of them refresh the read-only snapshot under
`.jired/`, which no file you edit depends on.

| Agent | File |
|---|---|
| Claude Code | `.claude/settings.json` — `permissions.allow` entries, merged in; anything else in the file is left alone |
| Codex | `.codex/rules/default.rules` — execpolicy `prefix_rule`s between `# BEGIN jired` and `# END jired`; Codex applies them once the project's `.codex/` layer is trusted |

Granted: `status` (and `st`), `diff`, `log`, `report`, `search`, `fetch`,
`whoami`, `projects`, `boards`, `sprints`, `list`, `show`, `fields`, `schema`,
`completion`, `version`,
`attachments list`, `config get|list|path`, `resolve --list`, `init-agents --print`, and
`pull`, `push`, `reset`, `clean` and `doctor` under `--dry-run`. Everything that
writes is left out on purpose.

Both systems match the start of the command line, so the flags go after the
subcommand — `jired status --json` is covered, `jired --json status` is not.
`jired init-agents` rewrites both files (`--no-docs` or `--no-permissions` for
one half only), and `jired doctor` refreshes them when they fall behind the
binary.

The generated block in `CLAUDE.md` and `AGENTS.md` opens with the version that
wrote it — `<!-- BEGIN jired v0.7.0 -->` — because a stale agent doc is worse
than none: it reads as authoritative, and it is what an agent plans against
before it runs anything. `jired status` names any file another version wrote in
`stale_agent_docs` and says so on the way past; `jired init-agents` or `jired
doctor` puts it right. Whatever you write outside the markers is kept.

Timestamps in JSON documents, in issue files and on the wire are UTC, so they
mean the same thing on every machine. Only the text jired prints to a terminal
follows the reader's timezone — see [Times](#times).

## What a file looks like

````markdown
---
key: PROJ-142
id: '10023'
type: Story
status: In Progress
summary: Fix login flow
labels:
- auth
- backend
assignee: '@[Ilya A](5b10a2ilya)'
story_points: 5
jired:
  base_updated: 2026-08-16T12:03:00Z
  content_hash: sha256:3eed44…
  lossy: true
---

# Fix login flow

## Description

Login fails when SSO is enabled. See [PROJ-99](…) for background.

<!--jired:raw:r0084fc-->
> ⚠️ **[jired: Jira panel (warning) — read-only]**
> Do not deploy before the cert rotation.
<!--/jired:raw:r0084fc-->

## Checklist

- [ ] `IN PROGRESS` Reproduce on staging
- [x] Confirm the SSO settings

## Children

- [ ] PROJ-143 Add integration test
- [x] PROJ-144 Update runbook

## Comments

### [c-10812] Jane D — 2026-08-15T10:32:00Z

Repro confirmed on staging.

## Attachments

- [login-trace.har](.PROJ-0142. Fix login flow/login-trace.har) (2.1 MiB)
````

The `PROJ-0142.` prefix is the issue's identity; the rest of the filename is
cosmetic and follows the summary. Issue numbers are zero-padded in filenames only,
so a listing sorts `PROJ-0009` before `PROJ-0010`; the key everywhere else — the
frontmatter, the CLI, the API — stays `PROJ-142`. `jired doctor` renames files that
were written before this convention. Blocks between `<!--jired:raw:…-->` markers are
Jira content Markdown cannot represent (panels, macros, media): leave them alone
and they push back byte-identical; edit one and `push` stops with exit code 6.

## The file format

`jired init` also writes these rules into `CLAUDE.md` and `AGENTS.md` for agents.
This is the same contract, for people.

### Frontmatter

Three classes of field, and jired treats them differently:

| Class | Fields | On edit |
|---|---|---|
| Editable | `type`, `status`, `priority`, `summary`, `labels`, `components`, `assignee`, `sprint`, `parent`, `epic`, `fix_versions`, `affects_versions`, `due_date`, `links`, `story_points`, `custom` | becomes an API call on the next `push` |
| Read-only | `reporter`, `created`, `updated`, `url`, `jired` | dropped with a warning on the next write |
| Identity | `key`, `id` | an error — jired would otherwise write one issue over another |

Anything else you add to the header is kept verbatim and never sent, so notes of
your own survive a pull. `summary` in the frontmatter wins over the `# ` heading
when the two disagree; the heading is written from it.

One exception to the first row: `epic` has no call behind it yet, so a change to
it is warned about rather than sent.

Links are single-entry maps, one per line, using the link type's own wording:

```yaml
links:
- blocks: PROJ-150
- relates to: PROJ-99
```

The direction is the line: `- is blocked by: TSK-266` and `- blocks: TSK-266`
are the two ends of the same link type. Push turns an added line into
`POST /issueLink`, a removed one into `DELETE /issueLink/{id}`, and resolves
the wording against the instance's own link types — a direction this Jira does
not define is refused at plan time (exit 2, visible in `--dry-run`), naming the
ones it does.

`due_date:` is Jira's own `duedate` — a date with no time, `2026-09-14`. It is a
system field, so it is named in the header rather than living under `custom:`.

`custom:` holds fields with no well-known name, keyed by their Jira id and written
in their simple form — a select list as `Platform`, a multi-select as a YAML list,
a user as `@[Name](id)`. Edit the value; push rebuilds the shape Jira wants
(`{"value": …}`, `{"name": …}`, `{"accountId": …}`) from what the server last sent
for that field. A value that could not be simplified is shown as raw JSON and sent
back unchanged — leave it alone unless you know the field. Deleting an entry clears
that field.

`jired fields` says what those ids are called on this instance, and
`jired fields --custom` narrows it to the ones this Jira added. A key under
`custom:` is any field id the server knows, including a system one jired does not
model by name — but a field jired *does* model has its own header key, and putting
it under `custom:` as well is two names for one value.

### Sections

The order is fixed — `# <summary>`, `## Description`, `## Children`,
`## Checklist`, `## Comments`, `## Attachments` — because both the parser and the
next reader locate content by heading. Sections you invent are kept and re-rendered after the
known ones.

**Comments.** An existing one is `### [c-10812] Jane D — 2026-08-15T10:32:00Z`;
edit the body under it to update it. To add one, append a section headed
`### [new]` — push creates it and rewrites the heading with the real id. A comment
heading anywhere but under `## Comments` is warned about and not pushed. Deleting
a section does nothing unless you pass `push --allow-comment-delete`; without it
the comment stays on the server.

**The two checkbox sections are different things.** `## Children` lists the
subtasks, which are issues of their own, as `- [ ] PROJ-143 Add integration test`;
ticking one transitions that subtask only when `children.done_transition` is set to
a status name, and otherwise push says so and moves on. Adding a line does **not**
create a subtask — use `jired create --parent PROJ-142`.

`## Checklist` is the checklist plugin's list inside this issue: a status the
checkbox does not already say is written in backticks ahead of the title, `### `
lines group items, and an indented line under an item is its note. Tick a box,
add an item or write a note and `push` sends the list back in the plugin's own
syntax — a whole issue can be created and finished from the CLI. Only what the
section can show is writable: the four statuses its markers spell (`TO DO`,
`IN PROGRESS`, `DONE`, `SKIPPED`), groups, and notes. If jired's rendering of a
checklist does not reproduce the text the server holds — a mandatory item, a
status the instance configured itself, a syntax this plugin version spells
differently — the edit is refused out loud rather than sent with the rest of the
field dropped. The plugin's fields are found by name; `checklist.field` names
the writable one where they are called something jired does not recognise.

### Lossy regions and conflicts

`lossy: true` in the `jired:` block means the file contains at least one
`<!--jired:raw:…-->` region: Jira content that Markdown cannot represent. Push
rebuilds those regions from the stored original, so leaving them alone is free.
Edit one and that issue fails with exit 6 and a list of the regions; `push
--allow-lossy` sends what the file says instead, as plain Markdown, replacing the
original markup. Other issues in the same push are unaffected.

A region can sit in a comment or a worklog note as well as in the description,
and the same rule covers it wherever it is. Indentation is not an edit: a region
written inside a list or a quote carries that container's indent on every line,
and push compares what is between the markers without it.

`pull --strategy merge` writes git-style `<<<<<<< LOCAL` / `=======` /
`>>>>>>> REMOTE` markers into the file and exits 5. Fix them with `jired resolve`:

```sh
jired resolve --list             # which issues and which fields
jired resolve --ours             # keep the local value everywhere both changed
jired resolve --theirs           # take the server's (asks first)
$EDITOR "PROJ-0142. Fix login flow.md" && jired resolve --done
```

`--done` refuses any file that still contains markers, so `<<<<<<<` cannot reach
Jira.

### Colour

Output is coloured when it goes to a terminal and plain when it goes to a pipe or
a file, decided for stdout and stderr separately — so `jired status > out.txt`
gets clean text while the warnings on stderr stay readable.

Issue keys are cyan, headings bold, meta and hints dim; what was there is red and
what replaced it is green, the same reading in `log`, `diff`, `pull` and `doctor`.
Statuses follow Jira's own categories rather than their names — `To Do` dim,
`In Progress` yellow, `Done` green — so an instance that renamed its workflow to
`Ready for QA` still reads correctly, and a category jired does not recognise is
left uncoloured rather than guessed at.

```sh
jired log --no-color        # plain, the spelling every other tool uses
jired log --color never     # the same thing
jired log --color always    # colour even into a pipe, e.g. for `less -R`
NO_COLOR=1 jired log        # off for good, from a shell profile
```

`--no-color` and `--color` conflict: naming both is a usage error rather than a
precedence rule to remember. An explicit `--color always` outranks `NO_COLOR`.

### Times

What jired prints is your own clock:

```
$ jired log
PROJ-142  2026-08-16 14:03:00  Ilya A
    priority         Medium → High
```

That is the reader's timezone, taken from the machine (`TZ` and the system zone),
with no zone marker — the offset is one you already know, and UTC on a screen is
only a subtraction to do. `log`, `status`, `report time` and every message jired writes to the
terminal follow it, and so do the times the `logwork` form shows and prefills.
(`report time` counts whole UTC days, so the date range in its header is not a
clock reading and does not move.)

What jired *keeps* stays UTC, because a record has to mean the same thing on
every machine that reads it: `--json` documents, the timestamps in issue files
and frontmatter, `.jired/state.db`, and everything sent to Jira.

Where the two meet — a worklog's start time, which the form shows you and then
writes into the file — the offset is spelled out, so it reads as the clock you
were looking at and still names one instant:

```sh
jired logwork PROJ-142 "2h" --started "2026-08-21 09:00"    # your timezone
jired logwork PROJ-142 "2h" --started "2026-08-21T09:00:00Z" # exactly that
```

A time that names its zone is kept in the words you wrote it in. One that does
not is read in yours — which is what you will type, having just been shown a time
without one. Scripts should name the zone.

## Commands

The loop is `fetch`, `pull`, `diff`, `push`; `status` says where you are. Every
command that names an issue takes either its key or the path to its file, so
`jired comment "PROJ-0142. Fix login flow.md" -m "on it"` works — you can tab-complete
the filename you were just editing instead of retyping the key.

| Command | What it does |
|---|---|
| `fetch [ISSUES…]` | update the local snapshot from the server without touching files; `--jql/--sprint/--board/--updated-since` widen the scope, `--full` ignores the incremental watermark, `--slim` skips changelogs to keep `state.db` small |
| `pull [ISSUES…]` | fetch and merge into the files; `-n/--dry-run` shows what would be written |
| `push [ISSUES…]` | send staged changes; shows the plan and asks first when interactive (`-y` skips), `-n/--dry-run` (alias `preview`) prints the exact API calls, `--refresh-stale` re-fetches and retries the issues the server moved on from |
| `diff [ISSUES…]` | `--against base` (default: what `push` would send) or `--against remote` (what `pull` would bring in); `--stat`, `--exit-code` |
| `status` | dirty, behind, staged, conflicted; `--all` also lists unchanged issues |
| `edit [ISSUE]` | change an issue on a form; without an issue, pick one from the list — see [The issue picker](#the-issue-picker) |
| `logwork [ISSUE] [TIME]` | book time against an issue; fills in a form when either is missing |
| `comment [ISSUE] [-m TEXT]` | stage a comment; without an issue, pick one from the list; without `-m` it opens `$VISUAL`/`$EDITOR`, or reads stdin when piped |
| `create --summary S` | write a `NEW-<hex>. Summary.md`; push creates the issue and renames the file to the real key. `--assignee`, `--sprint`, `--labels`, `--priority`, `--component`, `--epic` and `--set field=value` fill it in; `--template NAME\|PATH` starts from a skeleton |
| `transition ISSUES… --to STATUS` | stage a workflow change across several issues, each validated against its own workflow; `--list` shows what one issue can do, `--field k=v` fills a transition screen |
| `assign ISSUES… --me\|--user Q\|--none` | stage an assignee change across several issues; `--user` searches only the people who can be assigned the first one, not the whole directory |
| `label add\|remove\|set LABEL ISSUES…` | stage a label change across issues; `add`/`remove` keep the other labels, `set` replaces them |
| `parent ISSUES… --to ISSUE` | move sub-tasks under a different parent — see [Sub-tasks](#sub-tasks) |
| `set --field F --value V ISSUES…` | stage any editable field across issues, including `customfield_*` ids |
| `label add\|remove\|set LABEL ISSUES…` | stage a label change across issues |
| `set --field F --value V ISSUES…` | stage any editable field across issues |
| `attach ISSUE FILE…` | stage an upload into the issue's attachment folder |
| `attachments pull [ISSUES…]` / `list` | download attachment content over the stubs, or show what is present |
| `logwork [ISSUE] [TIME]` | book time — `jired logwork PROJ-142 "2h 30m"`; with either missing it opens a form, `--push` sends it straight away |
| `report time` | who booked how much time, a row per person and a column per day, off the files already on disk; `--days N` (default 7) and `--show-issues`, both of which the screen can change while it is open |
| `log [ISSUE]` | what changed, newest first — every issue in the workspace, or just one; a short change reads as `from → to` and an edited description reads as a diff; `--limit` takes a count (`20`, the default) or a span back from now (`1d`, `2w`, `36h`, `90m`), `--refresh` asks the server |
| `search "project = PROJ AND …"` | run JQL and print the matches (`--jql` is the same thing spelled as a flag) |
| `list [--summary-contains T] [--label L] [--status S…] [--not-status S…] [--category new\|indeterminate\|done] [--sprint S] [--assignee U] [--issue-type T]` | the same question asked of the working copy, offline; `--status` repeats to mean "any of", `--category` is Jira's own three-way reading of any workflow ("not finished yet" is `--not-status Done` on one instance and three flags on the next), `--issue-type` matches the whole name and not a substring; prints key, status and summary, or the fields `--columns key,type,assignee` names, or keys alone with `--keys`, for piping |
| `show ISSUE [--raw]` | everything the workspace knows about one issue; `--raw` is the server's own payload, unset fields included |
| `fields [TEXT] [--custom]` | what this instance calls its fields, so `customfield_10015` has a name |
| `resolve` | list and clear recorded conflicts — see above |
| `reset [ISSUES…]` | drop local changes and put the files back to the last sync; lists what would go and asks first, `-n/--dry-run` just lists, `--include-new` also deletes locally created issues |
| `clean` | remove files whose issues left the workspace scope; `--dry-run`, `--force` for ones with unpushed edits |
| `open [ISSUE]` | open it in a browser, or `--print` the URL; without an issue, pick one from the list |
| `whoami` | which account, which server, and where the credential is kept |
| `fetch-spec [-o DIR]` | write the server's own description of its REST API into a directory, one file per module, named for the server and its version — see [The server's own API reference](#the-servers-own-api-reference) |
| `projects` / `boards [--for-project KEY]` / `sprints [--board B]` | what you can address; `--board` takes a name or an id, and is needed only when the project's boards would answer differently; `boards` says which project each board is filed under |
| `sprint create [NAME]` / `sprint start\|edit\|close SPRINT` / `sprint add ISSUES… --sprint S` / `sprint remove ISSUES…` | make a sprint, run it, and move issues in and out of one; `close --move-incomplete-to S` carries the unfinished ones over — see [Sprints](#sprints) |
| `doctor` | check the workspace over and repair it — see below |
| `config get\|set\|unset\|list\|path\|edit` | read and write settings |
| `completion <shell>` | print a completion script |
| `init-agents` | rewrite `CLAUDE.md`, `AGENTS.md` and the read-only command permissions |
| `schema [NAME]` | the JSON Schema for a command's `--json` output |
| `version [--changelog]` | which binary this is, and what changed in it |

A 2xx from Jira is not evidence that Jira did anything: some payloads it accepts
and quietly ignores. So `push` re-reads each issue and checks the change is in it
before rewriting the file from the server. An op the server did not act on is
`failed`, carrying the request that was sent; the issue is `ok: false`, the push
exits 10, and your edit is still in the file — nothing is overwritten on the
strength of a success that was not one.

### History

`jired log` is what changed and who changed it, newest first, over one issue or
the whole workspace. `--limit` is either a count or a span back from now, so
"since yesterday" is a question you can ask directly:

```
$ jired log --limit 1d
PROJ-142  2026-08-16 14:03:00  Ilya A
    priority         Medium → High
    description      +1 -1
      @@ -2,4 +2,4 @@

       1. Log in
      -2. Log out
      +2. Wait five minutes
       3. Log in again
```

A short change reads as `from → to`. A field carrying prose is diffed instead —
Jira's changelog hands back the whole old and new text of a description, and a
page of it does not fit on a row. jired decides by the values, not by the field
name, so an instance's own "Acceptance Criteria" is diffed too. The diff is of
the server's own text rather than jired's markdown rendering of it, and one
field is capped at 30 lines; `--json` carries both texts whole.

### The issue picker

`jired edit`, `jired comment` and `jired open` with no issue named ask which one,
on a table you narrow by typing:

```
 Comment on which issue?                                                       12 of 37
 / sso
 Key        Summary                     Status        Pri     Assignee       Updated↓
 PROJ-100   SSO rollout                 To Do         Highest —              2d
 PROJ-142   Fix login flow              In Progress   High    Ilya A         5d
```

Typing filters on the key and the summary as a subsequence, so `pl142` finds
`PLAT-142` and `sso ro` finds the SSO rollout epic. The other columns are data
rather than filter fodder — typing `aug` finds an issue that says "August", not
every issue touched last month.

| Key | What it does |
|---|---|
| `^s` | sort by the next column, pointing the way that column is usually read |
| `^r` | reverse the direction |
| `^f` | narrow to one status, or to a whole status category |
| `⏎` / `esc` | take the selected issue, or none |

The columns are key, summary, status, priority, assignee, reporter, and the ages
of `updated`, `created` and `viewed`; the ones that do not fit are dropped from
the right, keeping the key, the summary and whichever column is sorted. `viewed`
is when *this machine* last went to the issue — the server does not know it, so
it starts empty and is lost with `state.db`. Rows with nothing in the column
being sorted go last in either direction.

`edit` and `comment` list only issues with a file, because that is where they
stage the change; `open` lists everything the workspace has a snapshot of, since
a browser URL needs no file.

Sort and filter are settings, so they survive the screen closing:

```sh
jired config set ui.issues.sort priority:asc          # ^s and ^r write this
jired config set ui.issues.filter category:done       # ^f writes this
jired config set ui.issues.filter status:"In Review"
```

The picker writes them into the workspace config, not the global one — the list
was rearranged here. `category:` takes Jira's own three buckets (`new`,
`indeterminate`, `done`), which is what survives a workflow whose statuses are
named something local.

### Sprints

`jired sprints` lists what a board has, `jired sprint create` adds one,
`start`/`edit`/`close` run it, and `jired sprint add`/`remove` move issues in and
out.

```sh
jired sprints                                  # the board's sprints, open ones only
jired sprints --board "Platform Board" --all   # by name, closed ones too

jired sprint create                            # fill in a form
jired sprint create "Sprint 44" --start 2026-09-01 --end 2026-09-15
jired sprint create "Sprint 44" --dry-run      # the exact API call, nothing sent

jired sprint edit "Sprint 44" --goal "Ship SSO."          # a goal after the fact
jired sprint start "Sprint 44"                            # future → active
jired sprint start 44 --start 2026-09-01 --end 2026-09-15 # dates and go, in one
jired sprint close "Sprint 43" --move-incomplete-to "Sprint 44"   # carry the rest over

jired sprint add PROJ-142 PROJ-150 --sprint "Sprint 44"   # stage the move
jired sprint add PROJ-142 --sprint 44 --push              # and send it
jired sprint remove PROJ-142                              # back to the backlog

jired sprint create "Sprint 44" --issues PROJ-142,PROJ-150 --push   # both, in one
```

`sprints` and `sprint create` take `--board` as either a name or an id, and
neither needs it when the project has one board. Nor does `sprints` when the
project's boards share one sprint pool and every one of them answers the same —
a common arrangement, and one where the flag would have made no difference; it is
asked for only when the boards genuinely differ, and the error then lists them.
`jired boards` reports which project each board belongs to, asking the board
itself on Data Center, where the listing does not say. A sprint has no file to
stage it in, so `sprint create` is the one write that goes straight to the server
instead of waiting for `push` — which is why it has a `--dry-run` of its own.
Jira takes both dates or neither, and the new sprint is always `future` until
someone starts it.

The form fills in the name the board is due — a board whose last sprint is
`Sprint 42` is offered `Sprint 43`, renumbered if you change the board and left
alone once you type over it — and offers the dates a sprint usually runs
between: next Monday, and one to four weeks after whatever the start says.
Without a terminal, `--board` and a name are all it needs; without either, it
exits 7 naming what is missing.

`sprint start`, `sprint edit` and `sprint close` are the rest of a sprint's life,
and have no file to stage into either, so they send immediately and take the same
`-n`. They are one endpoint underneath, and it *merges*: only the flags you give
are sent, and what is not sent is what the sprint keeps. `edit` is how a goal gets
set after the sprint exists — `create --goal` only sets one at creation — and
`--goal ''` takes it back off. `--name` renames, and since issue files spell a
sprint by name, jired then reminds you to `pull`.

The two things Jira refuses with a 400 that names nothing are refused here first,
with the reason: a sprint has no dates to run between (pass `--start`/`--end` and
it sets them and starts it in one call), and the board already has a sprint
running (named, with the command to close it).

`sprint close` is the half that strands issues. Closing a sprint leaves everything
unfinished pointing at a closed sprint, so this always names them, and
`--move-incomplete-to` carries them into the next sprint first — moved and pushed
before the close, because an issue cannot be got out of a sprint that is already
closed. "Unfinished" is Jira's own status *category* rather than a status name, so
it means the same thing on a workflow that says `Done` and one that says `Готово`.

`sprint add` and `sprint remove` are the scriptable single-field commands for an
issue's sprint, alongside `transition`, `assign` and `comment`: they write the
frontmatter and stage it, so the change shows up in `status` and `diff` and goes
out with the next `push` — or immediately with `--push`. `--sprint` takes a name
or an id, matched against the sprints this workspace has fetched, and a name no
board knows is an error rather than a push that quietly skips it. An issue
already in the sprint is left alone and said so, rather than staged as a change
that does nothing.

A sub-task is the one issue none of this reaches: it is not a board item, and
takes its sprint from whatever its parent is in. `push` refuses a `sprint:` on
one — exit 2, naming the parent to move instead — rather than making a board
call Jira answers 204 to and ignores. It shows in `push --dry-run`, and it
leaves your edit where you wrote it. Moving the sub-task itself is
[below](#sub-tasks).

The frontmatter is still the interface, and `sprint:` is a history — an issue
that rolls over stays in the sprint it came from — so a move appends to the list
and only its last entry is where the issue is now. `sprint remove` empties it,
which is what the backlog looks like in a file. See
[Grouping folders](#grouping-folders) for the `sprints/` view of where
everything is.

### Sub-tasks

```sh
jired parent PROJ-143,PROJ-144 --to PROJ-150    # stage the move
jired parent PROJ-143 --to PROJ-150 --push      # and send it
```

`parent:` in the frontmatter is where a sub-task hangs, and editing that line
does exactly what the command does — several issues at a time is the only reason
the command exists, that and not having to open four files to fix one
misfiling.

What `push` then does with it depends on the deployment, and neither the file
nor the command has to know. Cloud takes `parent` as an ordinary field update.
Data Center has no REST call for it at all: `PUT /issue` accepts the payload,
answers success and leaves the parent exactly where it was — the request behind
[JRASERVER-68763](https://jira.atlassian.com/browse/JRASERVER-68763), open since
2018 — so jired drives the same Move wizard the UI drives, posting the form
Jira itself serves rather than a payload guessed at from outside. `push
--dry-run` shows it as `move_parent` against `/secure/MoveSubTaskParent.jspa`,
which is the honest description: it is a web action, unversioned, and an
upgrade can move it.

Which is why the result is never taken on the wizard's word. `push` re-reads
the issue afterwards, and a parent that did not actually move is a failed
operation — exit 10, your edit still in the file, nothing overwritten — rather
than a green push and a line quietly rewritten back.

On Data Center, `push` asks the issue's edit screen what it carries before it
sends anything, and `create` asks the create screen for its project and type.
Jira configures fields per project, per issue type and per screen, so an issue
can have a priority while the screen you edit it through offers no way to set
one — and that write comes back a 400 about a screen, after the rest of the push
has already gone. Asked first, it is exit 2 at plan time: `--dry-run` shows it,
the field is named, the other issues carry on, and your edit stays in the file.
A server that will not describe a screen refuses nothing, because unknown is not
"no fields".

`assignee:` travels the same way, on both deployments: `push --dry-run` shows it
as `assign` against `/issue/{key}/assignee` rather than folded into the field
update. Assigning is not editing — Jira gives it a route because it is its own
permission — and on an instance whose edit screen does not carry `assignee`,
which is a perfectly ordinary configuration, the field update is refused while
the route works. It is verified on the re-read like the parent is.

**A page needs a credential Jira lets in the front door.** Data Center
authenticates its web pages through Seraph, which knows about basic auth and
about a session cookie — and not about a bearer token. A personal access token,
which is what `jired init` recommends on Data Center and what everything else
here uses happily, is redirected to `SessionTimeoutMessage.jspa` the moment it
asks for the Move wizard, and Jira serves that page with HTTP 200. So this one
operation, alone in jired, needs `auth.kind = basic`:

```sh
jired init --force --auth basic     # if the instance allows it
```

Under a token that cannot open a web session the move is refused in the plan —
exit 2, before anything is sent, and visible in `push --dry-run` — and `jired
parent` says so while staging. The staged line is still worth having: it is
what the file should say once you have moved the sub-task in Jira's own UI.

Two more moves have no wizard page at all and are refused the same way: a
sub-task cannot be left without a parent (that is a conversion, in Jira's UI),
and it cannot be moved to a parent in another project, because its key would
have to change with it.

### Grouping folders

`by-parent/`, `sprints/` and `boards/<board>/<column>/` are views onto the same
issue files, rebuilt from the database by `pull`, by `jired doctor --regroup`,
and by a `push` that moved an issue between sprints, columns or parents — so the
folder for a sprint you have just made appears with the first issue you move
into it. `sprints/` covers the sprint running now and the ones queued behind it;
closed sprints are left out until `sprints.include_closed`.
`links.mode` chooses how they point at the real file — `auto` (a symlink, or a hard
link where symlinks are not permitted, or a small Markdown stub if neither works),
or `symlink`/`hardlink`/`stub` to insist on one.

They are views with one exception: moving an issue's entry from one
`boards/<board>/<column>/` folder to another stages a transition to that column's
status, which is what dragging the card would have done. An explicit `status:` edit
in the file wins if the two disagree. Everything else about a grouping folder is
regenerated, so edits there are lost.

### Templates

`jired create --template bug` starts from `.jired/templates/bug.md` — an issue file
without the identity fields, so a team's usual description skeleton, labels and
components come pre-filled. `--template` also takes a path, so a skeleton can
live in the repository being worked on rather than being copied into every
workspace. The file gets a placeholder `NEW-<hex>` key until `push` creates the
issue in Jira and renames it to the real one.

Flags beat the template, and `--set field=value` beats both, which is how one
skeleton stamps out variations:

```sh
jired create -s 'Ship SSO' --template bug --set priority=Highest \
             --assignee nilsson.m --sprint 'Sprint 44' --labels regular --push
```

`--set` takes any editable frontmatter name or a `customfield_*` id, the same
vocabulary [`jired set`](#commands) uses.

### The server's own API reference

```sh
jired fetch-spec --output ./specs
```

Data Center instances describe the REST API they serve, and `fetch-spec` writes
that description out:

```
Corp Jira 10.7.4 at https://jira.corp.example/jira
  jira.corp.example-datacenter-10.7.4-api-2.wadl      1.2 MiB
  jira.corp.example-datacenter-10.7.4-agile-1.0.wadl  86.4 KiB
  auth-1 — this server does not serve one
2 files into /home/you/specs
```

One file per REST module the server describes, each named for the server and the
version that answered — because a bare `application.wadl` in a directory is a
file nobody can place six months later, and no two Jiras answer the same. What
comes back is [WADL](https://en.wikipedia.org/wiki/Web_Application_Description_Language):
paths, methods and parameters, not response bodies.

This is worth having because Atlassian does not publish it. There is no official
OpenAPI document for Data Center; the platform WADL is published per release on
`docs.atlassian.com`, and for the Agile API — `boards`, `sprints`, every sprint
move — there is nothing at all. Your instance serves both, for the version you
actually run.

jired keeps a copy of its own, under `specs/datacenter/10.7.4/` in the source
tree: that is the description every Data Center route it calls is checked
against in CI, so a route, a method or a query parameter that a Jira 10.7.4 does
not actually offer fails the build rather than your push. It is also why the
Data Center floor is 10.0. If your instance is a different version, `fetch-spec`
is how you get the description that matches it.

Jira Cloud is refused: an Atlassian-hosted site serves no description of itself,
and the [OpenAPI document](https://developer.atlassian.com/cloud/jira/platform/swagger-v3.v3.json)
that covers Cloud comes from Atlassian rather than from your site — jired names
that URL rather than fetching it under your site credential.

### Shell completions

```sh
jired completion zsh > "${fpath[1]}/_jired"
jired completion bash > ~/.local/share/bash-completion/completions/jired
jired completion fish > ~/.config/fish/completions/jired.fish
```

`jired completion --help` lists the rest. Config keys, schema names and enum
values complete; issue keys do not, because completion must not open the database.

From a clone, `make install` writes the bash and zsh ones for you — as does
`make install-completions` on its own, when only they need refreshing. Both go
to the XDG user directories, `~/.local/share/bash-completion/completions/jired`
and `~/.local/share/zsh/site-functions/_jired`, so neither needs root; override
`BASH_COMPLETION_DIR` or `ZSH_COMPLETION_DIR` to put them elsewhere. zsh only
reads its directory if that is on `fpath`, which the target checks and tells you
how to fix. `make uninstall` takes both files back out.

## Exit codes

| Code | Meaning |
|---|---|
| 0 | success |
| 1 | generic error |
| 2 | usage error |
| 3 | authentication failure |
| 4 | network failure |
| 5 | conflict (`pull` wrote nothing, or `--strategy merge` wrote conflict markers; `push` refused stale issues) |
| 6 | lossy conversion blocked |
| 7 | required value missing in non-interactive mode |
| 8 | workspace locked by another jired process |
| 9 | local state database corrupt |
| 10 | unsupported Jira server, or a change it accepted and did not apply |

## When something goes wrong

Every error names what failed and, where there is one, what to do about it. Add
`-v` to see the details (REST paths, pids, causes) that the default output keeps
out of the way.

| Exit | What happened | What to run |
|---|---|---|
| 2 | an edit left a file jired cannot parse | `jired diff` to see what changed, or `jired reset "PROJ-0142. Fix login flow.md"` to throw the edit away and start from the last sync |
| 3 | the credential was rejected — an expired or revoked token, or the wrong identity for this server | `jired whoami`, then `jired init --force` to authenticate again |
| 4 | DNS, connect, timeout or TLS | `jired doctor` verifies connectivity; see TLS and proxies below |
| 5 | `pull` found fields changed on both sides, or `push` refused an issue the server has moved on from | `jired pull --strategy merge` then `jired resolve`; for a stale push, `jired push --refresh-stale` re-fetches and retries the ones whose incoming change is unrelated |
| 6 | a read-only region was edited and its original markup cannot be rebuilt | `jired diff` to see it, restore the block, or `jired push --allow-lossy` |
| 8 | another jired process holds the workspace lock (the message names its pid and start time) | wait — the lock is released when that process exits, including if it is killed |
| 9 | a local database will not open or holds something unreadable | see "the databases" below |
| 10 | Jira Data Center older than 10.0 (Cloud is always supported) | nothing jired can do; jired's Data Center routes are checked against the API description a 10.x server publishes, and older releases are not tested against it |
| 10 | `push` sent a change the server answered success to and did not make | the op says which, with the request it sent; your edit is still in the file — make the change in Jira and `jired pull` |

### TLS

A self-signed or internally-issued certificate is a network failure with a TLS
cause. Point jired at the bundle instead of turning verification off:

```sh
jired --ca-cert /etc/ssl/corp-root.pem status   # or: jired config set tls.ca_cert …
```

`jired init` does this for you: when the certificate does not verify it fetches
and prints it — subject, issuer, validity, SHA-256 fingerprint — and pins it to
`.jired/trusted-ca.pem` if you confirm, after which every run uses it silently.
Check the fingerprint against what your Jira administrator publishes; `--yes` is
deliberately ignored for that question. `--insecure` (or `tls.insecure`) skips
verification entirely and prints a warning on every run — a last resort, not a
setting to leave on.

### Proxies

jired uses the system proxy configuration: `HTTPS_PROXY` (and `NO_PROXY` for the
hosts to reach directly). A TLS-intercepting corporate proxy needs its root
certificate through `--ca-cert`, same as above.

### Headless machines and CI

There is no keychain in a container, so name a passphrase and jired encrypts the
token into a file:

```sh
export JIRED_SECRET_KEY="$CI_JIRED_PASSPHRASE"
export JIRED_TOKEN="$CI_JIRA_TOKEN"
jired init --url https://acme.atlassian.net --email ci@acme.example \
           --project PROJ --no-input --json
jired pull --no-input --json
```

`--no-input` never prompts and exits 7 naming whatever is missing. Set
`JIRED_NO_KEYRING=1` to ignore a keychain that exists but is locked, and
`creds.storage` to choose between `local_db` (this workspace) and `user_config`
(shared by every workspace on the machine). `JIRED_SECRET_KEY` must be the same
value on every later run; without it the stored token cannot be decrypted.

### Cloud or Data Center

The URL decides: `*.atlassian.net` and `*.jira.com` are Cloud, anything else is
Data Center, and the server's own `serverInfo` corrects the guess during `init`.
The choice picks the REST version, the markup (ADF or wiki) and the credential
kind (Cloud: email + API token; DC: personal access token). A Cloud instance on a
vanity domain is the case that needs `jired init --deployment cloud`.

### The databases

`.jired/` holds two. `state.db` is a cache — the base snapshot, the remote
snapshot and the push journal — and deleting it is safe:

```sh
rm .jired/state.db && jired fetch
```

Your `.md` files are not touched; `fetch` refills the snapshot from the server.
`session.db` is the other one: it holds the account and the credential reference,
cannot be rebuilt from the server, and a corrupt one means `jired init --force`.
`jired doctor` reports which of the two is at fault.

## Configuration

Resolution order, highest first: CLI flag → `JIRED_*` environment variable →
`.jired/config.toml` → `$XDG_CONFIG_HOME/jired/config.toml` → built-in default →
interactive prompt. `init` writes every setting into `.jired/config.toml` with its
documentation as a comment, so the file is the whole picture; only the four values a
person actually has to supply — URL, email, token, project — are ever prompted for.
Run `jired config list --all` to see every setting, its value, and where it came
from.

Credentials go wherever `creds.storage` says: `keychain` (the default when the
machine has one), `local_db` for `.jired/session.db` in this workspace, or
`user_config` for `credentials.toml` in your user config directory, shared by
every workspace here. The two file-backed stores encrypt the token with a
passphrase from `JIRED_SECRET_KEY`, which is how headless environments work;
`jired doctor` moves the credential when you change the setting.

```sh
jired config set creds.storage user_config --global   # one credential, every workspace
jired doctor                                          # move the existing one there
```

A custom field nobody here works with can be kept out of the files entirely:

```sh
jired config add ignored_custom_fields customfield_10000   # an id, or the field's name
jired doctor                                               # take it out of the files that have it
```

Ignoring hides a field; it never clears one. The value stays on the server and
in the database, `push` refuses to send a copy left in a file, and
`jired config remove ignored_custom_fields customfield_10000` followed by another
`jired doctor` brings it back.

See [docs/security.md](docs/security.md), which is explicit about what each
store does and does not protect against.

## Reporting a problem

`jired --version` names the exact binary — release, commit, build time, compiler
and target — so a report says which source produced the behaviour:

```
$ jired --version
jired 0.3.0
commit:  9212430
built:   2026-08-21T10:49:39Z
rustc:   rustc 1.97.1 (8bab26f4f 2026-07-14)
target:  x86_64-unknown-linux-gnu
```

`-V` is the same thing on one line, and `jired version` is the same thing as a
command. A `-dirty` suffix on the commit means the binary was built from a tree
with uncommitted changes.

The changelog is compiled into the binary, so the question that follows "which
version is this" has an answer that does not need a checkout:

```sh
jired version --changelog          # what changed, up to the commit above
jired version --changelog --json   # the same, as `jired/version/1`
```

It is the changelog *that binary* was built from, which is the point: a
`CHANGELOG.md` in a checkout beside it describes wherever that checkout has got
to, and an installed binary has no checkout at all.

## Repairing a workspace

```sh
jired doctor --dry-run   # report only, never changes anything
jired doctor             # show the same list, then ask before repairing
jired doctor -y          # repair without asking (what a script gets anyway)
```

It checks the `.jired/` layout and permissions, the config file, the stored
credentials, filenames that predate a naming convention, issue files whose sections
predate one (subtasks still under `## Checklist` move to `## Children`), custom
fields that disagree with `ignored_custom_fields`, database rows pointing at the
wrong file, the `.gitignore` block, the generated agent docs and the agent
permission files — and repairs what it can. `--regroup` also rebuilds the grouping folders when nothing else needs
fixing. Run it after upgrading jired, and after changing what is ignored.

## Documentation

- [docs/security.md](docs/security.md) — credential storage, threat model, TLS
- [docs/plan/](docs/plan/README.md) — design and phased delivery plan
- [CHANGELOG.md](CHANGELOG.md) — what changed between versions

## License

MIT ([LICENSE-MIT](LICENSE-MIT)) or Apache-2.0 ([LICENSE-APACHE](LICENSE-APACHE)),
at your option.
