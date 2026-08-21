//! The machine-readable contract [M5.1].
//!
//! Agents parse jired's stdout, so a document with no published schema — or one
//! that quietly stops matching its schema — is a broken promise they cannot
//! detect. These tests make both failures loud:
//!
//! * every `out.emit(...)` in the source has a schema in `schemas/`;
//! * every schema is a valid JSON Schema whose `$id` matches its filename;
//! * the documents commands actually produce validate against them.

use assert_cmd::Command;
use serde_json::Value;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn published() -> BTreeSet<String> {
    std::fs::read_dir(repo().join("schemas"))
        .unwrap()
        .flatten()
        .filter_map(|e| {
            let p = e.path();
            (p.extension()? == "json").then(|| p.file_stem()?.to_str().map(str::to_string))?
        })
        .collect()
}

/// Names passed to `Out::emit` anywhere in the source.
///
/// Scanning the source rather than keeping a list is the point: a new command
/// that forgets its schema has to fail here, and a hand-maintained list would
/// simply be forgotten alongside it.
fn emitted() -> BTreeSet<String> {
    fn walk(dir: &Path, out: &mut BTreeSet<String>) {
        for entry in std::fs::read_dir(dir).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                let text = std::fs::read_to_string(&path).unwrap();
                collect(&text, out);
            }
        }
    }

    /// `emit("name"` and `emit(\n    "name"` both count.
    fn collect(text: &str, out: &mut BTreeSet<String>) {
        let mut rest = text;
        while let Some(idx) = rest.find(".emit(") {
            rest = &rest[idx + ".emit(".len()..];
            let head: String = rest.chars().take(80).collect();
            if let Some(start) = head.find('"') {
                let after = &head[start + 1..];
                if let Some(end) = after.find('"') {
                    let name = &after[..end];
                    // The first string literal after `emit(` is the schema name
                    // only when nothing but whitespace precedes it.
                    if head[..start].trim().is_empty()
                        && !name.is_empty()
                        && name.chars().all(|c| c.is_ascii_lowercase() || c == '-')
                    {
                        out.insert(name.to_string());
                    }
                }
            }
        }
    }

    let mut out = BTreeSet::new();
    walk(&repo().join("src"), &mut out);
    out
}

#[test]
fn every_emitted_document_has_a_published_schema() {
    let published = published();
    let missing: Vec<String> = emitted()
        .into_iter()
        .filter(|n| !published.contains(n))
        .collect();
    assert!(
        missing.is_empty(),
        "these documents are emitted with no schema in schemas/: {missing:?}\n\
         add one, or agents have no way to validate the output"
    );
}

#[test]
fn every_published_schema_is_reachable_from_the_cli() {
    // `error` and `frontmatter` describe formats rather than command output.
    let described_elsewhere = ["error", "frontmatter"];
    let emitted = emitted();
    let unused: Vec<String> = published()
        .into_iter()
        .filter(|n| !emitted.contains(n) && !described_elsewhere.contains(&n.as_str()))
        .collect();
    assert!(
        unused.is_empty(),
        "these schemas describe nothing jired emits: {unused:?}"
    );
}

#[test]
fn schema_command_lists_exactly_what_is_published() {
    let out = Command::cargo_bin("jired")
        .unwrap()
        .arg("schema")
        .output()
        .unwrap();
    let listed: BTreeSet<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::to_string)
        .collect();
    assert_eq!(listed, published());
}

#[test]
fn schema_ids_match_their_filenames() {
    for name in published() {
        let text = std::fs::read_to_string(repo().join(format!("schemas/{name}.json"))).unwrap();
        let doc: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(
            doc["$id"],
            format!("jired/{name}/1"),
            "{name}.json has a mismatched $id"
        );
        jsonschema::validator_for(&doc)
            .unwrap_or_else(|e| panic!("{name}.json is not a valid JSON Schema: {e}"));
    }
}

/// Drive a workspace through the commands and check each document.
#[test]
fn command_output_matches_its_published_schema() {
    let dir = TempDir::new().unwrap();
    let fixtures = dir.path().join(".fixtures");
    std::fs::create_dir_all(&fixtures).unwrap();
    for entry in std::fs::read_dir(repo().join("tests/fixtures/cloud"))
        .unwrap()
        .flatten()
    {
        std::fs::copy(entry.path(), fixtures.join(entry.file_name())).unwrap();
    }

    let run = |args: &[&str]| -> Option<Value> {
        let out = Command::cargo_bin("jired")
            .unwrap()
            .current_dir(dir.path())
            .env("JIRED_FIXTURES", &fixtures)
            .env("JIRED_NO_KEYRING", "1")
            .env("JIRED_SECRET_KEY", "passphrase")
            .env("JIRED_URL", "https://fixture.invalid")
            .env("JIRED_EMAIL", "ilya@example.invalid")
            .env("JIRED_TOKEN", "token")
            .env("JIRED_PROJECT", "PROJ")
            .args(args)
            .args(["--json", "--no-input", "--yes"])
            .output()
            .unwrap();
        serde_json::from_slice(&out.stdout).ok()
    };

    let scenarios: Vec<Vec<&str>> = vec![
        vec!["init", "--no-pull"],
        vec!["pull"],
        vec!["status"],
        vec!["diff"],
        vec!["whoami"],
        vec!["projects"],
        vec!["boards"],
        vec!["config", "list", "--all"],
        vec!["config", "get", "workspace.project"],
        vec!["fetch"],
        vec!["log", "PROJ-142"],
        vec!["search", "--jql", "project = PROJ"],
        vec!["open", "PROJ-142", "--print"],
        vec!["comment", "PROJ-142", "-m", "note"],
        vec!["transition", "PROJ-142", "--list"],
        vec!["assign", "PROJ-142", "--me"],
        vec!["create", "--summary", "New thing"],
        vec!["attachments", "list"],
        vec!["attachments", "pull", "--max-size", "1"],
        vec!["push", "--dry-run"],
        vec!["resolve", "--list"],
        vec!["clean", "--dry-run"],
        // Rebuilding the grouping folders belongs to `doctor`, which owns every
        // other repair; `clean --regroup` used to shadow clean's own purpose.
        vec!["doctor", "--regroup"],
        vec!["agent-docs"],
    ];

    let mut checked = 0;
    for args in scenarios {
        let Some(doc) = run(&args) else {
            panic!("`jired {}` produced no JSON", args.join(" "));
        };
        let tag = doc["schema"]
            .as_str()
            .unwrap_or_else(|| panic!("`jired {}` output has no schema tag", args.join(" ")));
        let name = tag
            .strip_prefix("jired/")
            .and_then(|t| t.strip_suffix("/1"))
            .unwrap_or_else(|| panic!("malformed schema tag {tag}"));

        let schema: Value = serde_json::from_str(
            &std::fs::read_to_string(repo().join(format!("schemas/{name}.json")))
                .unwrap_or_else(|_| panic!("no schema published for {tag}")),
        )
        .unwrap();
        let validator = jsonschema::validator_for(&schema).unwrap();
        let errors: Vec<String> = validator.iter_errors(&doc).map(|e| e.to_string()).collect();
        assert!(
            errors.is_empty(),
            "`jired {}` violated {tag}: {errors:?}\n{doc:#}",
            args.join(" ")
        );
        checked += 1;
    }
    assert!(checked >= 20, "only {checked} documents were checked");
}
