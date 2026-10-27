//! `jired version` — which binary this is, and what changed to make it.
//!
//! `--version` already names the exact source a binary came from. What it could
//! not answer is the question that follows it: *and what is different about
//! this one*. The changelog is compiled in rather than read from disk, because
//! the copy that matters is the one describing the binary in your hand — a
//! `CHANGELOG.md` next to it belongs to whatever the checkout has moved on to,
//! and an installed binary has no checkout at all.

use crate::cli::GlobalArgs;
use crate::error::{ExitCode, Result};
use crate::json::Out;
use clap::Args;

/// The changelog as it stood when this binary was built.
pub const CHANGELOG: &str = include_str!("../../../CHANGELOG.md");

#[derive(Args, Debug, Clone, Default)]
pub struct VersionArgs {
    /// Print the changelog this binary was built from
    #[arg(long)]
    pub changelog: bool,

    /// With --changelog, only the versions after this one: `--since 0.18.2`
    ///
    /// What a workspace whose agent docs were written by an older jired needs
    /// to read, and nothing it already knows.
    #[arg(long, value_name = "VERSION", requires = "changelog")]
    pub since: Option<String>,
}

pub fn run(_g: &GlobalArgs, args: &VersionArgs, out: Out) -> Result<ExitCode> {
    let changelog = match &args.since {
        Some(since) => newer_than(CHANGELOG, &parse_version(since)?),
        None => CHANGELOG.to_string(),
    };
    if args.changelog {
        match (&args.since, changelog.trim().is_empty()) {
            (Some(since), true) => out.say(format!("nothing has changed since {since}")),
            _ => out.say(changelog.trim_end()),
        }
    } else {
        out.say(format!("jired {}", crate::cli::LONG_VERSION));
        out.hint("`jired version --changelog` for what changed");
    }

    out.emit(
        "version",
        &serde_json::json!({
            "version": env!("CARGO_PKG_VERSION"),
            "commit": env!("JIRED_COMMIT"),
            "built": env!("JIRED_BUILD_TIME"),
            "rustc": env!("JIRED_RUSTC"),
            "target": env!("JIRED_TARGET"),
            // Only when asked for: 19 KB of prose in every `--json` document
            // would be paid for by every agent that never reads it.
            "changelog": args.changelog.then_some(changelog),
        }),
    );
    Ok(ExitCode::Success)
}

/// `0.18.2` as numbers, so `0.9.0` sorts before `0.10.0`.
fn parse_version(text: &str) -> Result<Vec<u64>> {
    text.trim()
        .trim_start_matches('v')
        .split('.')
        .map(|part| part.parse::<u64>())
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|_| {
            crate::error::JiredError::usage(format!("`{text}` is not a version"))
                .with_hint("a release number, as `jired version` prints it: `0.18.2`")
        })
}

/// The release sections of `changelog` newer than `since`, newest first as
/// the file has them. Everything before the first section — the preamble
/// about what the file is — is left out: whoever asks for a range has read it.
fn newer_than(changelog: &str, since: &[u64]) -> String {
    let mut out = String::new();
    let mut keep = false;
    for line in changelog.lines() {
        if let Some(rest) = line.strip_prefix("## [") {
            keep = rest
                .split(']')
                .next()
                .and_then(|v| parse_version(v).ok())
                .is_some_and(|v| v.as_slice() > since);
        } else if line.starts_with("## ") {
            // `## Unreleased`: not a release, so not something a version
            // range can be said to include.
            keep = false;
        }
        if keep {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_range_is_the_releases_after_the_one_named() {
        let log = "# Changelog\n\npreamble\n\n## [0.10.0] — x\n\nten\n## [0.9.1] — x\n\nnine-one\n## [0.9.0] — x\n\nnine\n";
        let after = newer_than(log, &parse_version("0.9.0").unwrap());
        assert!(
            after.contains("ten") && after.contains("nine-one"),
            "{after}"
        );
        assert!(
            !after.contains("\nnine\n") && !after.contains("preamble"),
            "{after}"
        );
        assert_eq!(newer_than(log, &parse_version("0.10.0").unwrap()), "");
        assert!(parse_version("latest").is_err());
    }

    /// A binary that cannot say what changed in it is the thing this command
    /// exists to prevent, and an empty `include_str!` fails silently.
    #[test]
    fn the_changelog_is_compiled_in() {
        assert!(
            CHANGELOG.starts_with("# Changelog"),
            "{:?}",
            &CHANGELOG[..40]
        );
        assert!(
            CHANGELOG.contains("## Unreleased") || CHANGELOG.contains("## ["),
            "the changelog has no release sections"
        );
    }
}
