//! Filenames and attachment folders [M1.11].
//!
//! The `KEY.` prefix is the issue's identity; the summary that follows is
//! cosmetic. Every lookup matches on the prefix, so renaming a file by hand or
//! having the summary change on either side never loses the association.

use unicode_segmentation::UnicodeSegmentation;

/// Longest filename jired will produce, before the `.md` extension.
pub const MAX_STEM: usize = 120;

/// Characters no mainstream filesystem accepts in a name.
const FORBIDDEN: &[char] = &['/', '\\', ':', '*', '?', '"', '<', '>', '|'];

/// Names Windows refuses regardless of extension.
const WINDOWS_RESERVED: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// Make an arbitrary summary safe to use inside a filename.
pub fn sanitize(summary: &str) -> String {
    let mut out = String::with_capacity(summary.len());
    let mut last_was_space = false;
    for ch in summary.chars() {
        let ch = if FORBIDDEN.contains(&ch) || ch.is_control() {
            ' '
        } else {
            ch
        };
        if ch.is_whitespace() {
            if !last_was_space && !out.is_empty() {
                out.push(' ');
            }
            last_was_space = true;
        } else {
            out.push(ch);
            last_was_space = false;
        }
    }
    // Windows silently strips trailing dots and spaces, which would make the
    // name jired writes differ from the one it later looks for.
    out.trim().trim_end_matches('.').trim().to_string()
}

/// Truncate on grapheme boundaries so a cut never lands inside a character.
pub fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut out = String::new();
    for g in text.graphemes(true) {
        if out.chars().count() + g.chars().count() > max.saturating_sub(1) {
            break;
        }
        out.push_str(g);
    }
    format!("{}…", out.trim_end())
}

/// The file stem for an issue: `PROJ-142. Fix login flow`.
pub fn file_stem(key: &str, summary: &str) -> String {
    let clean = sanitize(summary);
    let prefix = format!("{key}. ");
    let room = MAX_STEM.saturating_sub(prefix.chars().count());
    let stem = if clean.is_empty() {
        key.to_string()
    } else {
        format!("{prefix}{}", truncate(&clean, room))
    };
    guard_reserved(&stem)
}

pub fn file_name(key: &str, summary: &str) -> String {
    format!("{}.md", file_stem(key, summary))
}

/// Hidden sibling folder holding an issue's attachments.
pub fn attachment_dir(key: &str, summary: &str) -> String {
    format!(".{}", file_stem(key, summary))
}

/// Recover the issue key from a filename, ignoring the cosmetic summary.
///
/// Accepts locally created `NEW-*` keys too: their files are ordinary members
/// of the workspace until a push gives them a real key.
pub fn key_from_file_name(name: &str) -> Option<String> {
    let stem = name.strip_suffix(".md").unwrap_or(name);
    let candidate = stem.split(['.', ' ']).next()?;
    (crate::doc::is_issue_key(candidate) || crate::doc::is_temp_key(candidate))
        .then(|| candidate.to_string())
}

/// Grouping folders are visible and fixed, so they can never collide with the
/// dot-prefixed attachment folders (decision D13).
pub const GROUP_PARENT: &str = "by-parent";
pub const GROUP_SPRINTS: &str = "sprints";
pub const GROUP_BOARDS: &str = "boards";

pub fn is_group_root(name: &str) -> bool {
    matches!(name, GROUP_PARENT | GROUP_SPRINTS | GROUP_BOARDS)
}

/// Sanitize a sprint or board name for use as a directory.
pub fn group_dir(name: &str, disambiguator: Option<&str>) -> String {
    let clean = truncate(&sanitize(name), MAX_STEM);
    let clean = if clean.is_empty() {
        "unnamed".to_string()
    } else {
        clean
    };
    match disambiguator {
        Some(d) => guard_reserved(&format!("{clean} ({d})")),
        None => guard_reserved(&clean),
    }
}

