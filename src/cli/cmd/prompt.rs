//! Terminal prompting — the last rung of the config resolution chain.
//!
//! Deliberately line-based rather than a full TUI: this rung must work over ssh,
//! inside `git rebase --exec`, and anywhere the alternate screen is unavailable.
//! The richer wizard screens live in the TUI layer [M4].

use crate::config::Prompter;
use crate::error::Result;
use std::io::{BufRead, Write};

pub struct TerminalPrompter;

impl Prompter for TerminalPrompter {
    fn prompt(&mut self, key: &str, doc: &str, secret: bool) -> Result<Option<String>> {
        // Prompts go to stderr so `--json` stdout is never polluted.
        let mut err = std::io::stderr();
        writeln!(err, "{doc}")?;
        write!(err, "{key}: ")?;
        err.flush()?;

        let line = if secret {
            read_secret()?
        } else {
            let mut buf = String::new();
            std::io::stdin().lock().read_line(&mut buf)?;
            buf
        };
        let value = line.trim().to_string();
        Ok((!value.is_empty()).then_some(value))
    }
}

/// Read a line with terminal echo disabled, so a pasted token never lands in
/// the scrollback buffer.
///
/// `libc::termios` has no safe wrapper in this dependency set; the unsafe calls
/// are confined to this function and the original settings are always restored.
#[cfg(unix)]
#[allow(unsafe_code)]
fn read_secret() -> Result<String> {
    use std::os::unix::io::AsRawFd;
    let fd = std::io::stdin().as_raw_fd();
    let mut term: libc::termios = unsafe { std::mem::zeroed() };
    let has_term = unsafe { libc::tcgetattr(fd, &mut term) } == 0;
    let saved = term;
    if has_term {
        term.c_lflag &= !libc::ECHO;
        unsafe { libc::tcsetattr(fd, libc::TCSANOW, &term) };
    }
    let mut buf = String::new();
    let res = std::io::stdin().lock().read_line(&mut buf);
    if has_term {
        unsafe { libc::tcsetattr(fd, libc::TCSANOW, &saved) };
        eprintln!();
    }
    res?;
    Ok(buf)
}

#[cfg(not(unix))]
fn read_secret() -> Result<String> {
    let mut buf = String::new();
    std::io::stdin().lock().read_line(&mut buf)?;
    Ok(buf)
}

/// Ask a yes/no question; `assume_yes` short-circuits it for scripts.
pub fn confirm(question: &str, assume_yes: bool, interactive: bool) -> Result<bool> {
    if assume_yes {
        return Ok(true);
    }
    if !interactive {
        return Err(crate::error::JiredError::missing_value(format!(
            "confirmation required: {question}"
        ))
        .with_hint("pass --yes to confirm non-interactively"));
    }
    let mut err = std::io::stderr();
    write!(err, "{question} [y/N]: ")?;
    err.flush()?;
    let mut buf = String::new();
    std::io::stdin().lock().read_line(&mut buf)?;
    Ok(matches!(
        buf.trim().to_ascii_lowercase().as_str(),
        "y" | "yes"
    ))
}
