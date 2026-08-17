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

jired init --url https://acme.atlassian.net \
           --email you@acme.com --token "$JIRA_TOKEN" --project PROJ

jired status            # what's dirty, behind, or conflicted
jired pull              # write issue files
$EDITOR "PROJ-142. Fix login flow.md"
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

- [login-trace.har](.PROJ-142. Fix login flow/login-trace.har) (2.1 MiB)
````

The `PROJ-142.` prefix is the issue's identity; the rest of the filename is
cosmetic and follows the summary. Blocks between `<!--jired:raw:…-->` markers are
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
`.jired/config.toml` → `$XDG_CONFIG_HOME/jired/config.toml` → interactive prompt.
Run `jired config list --all` to see every setting, its value, and where it came
from.

Credentials go to the OS keychain. Headless environments set `JIRED_SECRET_KEY`
to a passphrase and jired stores an encrypted blob instead; see
[docs/plan/security-testing.md](docs/plan/security-testing.md) for the threat
model.

## Documentation

The design and phased delivery plan live in [docs/plan/](docs/plan/README.md).

## License

MIT OR Apache-2.0
