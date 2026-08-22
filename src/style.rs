//! Terminal styling shared by every command.
//!
//! Colour is a property of the *stream*, not of the caller: stdout can be a pipe
//! while stderr is a terminal, and the two must be decided separately. The mode
//! is resolved once at startup from `--color`, `NO_COLOR`, `TERM` and whether
//! each stream is a tty, so no code below the CLI layer has to ask again.

use std::sync::atomic::{AtomicU8, Ordering};

/// What `--color` asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, clap::ValueEnum)]
pub enum ColorChoice {
    /// Colour when the stream is a terminal and `NO_COLOR` is unset.
    #[default]
    Auto,
    /// Always emit escapes, even into a pipe.
    Always,
    /// Never emit escapes.
    Never,
}

// 0 = undecided, 1 = off, 2 = on. Atomics rather than a OnceLock because the
// tests set this per-case and the cost is a relaxed load.
static STDOUT_COLOR: AtomicU8 = AtomicU8::new(0);
static STDERR_COLOR: AtomicU8 = AtomicU8::new(0);

/// Decide colour for both streams. Called once, from `cli::run`.
pub fn init(choice: ColorChoice) {
    use is_terminal::IsTerminal;
    let (out, err) = match choice {
        ColorChoice::Always => (true, true),
        ColorChoice::Never => (false, false),
        ColorChoice::Auto => {
            // NO_COLOR is honoured whatever its value; see no-color.org.
            let allowed = std::env::var_os("NO_COLOR").is_none()
                && std::env::var("TERM").as_deref() != Ok("dumb");
            (
                allowed && std::io::stdout().is_terminal(),
                allowed && std::io::stderr().is_terminal(),
            )
        }
    };
    STDOUT_COLOR.store(1 + u8::from(out), Ordering::Relaxed);
    STDERR_COLOR.store(1 + u8::from(err), Ordering::Relaxed);
}

/// Force both streams, for tests and for `Ui`'s own decoration decision.
pub fn set_for_tests(on: bool) {
    STDOUT_COLOR.store(1 + u8::from(on), Ordering::Relaxed);
    STDERR_COLOR.store(1 + u8::from(on), Ordering::Relaxed);
}

pub fn stdout_color() -> bool {
    STDOUT_COLOR.load(Ordering::Relaxed) == 2
}

pub fn stderr_color() -> bool {
    STDERR_COLOR.load(Ordering::Relaxed) == 2
}

pub const RESET: &str = "\x1b[0m";
pub const BOLD: &str = "\x1b[1m";
pub const DIM: &str = "\x1b[2m";
pub const GREEN: &str = "\x1b[32m";
pub const YELLOW: &str = "\x1b[33m";
pub const RED: &str = "\x1b[31m";
pub const CYAN: &str = "\x1b[36m";

/// Wrap `text` in `code` when `on`.
pub fn paint(on: bool, code: &str, text: impl AsRef<str>) -> String {
    let text = text.as_ref();
    if on && !text.is_empty() {
        format!("{code}{text}{RESET}")
    } else {
        text.to_string()
    }
}

/// Style for stdout (reports, diffs, status).
pub fn out(code: &str, text: impl AsRef<str>) -> String {
    paint(stdout_color(), code, text)
}

/// Style for stderr (errors, warnings, prompts).
pub fn err(code: &str, text: impl AsRef<str>) -> String {
    paint(stderr_color(), code, text)
}

/// A Jira status, coloured by the category the server files it under.
///
/// By category and not by name, because the name is whatever the instance
/// renamed its workflow to — `Ready for QA`, `Pending Review`, `Пишем` — while
/// the category behind it is always one of three. A category jired does not
/// recognise is left uncoloured rather than guessed at: a status nobody can
/// place should not be dressed up as one of the three that were placed.
pub fn status(name: &str, category: &str) -> String {
    match category {
        "done" => out(GREEN, name),
        "indeterminate" => out(YELLOW, name),
        "new" => out(DIM, name),
        _ => name.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One test, because the mode is process-global and parallel cases would
    /// race each other.
    #[test]
    fn choice_decides_whether_escapes_are_emitted() {
        init(ColorChoice::Never);
        assert!(!stdout_color() && !stderr_color());
        assert_eq!(out(RED, "boom"), "boom");

        init(ColorChoice::Always);
        assert!(stdout_color() && stderr_color());
        assert_eq!(out(RED, "boom"), format!("{RED}boom{RESET}"));

        // An escape pair around nothing is invisible noise in a diff.
        assert_eq!(paint(true, RED, ""), "");

        // Statuses read by category, and an unplaceable one is left alone
        // rather than dressed up as one of the three that were placed.
        init(ColorChoice::Always);
        assert_eq!(status("Done", "done"), format!("{GREEN}Done{RESET}"));
        assert_eq!(
            status("Ready for QA", "indeterminate"),
            format!("{YELLOW}Ready for QA{RESET}")
        );
        assert_eq!(status("Backlog", "new"), format!("{DIM}Backlog{RESET}"));
        assert_eq!(status("Whatever", ""), "Whatever");
        assert_eq!(status("Whatever", "invented"), "Whatever");

        init(ColorChoice::Never);
        assert_eq!(status("Done", "done"), "Done");
    }
}
