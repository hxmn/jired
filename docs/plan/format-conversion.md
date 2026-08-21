# Markdown format & conversion spec

## File naming

- `KEY. Summary.md` with the issue number zero-padded to four digits,
  e.g. `PROJ-0142. Fix login flow.md`, so a directory listing sorts numerically.
  The padding is a filename convention only; the key stays `PROJ-142`.
- Sanitization: strip `/\:*?"<>|` and control chars, collapse whitespace, trim
  trailing dots/spaces (Windows), cap at 120 chars with `…`.
- Identity is the `KEY.` prefix (fallback: frontmatter `key`) — never the full name.
  Rename detection on pull/push matches by prefix so edits survive renames.
- Attachments live in a hidden sibling folder: `.PROJ-0142. Fix login flow/`;
  the folder rename tracks file renames.
- Grouping folders (`by-parent/`, `sprints/`, `boards/`) are visible and can never
  collide with dot-prefixed attachment folders (D13). Sprint/board name collisions
  disambiguate with ` (id)`.

## Example file — `PROJ-0142. Fix login flow.md`

````markdown
---
# frontmatter exactly as specified in architecture.md
---

# Fix login flow

## Description

Login fails when SSO is enabled. See PROJ-99 for background.

<!--jired:raw:r1a2b3-->
> ⚠️ **[jired: Jira panel (warning) — read-only. Edit in Jira or delete whole block with --allow-lossy]**
> Do not deploy before the cert rotation.
<!--/jired:raw:r1a2b3-->

Steps:

- [ ] Reproduce with SAML tenant
- [x] Capture HAR file

## Checklist

- [ ] PROJ-143 Add integration test
- [x] PROJ-144 Update runbook

## Comments

### [c-10812] Jane Doe — 2026-08-15T10:32:00Z

Repro confirmed on staging. cc @[Ilya A](5b10a2…)

### [new]

Fixed by rotating the IdP cert — see attached HAR.

## Attachments

- [login-trace.har](.PROJ-0142. Fix login flow/login-trace.har) (2.1 MiB)
- [screenshot.png](.PROJ-0142. Fix login flow/screenshot.png) (stub — run `jired attachments pull PROJ-142`)
````

Fixed section order, parseable by heading: `# summary` → `## Description` →
`## Checklist` → `## Comments` → `## Attachments`. Frontmatter `summary` is the
source of truth over the `#` heading (D6).

## Conversion pipeline

`ADF ⇄ JiredAst ⇄ Markdown ⇄ JiredAst ⇄ wiki` — one internal AST (D25).

Bidirectionally supported: paragraphs; headings 1–6 (Jira headings demoted +2 under
the file skeleton, so Jira h1 renders `###`); bold/italic/strike/code/links;
bullet/ordered lists; task lists (ADF `taskList`; wiki has none → lossy); code
blocks with language; blockquotes; GFM tables; rules; mentions `@[Name](id)`
(accountId on Cloud, username on DC); emoji as unicode; bare issue keys ⇄ Jira
issue links.

## Lossy-region mechanism (the invariant)

Any node with no faithful Markdown form (panel, expand, media, layout, macro,
unknown) is stored **verbatim** in `state.db.raw_block` and rendered between
`<!--jired:raw:ID-->` … `<!--/jired:raw:ID-->` markers containing an approximate,
read-only rendering.

- On push, the region is reassembled **from `raw_block.node_json`, never from the
  file text** — an untouched region pushes back byte-identical *by construction*.
- The between-marker text is hashed at render time; if changed at push time →
  exit 6 unless `--allow-lossy` (then best-effort conversion, raw block dropped,
  loss diff shown in `--dry-run`).
- Deleted marker pair = node deletion, also gated by `--allow-lossy`.
- `jired.lossy: true` in frontmatter whenever a file contains ≥1 raw block.

Worked example: the warning panel above survives ten pull/push cycles byte-identical;
editing text inside it makes `push` fail with
`{"code":6,"details":{"issue":"PROJ-142","region":"r1a2b3","node_type":"panel"}}`.

## Comments

- `### [c-ID] Author — timestamp` per comment; body edit → comment update on push
  (permission failure surfaces the server error without aborting other ops).
- New comment: `### [new] optional-note`; heading rewritten with the real ID after push.
- Deletion requires `--allow-comment-delete`; otherwise a deleted section is
  restored on pull and push warns.
- Inline (not sidecar) chosen (D2): one issue = one file is the agent-usability
  contract; a sidecar doubles rename tracking and splits context.

## Checklists (three cases)

| Case | Behavior |
|---|---|
| (a) Subtasks | Rendered `- [ ] KEY Title` under `## Checklist`. Checking a box maps to the configured done-transition — **off** until `checklist.done_transition` is set (D12). Title edits push subtask summary; new unprefixed lines create subtasks. |
| (b) ADF task lists in description (Cloud) | Round-trip in place as GFM task lists; checked state pushes. |
| (c) Plugin checklists ("Checklist for Jira" etc.) | Behind a `ChecklistAdapter` trait keyed on custom-field schema; v1: detection + read-only rendering only. |

## Attachments

- Lazy by default (D5): metadata stubs (`<name>.jiredstub` JSON) until
  `jired attachments pull`; eager auto-download only ≤ `attachments.auto_download_max`
  (default 256 KiB).
- New files placed in the dot-folder are uploaded on push; upload cap
  `attachments.max_upload` (default 50 MiB, server limit re-checked).
- MIME via extension + magic-byte sniff; sha256 recorded and verified on download.
- `## Attachments` section is auto-maintained (regenerated on pull/push).
