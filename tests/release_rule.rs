//! The release rule, held by a test.
//!
//! Every push to `main` is a release (`CLAUDE.md`): a version in `Cargo.toml`,
//! the same one in `Cargo.lock`, and a dated changelog section for it on top.
//! Nothing checked any of it, so a bump without the lockfile passed CI and then
//! broke `cargo install --locked` for everyone, and a version could ship with
//! no section saying what was in it.

use std::path::PathBuf;

fn read(name: &str) -> String {
    std::fs::read_to_string(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(name)).unwrap()
}

/// The `version = "…"` of the package, from `Cargo.toml`.
fn manifest_version() -> String {
    let text = read("Cargo.toml");
    let package = text.split("[package]").nth(1).expect("a [package] table");
    package
        .lines()
        .find_map(|l| l.strip_prefix("version = "))
        .map(|v| v.trim().trim_matches('"').to_string())
        .expect("a version")
}

#[test]
fn the_version_is_the_same_in_the_manifest_the_lockfile_and_the_changelog() {
    let version = manifest_version();
    assert_eq!(version, env!("CARGO_PKG_VERSION"));

    let lock = read("Cargo.lock");
    let ours = lock
        .split("[[package]]")
        .find(|p| p.contains("\nname = \"jired\"\n"))
        .expect("jired in Cargo.lock");
    assert!(
        ours.contains(&format!("\nversion = \"{version}\"\n")),
        "Cargo.lock does not have jired {version}; run `cargo build` and commit the lockfile"
    );

    let changelog = read("CHANGELOG.md");
    assert!(
        !changelog.contains("## Unreleased"),
        "an `## Unreleased` section: the version is decided at push time"
    );
    let top = changelog
        .lines()
        .find(|l| l.starts_with("## ["))
        .expect("a version section");
    let prefix = format!("## [{version}] — ");
    let date = top
        .strip_prefix(&prefix)
        .unwrap_or_else(|| panic!("the top section is `{top}`, not {version}"));
    assert!(
        chrono::NaiveDate::parse_from_str(date.trim(), "%Y-%m-%d").is_ok(),
        "the top section's date `{date}` is not YYYY-MM-DD"
    );
}
