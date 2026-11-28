//! How `diff`'s report reads on a terminal.

use crate::json::Out;
use crate::sync::diff::*;

impl DiffReport {
    pub fn render(&self, out: Out, stat_only: bool) {
        use crate::style::{self, BOLD, CYAN, DIM, GREEN, RED};

        if self.issues.is_empty() {
            out.say(format!("no differences between {}", self.mode.describe()));
            if self.mode == DiffMode::WorkingBase {
                out.hint("use \"jired diff --against remote\" to see what a pull would bring in");
            }
            return;
        }

        // Widths from the data: a fixed 14 wraps on a long key, and a fixed 18
        // wraps on almost every custom field name.
        let key_width = self
            .issues
            .iter()
            .map(|i| i.key.len())
            .max()
            .unwrap_or(10)
            .clamp(8, 30);

        if stat_only {
            // What changed is named, not counted: "0 field(s), +50 -7" for a
            // rewritten description read as nothing to send.
            for issue in &self.issues {
                let added: usize = issue.sections.iter().map(|s| s.added).sum();
                let removed: usize = issue.sections.iter().map(|s| s.removed).sum();
                let names: Vec<&str> = issue
                    .fields
                    .iter()
                    .filter(|f| f.sendable)
                    .map(|f| f.field.as_str())
                    .chain(issue.sections.iter().map(|s| s.section.as_str()))
                    .collect();
                let mut line = format!("{:<key_width$} {}", issue.key, names.join(", "));
                if added + removed > 0 {
                    line.push_str(&format!(
                        ", {} {}",
                        style::out(GREEN, format!("+{added}")),
                        style::out(RED, format!("-{removed}"))
                    ));
                }
                out.say(line);
            }
            out.say(format!(
                "{} issue(s), {} field(s) and {} section(s) changed, {} {}",
                self.stat.issues,
                self.stat.fields,
                self.stat.sections,
                style::out(GREEN, format!("+{}", self.stat.added)),
                style::out(RED, format!("-{}", self.stat.removed))
            ));
            return;
        }

        let field_width = self
            .issues
            .iter()
            .flat_map(|i| i.fields.iter().map(|f| f.field.len()))
            .max()
            .unwrap_or(14)
            .clamp(10, 32);

        for issue in &self.issues {
            out.say(format!(
                "── {} {} {}",
                style::out(CYAN, &issue.key),
                style::out(DIM, format!("({})", issue.status)),
                issue.file.as_deref().unwrap_or("")
            ));
            for f in &issue.fields {
                // A row a push cannot carry is worth seeing next to the ones it
                // can, as long as it says so rather than looking like work.
                let note = if !f.sendable {
                    style::out(DIM, "  (history, not sendable)")
                } else if let Some(origin) = f.origin {
                    style::out(DIM, format!("  ({origin})"))
                } else {
                    String::new()
                };
                out.say(format!(
                    "   {:<field_width$} {} → {}{note}",
                    f.field,
                    style::out(RED, render_value(&f.from)),
                    style::out(GREEN, render_value(&f.to))
                ));
            }
            for s in &issue.sections {
                out.say(style::out(BOLD, format!("   --- {} ---", s.section)));
                for line in s.unified.trim_end().lines() {
                    // Colouring the diff body is the difference between reading
                    // it and counting `+` signs.
                    out.say(match line.chars().next() {
                        Some('+') => style::out(GREEN, line),
                        Some('-') => style::out(RED, line),
                        Some('@') => style::out(CYAN, line),
                        _ => line.to_string(),
                    });
                }
            }
        }
    }
}

fn render_value(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) if s.is_empty() => "(empty)".into(),
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Null => "(none)".into(),
        other => other.to_string(),
    }
}
