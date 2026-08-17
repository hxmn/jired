# M4 — Grouping & TUI (size: M)

**Goal:** parent/sprint/board grouping folders regenerable from `state.db` on all
three OSes (with Windows fallback), board moves staged as transitions, and the
interactive TUI screens.

Depends on: M3 complete.

## Status

Grouping is delivered: `by-parent/`, `sprints/` and `boards/` are rebuilt from
`state.db` on every pull and by `clean --regroup`, links degrade
symlink → hard link → stub, and dragging a card between board columns stages a
transition that `status` and `push --dry-run` both account for.

The TUI (M4.6, M4.7) is not built. Every command works non-interactively today,
so nothing is blocked on it; what is missing is the interactive affordance.

## Acceptance criteria

- [x] `jired clean --regroup` reconstructs all grouping links from `state.db`
- [ ] Verified on macOS and Windows, and without symlink privilege (needs CI to run)
- [x] Moving a board symlink between column folders stages a transition visible in `status` and `push --dry-run` (D4)
- [ ] `init` wizard, pickers, pull/push previews, and conflict resolver work interactively and degrade per spec in `--no-input`
- [ ] TUI writes nothing to stdout (stdout reserved for `--json`)

## Tasks

### M4.1 Link abstraction (projector)
- [x] `links.mode = auto|symlink|hardlink|stub` config; auto: symlink → hardlink (same volume) → stub
- [x] Stub format: `.md` with `link:` frontmatter + relative path line
- [x] Windows falls back automatically when a symlink is refused (auto mode tries the next kind rather than reporting the error)
- [ ] `\\?\` long-path handling
- [x] All link artifacts tracked in projector so they are never treated as issues
- **Done when:** all three modes pass the same behavior suite; Windows CI runs both privilege states.

### M4.2 Parent/child grouping
- [x] `by-parent/<Parent file name>/` folders with links to children (epics + subtasks)
- [x] Updated on pull; parent rename tracks folder rename
- **Done when:** epic hierarchy from fixtures projects correctly and regenerates via `--regroup`.

### M4.3 Sprint grouping
- [x] `sprints/<Sprint Name>/` links; closed sprints skipped by default (`sprints.include_closed`)
- [x] Name-collision disambiguation with ` (id)`
- **Done when:** sprint fixture set projects and respects the closed-sprint default.

### M4.4 Board grouping + move staging (D4)
- [x] `boards/<Board>/<Column>/` links from board config + issue status
- [x] Detect link moved between column folders → stage transition (resolved via status-mapping of target column)
- [x] Staged move shown in `status` and `push --dry-run`; ambiguous column→status mapping → warning, move ignored
- **Done when:** move-detection E2E passes and push applies the staged transition.

### M4.5 `clean --regroup` + orphan link handling
- [x] Full regeneration of all grouping trees from `state.db` (groups are never source of truth)
- [x] Dangling links (target renamed/removed) repaired or removed
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
