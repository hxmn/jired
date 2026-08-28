# Working in this repository

This is jired's own source tree, not a jired workspace. (`jired init-agents`
writes a *different* `CLAUDE.md`, into the workspace where issue files live.
This file is for people and agents changing jired itself.)

## Every push to `main` ships a version

A commit that reaches `main` is a release: there is no separate release branch,
so whatever is on `main` is what `cargo install --git` builds. Before pushing:

1. Bump `version` in `Cargo.toml`.
2. Add a matching, dated section to `CHANGELOG.md` — `## [0.3.0] — 2026-09-14`.
   Never leave the work under `## Unreleased`; the version is decided at push
   time, not later.
3. Run `cargo build` so `Cargo.lock` records the new version too, and commit it.

Which digit moves, [semver](https://semver.org) before 1.0:

| The change | Bump | Example |
|---|---|---|
| A command, flag, exit code or `--json` field changes meaning or spelling | minor | `agent-docs` renamed to `init-agents` |
| A new command, flag, or field | minor | `jired sprint add` |
| Behaviour fixed with no new surface | patch | the stale-push comparison |

Pre-1.0 there is no major bump: breaking changes go in the minor, which is why
the changelog's **Breaking** section is the first thing a reader sees in a
version.

Amending or rebasing before anything is pushed does not need a new version — the
rule is about what lands on `main`, not about how many commits it took. Two
change sets pushed separately are two versions.

## Everything a change touches

A change is not done when it compiles. The same tree holds the code, the
contract and the prose that describes both, and they are tested against each
other:

- **Tests.** Unit tests beside the code, end-to-end tests in `tests/e2e.rs`
  against the recorded fixture client (`JIRED_FIXTURES`). A bug fix gets a test
  that fails without it — check that it does.
- **Schemas.** Every `out.emit("name", …)` needs `schemas/name.json` registered
  in `cmd::schema::SCHEMAS`; `tests/schema_contract.rs` fails otherwise.
- **Generated docs.** `src/cli/cmd/init_agents.rs` renders `CLAUDE.md` and
  `AGENTS.md` for a workspace from the CLI's own definitions. A claim written
  there is a promise the code has to keep.
- **`README.md`** for people, **`docs/plan/commands.md`** for the specification.
- `cargo fmt` and `cargo clippy --all-targets` are clean before a push.

## House style

Comments say *why*, not *what*: the code already says what. A comment earns its
place by recording the reason a thing is the way it is — the failure it
prevents, the deployment that behaves differently, the alternative that was
tried. Match the density and voice of the file you are in.
