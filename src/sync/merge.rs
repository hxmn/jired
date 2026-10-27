//! Three-way merge [M2.3].
//!
//! Field-level for frontmatter and comments, hunk-level for body text. A
//! conflict is the same field or the same region of text changed on both sides
//! relative to base — anything else merges without asking.

use crate::cli::cmd::pull::Strategy;
use similar::{DiffOp, TextDiff};

/// What a three-way comparison of one value says to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// Neither side moved.
    Unchanged,
    /// Only the working copy changed; keep it.
    KeepLocal,
    /// Only the server changed; take it.
    TakeRemote,
    /// Both changed, to the same value.
    Agreed,
    /// Both changed, differently.
    Conflict,
}

impl Decision {
    pub fn is_conflict(self) -> bool {
        self == Decision::Conflict
    }
}

/// Compare one field across base, remote, and working copy.
pub fn decide<T: PartialEq>(base: &T, remote: &T, working: &T) -> Decision {
    let remote_moved = remote != base;
    let local_moved = working != base;
    match (remote_moved, local_moved) {
        (false, false) => Decision::Unchanged,
        (false, true) => Decision::KeepLocal,
        (true, false) => Decision::TakeRemote,
        (true, true) if remote == working => Decision::Agreed,
        (true, true) => Decision::Conflict,
    }
}

/// Resolve a decision under a pull strategy, reporting whether it conflicted.
///
/// `Abort` is handled by the caller (it must stop before writing anything), so
/// here it behaves like "keep local" for the purpose of building the result.
pub fn apply<'a, T>(
    decision: Decision,
    remote: &'a T,
    working: &'a T,
    strategy: Strategy,
) -> &'a T {
    match decision {
        Decision::Unchanged | Decision::KeepLocal => working,
        Decision::TakeRemote | Decision::Agreed => remote,
        Decision::Conflict => match strategy {
            Strategy::Theirs => remote,
            _ => working,
        },
    }
}

