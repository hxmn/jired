//! Build metadata: which commit a binary came from, and when it was built.
//!
//! A bug report that says "jired 0.1.0" is nearly useless between releases; one
//! that says `0.1.0 (9212430 2026-08-21T14:22:31Z)` names the exact source.
//!
//! Everything here degrades rather than fails: building from a source tarball
//! with no `.git` yields `unknown` for the commit, and the version string still
//! carries what it can. `SOURCE_DATE_EPOCH` is honoured so a distribution can
//! still produce a reproducible build.

use std::path::Path;
use std::process::Command;

fn main() {
    rerun_when_head_moves();
    println!("cargo:rerun-if-env-changed=SOURCE_DATE_EPOCH");

    println!("cargo:rustc-env=JIRED_COMMIT={}", commit());
    println!("cargo:rustc-env=JIRED_BUILD_TIME={}", build_time());
    println!(
        "cargo:rustc-env=JIRED_TARGET={}",
        std::env::var("TARGET").unwrap_or_else(|_| "unknown".into())
    );
    println!("cargo:rustc-env=JIRED_RUSTC={}", rustc_version());
}

/// The commit, short, with `-dirty` when the tree has uncommitted changes.
///
/// The suffix matters more than it looks: most binaries that misbehave in
/// development were built from a tree that no commit describes.
fn commit() -> String {
    let Some(sha) = git(&["rev-parse", "--short", "HEAD"]) else {
        return "unknown".into();
    };
    match git(&["status", "--porcelain", "--untracked-files=no"]) {
        Some(changes) if !changes.is_empty() => format!("{sha}-dirty"),
        _ => sha,
    }
}

/// Build time as RFC 3339 in UTC, to the second.
fn build_time() -> String {
    let epoch = match std::env::var("SOURCE_DATE_EPOCH")
        .ok()
        .and_then(|v| v.trim().parse::<i64>().ok())
    {
        Some(fixed) => fixed,
        None => std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0),
    };
    format_utc(epoch)
}

/// `1970-01-01T00:00:00Z` from a Unix timestamp, without pulling a date crate
/// into the build graph for one line of arithmetic.
fn format_utc(epoch: i64) -> String {
    let (days, secs) = (epoch.div_euclid(86_400), epoch.rem_euclid(86_400));
    let (hour, minute, second) = (secs / 3600, (secs % 3600) / 60, secs % 60);

    // Days since 1970-01-01 to a civil date (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { y + 1 } else { y };

    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

fn rustc_version() -> String {
    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".into());
    Command::new(rustc)
        .arg("--version")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|v| v.trim().to_string())
        .unwrap_or_else(|| "unknown".into())
}

fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8(out.stdout).ok())
        .flatten()
        .map(|s| s.trim().to_string())
}

/// Rebuild when the checked-out commit changes.
///
/// `.git/HEAD` covers switching branches; the ref it names covers committing on
/// the branch already checked out. In a worktree `.git` is a file rather than a
/// directory, and there is nothing useful to watch — a stale commit string is a
/// far smaller problem than a build script that fails.
///
/// What this deliberately does not watch is the source tree: an incremental
/// rebuild after an edit keeps the metadata from the last run, so on a developer
/// machine the timestamp is when the commit was last resolved rather than when
/// the binary was linked, and `-dirty` can lag by one edit. A release is built
/// from a clean tree into a fresh target directory, where both are exact.
fn rerun_when_head_moves() {
    let git_dir = Path::new(".git");
    if !git_dir.is_dir() {
        return;
    }
    println!("cargo:rerun-if-changed=.git/HEAD");
    let Ok(head) = std::fs::read_to_string(git_dir.join("HEAD")) else {
        return;
    };
    if let Some(reference) = head.strip_prefix("ref: ") {
        let path = git_dir.join(reference.trim());
        if path.is_file() {
            println!("cargo:rerun-if-changed={}", path.display());
        }
    }
}
