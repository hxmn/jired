# M4 — Grouping & TUI (size: M)

**Goal:** parent/sprint/board grouping folders regenerable from `state.db` on all
three OSes (with Windows fallback), board moves staged as transitions, and the
interactive TUI screens.

Depends on: M3 complete.

## Acceptance criteria

- [ ] `jired clean --regroup` reconstructs all grouping links from `state.db` on Linux, macOS, and Windows (with and without symlink privilege)
- [ ] Moving a board symlink between column folders stages a transition visible in `status` and `push --dry-run` (D4)
- [ ] `init` wizard, pickers, pull/push previews, and conflict resolver work interactively and degrade per spec in `--no-input`
- [ ] TUI writes nothing to stdout (stdout reserved for `--json`)

## Tasks

### M4.1 Link abstraction (projector)
- [ ] `links.mode = auto|symlink|hardlink|stub` config; auto: symlink → hardlink (same volume) → stub
- [ ] Stub format: `.md` with `link:` frontmatter + relative path line
- [ ] Windows detection of `ERROR_PRIVILEGE_NOT_HELD`; `\\?\` long-path handling
- [ ] All link artifacts tracked in projector so they are never treated as issues
- **Done when:** all three modes pass the same behavior suite; Windows CI runs both privilege states.

### M4.2 Parent/child grouping
- [ ] `by-parent/<Parent file name>/` folders with links to children (epics + subtasks)
- [ ] Updated on pull; parent rename tracks folder rename
- **Done when:** epic hierarchy from fixtures projects correctly and regenerates via `--regroup`.

### M4.3 Sprint grouping
- [ ] `sprints/<Sprint Name>/` links; closed sprints skipped by default (`sprints.include_closed`)
- [ ] Name-collision disambiguation with ` (id)`
- **Done when:** sprint fixture set projects and respects the closed-sprint default.

### M4.4 Board grouping + move staging (D4)
- [ ] `boards/<Board>/<Column>/` links from board config + issue status
- [ ] Detect link moved between column folders → stage transition (resolved via status-mapping of target column)
- [ ] Staged move shown in `status` and `push --dry-run`; ambiguous column→status mapping → warning, move ignored
- **Done when:** move-detection E2E passes and push applies the staged transition.

### M4.5 `clean --regroup` + orphan link handling
- [ ] Full regeneration of all grouping trees from `state.db` (groups are never source of truth)
- [ ] Dangling links (target renamed/removed) repaired or removed
- **Done when:** deleting all grouping folders + `clean --regroup` restores them byte-for-byte.

### M4.6 TUI framework
- [ ] Shared ratatui component kit: list+fuzzy filter, form, diff pane, confirm dialog; keys `jk`/arrows, `/`, `enter`, `q`/`esc`, `?`
- [ ] Prompt widget as the last rung of the config resolver (exit 7 in `--no-input`)
- [ ] All TUI output on the alternate screen / stderr; stdout untouched
- **Done when:** component kit has smoke tests via `ratatui` TestBackend.

### M4.7 TUI screens
- [ ] Init wizard: url → auth kind → creds → verify → project picker → options
- [ ] Project/board/sprint pickers (shared with `list --pick` flows and `assign` user picker)
- [ ] Pull preview: issue tree with changed fields; accept/abort
- [ ] Push preview: op list with per-op diff; confirm (requires `--yes` when non-interactive)
- [ ] Conflict resolver: conflict list → per-field ours/theirs, `e` → `$EDITOR` for body
- [ ] Each screen's `--no-input` degradation matches the spec table
- **Done when:** wizard-driven init on a clean machine reaches a working pull; degradation tests pass.
