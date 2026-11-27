//! What every test that runs the binary shares: an environment of its own.
//!
//! The binary reads the user's config and keeps credentials in the user's
//! config directory, and both were the developer's own: a `creds.storage`
//! set globally sent fixture credentials into the real credentials file, and
//! an exported `JIRED_*` setting changed what the tests saw. CI never
//! noticed, because its runner has no config. So the environment is cleared,
//! and every variable a config or data directory is found by points into a
//! directory of the test's own.

#![allow(dead_code)]

use assert_cmd::Command;
use std::path::{Path, PathBuf};

/// What a process needs to start at all, kept from the real environment.
const KEPT: &[&str] = &[
    "PATH",
    "TMPDIR",
    "TEMP",
    "TMP",
    "SYSTEMROOT",
    "SystemRoot",
    "WINDIR",
    "COMSPEC",
    "PATHEXT",
    "RUST_BACKTRACE",
    "LLVM_PROFILE_FILE",
];

/// The binary, with an environment of nothing but [`KEPT`] and a home in
/// `home`.
pub fn jired(home: &Path) -> Command {
    let mut cmd = Command::cargo_bin("jired").expect("binary");
    cmd.env_clear();
    for name in KEPT {
        if let Some(value) = std::env::var_os(name) {
            cmd.env(name, value);
        }
    }
    let config = home.join(".config");
    cmd.env("HOME", home)
        .env("USERPROFILE", home)
        .env("XDG_CONFIG_HOME", &config)
        .env("XDG_DATA_HOME", home.join(".local/share"))
        .env("XDG_CACHE_HOME", home.join(".cache"))
        .env("XDG_STATE_HOME", home.join(".local/state"))
        .env("APPDATA", &config)
        .env("LOCALAPPDATA", home.join(".local"));
    cmd
}

/// A home for a command that has no test directory to put one in: one per
/// test process, empty when it starts.
pub fn process_home() -> PathBuf {
    static HOME: std::sync::OnceLock<tempfile::TempDir> = std::sync::OnceLock::new();
    HOME.get_or_init(|| tempfile::TempDir::new().expect("tempdir"))
        .path()
        .to_path_buf()
}
