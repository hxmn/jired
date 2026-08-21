//! Progress reporting for the commands that can take a while.
//!
//! Everything here writes to **stderr**, so `--json` stdout and a piped report
//! stay exactly as they were. It is silent whenever the output belongs to a
//! machine (`--json`, `--quiet`) or the terminal cannot animate, which is also
//! what keeps test output readable.

use crate::json::Out;
use indicatif::{ProgressBar, ProgressStyle};
use std::time::Duration;

pub struct Progress {
    bar: Option<ProgressBar>,
}

impl Progress {
    /// A spinner for work whose size is not known yet.
    pub fn spinner(out: Out, message: &str) -> Self {
        if !Self::visible(out) {
            return Self { bar: None };
        }
        let bar = ProgressBar::new_spinner();
        bar.set_style(
            ProgressStyle::with_template("  {spinner} {msg}")
                .unwrap_or_else(|_| ProgressStyle::default_spinner()),
        );
        bar.set_message(message.to_string());
        bar.enable_steady_tick(Duration::from_millis(90));
        Self { bar: Some(bar) }
    }

    /// A counted bar for a loop of known length.
    pub fn bar(out: Out, total: usize, message: &str) -> Self {
        // Below a handful of items the bar flashes past and is pure noise.
        if !Self::visible(out) || total < 5 {
            return Self { bar: None };
        }
        let bar = ProgressBar::new(total as u64);
        bar.set_style(
            ProgressStyle::with_template("  {spinner} {msg} {pos}/{len} [{bar:24}]")
                .unwrap_or_else(|_| ProgressStyle::default_bar())
                .progress_chars("=> "),
        );
        bar.set_message(message.to_string());
        Self { bar: Some(bar) }
    }

    fn visible(out: Out) -> bool {
        use is_terminal::IsTerminal;
        !out.json && !out.quiet && std::io::stderr().is_terminal()
    }

    pub fn set_message(&self, message: impl Into<String>) {
        if let Some(b) = &self.bar {
            b.set_message(message.into());
        }
    }

    pub fn inc(&self) {
        if let Some(b) = &self.bar {
            b.inc(1);
        }
    }

    /// Remove the bar before anything else prints, so a report never lands on
    /// top of a half-drawn line. `Drop` does the same for the error paths.
    pub fn finish(&self) {
        if let Some(b) = &self.bar {
            b.finish_and_clear();
        }
    }
}

impl Drop for Progress {
    fn drop(&mut self) {
        if let Some(b) = &self.bar {
            b.finish_and_clear();
        }
    }
}
