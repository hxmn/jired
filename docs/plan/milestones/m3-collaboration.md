# M3 — Collaboration: comments, attachments, checklists, issue lifecycle (size: L)

**Goal:** the full agent workflow recipe
(`status → pull → edit → diff → push --dry-run → push`) passes end-to-end on both
deployments, including comments, attachments, subtask checklists, and issue
creation/transition.

Depends on: M2 complete.

## Status

Delivered except `search --pull`, per-field conflict resolution, and the
subtask-authoring half of checklists; each is unticked below and reports itself
as unimplemented rather than doing something approximate.

## Acceptance criteria

- [x] Add / edit / delete (gated) comments round-trip on Cloud and DC
- [x] Attachment stubs, `attachments pull`, and new-file upload work with sha256 verification
- [x] `create` produces a `NEW-*` file that becomes a real issue (file renamed, mapping reported) on push
- [x] Subtask checklist renders and (with `checklist.done_transition` configured) checking a box transitions the subtask
- [x] `resolve` clears recorded conflicts non-interactively
- [x] Full agent workflow E2E test green against fixtures for Cloud and DC

## Tasks

### M3.1 Comments — pull & render
- [x] Fetch comments into `comment` table (own `updated` versioning, D21)
- [x] Render `### [c-ID] Author — timestamp` sections; comment bodies through the same conversion pipeline (raw blocks in `comment:<id>` containers)
- [x] Three-way merge treats each comment as a field
- **Done when:** comment corpora render and re-parse losslessly on both deployments.

### M3.2 Comments — push
- [x] New-comment creation from `### [new]` (heading rewritten with real ID after push)
- [x] Comment edit → update with per-comment stale check; permission failure surfaces server error without aborting other ops
- [x] Deletion only with `--allow-comment-delete`; otherwise restore on pull + push warning
- [x] `jired comment KEY -m "text" [--push]` convenience command
- **Done when:** comment CRUD fixture tests pass incl. the gated-delete path.

### M3.3 Attachments
- [x] Stub files (`<name>.jiredstub` JSON) + auto-download ≤ `attachments.auto_download_max` (256 KiB default)
- [x] `jired attachments pull [KEY…] [--max-size N]`: streamed download, sha256 verify, stub replacement
- [x] Upload of new files in the dot-folder on push (MIME sniff, `attachments.max_upload` cap checked before the copy)
- [x] Auto-maintained `## Attachments` section; dot-folder rename tracking
- [x] `jired attach KEY FILE…`
- **Done when:** stub→download→upload cycle passes E2E with hash verification.

### M3.4 Checklists (cases a + b)
- [x] (a) Subtasks rendered `- [ ] KEY Title` under `## Checklist`
- [ ] Subtask title edits push the summary; new unprefixed lines create subtasks
- [x] Checkbox → `checklist.done_transition` mapping, **off by default** (D12); preview in `push --dry-run`
- [ ] (b) ADF taskList in description round-trips as GFM task list; checked-state pushes
- **Done when:** both cases pass fixture tests; unchecked default confirmed by test.

### M3.5 `jired create`
- [x] Template Markdown files (`.jired/templates/`, `--template`), `NEW-<6hex>` keys (D14)
- [x] Push: create API → real key → file+folder rename → `new_key` in `jired/push/1` output
- [x] Locally created issues push before existing ones, so a parent or epic reference resolves to a real key
- **Done when:** creating an epic + child in one push links them correctly.

### M3.6 `jired transition` / `jired assign`
- [x] `transition --to NAME [--fields k=v]`: validate against `transition_cache` (refresh on miss); missing required fields → exit 2 with machine-readable list
- [x] `assign [--me|--user Q]` with user search; ambiguity → picker or exit 7
- **Done when:** invalid transition and required-field cases produce the specified errors.

### M3.7 `jired search` / `jired log` / `jired open`
- [x] `search --jql [--limit]` table + `jired/search/1` JSON
- [ ] `search --pull` to bring matches into the workspace
- [x] `log KEY [--limit] [--refresh]`, cached per issue. Fetched on demand rather than with every `fetch`: changelogs are large and rarely wanted, and pulling them wholesale is the difference between a fast sync and a slow one
- [x] `open KEY` via `open` crate; URL printed in JSON/non-TTY
- **Done when:** all three pass fixture-backed E2E tests offline where applicable.

### M3.8 `jired resolve` (non-TUI)
- [x] List recorded conflicts; `--ours`/`--theirs` re-run the same three-way merge under that strategy; `--done` accepts a hand-merged file, refusing it while markers remain
- [ ] Per-field resolution (`--field F`); reports that it is unimplemented rather than resolving the rest of the issue behind the user's back
- [ ] Body hunks: launch `$EDITOR` on the marker file (today: edit by hand, then `resolve --done`, which validates the markers are gone)
- [x] Resolution updates working copy + `conflict.resolved`; `status` reflects it
- **Done when:** a conflicted workspace can be fully resolved non-interactively and then pushed.

### M3.9 `jired clean`
- [x] Detect orphans (files/stubs whose issues left scope or were deleted remotely)
- [x] `--dry-run` preview; never removes dirty files without `--force`
- **Done when:** orphan scenarios (deleted remotely, JQL scope shrink) clean correctly.
