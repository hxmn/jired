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
}

pub fn run(_g: &GlobalArgs, args: &VersionArgs, out: Out) -> Result<ExitCode> {
    if args.changelog {
        out.say(CHANGELOG.trim_end());
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
            "changelog": args.changelog.then_some(CHANGELOG),
        }),
    );
    Ok(ExitCode::Success)
}

#[cfg(test)]
mod tests {
    use super::*;

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