fn guard_reserved(stem: &str) -> String {
    let head = stem.split('.').next().unwrap_or(stem).to_ascii_uppercase();
    if WINDOWS_RESERVED.contains(&head.as_str()) {
        return format!("{stem}_");
    }
    stem.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forbidden_characters_become_spaces() {
        assert_eq!(sanitize("a/b\\c:d*e?f\"g<h>i|j"), "a b c d e f g h i j");
    }

    #[test]
    fn whitespace_collapses_and_trims() {
        assert_eq!(sanitize("  hello \t\n world  "), "hello world");
        assert_eq!(sanitize("\u{0}control\u{7}chars"), "control chars");
    }

    #[test]
    fn trailing_dots_and_spaces_are_removed_for_windows() {
        assert_eq!(sanitize("Fix the thing..."), "Fix the thing");
        assert_eq!(sanitize("trailing   "), "trailing");
    }

    #[test]
    fn file_stem_has_the_documented_shape() {
        assert_eq!(
            file_stem("PROJ-142", "Fix login flow"),
            "PROJ-142. Fix login flow"
        );
        assert_eq!(
            file_name("PROJ-142", "Fix login flow"),
            "PROJ-142. Fix login flow.md"
        );
        assert_eq!(
            attachment_dir("PROJ-142", "Fix login flow"),
            ".PROJ-142. Fix login flow"
        );
    }

    #[test]
    fn long_summaries_are_capped_with_an_ellipsis() {
        let stem = file_stem("PROJ-1", &"x".repeat(500));
        assert!(stem.chars().count() <= MAX_STEM, "{}", stem.chars().count());
        assert!(stem.ends_with('…'));
        assert!(stem.starts_with("PROJ-1. "));
    }

    #[test]
    fn truncation_never_splits_a_grapheme() {
        let text = "аб🇺🇦гд";
        let cut = truncate(text, 3);
        assert!(text.starts_with(cut.trim_end_matches('…')));
        assert!(cut.chars().count() <= 3);
    }

    #[test]
    fn an_empty_summary_still_yields_a_usable_name() {
        assert_eq!(file_stem("PROJ-1", "   "), "PROJ-1");
        assert_eq!(file_name("PROJ-1", ""), "PROJ-1.md");
    }

    #[test]
    fn key_is_recovered_from_any_summary_spelling() {
        assert_eq!(
            key_from_file_name("PROJ-142. Fix login flow.md").as_deref(),
            Some("PROJ-142")
        );
        // A hand-renamed file still resolves, because only the prefix matters.
        assert_eq!(
            key_from_file_name("PROJ-142. totally different name.md").as_deref(),
            Some("PROJ-142")
        );
        assert_eq!(
            key_from_file_name("PROJ-142.md").as_deref(),
            Some("PROJ-142")
        );
        assert_eq!(key_from_file_name("notes.md"), None);
        assert_eq!(key_from_file_name("README.md"), None);
    }

    #[test]
    fn a_locally_created_issue_is_found_by_its_temporary_key() {
        assert_eq!(
            key_from_file_name("NEW-5e0227. Audit the SSO config.md").as_deref(),
            Some("NEW-5e0227")
        );
        assert_eq!(
            file_name("NEW-5e0227", "Audit the SSO config"),
            "NEW-5e0227. Audit the SSO config.md"
        );
    }

    #[test]
    fn windows_reserved_names_are_escaped() {
        assert_eq!(group_dir("CON", None), "CON_");
        assert_eq!(group_dir("com1", None), "com1_");
        assert_eq!(group_dir("Sprint 42", None), "Sprint 42");
    }

    #[test]
    fn group_directories_disambiguate_on_request() {
        assert_eq!(group_dir("Sprint 42", Some("7")), "Sprint 42 (7)");
        assert_eq!(group_dir("  ", None), "unnamed");
    }

    #[test]
    fn group_roots_are_recognised() {
        assert!(is_group_root("sprints"));
        assert!(is_group_root("by-parent"));
        assert!(is_group_root("boards"));
        assert!(!is_group_root(".PROJ-1. x"));
    }
}
