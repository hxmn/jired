//! Terminal prompting — the last rung of the config resolution chain, plus the
//! line-based chrome the `init` wizard is built from.
//!
//! Deliberately line-based rather than a full TUI: this rung must work over ssh,
//! inside `git rebase --exec`, and anywhere the alternate screen is unavailable.
//! The richer wizard screens live in the TUI layer [M4].
//!
//! Everything here writes to stderr. stdout belongs to results — the JSON
//! document in `--json` mode, the summary otherwise — so a wizard can run in
//! front of a pipeline without corrupting it.

use crate::config::Prompter;
use crate::error::{JiredError, Result};
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

/// Line-based chrome for an interactive command.
///
/// One type carries both halves of the decision every prompt has to make: may we
/// ask at all (`interactive`), and may we decorate (`decorate` is off in JSON and
/// quiet modes, where the output is somebody else's input).
pub struct Ui {
    interactive: bool,
    decorate: bool,
    color: bool,
}

const RESET: &str = "\x1b[0m";
const BOLD: &str = "\x1b[1m";
const DIM: &str = "\x1b[2m";
const GREEN: &str = "\x1b[32m";
const YELLOW: &str = "\x1b[33m";
const RED: &str = "\x1b[31m";
const CYAN: &str = "\x1b[36m";

impl Ui {
    pub fn new(interactive: bool, decorate: bool) -> Self {
        use is_terminal::IsTerminal;
        let color = decorate
            && std::io::stderr().is_terminal()
            && std::env::var_os("NO_COLOR").is_none()
            && std::env::var("TERM").as_deref() != Ok("dumb");
        Self {
            interactive,
            decorate,
            color,
        }
    }

    pub fn interactive(&self) -> bool {
        self.interactive
    }

    fn paint(&self, code: &str, text: &str) -> String {
        if self.color {
            format!("{code}{text}{RESET}")
        } else {
            text.to_string()
        }
    }

    /// Chrome: skipped when the output belongs to a machine.
    fn line(&self, text: impl AsRef<str>) {
        if self.decorate {
            self.emit(text);
        }
    }

    /// A question, or part of one. Always printed: asking without showing what
    /// is being asked would leave the user staring at a bare cursor.
    fn emit(&self, text: impl AsRef<str>) {
        let _ = writeln!(std::io::stderr(), "{}", text.as_ref());
    }

    pub fn blank(&self) {
        self.line("");
    }

    /// Opening banner of a wizard.
    pub fn title(&self, text: &str) {
        self.blank();
        self.line(self.paint(BOLD, text));
    }

    /// A fact the user does not have to act on.
    pub fn info(&self, text: impl AsRef<str>) {
        self.line(format!("  {}", text.as_ref()));
    }

    /// Secondary detail, indented under whatever came before it.
    pub fn detail(&self, text: impl AsRef<str>) {
        self.line(format!("  {}", self.paint(DIM, text.as_ref())));
    }

    /// Something worked.
    pub fn good(&self, text: impl AsRef<str>) {
        self.line(format!("{} {}", self.paint(GREEN, "✓"), text.as_ref()));
    }

    /// Something needs attention but is not fatal.
    pub fn caution(&self, text: impl AsRef<str>) {
        self.line(format!("{} {}", self.paint(YELLOW, "!"), text.as_ref()));
    }

    /// Something failed; the wizard usually offers a way out next.
    pub fn failure(&self, text: impl AsRef<str>) {
        self.line(format!("{} {}", self.paint(RED, "✗"), text.as_ref()));
    }

    /// `label: value` in the closing summary.
    pub fn field(&self, label: &str, value: impl AsRef<str>) {
        self.line(format!(
            "  {:<11} {}",
            self.paint(DIM, label),
            value.as_ref()
        ));
    }

    /// A line that belongs to a question — a menu row, a correction — rather
    /// than to the decoration around it, so it prints even in quiet modes.
    pub fn question_line(&self, text: impl AsRef<str>) {
        self.emit(format!("  {}", text.as_ref()));
    }

    /// Ask for a line of text.
    ///
    /// `help` is shown once above the input; `default` is used when the user
    /// just presses enter. With neither a default nor `allow_empty`, an empty
    /// answer re-asks rather than failing — a mistyped enter should not cost
    /// somebody their half-finished setup.
    pub fn ask(&self, label: &str, help: Option<&str>, default: Option<&str>) -> Result<String> {
        loop {
            let value = self.ask_raw(label, help, default)?;
            if !value.is_empty() {
                return Ok(value);
            }
            if let Some(d) = default {
                return Ok(d.to_string());
            }
            self.emit(format!(
                "  {}",
                self.paint(DIM, "a value is needed here; press ctrl-c to stop")
            ));
        }
    }

