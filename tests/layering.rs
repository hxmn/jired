//! The layering `docs/plan/architecture.md` draws, held to by the source.
//!
//! Nothing in the compiler stops a lower module from reaching up: a `use
//! crate::cli::…` in the engine builds as happily as any other. It had, until
//! the engine could not be driven without clap's types and the client could not
//! be built without the command line's context. This reads the source for the
//! paths that undid that, so the next one fails here rather than in a review.

use std::path::{Path, PathBuf};

/// A module, and the modules nothing in it may name.
const RULES: &[(&str, &[&str])] = &[
    // The engine is driven through options structs and a `Context` that live
    // outside the command line, and its reports are drawn there: colour is not
    // the engine's to choose.
    ("sync", &["cli", "style"]),
    ("rules", &["cli"]),
    // The client is handed a finished connection; where it was looked up is
    // the composing layer's business.
    ("api", &["cli", "store", "config", "doc"]),
    // The wire model and the file model share their values through `value`,
    // not through each other.
    ("doc", &["api", "cli", "sync"]),
    // A setting is checked when it is set, which must not need a terminal.
    ("config", &["tui", "cli", "sync"]),
];

/// Modules that are one file rather than a directory: a leaf names nothing.
const LEAVES: &[&str] = &["value"];

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap().flatten() {
        let path = entry.path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// Whether `code` names `crate::<module>` as a path, not a longer name that
/// starts the same.
fn names(code: &str, module: &str) -> bool {
    let needle = format!("crate::{module}");
    code.match_indices(&needle).any(|(i, _)| {
        !code[i + needle.len()..]
            .chars()
            .next()
            .is_some_and(|c| c.is_alphanumeric() || c == '_')
    })
}

#[test]
fn lower_modules_do_not_name_the_ones_above_them() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut found = Vec::new();
    for (module, banned) in RULES {
        let mut files = Vec::new();
        rust_files(&src.join(module), &mut files);
        assert!(!files.is_empty(), "no source under src/{module}");
        for file in files {
            let text = std::fs::read_to_string(&file).unwrap();
            for (n, line) in text.lines().enumerate() {
                // Prose may mention a module; only code may depend on one.
                let code = line.split("//").next().unwrap_or("");
                for b in *banned {
                    if names(code, b) {
                        found.push(format!(
                            "{}:{}: {module} names crate::{b}",
                            file.strip_prefix(&src).unwrap().display(),
                            n + 1
                        ));
                    }
                }
            }
        }
    }
    for leaf in LEAVES {
        let file = src.join(format!("{leaf}.rs"));
        let text = std::fs::read_to_string(&file).unwrap();
        for (n, line) in text.lines().enumerate() {
            let code = line.split("//").next().unwrap_or("");
            if code.contains("crate::") {
                found.push(format!("{leaf}.rs:{}: a leaf names {}", n + 1, code.trim()));
            }
        }
    }
    assert!(found.is_empty(), "{}", found.join("\n"));
}

#[test]
fn a_longer_name_is_not_the_module() {
    assert!(names("use crate::cli::context;", "cli"));
    assert!(names("crate::cli", "cli"));
    assert!(!names("use crate::client::JiraApi;", "cli"));
}
