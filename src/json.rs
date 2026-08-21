//! Versioned JSON output contract [M1.2, D19].
//!
//! In `--json` mode stdout carries exactly one `jired/<name>/1` document and
//! human-facing text is diverted to stderr, so agents can parse stdout blindly.

use crate::error::JiredError;
use crate::style::{self, BOLD, DIM, RED, YELLOW};
use serde::Serialize;
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

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

/// Warnings raised before or outside a command's own result document.
///
/// In JSON mode `warn` cannot print — stderr must stay a single parseable error
/// document — so the text is parked here and folded into whatever document the
/// command emits. Nothing the user needs to act on is silently dropped.
static PARKED_WARNINGS: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// Set once stdout has gone away (`head` closed the pipe). Further writes are
/// pointless, and the process should end quietly rather than on a panic.
static STDOUT_CLOSED: AtomicBool = AtomicBool::new(false);

/// True when a downstream reader closed the pipe; `cli::run` turns this into a
/// success exit rather than a broken-pipe panic.
pub fn stdout_is_closed() -> bool {
    STDOUT_CLOSED.load(Ordering::Relaxed)
}

fn note_write(result: std::io::Result<()>) {
    if let Err(e) = result {
        if e.kind() == std::io::ErrorKind::BrokenPipe {
            STDOUT_CLOSED.store(true, Ordering::Relaxed);
        }
    }
}

/// Verbosity, so the error renderer can decide how much to show.
static VERBOSE: AtomicBool = AtomicBool::new(false);

pub fn set_verbose(on: bool) {
    VERBOSE.store(on, Ordering::Relaxed);
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
    ///
    /// Any warning parked while JSON mode suppressed it is merged in here, so a
    /// consumer sees it in the one place it looks.
    pub fn emit<T: Serialize>(&self, name: &str, payload: &T) {
        if !self.json {
            drain_parked();
            return;
        }
        let mut doc = envelope(name, payload);
        let parked = drain_parked();
        if !parked.is_empty() {
            if let serde_json::Value::Object(map) = &mut doc {
                let entry = map
                    .entry("warnings")
                    .or_insert_with(|| serde_json::Value::Array(Vec::new()));
                if let serde_json::Value::Array(list) = entry {
                    list.extend(parked.into_iter().map(serde_json::Value::from));
                }
            }
        }
        let mut stdout = std::io::stdout().lock();
        note_write(
            serde_json::to_writer_pretty(&mut stdout, &doc)
                .map_err(std::io::Error::other)
                .and_then(|_| stdout.write_all(b"\n"))
                .and_then(|_| stdout.flush()),
        );
    }

    /// Human-facing line, written to stdout.
    ///
    /// Suppressed entirely in JSON mode: stdout is reserved for the result
    /// document, and prose on stderr would sit alongside the structured error
    /// and make *that* stream unparseable too. Everything narration conveys is
    /// already in the JSON.
    pub fn say(&self, msg: impl AsRef<str>) {
        if self.quiet || self.json || stdout_is_closed() {
            return;
        }
        let mut stdout = std::io::stdout().lock();
        note_write(writeln!(stdout, "{}", msg.as_ref()));
    }

    /// Warning about something the user should act on.
    ///
    /// In JSON mode the text is parked for the result document rather than
    /// printed, for the reason given on [`say`](Self::say).
    pub fn warn(&self, msg: impl AsRef<str>) {
        let msg = msg.as_ref();
        if self.json {
            if let Ok(mut parked) = PARKED_WARNINGS.lock() {
                parked.push(msg.to_string());
            }
            return;
        }
        eprintln!("{} {msg}", style::err(YELLOW, "warning:"));
    }

    /// A suggested next command, in git's trailer style.
    pub fn hint(&self, msg: impl AsRef<str>) {
        self.say(style::out(DIM, format!("  ({})", msg.as_ref())));
    }

    pub fn report_error(&self, err: &JiredError) {
        if self.json {
            let mut doc = err.to_json();
            let parked = drain_parked();
            if !parked.is_empty() {
                if let serde_json::Value::Object(map) = &mut doc {
                    map.insert("warnings".into(), serde_json::json!(parked));
                }
            }
            let mut stderr = std::io::stderr().lock();
            let _ = serde_json::to_writer(&mut stderr, &doc);
            let _ = stderr.write_all(b"\n");
            return;
        }

        eprintln!("{} {err}", style::err(RED, "error:"));
        if let Some(hint) = &err.hint {
            eprintln!("{} {hint}", style::err(BOLD, "hint:"));
        }

        let verbose = VERBOSE.load(Ordering::Relaxed);
        if verbose {
            if let serde_json::Value::Object(details) = &err.details {
                for (k, v) in details {
                    let value = match v {
                        serde_json::Value::String(s) => s.clone(),
                        other => other.to_string(),
                    };
                    eprintln!("  {}", style::err(DIM, format!("{k}: {value}")));
                }
            }
        }

        // The exit code is the contract agents and shell scripts read; naming it
        // next to the message saves a trip to the docs.
        let tail = format!(
            "(exit {} · {}{})",
            err.exit_code().as_i32(),
            err.kind.as_str(),
            if verbose {
                ""
            } else {
                " — re-run with -v for details"
            }
        );
        eprintln!("{}", style::err(DIM, tail));
    }
}

fn drain_parked() -> Vec<String> {
    PARKED_WARNINGS
        .lock()
        .map(|mut p| std::mem::take(&mut *p))
        .unwrap_or_default()
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

    /// A warning raised in JSON mode must survive into the result document
    /// rather than corrupting stderr.
    #[test]
    fn json_mode_parks_warnings_instead_of_printing_them() {
        drain_parked();
        let out = Out::new(true, false);
        out.warn("token stored unencrypted");
        assert_eq!(drain_parked(), vec!["token stored unencrypted".to_string()]);
    }
}