    /// Ask for a line of text, accepting an empty answer as "no answer".
    pub fn ask_optional(&self, label: &str, help: Option<&str>) -> Result<Option<String>> {
        let value = self.ask_raw(label, help, None)?;
        Ok((!value.is_empty()).then_some(value))
    }

    fn ask_raw(&self, label: &str, help: Option<&str>, default: Option<&str>) -> Result<String> {
        self.require_interactive(label)?;
        self.emit("");
        if !label.is_empty() {
            let heading = match default {
                Some(d) => format!("{label} {}", self.paint(DIM, &format!("[{d}]"))),
                None => label.to_string(),
            };
            self.emit(self.paint(BOLD, &heading));
        }
        if let Some(h) = help {
            for l in h.lines() {
                self.emit(format!("  {}", self.paint(DIM, l)));
            }
        }
        let mut err = std::io::stderr();
        write!(err, "{} ", self.paint(CYAN, ">"))?;
        err.flush()?;
        let mut buf = String::new();
        let read = std::io::stdin().lock().read_line(&mut buf)?;
        if read == 0 {
            // stdin closed mid-wizard: nothing is going to arrive, so say so
            // instead of spinning on empty reads.
            return Err(JiredError::missing_value(format!(
                "input ended while asking for {label}"
            )));
        }
        Ok(buf.trim().to_string())
    }

    /// Ask for a credential, with terminal echo disabled.
    pub fn ask_secret(&self, label: &str, help: Option<&str>) -> Result<String> {
        loop {
            self.require_interactive(label)?;
            self.emit("");
            self.emit(self.paint(BOLD, label));
            if let Some(h) = help {
                for l in h.lines() {
                    self.emit(format!("  {}", self.paint(DIM, l)));
                }
            }
            let mut err = std::io::stderr();
            write!(err, "{} ", self.paint(CYAN, ">"))?;
            err.flush()?;
            let value = read_secret()?.trim().to_string();
            if !value.is_empty() {
                return Ok(value);
            }
            self.emit(format!(
                "  {}",
                self.paint(
                    DIM,
                    "nothing was pasted; try again, or press ctrl-c to stop"
                )
            ));
        }
    }

    /// Ask a yes/no question with an explicit default.
    pub fn confirm(&self, question: &str, default_yes: bool) -> Result<bool> {
        if !self.interactive {
            return Ok(default_yes);
        }
        let hint = if default_yes { "[Y/n]" } else { "[y/N]" };
        self.emit("");
        let mut err = std::io::stderr();
        write!(
            err,
            "{} {} {} ",
            self.paint(CYAN, ">"),
            question,
            self.paint(DIM, hint)
        )?;
        err.flush()?;
        let mut buf = String::new();
        std::io::stdin().lock().read_line(&mut buf)?;
        Ok(match buf.trim().to_ascii_lowercase().as_str() {
            "y" | "yes" => true,
            "n" | "no" => false,
            _ => default_yes,
        })
    }

    /// Offer a short numbered menu and return the chosen index.
    pub fn choose(&self, label: &str, options: &[String], default: usize) -> Result<usize> {
        loop {
            self.emit("");
            self.emit(self.paint(BOLD, label));
            for (i, option) in options.iter().enumerate() {
                self.emit(format!("  {:>2}. {option}", i + 1));
            }
            let answer = self.ask_raw("", None, Some(&(default + 1).to_string()))?;
            match answer.parse::<usize>() {
                Ok(n) if n >= 1 && n <= options.len() => return Ok(n - 1),
                _ if answer.is_empty() => return Ok(default),
                _ => self.emit(format!(
                    "  {}",
                    self.paint(
                        DIM,
                        &format!("enter a number between 1 and {}", options.len())
                    )
                )),
            }
        }
    }

    /// Run `f` while a spinner turns, so a slow network call does not look like
    /// a hang. Falls back to a plain line when there is nothing to animate.
    pub fn working<T>(&self, message: &str, f: impl FnOnce() -> T) -> T {
        if !self.decorate || !self.color {
            self.line(format!("  {message}"));
            return f();
        }
        let spinner = indicatif::ProgressBar::new_spinner();
        spinner.set_style(
            indicatif::ProgressStyle::with_template("  {spinner} {msg}")
                .unwrap_or_else(|_| indicatif::ProgressStyle::default_spinner()),
        );
        spinner.set_message(message.to_string());
        spinner.enable_steady_tick(std::time::Duration::from_millis(90));
        let result = f();
        spinner.finish_and_clear();
        result
    }

    fn require_interactive(&self, what: &str) -> Result<()> {
        if self.interactive {
            return Ok(());
        }
        Err(
            JiredError::missing_value(format!("{what} is needed and prompting is disabled"))
                .with_hint("supply it as a flag or environment variable"),
        )
    }
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
