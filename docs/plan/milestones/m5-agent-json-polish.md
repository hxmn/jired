# M5 — Agent support, JSON finalization, polish, 1.0 (size: M)

**Goal:** stable machine contracts (schemas validated in CI), generated agent docs,
shell completion, plugin-checklist read support, security sign-off, release
packaging.

Depends on: M4 complete.

## Acceptance criteria

- [ ] Every command's `--json` output validates against its shipped schema in CI (E2E harness asserts it)
- [ ] `CLAUDE.md`/`AGENTS.md` are generated, complete, and kept in sync from one source of truth
- [ ] Security checklist signed off (redaction audit, file modes, gitignore, threat-model docs)
- [ ] Release artifacts build for Linux/macOS/Windows

## Tasks

### M5.1 JSON schema finalization + `jired schema`
- [ ] Freeze `jired/*/1` schemas (init, fetch, pull, push, diff, status, search, error, frontmatter, …) as embedded JSON Schema documents
- [ ] `jired schema [name]` prints them; deterministic ordering everywhere audited
- [ ] CI gate: every E2E JSON output validated with `jsonschema`
- **Done when:** the CI schema gate is red on any accidental output change.

### M5.2 Agent docs generation
- [ ] `CLAUDE.md`/`AGENTS.md` generated from the same source of truth as `--help`, env-var list, exit-code table, frontmatter editability table
- [ ] Content: file/folder conventions, frontmatter schema + editability, comments/checklist how-to, full command reference with `--json`/`--no-input`, exit codes, safe-workflow recipe (`status → pull → edit → diff → push --dry-run → push`)
- [ ] `jired agent-docs` regenerates only between sentinel markers; `init` writes initial docs
- [ ] Doc examples use `$JIRED_TOKEN` only (never literals) — enforced by test
- **Done when:** docs regenerate reproducibly and a drift test compares them against the CLI's introspected surface.

### M5.3 Shell completion
- [ ] `jired completion bash|zsh|fish|powershell` via `clap_complete`
- **Done when:** completions generate and smoke-load in bash and zsh CI steps.

### M5.4 Checklist plugin adapter (read-only)
- [ ] `ChecklistAdapter` trait keyed on custom-field schema detection
- [ ] "Checklist for Jira" read-only rendering under `## Checklist` (clearly marked read-only; edits warn)
- **Done when:** plugin fixture renders and local edits produce the warning, not a push.

### M5.5 Security hardening & audit
- [ ] Redaction audit: grep-based leak tests over logs, JSON output, error chains for all fixture secrets
- [ ] File-mode enforcement tests (0700/0600) incl. warn+fix path
- [ ] Threat-model doc for the encrypted-blob fallback published in user docs (D11)
- [ ] `--insecure` red-warning-on-every-run verified
- [ ] `cargo deny` advisories clean; dependency review
- **Done when:** the security checklist in [security-testing.md](../security-testing.md) is fully ticked.

### M5.6 Performance & large-workspace polish
- [ ] 10k-issue fixture benchmark: fetch, dirty scan (<1s target), pull, status
- [ ] `fetch --slim` (no changelogs) documented; state.db size measured and documented
- [ ] Progress bars/backoff display for long fetches (stderr only)
- **Done when:** benchmarks recorded in CI and within targets.

### M5.7 Release engineering
- [ ] Versioning + CHANGELOG; `--version` includes deployment-support matrix
- [ ] cargo-dist (or equivalent) artifacts: Linux (musl), macOS (universal), Windows
- [ ] README quickstart (human + agent), man page from clap
- [ ] Weekly live-sandbox fixture re-verification job enabled
- **Done when:** tagged release produces installable artifacts for all three OSes.
