# M5 — Agent support, JSON finalization, polish, 1.0 (size: M)

**Goal:** stable machine contracts (schemas validated in CI), generated agent docs,
shell completion, plugin-checklist read support, security sign-off, release
packaging.

Depends on: M4 complete.

## Status

The machine-readable contract is in place: every document jired can emit has a
published schema, a drift guard fails the build if a new one appears without
schema, and the generated agent docs cannot omit a command, setting or exit
code.

Security is signed off: threat model published in [docs/security.md](../../security.md),
leak guards verified by injecting a leak, `cargo deny` clean, and the scale
benchmark run and recorded.

Not done: the checklist-plugin adapter (M5.4), progress reporting for long
fetches, and release packaging (M5.7).

## Acceptance criteria

- [x] Every command's `--json` output validates against its shipped schema in CI (E2E harness asserts it)
- [x] `CLAUDE.md`/`AGENTS.md` are generated, complete, and kept in sync from one source of truth
- [x] Security checklist signed off (redaction audit, file modes, gitignore, threat-model docs)
- [ ] Release artifacts build for Linux/macOS/Windows

## Tasks

### M5.1 JSON schema finalization + `jired schema`
- [x] Publish a `jired/*/1` schema for all 28 documents, embedded in the binary
- [x] `jired schema [name]` prints them; deterministic ordering everywhere audited
- [x] Drift guard: a test scans the source for `emit(...)` calls and fails if any document has no published schema, so a new command cannot ship without one
- [x] 24 command outputs validated against their schemas
- **Done when:** the CI schema gate is red on any accidental output change.

### M5.2 Agent docs generation
- [x] `CLAUDE.md`/`AGENTS.md` generated from the CLI definition, the env-var table and the exit-code table
- [x] Drift guard: every command, setting and exit code must appear in the generated docs
- [x] Content: file/folder conventions, frontmatter schema + editability, comments/checklist how-to, full command reference with `--json`/`--no-input`, exit codes, safe-workflow recipe (`status → pull → edit → diff → push --dry-run → push`)
- [x] `jired agent-docs` regenerates only between sentinel markers; `init` writes initial docs
- [x] Doc examples use `$JIRED_TOKEN` only (never literals) — enforced by test
- **Done when:** docs regenerate reproducibly and a drift test compares them against the CLI's introspected surface.

### M5.3 Shell completion
- [x] `jired completion bash|zsh|fish|powershell` via `clap_complete`
- **Done when:** completions generate and smoke-load in bash and zsh CI steps.

### M5.4 Checklist plugin adapter (read-only)
- [ ] `ChecklistAdapter` trait keyed on custom-field schema detection
- [ ] "Checklist for Jira" read-only rendering under `## Checklist` (clearly marked read-only; edits warn)
- **Done when:** plugin fixture renders and local edits produce the warning, not a push.

### M5.5 Security hardening & audit
- [x] Redaction audit: grep-based leak tests over logs, JSON output, error chains for all fixture secrets
- [x] File-mode enforcement tests (0700/0600) incl. warn+fix path
- [x] Threat-model doc for the encrypted-blob fallback published in user docs (D11)
- [x] `--insecure` red-warning-on-every-run verified
- [x] `cargo deny` advisories clean; dependency review
- **Done when:** the security checklist in [security-testing.md](../security-testing.md) is fully ticked.

### M5.6 Performance & large-workspace polish
- [x] Scale benchmark (2000 issues, extrapolating cleanly to 10k): numbers recorded in [security-testing.md](../security-testing.md); the dirty-scan target is met with room to spare
- [x] state.db size measured and documented (~2.1 KiB/issue)
- [ ] `fetch --slim` behaviour documented
- [ ] Progress bars/backoff display for long fetches (stderr only)
- **Done when:** benchmarks recorded in CI and within targets.

### M5.7 Release engineering
- [ ] Versioning + CHANGELOG; `--version` includes deployment-support matrix
- [ ] cargo-dist (or equivalent) artifacts: Linux (musl), macOS (universal), Windows
- [ ] README quickstart (human + agent), man page from clap
- [ ] Weekly live-sandbox fixture re-verification job enabled
- **Done when:** tagged release produces installable artifacts for all three OSes.
