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
table. Every command takes `--json` and `--no-input`. In JSON mode both streams are
machine-readable: stdout carries one versioned result document, stderr carries
the structured error and nothing else. `--no-input` never prompts and exits 7
naming any missing value.

```sh
jired pull --json --no-input || test $? -eq 5   # 5 means conflicts; the default strategy wrote nothing
jired schema pull                               # JSON Schema for that output
```

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
| Editable | `type`, `status`, `priority`, `summary`, `labels`, `components`, `assignee`, `sprint`, `parent`, `epic`, `fix_versions`, `affects_versions`, `links`, `story_points`, `custom` | becomes an API call on the next `push` |
| Read-only | `reporter`, `created`, `updated`, `url`, `jired` | dropped with a warning on the next write |
| Identity | `key`, `id` | an error — jired would otherwise write one issue over another |

Anything else you add to the header is kept verbatim and never sent, so notes of
your own survive a pull. `summary` in the frontmatter wins over the `# ` heading
when the two disagree; the heading is written from it.

Links are single-entry maps, one per line, using the link type's own wording:

```yaml
links:
- blocks: PROJ-150
- relates to: PROJ-99
```

`custom:` holds fields with no well-known name, keyed by their Jira id and written
in their simple form — a select list as `Platform`, a multi-select as a YAML list,
a user as `@[Name](id)`. Edit the value; push rebuilds the shape Jira wants
(`{"value": …}`, `{"name": …}`, `{"accountId": …}`) from what the server last sent
for that field. A value that could not be simplified is shown as raw JSON and sent
back unchanged — leave it alone unless you know the field. Deleting an entry clears
that field.

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

`## Checklist` is the checklist plugin's list inside this issue, read from its
custom field and shown read-only: a status the checkbox does not already say is
written in backticks ahead of the title, edits to it are reported by `push` rather
than sent, and the next write from the server restores it.

### Lossy regions and conflicts

