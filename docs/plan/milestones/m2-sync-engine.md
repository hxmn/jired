# M2 — Sync engine: merge, diff, push (size: XL)

**Goal:** the full git-like loop for fields, summary, and description:
edit locally → `diff` → `pull` merges cleanly or conflicts loudly → `push` with
optimistic concurrency and byte-identical round-trip of untouched lossy regions.

Depends on: M1 complete.

## Status

Delivered. The conversion round-trip is pinned by a property suite that asserts
*idempotence* rather than raw identity: each format normalizes (ADF flattens
mark sets, wiki mentions carry no display name, Markdown recognises bare issue
keys), so what matters is that a second pass changes nothing — otherwise every
pull would rewrite files and every diff would show phantom changes. That suite
found three real wiki-converter defects, all fixed: literal `[`/`{` re-parsing
as links and macros, `|` in a link label splitting the link, and prose starting
with a block marker becoming a table or list.

## Acceptance criteria

- [x] Local edit + non-conflicting remote edit → `pull` merges both, no data loss
- [x] Local edit + conflicting remote edit → `pull` exits 5 writing nothing (default) or writes markers (`--strategy merge`)
- [x] `diff` works in all three modes, offline, with `--stat` and `--json`
- [x] `push` round-trips an untouched lossy region byte-identically (fixture asserts request body)
- [x] Stale remote → `push` refuses with exit 5 unless `--force`; partial failure never advances base for failed issues
- [x] Property-based round-trip suites (MD⇄AST, ADF⇄AST, wiki⇄AST) green in CI

## Tasks

### M2.1 Working-copy parser
- [x] Frontmatter parse with editable/RO classification; RO edits → warning; `key`/`id` edits → error
- [x] Section splitter by fixed headings (Description/Checklist/Comments/Attachments), tolerant of missing sections
- [x] Comment-section parser (`[c-ID]`, `[new]`, deleted detection)
- [x] Markdown → JiredAst (comrak) incl. raw-marker reassembly from `raw_block`
- [x] Malformed-file taxonomy: recoverable warnings vs hard errors (exit 1 with file+line)
- **Done when:** every M1 rendered file re-parses to an AST equal to what produced it.

### M2.2 Change detection
- [x] Per-section content hashes; full-workspace dirty scan
- [ ] mtime fast-path skip (hash-only today; measure before adding)
- [x] Parsed field-by-field comparison vs base (incl. `custom:` passthrough)
- [ ] `summary` change → rename intent; heading/frontmatter divergence warning (D6)
- **Done when:** dirty scan of 1k files completes < 1s and reports exactly the edited fields.

### M2.3 Three-way merge (fields + body)
- [x] Field-level merge per pseudocode in [sync-algorithm.md](../sync-algorithm.md)
- [x] Body hunk merge: base→remote and base→working diffs (`similar`), non-overlapping auto-merge, overlap → conflict
- [x] Conflict recording in `conflict` table; deterministic conflict listing
- [x] Strategies: abort (default), `theirs`, `ours`, `merge` (git-style body markers), `--force`
- [ ] `# jired-conflict` frontmatter syntax for conflicting *fields* (body hunks only today)
- [x] Unit tests: full lb/lr/lw truth table per field type + hunk overlap cases
- **Done when:** the truth-table test and marker-format snapshots pass.

### M2.4 Full `jired pull`
- [x] Wire merge into pull; abort-before-any-write guarantee (all-or-nothing per run)
- [x] Change summary output (added/modified/deleted, changed fields) + `jired/pull/1` JSON
- [x] Remote rename + local-edit preservation (KEY-prefix matching) under merge
- [x] E2E fixture tests for the two acceptance scenarios
- **Done when:** acceptance scenarios 1–2 pass end-to-end.

### M2.5 `jired diff`
- [x] Modes: default (working↔base), `--remote` (base↔remote), `--all` (working↔remote)
- [x] Unified diff for bodies, field table for metadata, `--stat`, `--exit-code`
- [x] `jired/diff/1` JSON (per-issue fields + body hunks)
- [ ] Snapshot tests for all three modes
- **Done when:** diff output is deterministic and validated against its schema.

### M2.6 Markdown → ADF / wiki (write path)
- [x] JiredAst → ADF serializer; JiredAst → wiki printer
- [x] Raw-block reassembly: push builds lossy regions from `raw_block.node_json`, never file text
- [x] Property tests (proptest): random AST → MD → AST equal; random ADF → AST → ADF byte-equal; random wiki → AST → wiki equal
- [x] Golden corpus: byte-identical ADF for documents containing every supported node
- **Done when:** property suites run 1k cases each in CI without shrink failures.

### M2.7 Lossy-region edit detection
- [x] Render-time hash per marker region; push-time comparison
- [x] Edited/deleted region without `--allow-lossy` → exit 6 with `{issue, region, node_type}` details
- [ ] `--allow-lossy`: best-effort conversion, raw_block dropped, loss diff in `--dry-run`
- [x] `jired.lossy` frontmatter flag maintenance
- **Done when:** editing inside a panel blocks push with exit 6 and a correct JSON detail payload.

### M2.8 Push planner & journal
- [x] Op planner: working-vs-base delta → ordered ops (D23)
- [ ] Parents-before-children ordering (needs `create_issue`, M3.5)
- [x] `pending_op` journal writes (single txn) + `--resume` replay
- [x] `--dry-run`: exact API calls + diffs, human and `jired/push/1` JSON (identical op list to real run)
- **Done when:** dry-run and real run produce the same op list on fixtures; kill -9 mid-push then `--resume` completes.

### M2.9 Push executor (fields, summary, description, transitions-from-status)
- [x] Per-issue concurrency check (GET `updated` vs `base_updated`; `--force` bypass)
- [x] Execute field PUT, description PUT, staged status transition; abort issue on first op failure
- [x] Per-issue base advancement: re-GET → base := response → rewrite file (new `updated`) → clear journal rows
- [x] Rename-on-push after summary change (file + attachment folder, D7)
- [x] Partial-failure reporting: per-issue results, exit 1/5 semantics
- [x] Fixture tests: stale refusal, mid-push failure + `--resume`, byte-identical lossy body
- **Done when:** acceptance criteria 4–5 pass end-to-end (Cloud fixtures today; DC fixtures pending M1.12).

### M2.10 `jired status`
- [x] Dirty files, new local issues, staged transitions, conflicts, last-fetch age, pending journal
- [x] `jired/status/1` JSON; fully offline
- **Done when:** status output matches injected workspace states in E2E tests.
