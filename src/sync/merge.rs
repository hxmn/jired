//! Three-way merge [M2.3].
//!
//! Field-level for frontmatter and comments, hunk-level for body text. A
//! conflict is the same field or the same region of text changed on both sides
//! relative to base — anything else merges without asking.

use crate::sync::pull::Strategy;
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
    let (mut oi, mut ti) = (0usize, 0usize);

    loop {
        // The next change to start, from either side; an insertion goes before
        // a change that starts at the same line, as it reads in either file.
        let first = match (ours_changes.get(oi), theirs_changes.get(ti)) {
            (None, None) => break,
            (Some(o), None) => o,
            (None, Some(t)) => t,
            (Some(o), Some(t)) => {
                if (o.start, o.end) <= (t.start, t.end) {
                    o
                } else {
                    t
                }
            }
        };
        pending.extend(
            base_lines[cursor..first.start]
                .iter()
                .map(|l| l.to_string()),
        );

        // Every change from either side that reaches into the region grows it,
        // until neither side's next change does. Taking one side's change and
        // then the other's when they only partly overlapped moved the cursor
        // back, and base lines one side had deleted came back with no conflict.
        let mut region = Region {
            start: first.start,
            end: first.end,
        };
        let (o_from, t_from) = (oi, ti);
        loop {
            if let Some(o) = ours_changes.get(oi).filter(|c| region.reached_by(c)) {
                region.grow(o);
                oi += 1;
            } else if let Some(t) = theirs_changes.get(ti).filter(|c| region.reached_by(c)) {
                region.grow(t);
                ti += 1;
            } else {
                break;
            }
        }

        let ours_part = region.apply(&base_lines, &ours_changes[o_from..oi]);
        let theirs_part = region.apply(&base_lines, &theirs_changes[t_from..ti]);
        if o_from == oi || t_from == ti || ours_part == theirs_part {
            pending.extend(if o_from == oi { theirs_part } else { ours_part });
        } else {
            flush(&mut hunks, &mut pending);
            hunks.push(Hunk::Conflict {
                base: slice_lines(&base_lines, region.start, region.end - region.start),
                ours: ours_part,
                theirs: theirs_part,
            });
        }
        cursor = region.end;
    }
    pending.extend(base_lines[cursor..].iter().map(|l| l.to_string()));
    flush(&mut hunks, &mut pending);
    TextMerge { hunks }
}

/// The base lines `start..end` that a run of changes from both sides covers.
struct Region {
    start: usize,
    end: usize,
}

impl Region {
    /// Whether `c` touches lines this region already changes. Changes that only
    /// meet at a boundary are applied one after the other, as before; two
    /// insertions at the same point are one region, whose order nobody chose.
    fn reached_by(&self, c: &Change) -> bool {
        match (self.start == self.end, c.start == c.end) {
            (true, true) => c.start == self.start,
            (true, false) => c.start < self.start && self.start < c.end,
            (false, true) => self.start < c.start && c.start < self.end,
            (false, false) => c.start < self.end && self.start < c.end,
        }
    }

    fn grow(&mut self, c: &Change) {
        self.start = self.start.min(c.start);
        self.end = self.end.max(c.end);
    }

    /// The region as one side wrote it: its changes, and base between them.
    fn apply(&self, base: &[&str], side: &[Change]) -> Vec<String> {
        let mut out = Vec::new();
        let mut at = self.start;
        for c in side {
            out.extend(base[at..c.start].iter().map(|l| l.to_string()));
            out.extend(c.lines.iter().cloned());
            at = c.end;
        }
        out.extend(base[at..self.end].iter().map(|l| l.to_string()));
        out
    }
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

    fn twelve() -> String {
        (0..12)
            .map(|i| format!("b{i}"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn with(base: &str, edit: impl Fn(usize, &str) -> Vec<String>) -> String {
        let lines: Vec<String> = base
            .lines()
            .enumerate()
            .flat_map(|(i, l)| edit(i, l))
            .collect();
        lines.join("\n")
    }

    /// One side's change spanning lines where the other's starts: both reach
    /// the same base lines, so it is a conflict, and no line one side deleted
    /// comes back.
    #[test]
    fn a_change_inside_the_other_sides_change_conflicts() {
        let base = twelve();
        let rewrite = with(&base, |i, l| match i {
            2 => vec!["OURS-REWRITE".into()],
            3..=8 => vec![],
            _ => vec![l.into()],
        });
        let insert = with(&base, |i, l| match i {
            5 => vec!["THEIRS-INSERT".into(), l.into()],
            _ => vec![l.into()],
        });
        let m = merge_text(&base, &rewrite, &insert);
        assert!(m.has_conflict(), "{}", m.render("X"));
        assert_eq!(m.render_side(false), rewrite);
        assert_eq!(m.render_side(true), insert);

        let delete = with(&base, |i, l| match i {
            2..=8 => vec![],
            _ => vec![l.into()],
        });
        let change = with(&base, |i, l| match i {
            5 => vec!["THEIRS-b5".into()],
            _ => vec![l.into()],
        });
        let m = merge_text(&base, &delete, &change);
        assert!(m.has_conflict(), "{}", m.render("X"));
        assert_eq!(m.render_side(false), delete);
        assert_eq!(m.render_side(true), change);
    }

    /// Changes that only meet at a line boundary are still both applied.
    #[test]
    fn changes_that_meet_at_a_boundary_both_apply() {
        let m = merge_text("a\nb\nc", "a\nNEW\nb\nc", "a\nB\nc");
        assert!(!m.has_conflict(), "{:?}", m.hunks);
        assert_eq!(m.render("X"), "a\nNEW\nB\nc");
    }

    /// One edit of base: per line keep, drop, replace or insert before it.
    fn side(tag: &'static str) -> impl proptest::strategy::Strategy<Value = String> {
        use proptest::prelude::*;
        prop::collection::vec(0u8..8, 12).prop_map(move |ops| {
            let base = twelve();
            with(&base, |i, l| match ops[i] {
                0 => vec![],
                1 => vec![format!("{tag}{i}")],
                2 => vec![format!("{tag}{i}"), l.into()],
                _ => vec![l.into()],
            })
        })
    }

    proptest::proptest! {
        /// Whatever both sides did, neither side's reading of the merge holds
        /// a base line twice, and which side is "ours" changes nothing but the
        /// labels.
        #[test]
        fn a_merge_repeats_no_base_line_and_is_symmetric(ours in side("o"), theirs in side("t")) {
            let base = twelve();
            let m = merge_text(&base, &ours, &theirs);
            for taken in [m.render_side(false), m.render_side(true)] {
                for line in base.lines() {
                    proptest::prop_assert!(
                        taken.lines().filter(|l| *l == line).count() <= 1,
                        "{line} twice in\n{taken}"
                    );
                }
            }
            let flipped = merge_text(&base, &theirs, &ours);
            proptest::prop_assert_eq!(m.has_conflict(), flipped.has_conflict());
            proptest::prop_assert_eq!(m.render_side(false), flipped.render_side(true));
            proptest::prop_assert_eq!(m.render_side(true), flipped.render_side(false));
            proptest::prop_assert_eq!(merge_text(&base, &ours, &base).render("X"), ours.clone());
            proptest::prop_assert_eq!(merge_text(&base, &ours, &ours).render("X"), ours);
        }
    }

    #[test]
    fn clean_text_has_no_markers() {
        assert!(!has_markers("just\nsome\ntext"));
        assert!(has_markers("a\n=======\nb"));
    }
}
