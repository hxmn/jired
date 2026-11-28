//! How `reset`'s report reads on a terminal.

use crate::json::Out;
use crate::sync::reset::*;

impl ResetReport {
    /// The listing, shown as a preview before the question and again as the
    /// outcome after it. Same lines both times, so what was agreed to and what
    /// happened can be compared without reading two formats.
    pub fn render(&self, out: Out) {
        use crate::style::{self, CYAN, DIM, YELLOW};

        let width = self.key_width();
        for r in &self.restored {
            out.say(format!(
                "  {}  {}",
                style::out(CYAN, format!("{:<width$}", r.key)),
                if r.dropped.is_empty() {
                    r.file.clone()
                } else {
                    format!("[{}]", r.dropped.join(", "))
                }
            ));
        }
        for r in &self.removed {
            out.say(format!(
                "  {}  {}  {}",
                style::out(CYAN, format!("{:<width$}", r.key)),
                r.file,
                style::out(DIM, "(created locally, never pushed)")
            ));
        }
        for key in &self.moves_undone {
            out.say(format!(
                "  {}  {}",
                style::out(CYAN, format!("{:<width$}", key)),
                style::out(DIM, "board move put back")
            ));
        }
        for k in &self.kept {
            out.say(style::out(
                YELLOW,
                format!("  {:<width$}  kept: {}", k.key, k.reason),
            ));
        }
        for key in &self.clean {
            out.say(style::out(
                DIM,
                format!("  {key:<width$}  no local changes"),
            ));
        }
        for w in &self.warnings {
            out.warn(w);
        }
    }

    fn key_width(&self) -> usize {
        self.restored
            .iter()
            .chain(&self.removed)
            .map(|r| r.key.len())
            .chain(self.kept.iter().map(|k| k.key.len()))
            .chain(self.moves_undone.iter().map(String::len))
            .chain(self.clean.iter().map(String::len))
            .max()
            .unwrap_or(10)
            .clamp(8, 30)
    }
}
