# M3 — Collaboration: comments, attachments, checklists, issue lifecycle (size: L)

**Goal:** the full agent workflow recipe
(`status → pull → edit → diff → push --dry-run → push`) passes end-to-end on both
deployments, including comments, attachments, subtask checklists, and issue
creation/transition.

Depends on: M2 complete.

## Acceptance criteria

- [ ] Add / edit / delete (gated) comments round-trip on Cloud and DC
- [ ] Attachment stubs, `attachments pull`, and new-file upload work with sha256 verification
- [ ] `create` produces a `NEW-*` file that becomes a real issue (file renamed, mapping reported) on push
- [ ] Subtask checklist renders and (with `checklist.done_transition` configured) checking a box transitions the subtask
- [ ] `resolve` clears recorded conflicts non-interactively and via `$EDITOR`
- [ ] Full agent workflow E2E test green against fixtures for Cloud and DC

## Tasks

### M3.1 Comments — pull & render
- [ ] Fetch comments into `comment` table (own `updated` versioning, D21)
- [ ] Render `### [c-ID] Author — timestamp` sections; comment bodies through the same conversion pipeline (raw blocks in `comment:<id>` containers)
- [ ] Three-way merge treats each comment as a field
- **Done when:** comment corpora render and re-parse losslessly on both deployments.

### M3.2 Comments — push
- [ ] New-comment creation from `### [new]` (heading rewritten with real ID after push)
- [ ] Comment edit → update with per-comment stale check; permission failure surfaces server error without aborting other ops
- [ ] Deletion only with `--allow-comment-delete`; otherwise restore on pull + push warning
- [ ] `jired comment KEY -m "text" [--push]` convenience command
- **Done when:** comment CRUD fixture tests pass incl. the gated-delete path.

### M3.3 Attachments
- [ ] Stub files (`<name>.jiredstub` JSON) + auto-download ≤ `attachments.auto_download_max` (256 KiB default)
- [ ] `jired attachments pull [KEY…] [--max-size N]`: streamed download, sha256 verify, stub replacement
- [ ] Upload of new files in dot-folder on push (MIME sniff, `attachments.max_upload` cap, server-limit re-check)
- [ ] Auto-maintained `## Attachments` section; dot-folder rename tracking
- [ ] `jired attach KEY FILE…`
- **Done when:** stub→download→upload cycle passes E2E with hash verification.

### M3.4 Checklists (cases a + b)
- [ ] (a) Subtasks rendered `- [ ] KEY Title` under `## Checklist`; title edits push subtask summary; new unprefixed lines create subtasks
- [ ] Checkbox → `checklist.done_transition` mapping, **off by default** (D12); preview in `push --dry-run`
- [ ] (b) ADF taskList in description round-trips as GFM task list; checked-state pushes
- **Done when:** both cases pass fixture tests; unchecked default confirmed by test.

### M3.5 `jired create`
- [ ] Template Markdown files (`.jired/templates/`, `--template`), `NEW-<6hex>` keys (D14)
- [ ] Push: create API → real key → file+folder rename → `new_key` in `jired/push/1` output
- [ ] Parents-before-children ordering with mixed new/existing issues
- **Done when:** creating an epic + child in one push links them correctly.

### M3.6 `jired transition` / `jired assign`
- [ ] `transition --to NAME [--fields k=v]`: validate against `transition_cache` (refresh on miss); missing required fields → exit 2 with machine-readable list
- [ ] `assign [--me|--user Q]` with user search; ambiguity → picker or exit 7
- **Done when:** invalid transition and required-field cases produce the specified errors.

### M3.7 `jired search` / `jired log` / `jired open`
- [ ] `search --jql [--limit] [--pull]` table + `jired/search/1` JSON
- [ ] `log KEY [--limit]` from cached changelog (`expand=changelog` in fetch)
- [ ] `open KEY` via `open` crate; URL printed in JSON/non-TTY
- **Done when:** all three pass fixture-backed E2E tests offline where applicable.

### M3.8 `jired resolve` (non-TUI)
- [ ] List recorded conflicts; `--ours/--theirs [--field F]` per-field resolution
- [ ] Body hunks: launch `$EDITOR` on marker file; validate markers removed on save
- [ ] Resolution updates working copy + `conflict.resolved`; `status` reflects it
- **Done when:** a conflicted workspace can be fully resolved non-interactively and then pushed.

### M3.9 `jired clean`
- [ ] Detect orphans (files/stubs whose issues left scope or were deleted remotely)
- [ ] `--dry-run` preview; never removes dirty files without `--force`
- **Done when:** orphan scenarios (deleted remotely, JQL scope shrink) clean correctly.
