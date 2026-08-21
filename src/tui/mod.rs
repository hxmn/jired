//! Full-screen interactive screens [M4.6].
//!
//! Two rules shape everything here. The alternate screen is entered on
//! **stderr**, never stdout, so a screen can run in front of `--json` without
//! corrupting the result document. And every screen is a plain state machine —
//! [`Screen::handle`] takes a key and [`Screen::render`] draws a frame — so the
//! interesting behaviour is testable against a `TestBackend` without a terminal
//! anywhere in the loop.
//!
//! The line-based prompts in [`crate::cli::cmd::prompt`] remain the fallback:
//! this layer is only reachable when [`available`] says the terminal can do it,
//! and callers degrade to prompts and then to exit 7 when it cannot.

pub mod create;
mod field;
mod input;
mod picker;
mod theme;

use crate::error::{JiredError, Result};
use crossterm::event::{self, Event, KeyEvent, KeyEventKind};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::ExecutableCommand;
use ratatui::backend::CrosstermBackend;
use ratatui::{Frame, Terminal};
use std::io::Stderr;
use std::sync::atomic::{AtomicBool, Ordering};

/// What a key press did to a screen.
pub enum Flow<T> {
    /// Keep running.
    Continue,
    /// The user finished; this is the result.
    Done(T),
    /// The user backed out; nothing was produced.
    Cancel,
}

/// One interactive screen.
pub trait Screen {
    /// What a completed run of this screen produces.
    type Output;

    fn render(&mut self, frame: &mut Frame);
    fn handle(&mut self, key: KeyEvent) -> Flow<Self::Output>;
}

/// Whether a full-screen interface stands a chance of working.
///
/// The screens draw on stderr and read from stdin, so both have to be a
/// terminal; `TERM=dumb` and `NO_COLOR`-style opt-outs are honoured because a
/// caller that cannot render escape sequences is better served by prompts.
pub fn available() -> bool {
    use is_terminal::IsTerminal;
    if std::env::var_os("JIRED_NO_TUI").is_some_and(|v| !v.is_empty()) {
        return false;
    }
    std::io::stdin().is_terminal()
        && std::io::stderr().is_terminal()
        && std::env::var("TERM").as_deref() != Ok("dumb")
}

/// Run a screen to completion, returning `None` when the user cancelled.
///
/// The terminal is restored on every exit path — normal return, error, or
/// panic — because a raw-mode terminal left behind is worse than whatever went
/// wrong.
pub fn run<S: Screen>(screen: &mut S) -> Result<Option<S::Output>> {
    let mut terminal = enter()?;
    let outcome = event_loop(&mut terminal, screen);
    leave();
    outcome
}

fn event_loop<S: Screen>(
    terminal: &mut Terminal<CrosstermBackend<Stderr>>,
    screen: &mut S,
) -> Result<Option<S::Output>> {
    loop {
        terminal.draw(|frame| screen.render(frame))?;
        match event::read()? {
            // Windows reports press *and* release; acting on both would type
            // every character twice.
            Event::Key(key) if key.kind == KeyEventKind::Press => match screen.handle(key) {
                Flow::Continue => {}
                Flow::Done(value) => return Ok(Some(value)),
                Flow::Cancel => return Ok(None),
            },
            // A resize invalidates the whole buffer; the redraw at the top of
            // the loop is the response.
            Event::Resize(_, _) => terminal.clear()?,
            _ => {}
        }
    }
}

/// True while the alternate screen is up, so the panic hook and `leave` do not
/// emit escape sequences into a terminal that is already back to normal.
static ACTIVE: AtomicBool = AtomicBool::new(false);

fn enter() -> Result<Terminal<CrosstermBackend<Stderr>>> {
    install_panic_hook();
    enable_raw_mode().map_err(|e| {
        JiredError::generic(format!("cannot switch the terminal to raw mode: {e}"))
            .with_hint("run with --no-input to skip the interactive screen")
    })?;
    let mut stderr = std::io::stderr();
    if let Err(e) = stderr.execute(EnterAlternateScreen) {
        let _ = disable_raw_mode();
        return Err(JiredError::generic(format!(
            "cannot open the alternate screen: {e}"
        )));
    }
    ACTIVE.store(true, Ordering::SeqCst);
    let terminal = Terminal::new(CrosstermBackend::new(stderr));
    if terminal.is_err() {
        leave();
    }
    Ok(terminal?)
}

fn leave() {
    if !ACTIVE.swap(false, Ordering::SeqCst) {
        return;
    }
    let _ = std::io::stderr().execute(LeaveAlternateScreen);
    let _ = disable_raw_mode();
}

/// Chain a terminal restore in front of whatever hook is installed, once per
/// process. Without it a panic inside a screen leaves the user with an
/// invisible cursor, no echo, and the panic message painted over the app.
fn install_panic_hook() {
    static INSTALLED: std::sync::Once = std::sync::Once::new();
    INSTALLED.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            leave();
            previous(info);
        }));
    });
}

/// Render a screen once at a fixed size and return the text of each row.
///
/// Every screen test goes through this: rendering independent of the terminal
/// is the whole point of the [`Screen`] split.
#[cfg(test)]
pub(crate) fn render_lines<S: Screen>(screen: &mut S, width: u16, height: u16) -> Vec<String> {
    use ratatui::backend::TestBackend;
    let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("test terminal");
    terminal.draw(|frame| screen.render(frame)).expect("draw");
    let buffer = terminal.backend().buffer().clone();
    (0..buffer.area.height)
        .map(|y| {
            (0..buffer.area.width)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
                .trim_end()
                .to_string()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn availability_can_be_switched_off_from_the_environment() {
        // The check itself needs a terminal, which tests do not have; what is
        // worth pinning is that the escape hatch short-circuits it.
        temp_env_var("JIRED_NO_TUI", "1", || assert!(!available()));
    }

    fn temp_env_var(key: &str, value: &str, f: impl FnOnce()) {
        // Tests run in threads; this one only touches a variable no other test
        // reads, so the race that matters cannot happen.
        #[allow(unsafe_code)]
        unsafe {
            std::env::set_var(key, value)
        };
        f();
        #[allow(unsafe_code)]
        unsafe {
            std::env::remove_var(key)
        };
    }
}
