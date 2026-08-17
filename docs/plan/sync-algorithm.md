# Sync & conflict algorithm

Three states: **remote** (server, observable only via network), **base** (last synced
snapshot in `state.db`), **working copy** (Markdown files). `fetch` updates the
remote-tracking snapshot; `pull` = fetch + three-way merge; `push` = apply
working-copy delta with optimistic concurrency, then advance base.

Change detection: per-section content hash + parsed field comparison (D18); mtime
only as a fast-path skip. Fast path: skip issues where file hash == `base_md_hash`
**and** `remote_updated == base_updated`.

## Pull (three-way merge)

```text
pull(issue):
  base    = state.base_raw / base_desc
  remote  = state.remote_raw / remote_desc
  working = parse(file)                    # frontmatter fields + body sections

  for each field f in editable_fields ∪ {each comment} ∪ {description}:
    lb, lr, lw = base[f], remote[f], working[f]
    if lr == lb and lw == lb: keep lb
    elif lr == lb:            keep lw                    # local-only change (preserved)
    elif lw == lb:            take lr                    # remote-only change
    elif lw == lr:            take lr                    # both changed identically
    else:                     CONFLICT(f)                # both diverged

  description granularity: convert base/remote/working bodies to JiredAst,
    line-diff base→remote and base→working (`similar`); non-overlapping hunks
    auto-merge; overlapping hunks → conflict.

  strategy=abort (default): any conflict anywhere → write NOTHING, record rows in
    `conflict` table, exit 5 listing files/fields.
  strategy=merge: write body with git-style markers
      <<<<<<< LOCAL / ======= / >>>>>>> REMOTE (PROJ-142@2026-08-16T14:03Z)
    field conflicts serialized as `field: {ours: X, theirs: Y}  # jired-conflict`
    and surfaced in `status`.
  strategy=theirs / ours / --force: pick one side wholesale.

  after clean merge: projector writes file (rename if summary changed remotely,
    matching by KEY prefix), updates symlinks/groups, sets base := remote,
    records base_md_hash.
```

**Conflict definition:** the same field/hunk changed in both working copy and remote
relative to base. Comments are individually versioned (own `updated`) and count as
fields (D21).

## Push (optimistic concurrency + journal)

```text
push(issue):
  ops = plan(working vs base)          # D23 order: create → fields → description
                                       # → transitions → comments → attachments;
                                       # issues ordered parents-before-children
  journal: insert pending_op rows (status='pending') in one txn
  live = GET issue (fields=updated)    # concurrency check (D8)
  if live.updated != base.base_updated and not --force:
      mark issue failed(stale) → contributes exit 5; instruct `jired pull`; SKIP issue
  for op in ops:
      execute; on success mark done
      on failure: mark failed + record error; ABORT remaining ops for this issue
                  (base NOT advanced; file untouched; issue still dirty)
  if all ops done:
      re-GET issue → base := response
      rewrite file (new `updated`, real comment/issue IDs, rename if summary changed)
      clear this issue's pending_op rows
```

- Partial failure: exit 1 with per-issue results; `.state.db` never claims success
  that didn't happen (base advances only per confirmed issue).
- Crash recovery: `jired push --resume` replays `pending_op` rows still `pending`;
  `jired status` shows the journal.
- Comment updates additionally check the comment's own `base_updated`.
- Known limitation: Jira has no compare-and-set for issue edits, so a GET→PUT race
  window remains (documented; window minimized, PUT response `updated` re-verified).

## `jired resolve`

Field-level pick ours/theirs (TUI list or `--ours/--theirs [--field F]`), `$EDITOR`
for body hunks with markers. Non-interactive mode requires explicit flags, else
exit 7. No built-in hunk-merge TUI editor in v1 (D24).
