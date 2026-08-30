# jired — Implementation Plan

**jired** is an offline-first Rust CLI/TUI for working with Jira from the filesystem:
issues become Markdown files, and a git-like `fetch`/`pull`/`push`/`diff` cycle syncs
them with Jira Cloud (REST v3, ADF) or Jira Data Center (REST v2, wiki markup).

## Plan layout

| Doc | Contents |
|---|---|
| [architecture.md](architecture.md) | Components, data flows, SQLite schemas, frontmatter schema, JSON versioning |
| [commands.md](commands.md) | Every command: flags, behavior, exit codes, JSON shapes |
| [format-conversion.md](format-conversion.md) | Markdown file format, ADF/wiki ⇄ MD conversion, lossy-region mechanism |
| [sync-algorithm.md](sync-algorithm.md) | Three-way merge, conflict model, push concurrency protocol |
| [security-testing.md](security-testing.md) | Secret storage, threat model, redaction, testing strategy, CI matrix |
| [milestones/](milestones/) | Phased delivery — one file per milestone with task checklists |

## Milestones

| ID | File | Scope | Status |
|---|---|---|---|
| M1 | [m1-foundation.md](milestones/m1-foundation.md) | Skeleton, config, auth, API clients, `init`/`fetch`, read-only pull | done |
| M2 | [m2-sync-engine.md](milestones/m2-sync-engine.md) | Three-way merge, `diff`, MD→ADF/wiki, `push`, `status` | done |
| M3 | [m3-collaboration.md](milestones/m3-collaboration.md) | Comments, attachments, checklists, `create`/`transition`/`assign`/`log`/`resolve`/`clean` | done but for `search --pull` and per-field resolve |
| M4 | [m4-grouping-tui.md](milestones/m4-grouping-tui.md) | Grouping folders, link fallback, board moves, TUI | grouping done; TUI framework and the `create` form done; **other screens not built** |
| M5 | [m5-agent-json-polish.md](milestones/m5-agent-json-polish.md) | JSON schemas, agent docs, completion, hardening, release | contract, security and the checklist adapter done; **packaging not built** |
| M6 | [m6-datacenter-spec-client.md](milestones/m6-datacenter-spec-client.md) | Data Center client rewritten against the server's own API description (10.7.4 WADL): route table + contract test, assignable users, complete comments, assignee route, screen checks, floor 10.0 | **planned** |

## Where the project stands

Working end to end against recorded fixtures for **both Cloud and Data Center**:

```
init → pull → edit → status/diff → push --dry-run → push
```

plus `comment`, `transition`, `assign`, `create`, `attach`,
`attachments pull`, `log`, `search`, `resolve`, `clean`, `doctor`, `edit`, `config`,
`whoami`, `projects`/`boards`/`sprints`, `open`, `completion`, `schema` and
`init-agents`.

`init` is a line-based wizard rather than the planned TUI screen: it guesses the
deployment from the URL, the credential from the deployment, offers to pin an
untrusted TLS certificate after showing it, and writes a config file documenting
every setting — so no later command has to ask the user anything.

Guards that hold the design in place, each verified rather than asserted:

| Guard | What it prevents |
|---|---|
| Property suite on conversion idempotence | A pull that rewrites files it did not need to, or a push that reshapes an issue |
| Raw-block reassembly from stored originals | An untouched panel, macro or media node changing on push |
| Schema drift test | A command shipping a JSON document agents cannot validate |
| Agent-docs drift test | Docs that no longer describe the binary |
| Leak tests (themselves leak-tested) | A credential reaching output, a fixture or a config file |
| Scale benchmark | Silent quadratic behaviour as a workspace grows |

**Not built:** the TUI, release packaging, and
recording fixtures from real instances (`jired-record` exists but needs
credentials to run).

## Tracking conventions

- Every task has an ID (`M1.3`) and a checkbox list. Tick boxes in the milestone file
  in the same PR that implements them; reference the ID in commit messages
  (`feat(config): resolver precedence chain [M1.3]`).
- A task is done only when its **Done when** line holds (tests included).
- A milestone is done when its *Acceptance criteria* checklist at the top of its file
  is fully ticked.

## Key decisions (summary)

Full rationale lives with each decision ID in the reference docs.

| ID | Decision |
|---|---|
| D1 | All machine state in `.jired/` (`session.db`, `state.db`, `config.toml`, `logs/`, `lock`) |
| D2/D3 | Comments inline under `## Comments`; no truncation in v1 |
| D4 | Board symlink move = staged transition, applied on push ⚠ |
| D5 | Attachments lazy by default; auto-download ≤ 256 KiB |
| D6 | Frontmatter `summary` is source of truth over `#` heading |
| D7 | Summary change → jired renames file after confirmed push |
| D8 | Optimistic concurrency = GET `updated` → compare base → PUT (no server CAS exists) |
| D9/D10 | `comrak` for Markdown AST; `serde-yaml-ng` instead of archived `serde_yaml` |
| D11 | Headless secrets: scrypt(`JIRED_SECRET_KEY`) encrypted blob; plaintext only with `--insecure-store` ⚠ |
| D12 | Checkbox→done-transition off until `children.done_transition` configured ⚠ |
| D13 | Grouping roots: `by-parent/`, `sprints/`, `boards/` |
| D14 | Temp keys `NEW-<6 hex>` |
| D15 | Exit codes: 0 ok, 1 generic, 2 usage, 3 auth, 4 network, 5 conflict, 6 lossy-blocked, 7 missing value (non-interactive), 8 locked, 9 corrupt state, 10 unsupported server |
| D16 | tokio + async reqwest internally, sync command facade |
| D17 | Two DBs: `session.db` precious/0600, `state.db` deletable cache (+ push journal) |
| D18 | Change detection by content hash + parsed field compare, never mtime alone |
| D19 | JSON outputs carry `"schema": "jired/<name>/1"` |
| D20 | DC floor: Jira ≥ 10.0 — settled by M6, which checks every Data Center route against the server's own 10.7.4 description |
| D22 | UTC RFC3339 in all serialized data; local TZ only for display |
| D23 | Push order per issue: create → fields → description → transitions → comments → attachments; parents before children |
| D25 | Hand-written wiki⇄AST converter sharing `JiredAst` with ADF |
| D26 | Client rate limiter (8 rps Cloud / 4 rps DC) + `Retry-After` backoff, 5 retries |
| D27 | Data Center routes come from a hand-written table (`src/api/datacenter/routes.rs`), named by the WADL's own method ids and checked against the vendored 10.7.4 description in CI; no codegen, since the WADL carries no payload schema |

## Open `[NEEDS CONFIRMATION]` items (⚠ above)

1. **D4** — board symlink move staged as transition (vs ignored/rejected)?
2. **D11** — headless fallback requires `JIRED_SECRET_KEY` (else explicit `--insecure-store`)?
3. **D12** — checkbox→transition disabled by default?
4. **gitignore** — should attachment dot-folders default to gitignored?
