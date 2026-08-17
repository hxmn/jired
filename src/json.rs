//! Versioned JSON output contract [M1.2, D19].
//!
//! In `--json` mode stdout carries exactly one `jired/<name>/1` document and
//! human-facing text is diverted to stderr, so agents can parse stdout blindly.

use crate::error::JiredError;
use serde::Serialize;
use std::io::Write;

/// Wraps a command payload with its schema tag.
pub fn envelope<T: Serialize>(name: &str, payload: &T) -> serde_json::Value {
    let mut obj = serde_json::Map::new();
    obj.insert("schema".into(), format!("jired/{name}/1").into());
    match serde_json::to_value(payload) {
        Ok(serde_json::Value::Object(map)) => obj.extend(map),
        Ok(other) => {
            obj.insert("value".into(), other);
        }
        Err(e) => {
            obj.insert("error".into(), e.to_string().into());
        }
    }
    serde_json::Value::Object(obj)
}

/// Output sink shared by every command handler.
///
/// Keeping stdout behind this type is what guarantees the TUI and progress
/// reporting can never corrupt machine-readable output.
#[derive(Debug, Clone, Copy)]
pub struct Out {
    pub json: bool,
    pub quiet: bool,
}

impl Out {
    pub fn new(json: bool, quiet: bool) -> Self {
        Self { json, quiet }
    }

    /// Emit the single machine-readable result document (no-op outside JSON mode).
    pub fn emit<T: Serialize>(&self, name: &str, payload: &T) {
        if !self.json {
            return;
        }
        let doc = envelope(name, payload);
        let mut stdout = std::io::stdout().lock();
        let _ = serde_json::to_writer_pretty(&mut stdout, &doc);
        let _ = stdout.write_all(b"\n");
    }

    /// Human-facing line, written to stdout.
    ///
    /// Suppressed entirely in JSON mode: stdout is reserved for the result
    /// document, and prose on stderr would sit alongside the structured error
    /// and make *that* stream unparseable too. Everything narration conveys is
    /// already in the JSON.
    pub fn say(&self, msg: impl AsRef<str>) {
        if self.quiet || self.json {
            return;
        }
        println!("{}", msg.as_ref());
    }

    /// Warning about something the user should act on.
    ///
    /// Suppressed in JSON mode for the same reason as [`say`](Self::say);
    /// commands that model warnings carry them in their result document.
    pub fn warn(&self, msg: impl AsRef<str>) {
        if self.json {
            return;
        }
        eprintln!("warning: {}", msg.as_ref());
    }

    pub fn report_error(&self, err: &JiredError) {
        if self.json {
            let mut stderr = std::io::stderr().lock();
            let _ = serde_json::to_writer(&mut stderr, &err.to_json());
            let _ = stderr.write_all(b"\n");
        } else {
            eprintln!("error: {err}");
            if let Some(hint) = &err.hint {
                eprintln!("hint: {hint}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envelope_tags_schema_and_flattens_payload() {
        #[derive(Serialize)]
        struct P {
            fetched: u32,
        }
        let v = envelope("fetch", &P { fetched: 3 });
        assert_eq!(v["schema"], "jired/fetch/1");
        assert_eq!(v["fetched"], 3);
    }
}
