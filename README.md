# jired

Work with Jira issues as Markdown files, offline-first.

`jired` fetches issues into the current directory as `.md` files and gives you a
git-like cycle — `fetch`, `pull`, `diff`, `push` — to sync them back. All reads
and edits work without a network; only `fetch`, `pull`, and `push` need one.

Supports **Jira Cloud** (REST v3, ADF) and **Jira Data Center** (REST v2, wiki
markup) behind one interface.

## Quickstart

```sh
mkdir my-sprint && cd my-sprint

jired init               # asks for the URL, who you are, and which project

jired status            # what's dirty, behind, or conflicted
jired pull              # write issue files, and say what changed
$EDITOR "PROJ-0142. Fix login flow.md"
jired create            # fill in a new issue on a form, or pass --summary
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
jired pull --json --no-input || test $? -eq 5   # 5 means conflicts, nothing written
jired schema pull                               # JSON Schema for that output
```

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

## Exit codes

| Code | Meaning |
|---|---|
| 0 | success |
| 1 | generic error |
| 2 | usage error |
| 3 | authentication failure |
| 4 | network failure |
| 5 | conflict (pull wrote nothing; push refused stale issues) |
| 6 | lossy conversion blocked |
| 7 | required value missing in non-interactive mode |
| 8 | workspace locked by another jired process |
| 9 | local state database corrupt |
| 10 | unsupported Jira server |

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
jired doctor --dry-run   # what is wrong
jired doctor             # fix it
```

It checks the `.jired/` layout and permissions, the config file, the stored
credentials, filenames that predate a naming convention, database rows pointing at
the wrong file, the `.gitignore` block and the generated agent docs — and repairs
what it can. Run it after upgrading jired.

## Documentation

- [docs/security.md](docs/security.md) — credential storage, threat model, TLS
- [docs/plan/](docs/plan/README.md) — design and phased delivery plan

## License

MIT OR Apache-2.0