`lossy: true` in the `jired:` block means the file contains at least one
`<!--jired:raw:…-->` region: Jira content that Markdown cannot represent. Push
rebuilds those regions from the stored original, so leaving them alone is free.
Edit one and that issue fails with exit 6 and a list of the regions; `push
--allow-lossy` sends it anyway, replacing the original markup with plain Markdown.
Other issues in the same push are unaffected.

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
| `push [ISSUES…]` | send staged changes; shows the plan and asks first when interactive (`-y` skips), `-n/--dry-run` (alias `preview`) prints the exact API calls |
| `diff [ISSUES…]` | `--against base` (default: what `push` would send) or `--against remote` (what `pull` would bring in); `--stat`, `--exit-code` |
| `status` | dirty, behind, staged, conflicted; `--all` also lists unchanged issues |
| `edit [ISSUE]` | change an issue on a form; without an issue, pick one from the list — see [The issue picker](#the-issue-picker) |
| `logwork [ISSUE] [TIME]` | book time against an issue; fills in a form when either is missing |
| `comment [ISSUE] [-m TEXT]` | stage a comment; without an issue, pick one from the list; without `-m` it opens `$VISUAL`/`$EDITOR`, or reads stdin when piped |
| `create --summary S` | write a `NEW-<hex>. Summary.md`; push creates the issue and renames the file to the real key. `--template NAME` starts from `.jired/templates/NAME.md` |
| `transition ISSUE --to STATUS` | stage a workflow change; `--list` shows what is available, `--field k=v` fills a transition screen |
| `assign ISSUE --me\|--user Q\|--none` | stage an assignee change |
| `attach ISSUE FILE…` | stage an upload into the issue's attachment folder |
| `attachments pull [ISSUES…]` / `list` | download attachment content over the stubs, or show what is present |
| `logwork [ISSUE] [TIME]` | book time — `jired logwork PROJ-142 "2h 30m"`; with either missing it opens a form, `--push` sends it straight away |
| `report time` | who booked how much time, a row per person and a column per day, off the files already on disk; `--days N` (default 7) and `--show-issues`, both of which the screen can change while it is open |
| `log [ISSUE]` | what changed, newest first — every issue in the workspace, or just one; `--refresh` asks the server |
| `search "project = PROJ AND …"` | run JQL and print the matches (`--jql` is the same thing spelled as a flag) |
| `resolve` | list and clear recorded conflicts — see above |
| `reset [ISSUES…]` | drop local changes and put the files back to the last sync; lists what would go and asks first, `-n/--dry-run` just lists, `--include-new` also deletes locally created issues |
| `clean` | remove files whose issues left the workspace scope; `--dry-run`, `--force` for ones with unpushed edits |
| `open [ISSUE]` | open it in a browser, or `--print` the URL; without an issue, pick one from the list |
| `whoami` | which account, which server, and where the credential is kept |
| `projects` / `boards [--for-project KEY]` / `sprints [--board B]` | what you can address; `--board` takes a name or an id, and a project with one board needs neither |
| `sprint create [NAME]` | make a sprint on a board — see [Sprints](#sprints) |
| `doctor` | check the workspace over and repair it — see below |
| `config get\|set\|unset\|list\|path\|edit` | read and write settings |
| `completion <shell>` | print a completion script |
| `agent-docs` | regenerate `CLAUDE.md` and `AGENTS.md` |
| `schema [NAME]` | the JSON Schema for a command's `--json` output |

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

`jired sprints` lists what a board has; `jired sprint create` adds one.

```sh
jired sprints                                  # the board's sprints, open ones only
jired sprints --board "Platform Board" --all   # by name, closed ones too

jired sprint create                            # fill in a form
jired sprint create "Sprint 44" --start 2026-09-01 --end 2026-09-15
jired sprint create "Sprint 44" --dry-run      # the exact API call, nothing sent
```

Both take `--board` as either a name or an id, and neither needs it when the
project has one board. A sprint has no file to stage it in, so `sprint create`
is the one write that goes straight to the server instead of waiting for
`push` — which is why it has a `--dry-run` of its own. Jira takes both dates or
neither, and the new sprint is always `future` until someone starts it.

The form fills in the name the board is due — a board whose last sprint is
`Sprint 42` is offered `Sprint 43`, renumbered if you change the board and left
alone once you type over it — and offers the dates a sprint usually runs
between: next Monday, and one to four weeks after whatever the start says.
Without a terminal, `--board` and a name are all it needs; without either, it
exits 7 naming what is missing.

Putting an issue *into* a sprint is a file edit like any other: set `sprint:` in
the frontmatter and `push` moves it. See [Grouping folders](#grouping-folders)
for the `sprints/` view of where everything is.

### Grouping folders

`by-parent/`, `sprints/` and `boards/<board>/<column>/` are views onto the same
issue files, rebuilt from the database by `pull` and by `jired doctor --regroup`.
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
components come pre-filled. The file gets a placeholder `NEW-<hex>` key until
`push` creates the issue in Jira and renames it to the real one.

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
| 10 | unsupported Jira server |

## When something goes wrong

Every error names what failed and, where there is one, what to do about it. Add
`-v` to see the details (REST paths, pids, causes) that the default output keeps
out of the way.

| Exit | What happened | What to run |
|---|---|---|
| 2 | an edit left a file jired cannot parse | `jired diff` to see what changed, or `jired reset "PROJ-0142. Fix login flow.md"` to throw the edit away and start from the last sync |
| 3 | the credential was rejected — an expired or revoked token, or the wrong identity for this server | `jired whoami`, then `jired init --force` to authenticate again |
| 4 | DNS, connect, timeout or TLS | `jired doctor` verifies connectivity; see TLS and proxies below |
| 5 | `pull` found fields changed on both sides, or `push` refused an issue the server has moved on from | `jired pull --strategy merge` then `jired resolve`; for a stale push, `jired pull` first |
| 6 | a read-only region was edited and its original markup cannot be rebuilt | `jired diff` to see it, restore the block, or `jired push --allow-lossy` |
| 8 | another jired process holds the workspace lock (the message names its pid and start time) | wait — the lock is released when that process exits, including if it is killed |
| 9 | a local database will not open or holds something unreadable | see "the databases" below |
| 10 | Jira Data Center older than 9.0 (Cloud is always supported) | nothing jired can do; the older releases lack the PAT auth and v2 endpoints it depends on |

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
jired 0.1.0
commit:  9212430
built:   2026-08-21T10:49:39Z
rustc:   rustc 1.97.1 (8bab26f4f 2026-07-14)
target:  x86_64-unknown-linux-gnu
```

`-V` is the same thing on one line. A `-dirty` suffix on the commit means the
binary was built from a tree with uncommitted changes.

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
wrong file, the `.gitignore` block and the generated agent docs — and repairs what
it can. `--regroup` also rebuilds the grouping folders when nothing else needs
fixing. Run it after upgrading jired, and after changing what is ignored.

## Documentation

- [docs/security.md](docs/security.md) — credential storage, threat model, TLS
- [docs/plan/](docs/plan/README.md) — design and phased delivery plan
- [CHANGELOG.md](CHANGELOG.md) — what changed between versions

## License

MIT ([LICENSE-MIT](LICENSE-MIT)) or Apache-2.0 ([LICENSE-APACHE](LICENSE-APACHE)),
at your option.
