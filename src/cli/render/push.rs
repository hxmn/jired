//! How `push`'s report reads on a terminal.

use crate::json::Out;
use crate::sync::push::*;
use serde_json::Value;

impl PushReport {
    /// The lines `--dry-run` and the pre-push confirmation share.
    pub fn plan_lines(&self, include_requests: bool) -> Vec<String> {
        use crate::style::{self, CYAN, DIM, GREEN, RED};
        let mut lines = Vec::new();
        for r in &self.results {
            let head = match (&r.new_key, r.ok) {
                (Some(k), _) => format!("{} → {k}", r.key),
                (None, true) => r.key.clone(),
                (None, false) => format!("{} (failed)", r.key),
            };
            lines.push(format!("── {}", style::out(CYAN, head)));
            for op in &r.ops {
                let mark = match op.status {
                    OpOutcome::Planned => String::new(),
                    OpOutcome::Done => style::out(GREEN, "  ✓"),
                    OpOutcome::Skipped => style::out(DIM, "  (skipped)"),
                    OpOutcome::Failed => style::out(RED, "  ✗"),
                };
                lines.push(format!(
                    "   {:<20} {}{}",
                    op.kind,
                    op.summary.as_deref().unwrap_or(""),
                    mark
                ));
                if include_requests {
                    if let Some(req) = &op.request {
                        lines.push(format!("     {}", style::out(DIM, compact(req))));
                    }
                }
                if let Some(e) = &op.error {
                    lines.push(format!("     {} {e}", style::out(RED, "error:")));
                }
            }
            if let Some(e) = &r.error {
                lines.push(format!("   {} {e}", style::out(RED, "error:")));
            }
            // The hint is the actionable half of a failure; the report used to
            // keep only the message and throw this away.
            if let Some(h) = &r.hint {
                lines.push(format!("   {} {h}", style::out(DIM, "hint:")));
            }
        }
        lines
    }

    /// The rich-text fields of every planned request, as the server will
    /// receive them, for `--show-body`.
    ///
    /// The request body in `--json` holds the same text, but escaped into a
    /// JSON string, and the wire form is exactly what a reader needs to see:
    /// it is where a literal `+x+` turning into an underline shows.
    pub fn body_lines(&self) -> Vec<String> {
        use crate::style::{self, CYAN, DIM};
        let mut lines = Vec::new();
        for r in &self.results {
            for op in &r.ops {
                let Some(body) = op.request.as_ref().and_then(|q| q.get("body")) else {
                    continue;
                };
                for (name, value) in rich_text_fields(body) {
                    lines.push(format!(
                        "── {} {} {}",
                        style::out(CYAN, &r.key),
                        name,
                        style::out(DIM, format!("({}, as sent)", op.kind))
                    ));
                    let text = match value {
                        Value::String(s) => s.clone(),
                        Value::Null => "(cleared)".to_string(),
                        other => serde_json::to_string_pretty(other).unwrap_or_default(),
                    };
                    lines.extend(text.lines().map(|l| {
                        format!("{} {l}", style::out(DIM, "│"))
                            .trim_end()
                            .to_string()
                    }));
                }
            }
        }
        lines
    }

    pub fn render(&self, out: Out) {
        self.render_with(out, false);
    }

    /// [`PushReport::render`], with the text of every planned body under
    /// `show_body` in place of the one-line request.
    pub fn render_with(&self, out: Out, show_body: bool) {
        if self.results.is_empty() {
            out.say("nothing to push");
            out.hint("use \"jired status\" to see what jired thinks has changed");
            for w in &self.warnings {
                out.warn(w);
            }
            return;
        }
        for line in self.plan_lines(self.dry_run && !show_body) {
            out.say(line);
        }
        if show_body {
            out.say("");
            for line in self.body_lines() {
                out.say(line);
            }
        }
        if !self.stale.is_empty() {
            out.say(format!(
                "\n{} issue(s) changed on the server since your last sync: {}",
                self.stale.len(),
                self.stale.join(", ")
            ));
            out.hint("use \"jired pull\" first, or \"jired push --force\" to overwrite");
        }
        for w in &self.warnings {
            out.warn(w);
        }
        if self.dry_run {
            out.say("\ndry run — nothing was sent");
            return;
        }

        // A count is the one thing a partial failure has to say plainly.
        let failed = self.results.iter().filter(|r| !r.ok).count();
        let total = self.results.len();
        out.say(match failed {
            0 => format!("\npushed {total} issue(s)"),
            n if n == total => format!("\n{n} issue(s) failed, none pushed"),
            n => format!("\npushed {} of {total} issue(s), {n} failed", total - n),
        });
        if failed > 0 {
            out.hint("fix the causes above, then \"jired push --resume\"");
        }
        match &self.verify {
            None => {}
            Some(found) if found.is_empty() => {
                out.say("verified: the server holds what the files asked for")
            }
            Some(found) => {
                use crate::style::{self, CYAN, RED};
                out.say(format!(
                    "\n{} {} field(s) the server does not hold as the file asked:",
                    style::out(RED, "verify:"),
                    found.len()
                ));
                for m in found {
                    out.say(format!(
                        "  {} {:<16} expected {}, the server has {}",
                        style::out(CYAN, &m.key),
                        m.field,
                        compact(&m.expected),
                        compact(&m.actual)
                    ));
                }
                out.hint(
                    "each file now shows what the server holds; write the expected value \
                     again and push, or make the change in Jira",
                );
            }
        }
    }
}

/// The rich-text values in one request body, by name: the description of an
/// edit, the body of a comment, the note of a worklog.
fn rich_text_fields(body: &Value) -> Vec<(&'static str, &Value)> {
    let mut found = Vec::new();
    if let Some(d) = body.get("fields").and_then(|f| f.get("description")) {
        found.push(("description", d));
    }
    if let Some(b) = body.get("body") {
        found.push(("comment", b));
    }
    if let Some(c) = body.get("comment") {
        found.push(("worklog comment", c));
    }
    found
}

/// Truncate a request body for display.
///
/// Counting characters rather than bytes: a summary with an accent in it is the
/// common case, and `&s[..240]` would panic on the byte boundary.
fn compact(v: &Value) -> String {
    let s = v.to_string();
    if s.chars().count() > 240 {
        format!("{}…", s.chars().take(240).collect::<String>())
    } else {
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A summary with an accent used to panic the whole command: `&s[..240]`
    /// slices bytes, and 240 need not be a character boundary.
    #[test]
    fn compact_never_splits_a_multibyte_character() {
        let long: String = "é".repeat(400);
        let text = compact(&json!({ "summary": long }));
        assert!(text.ends_with('…'));
    }
}
