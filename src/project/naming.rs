//! Filenames and attachment folders [M1.11].
//!
//! The `KEY.` prefix is the issue's identity; the summary that follows is
//! cosmetic. Every lookup matches on the prefix, so renaming a file by hand or
//! having the summary change on either side never loses the association.
//!
//! The number in that prefix is zero-padded, because `PROJ-9` sorts after
//! `PROJ-10` in every file manager, shell glob and editor sidebar there is. The
//! padding is a filename convention only: the key in the frontmatter, the
//! database and the API stays `PROJ-9`, and [`key_from_file_name`] strips the
//! padding back off. `jired doctor` renames files that predate the convention.

use unicode_segmentation::UnicodeSegmentation;

/// Longest filename jired will produce, before the `.md` extension.
pub const MAX_STEM: usize = 120;

/// Digits an issue number is padded to inside a filename.
///
/// Four covers the first 9999 issues of a project, which is where sorting
/// actually matters; beyond that names simply grow, and still sort correctly
/// among themselves.
pub const KEY_PAD: usize = 4;

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

/// `PROJ-142` → `PROJ-0142`, for use in filenames only.
///
/// Temporary `NEW-*` keys carry a hex suffix rather than an issue number and
/// are left exactly as they are.
pub fn padded_key(key: &str) -> String {
    if crate::doc::is_temp_key(key) || !crate::doc::is_issue_key(key) {
        return key.to_string();
    }
    let Some((project, number)) = key.split_once('-') else {
        return key.to_string();
    };
    format!("{project}-{number:0>KEY_PAD$}")
}

/// `PROJ-0142` → `PROJ-142`: the key as Jira knows it.
pub fn unpadded_key(key: &str) -> String {
    if crate::doc::is_temp_key(key) {
        return key.to_string();
    }
    let Some((project, number)) = key.split_once('-') else {
        return key.to_string();
    };
    if number.is_empty() || !number.chars().all(|c| c.is_ascii_digit()) {
        return key.to_string();
    }
    let trimmed = number.trim_start_matches('0');
    format!(
        "{project}-{}",
        if trimmed.is_empty() { "0" } else { trimmed }
    )
}

/// The file stem for an issue: `PROJ-0142. Fix login flow`.
pub fn file_stem(key: &str, summary: &str) -> String {
    let key = &padded_key(key);
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
    if crate::doc::is_temp_key(candidate) {
        return Some(candidate.to_string());
    }
    // Padded or not, the file names the same issue.
    crate::doc::is_issue_key(candidate).then(|| unpadded_key(candidate))
}

/// The key a person means, from whatever they typed.
///
/// Three spellings all reach the same issue: the key itself in any case, the
/// file's name, and a path to it — which is what a shell's tab-completion
/// produces, and the most likely thing to be typed for an issue whose summary
/// is hard to spell.
///
/// Case is not simply uppercased: a locally created `NEW-8bf7b6` carries a hex
/// suffix that was generated in lowercase, and uppercasing it invents a key the
/// workspace does not have.
///
/// One spelling is refused: several keys in one argument. `"TSK-487 TSK-556"`
/// reads exactly like a file name — a key, then a summary — so it used to come
/// out as `TSK-487` with the rest dropped as the summary, and a push named
/// three issues, sent one and reported success. It is what an unsplit shell
/// variable produces (zsh does not word-split `$keys`), and nothing about it
/// is a file name: the summary half is made of nothing but keys.
pub fn key_from_input(input: &str) -> crate::error::Result<String> {
    if let Some(keys) = crammed_keys(input) {
        return Err(crate::error::JiredError::usage(format!(
            "`{}` is {} issue keys in one argument",
            input.trim(),
            keys.len()
        ))
        .with_detail("argument", input.to_string())
        .with_detail("keys", serde_json::json!(keys))
        .with_hint(format!(
            "pass them as separate arguments — `{}` — or split the variable \
             (`${{=keys}}` in zsh)",
            keys.join(" ")
        )));
    }
    Ok(key_of(input))
}

/// The keys an argument holds when it is nothing but keys, two or more of them.
fn crammed_keys(input: &str) -> Option<Vec<String>> {
    let input = input.trim().trim_matches('"');
    // A path or a file name is one issue, whatever its summary says.
    if input.contains(['/', '\\']) || input.ends_with(".md") {
        return None;
    }
    let tokens: Vec<&str> = input
        .split(|c: char| c.is_whitespace() || c == ',')
        .filter(|t| !t.is_empty())
        .collect();
    let all_keys = tokens
        .iter()
        .all(|t| crate::doc::is_issue_key(&t.to_ascii_uppercase()) || crate::doc::is_temp_key(t));
    (tokens.len() > 1 && all_keys).then(|| tokens.iter().map(|t| key_of(t)).collect())
}