/// One region of a three-way text merge.
#[derive(Debug, Clone, PartialEq)]
pub enum Hunk {
    Merged(Vec<String>),
    Conflict {
        base: Vec<String>,
        ours: Vec<String>,
        theirs: Vec<String>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct TextMerge {
    pub hunks: Vec<Hunk>,
}

impl TextMerge {
    pub fn has_conflict(&self) -> bool {
        self.hunks
            .iter()
            .any(|h| matches!(h, Hunk::Conflict { .. }))
    }

    /// Render with git-style markers; conflicts are labelled with the issue.
    pub fn render(&self, label: &str) -> String {
        let mut out: Vec<String> = Vec::new();
        for hunk in &self.hunks {
            match hunk {
                Hunk::Merged(lines) => out.extend(lines.iter().cloned()),
                Hunk::Conflict { ours, theirs, .. } => {
                    out.push("<<<<<<< LOCAL".to_string());
                    out.extend(ours.iter().cloned());
                    out.push("=======".to_string());
                    out.extend(theirs.iter().cloned());
                    out.push(format!(">>>>>>> REMOTE ({label})"));
                }
            }
        }
        out.join("\n")
    }

    /// Take one side wholesale, used by `--strategy ours|theirs`.
    pub fn render_side(&self, theirs: bool) -> String {
        let mut out: Vec<String> = Vec::new();
        for hunk in &self.hunks {
            match hunk {
                Hunk::Merged(lines) => out.extend(lines.iter().cloned()),
                Hunk::Conflict {
                    ours: o, theirs: t, ..
                } => out.extend(if theirs { t.clone() } else { o.clone() }),
            }
        }
        out.join("\n")
    }
}

/// A replacement of `base[start..end]` with `lines`.
#[derive(Debug, Clone)]
struct Change {
    start: usize,
    end: usize,
    lines: Vec<String>,
}

fn changes(base: &[&str], other: &str) -> Vec<Change> {
    let other_lines: Vec<&str> = other.lines().collect();
    let base_text = base.join("\n");
    let other_text = other_lines.join("\n");
    let diff = TextDiff::from_lines(&base_text, &other_text);

    let mut out = Vec::new();
    for op in diff.ops() {
        match *op {
            DiffOp::Equal { .. } => {}
            DiffOp::Delete {
                old_index, old_len, ..
            } => out.push(Change {
                start: old_index,
                end: old_index + old_len,
                lines: Vec::new(),
            }),
            DiffOp::Insert {
                old_index,
                new_index,
                new_len,
            } => out.push(Change {
                start: old_index,
                end: old_index,
                lines: slice_lines(&other_lines, new_index, new_len),
            }),
            DiffOp::Replace {
                old_index,
                old_len,
                new_index,
                new_len,
            } => out.push(Change {
                start: old_index,
                end: old_index + old_len,
                lines: slice_lines(&other_lines, new_index, new_len),
            }),
        }
    }
    out
}

fn slice_lines(lines: &[&str], start: usize, len: usize) -> Vec<String> {
    lines
        .iter()
        .skip(start)
        .take(len)
        .map(|s| s.to_string())
        .collect()
}

/// Three-way line merge of body text.
///
/// Changes that touch disjoint regions of base are both applied; changes whose
/// base ranges overlap become a conflict hunk.
pub fn merge_text(base: &str, ours: &str, theirs: &str) -> TextMerge {
    let base_lines: Vec<&str> = base.lines().collect();

    if ours == theirs {
        return TextMerge {
            hunks: vec![Hunk::Merged(ours.lines().map(str::to_string).collect())],
        };
    }
    if base == ours {
        return TextMerge {
            hunks: vec![Hunk::Merged(theirs.lines().map(str::to_string).collect())],
        };
    }
    if base == theirs {
        return TextMerge {
            hunks: vec![Hunk::Merged(ours.lines().map(str::to_string).collect())],
        };
    }

    let ours_changes = changes(&base_lines, ours);
    let theirs_changes = changes(&base_lines, theirs);

    let mut hunks: Vec<Hunk> = Vec::new();
    let mut pending: Vec<String> = Vec::new();
    let mut cursor = 0usize;
    let mut oi = 0usize;
    let mut ti = 0usize;

    while cursor < base_lines.len() || oi < ours_changes.len() || ti < theirs_changes.len() {
        let next_ours = ours_changes.get(oi);
        let next_theirs = theirs_changes.get(ti);

        let ours_here = next_ours.filter(|c| c.start <= cursor);
        let theirs_here = next_theirs.filter(|c| c.start <= cursor);

        match (ours_here, theirs_here) {
            (Some(o), Some(t)) => {
                // Overlapping base ranges, or two insertions at the same point
                // with different content: neither side can be applied blindly.
                let overlap = o.start.max(t.start) < o.end.min(t.end)
                    || (o.start == t.start && o.end == t.end);
                if overlap && o.lines != t.lines {
                    flush(&mut hunks, &mut pending);
                    hunks.push(Hunk::Conflict {
                        base: slice_lines(
                            &base_lines,
                            o.start.min(t.start),
                            o.end.max(t.end) - o.start.min(t.start),
                        ),
                        ours: o.lines.clone(),
                        theirs: t.lines.clone(),
                    });
                    cursor = o.end.max(t.end);
                    oi += 1;
                    ti += 1;
                } else if o.lines == t.lines {
                    pending.extend(o.lines.iter().cloned());
                    cursor = o.end.max(t.end);
                    oi += 1;
                    ti += 1;
                } else {
                    // Disjoint: apply the earlier one and re-evaluate.
                    let (change, is_ours) = if o.end <= t.start {
                        (o, true)
                    } else {
                        (t, false)
                    };
                    pending.extend(change.lines.iter().cloned());
                    cursor = change.end;
                    if is_ours {
                        oi += 1;
                    } else {
                        ti += 1;
                    }
                }
            }
            (Some(o), None) => {
                pending.extend(o.lines.iter().cloned());
                cursor = o.end;
                oi += 1;
            }
            (None, Some(t)) => {
                pending.extend(t.lines.iter().cloned());
                cursor = t.end;
                ti += 1;
            }
            (None, None) => {
                if cursor < base_lines.len() {
                    pending.push(base_lines[cursor].to_string());
                    cursor += 1;
                } else {
                    break;
                }
            }
        }
    }
    flush(&mut hunks, &mut pending);
    TextMerge { hunks }
}

fn flush(hunks: &mut Vec<Hunk>, pending: &mut Vec<String>) {
    if !pending.is_empty() {
        hunks.push(Hunk::Merged(std::mem::take(pending)));
    }
}

/// Collapse the conflict markers a merging pull wrote into `text` to one side.
///
/// For `resolve --ours`/`--theirs` after `pull --strategy merge`: by then base
/// has moved to the server's copy, so the merge cannot be run again, and the
/// markers are the only record left of which lines were whose.
pub fn take_side_of_markers(text: &str, theirs: bool) -> String {
    enum In {
        Neither,
        Ours,
        Theirs,
    }
    let mut at = In::Neither;
    let mut out: Vec<&str> = Vec::new();
    for line in text.lines() {
        match at {
            In::Neither if line.starts_with("<<<<<<< ") => at = In::Ours,
            In::Ours if line == "=======" => at = In::Theirs,
            In::Theirs if line.starts_with(">>>>>>> ") => at = In::Neither,
            In::Neither => out.push(line),
            In::Ours if !theirs => out.push(line),
            In::Theirs if theirs => out.push(line),
            _ => {}
        }
    }
    let mut joined = out.join("\n");
    if text.ends_with('\n') {
        joined.push('\n');
    }
    joined
}

/// True when a file still holds unresolved conflict markers.
pub fn has_markers(text: &str) -> bool {
    text.lines()
        .any(|l| l.starts_with("<<<<<<< ") || l.starts_with(">>>>>>> ") || l == "=======")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn field_decision_truth_table() {
        // base, remote, working
        assert_eq!(decide(&"a", &"a", &"a"), Decision::Unchanged);
        assert_eq!(decide(&"a", &"a", &"b"), Decision::KeepLocal);
        assert_eq!(decide(&"a", &"b", &"a"), Decision::TakeRemote);
        assert_eq!(decide(&"a", &"b", &"b"), Decision::Agreed);
        assert_eq!(decide(&"a", &"b", &"c"), Decision::Conflict);
    }

    #[test]
    fn strategy_decides_which_side_wins_a_conflict() {
        let remote = "theirs";
        let working = "ours";
        assert_eq!(
            *apply(Decision::Conflict, &remote, &working, Strategy::Theirs),
            "theirs"
        );
        assert_eq!(
            *apply(Decision::Conflict, &remote, &working, Strategy::Ours),
            "ours"
        );
        // Non-conflicting decisions ignore the strategy entirely.
        assert_eq!(
            *apply(Decision::TakeRemote, &remote, &working, Strategy::Ours),
            "theirs"
        );
        assert_eq!(
            *apply(Decision::KeepLocal, &remote, &working, Strategy::Theirs),
            "ours"
        );
    }

    #[test]
    fn identical_sides_merge_without_a_conflict() {
        let m = merge_text("a\nb\n", "a\nX\n", "a\nX\n");
        assert!(!m.has_conflict());
        assert_eq!(m.render("PROJ-1"), "a\nX");
    }

    #[test]
    fn one_sided_changes_are_applied() {
        let m = merge_text("a\nb\nc", "a\nb\nc", "a\nB\nc");
        assert!(!m.has_conflict());
        assert_eq!(m.render("PROJ-1"), "a\nB\nc");

        let m = merge_text("a\nb\nc", "a\nB\nc", "a\nb\nc");
        assert_eq!(m.render("PROJ-1"), "a\nB\nc");
    }

    #[test]
    fn disjoint_edits_on_both_sides_both_survive() {
        let base = "one\ntwo\nthree\nfour\nfive";
        let ours = "ONE\ntwo\nthree\nfour\nfive";
        let theirs = "one\ntwo\nthree\nfour\nFIVE";
        let m = merge_text(base, ours, theirs);
        assert!(!m.has_conflict(), "{:?}", m.hunks);
        let out = m.render("PROJ-1");
        assert!(out.contains("ONE"), "{out}");
        assert!(out.contains("FIVE"), "{out}");
    }

    #[test]
    fn overlapping_edits_conflict_and_render_markers() {
        let m = merge_text("a\nb\nc", "a\nOURS\nc", "a\nTHEIRS\nc");
        assert!(m.has_conflict());
        let text = m.render("PROJ-142@2026-08-16T14:03Z");
        assert!(text.contains("<<<<<<< LOCAL"));
        assert!(text.contains("OURS"));
        assert!(text.contains("======="));
        assert!(text.contains("THEIRS"));
        assert!(text.contains(">>>>>>> REMOTE (PROJ-142@2026-08-16T14:03Z)"));
        assert!(has_markers(&text));
    }

    #[test]
    fn a_conflict_can_be_collapsed_to_either_side() {
        let m = merge_text("a\nb\nc", "a\nOURS\nc", "a\nTHEIRS\nc");
        assert_eq!(m.render_side(false), "a\nOURS\nc");
        assert_eq!(m.render_side(true), "a\nTHEIRS\nc");
    }

    #[test]
    fn appending_on_both_sides_conflicts_rather_than_interleaving() {
        let m = merge_text("a", "a\nours", "a\ntheirs");
        assert!(m.has_conflict());
    }

    #[test]
    fn markers_collapse_to_either_side() {
        let m = merge_text("a\nb\nc", "a\nours\nc", "a\ntheirs\nc");
        let written = m.render("PROJ-1");
        assert_eq!(take_side_of_markers(&written, false), "a\nours\nc");
        assert_eq!(take_side_of_markers(&written, true), "a\ntheirs\nc");
        assert_eq!(take_side_of_markers("plain\n", true), "plain\n");
    }

    #[test]
    fn clean_text_has_no_markers() {
        assert!(!has_markers("just\nsome\ntext"));
        assert!(has_markers("a\n=======\nb"));
    }
}