fn key_of(input: &str) -> String {
    let input = input.trim().trim_matches('"');
    // A path from tab-completion; the last component is the file name.
    let tail = input.rsplit(['/', '\\']).next().unwrap_or(input);
    let candidate = key_from_file_name(tail)
        .unwrap_or_else(|| tail.strip_suffix(".md").unwrap_or(tail).to_string());

    let temp = crate::doc::TEMP_KEY_PREFIX.trim_end_matches('-');
    match candidate.split_once('-') {
        Some((prefix, suffix)) if prefix.eq_ignore_ascii_case(temp) => format!(
            "{}{}",
            crate::doc::TEMP_KEY_PREFIX,
            suffix.to_ascii_lowercase()
        ),
        _ => candidate.to_uppercase(),
    }
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
            "PROJ-0142. Fix login flow"
        );
        assert_eq!(
            file_name("PROJ-142", "Fix login flow"),
            "PROJ-0142. Fix login flow.md"
        );
        assert_eq!(
            attachment_dir("PROJ-142", "Fix login flow"),
            ".PROJ-0142. Fix login flow"
        );
    }

    /// The whole point of padding: a plain sort has to put 9 before 10.
    #[test]
    fn padded_names_sort_numerically() {
        let mut names: Vec<String> = [7, 10, 9, 100, 1234]
            .iter()
            .map(|n| file_name(&format!("PROJ-{n}"), "x"))
            .collect();
        names.sort();
        let keys: Vec<Option<String>> = names.iter().map(|n| key_from_file_name(n)).collect();
        assert_eq!(
            keys,
            ["PROJ-7", "PROJ-9", "PROJ-10", "PROJ-100", "PROJ-1234"]
                .iter()
                .map(|k| Some(k.to_string()))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn padding_is_a_filename_convention_and_round_trips() {
        assert_eq!(padded_key("PROJ-9"), "PROJ-0009");
        assert_eq!(padded_key("PROJ-12345"), "PROJ-12345");
        assert_eq!(unpadded_key("PROJ-0009"), "PROJ-9");
        assert_eq!(unpadded_key("PROJ-12345"), "PROJ-12345");
        // A hex temp key is not a number and must survive untouched.
        assert_eq!(padded_key("NEW-0052"), "NEW-0052");
        assert_eq!(unpadded_key("NEW-0052"), "NEW-0052");
        // Not a key at all.
        assert_eq!(padded_key("notes"), "notes");
    }

    #[test]
    fn long_summaries_are_capped_with_an_ellipsis() {
        let stem = file_stem("PROJ-1", &"x".repeat(500));
        assert!(stem.chars().count() <= MAX_STEM, "{}", stem.chars().count());
        assert!(stem.ends_with('…'));
        assert!(stem.starts_with("PROJ-0001. "));
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
        assert_eq!(file_stem("PROJ-1", "   "), "PROJ-0001");
        assert_eq!(file_name("PROJ-1", ""), "PROJ-0001.md");
    }

    #[test]
    fn key_is_recovered_from_any_summary_spelling() {
        assert_eq!(
            key_from_file_name("PROJ-0142. Fix login flow.md").as_deref(),
            Some("PROJ-142")
        );
        // Files written before the padding convention still resolve.
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

    /// Every spelling a person might reasonably type has to reach the issue.
    #[test]
    fn a_key_is_recognized_however_it_was_typed() {
        for input in [
            "PROJ-142",
            "proj-142",
            "PROJ-0142",
            "PROJ-0142. Fix login flow.md",
            "./PROJ-0142. Fix login flow.md",
            "/tmp/ws/PROJ-0142. Fix login flow.md",
            "\"PROJ-142\"",
        ] {
            assert_eq!(key_from_input(input).unwrap(), "PROJ-142", "{input}");
        }
    }

    /// An unsplit shell variable is several keys in one argument, and reads
    /// exactly like a file name; only a summary made of nothing but keys tells
    /// the two apart.
    #[test]
    fn several_keys_in_one_argument_are_refused_and_a_summary_is_not() {
        for input in [
            "TSK-487 TSK-556 TSK-578",
            "tsk-487 tsk-556",
            "TSK-487,TSK-556",
            " TSK-487\tTSK-556 ",
        ] {
            let err = key_from_input(input).unwrap_err();
            assert_eq!(err.exit_code().as_i32(), 2, "{input}");
            assert!(
                err.hint.as_deref().unwrap().contains("TSK-487 TSK-556"),
                "{input}"
            );
        }
        for input in [
            "TSK-487 Fix login",
            "TSK-0487. TSK-556 is a duplicate.md",
            "./TSK-487 TSK-556",
            "TSK-487",
        ] {
            assert_eq!(key_from_input(input).unwrap(), "TSK-487", "{input}");
        }
    }

    /// The bug this exists for: a temp key's hex suffix is generated lowercase,
    /// so uppercasing what was typed names an issue that does not exist.
    #[test]
    fn a_temporary_key_keeps_the_case_it_was_generated_with() {
        for input in [
            "NEW-8bf7b6",
            "new-8bf7b6",
            "NEW-8BF7B6",
            "NEW-8bf7b6. [CARE] Забота о сотрудниках в спринте №6.md",
        ] {
            assert_eq!(key_from_input(input).unwrap(), "NEW-8bf7b6", "{input}");
        }
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
