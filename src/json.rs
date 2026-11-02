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

/// How much of the result document to print.
///
/// A full document is a complete record, which is right for a person reading it
/// once and wrong for a caller that wanted three values: every `--json` call
/// otherwise ends in a script that re-prints a subset of what jired just built.
/// Both settings are opt-in, because the shape a schema promises is worth more
/// than the bytes to everyone who is not counting them.
#[derive(Debug, Clone, Default)]
pub struct Shape {
    /// Dotted paths to keep; everything else is pruned. Empty keeps it all.
    pub fields: Vec<String>,
    /// Drop nulls and empty collections.
    pub compact: bool,
}

static SHAPE: Mutex<Option<Shape>> = Mutex::new(None);

/// Set the projection every later `emit` applies. Called once, from `cli::run`.
pub fn set_shape(shape: Shape) {
    if let Ok(mut slot) = SHAPE.lock() {
        *slot = Some(shape);
    }
}

/// Apply `--fields` and `--json-compact` to a finished document.
///
/// `schema` always survives: a document that cannot say what it is has stopped
/// being one, and a caller that asked for two fields still has to be able to
/// tell what it is reading.
fn shaped(mut doc: serde_json::Value) -> serde_json::Value {
    let Ok(slot) = SHAPE.lock() else {
        return doc;
    };
    let Some(shape) = slot.as_ref() else {
        return doc;
    };
    if !shape.fields.is_empty() {
        doc = project(&doc, &shape.fields);
    }
    if shape.compact {
        prune(&mut doc);
    }
    doc
}

/// Keep only the named paths, in the shape the document already has.
///
/// A path walks objects by name and *maps over* arrays, so `results.ok` keeps
/// the `ok` of every result rather than needing an index nobody knows in
/// advance. That is what makes `--fields results.key,results.ok` answer "how did
/// each issue do" in one call.
fn project(doc: &serde_json::Value, paths: &[String]) -> serde_json::Value {
    let mut out = serde_json::Map::new();
    if let Some(schema) = doc.get("schema") {
        out.insert("schema".into(), schema.clone());
    }
    for path in paths {
        let mut parts = path.split('.').filter(|p| !p.is_empty());
        let Some(head) = parts.next() else { continue };
        let Some(value) = doc.get(head) else { continue };
        let rest: Vec<&str> = parts.collect();
        let kept = narrow(value, &rest);
        match out.get_mut(head) {
            Some(existing) => merge(existing, kept),
            None => {
                out.insert(head.to_string(), kept);
            }
        }
    }
    serde_json::Value::Object(out)
}

/// Two paths under one head merge rather than replacing each other — at every
/// depth. Merging one level down only, `results.ops.kind,results.ops.status`
/// kept the `ops` of whichever path came last, so a list asking for an op's
/// kind, status and error came back as the error alone, and `--json-compact`
/// pruned that to `{}` for every op that had not failed.
fn merge(into: &mut serde_json::Value, from: serde_json::Value) {
    match (into, from) {
        (serde_json::Value::Object(a), serde_json::Value::Object(b)) => {
            for (k, v) in b {
                match a.get_mut(&k) {
                    Some(existing) => merge(existing, v),
                    None => {
                        a.insert(k, v);
                    }
                }
            }
        }
        (serde_json::Value::Array(a), serde_json::Value::Array(b)) => {
            for (x, y) in a.iter_mut().zip(b) {
                merge(x, y);
            }
        }
        (into, from) => *into = from,
    }
}

/// The part of `value` that `rest` names, mapping over arrays as it goes.
fn narrow(value: &serde_json::Value, rest: &[&str]) -> serde_json::Value {
    let Some((head, tail)) = rest.split_first() else {
        return value.clone();
    };
    match value {
        serde_json::Value::Array(items) => serde_json::Value::Array(
            items
                .iter()
                .map(|i| narrow(i, rest))
                .filter(|v| !v.is_null())
                .collect(),
        ),
        serde_json::Value::Object(map) => {
            let mut out = serde_json::Map::new();
            if let Some(found) = map.get(*head) {
                out.insert((*head).to_string(), narrow(found, tail));
            }
            serde_json::Value::Object(out)
        }
        _ => serde_json::Value::Null,
    }
}

/// Drop nulls and empty collections, everywhere.
fn prune(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(map) => {
            for v in map.values_mut() {
                prune(v);
            }
            map.retain(|k, v| k == "schema" || !is_empty(v));
        }
        serde_json::Value::Array(items) => {
            for v in items.iter_mut() {
                prune(v);
            }
        }
        _ => {}
    }
}

fn is_empty(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Null => true,
        serde_json::Value::Array(a) => a.is_empty(),
        serde_json::Value::Object(o) => o.is_empty(),
        _ => false,
    }
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
    /// Nothing on either stream unless the run fails. Implies `quiet`, and
    /// takes the warnings with it — see [`Out::warn`].
    pub silent: bool,
}

impl Out {
    pub fn new(json: bool, quiet: bool) -> Self {
        Self {
            json,
            quiet,
            silent: false,
        }
    }

    /// The sink `--silent` asks for: quiet, and without the warnings.
    ///
    /// `quiet` is set along with it rather than checked beside it everywhere,
    /// so nothing that already asks "am I quiet" — the progress bars, every
    /// `say` — has to learn a second question with the same answer.
    pub fn silent(json: bool) -> Self {
        Self {
            json,
            quiet: true,
            silent: true,
        }
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
        // Shaped last, so a warning parked before the projection can still be
        // asked for by name and is still dropped by `--fields` when it was not.
        let doc = shaped(doc);
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
    /// printed, for the reason given on [`say`](Self::say) — and it is parked
    /// under `--silent` too: silence is about the streams a person reads, and
    /// dropping a warning out of the document a machine reads would be losing
    /// it rather than quieting it.
    pub fn warn(&self, msg: impl AsRef<str>) {
        let msg = msg.as_ref();
        if self.json {
            if let Ok(mut parked) = PARKED_WARNINGS.lock() {
                parked.push(msg.to_string());
            }
            return;
        }
        if self.silent {
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

    fn doc() -> serde_json::Value {
        serde_json::json!({
            "schema": "jired/push/1",
            "summary": {"ok": 2, "failed": 0, "stale": []},
            "dry_run": false,
            "results": [
                {"key": "TSK-1", "ok": true, "new_key": null, "error": null, "ops": []},
                {"key": "TSK-2", "ok": false, "new_key": null, "error": "boom", "ops": []},
            ],
            "warnings": [],
        })
    }

    /// The reason `--fields` exists: reading two values out of a document
    /// should not need a script that re-prints what jired just built.
    #[test]
    fn a_projection_keeps_the_named_paths_and_the_schema() {
        let kept = project(&doc(), &["summary.ok".to_string()]);
        assert_eq!(kept["schema"], "jired/push/1");
        assert_eq!(kept["summary"]["ok"], 2);
        assert!(kept["summary"].get("failed").is_none(), "{kept}");
        assert!(kept.get("results").is_none(), "{kept}");
    }

    /// A path maps over an array, because an index nobody knows in advance is
    /// no way to ask "how did each issue do".
    #[test]
    fn a_path_through_an_array_answers_for_every_element() {
        let kept = project(
            &doc(),
            &["results.key".to_string(), "results.ok".to_string()],
        );
        assert_eq!(kept["results"][0]["key"], "TSK-1");
        assert_eq!(kept["results"][0]["ok"], true);
        assert_eq!(kept["results"][1]["key"], "TSK-2");
        assert_eq!(kept["results"][1]["ok"], false);
        // Two paths under one head merge rather than the second replacing it.
        assert!(kept["results"][0].get("error").is_none(), "{kept}");
    }

    /// Three paths into the ops of every result: each op keeps all three. The
    /// merge went one level deep, so the last path's `ops` replaced the
    /// others', and a push that had not failed showed `[{}, {}]` once
    /// `--json-compact` dropped the null errors.
    #[test]
    fn paths_into_a_nested_array_merge_at_every_depth() {
        let mut d = doc();
        d["results"][0]["ops"] = serde_json::json!([
            {"kind": "update_fields", "status": "done", "summary": "x", "error": null},
            {"kind": "transition", "status": "done", "summary": "y", "error": null},
        ]);
        let mut kept = project(
            &d,
            &[
                "results.key".to_string(),
                "results.ops.kind".to_string(),
                "results.ops.status".to_string(),
                "results.ops.error".to_string(),
            ],
        );
        prune(&mut kept);
        assert_eq!(
            kept["results"][0],
            serde_json::json!({"key": "TSK-1", "ops": [
                {"kind": "update_fields", "status": "done"},
                {"kind": "transition", "status": "done"},
            ]})
        );
    }

    #[test]
    fn a_path_nobody_has_is_left_out_rather_than_being_an_error() {
        let kept = project(&doc(), &["nonsense".to_string()]);
        assert_eq!(kept["schema"], "jired/push/1");
        assert_eq!(kept.as_object().unwrap().len(), 1);
    }

    /// Nulls and empties are what a caller pays for and never reads.
    #[test]
    fn compacting_drops_nulls_and_empties_but_never_the_schema() {
        let mut d = doc();
        prune(&mut d);
        assert_eq!(d["schema"], "jired/push/1");
        assert!(d["results"][0].get("new_key").is_none(), "{d}");
        assert!(d["results"][0].get("ops").is_none(), "an empty list: {d}");
        assert!(d.get("warnings").is_none(), "{d}");
        // `false` is a value, not an absence.
        assert_eq!(d["dry_run"], false);
        assert_eq!(d["results"][1]["error"], "boom");
        // And `stale: []` goes, while the summary it sits in stays.
        assert_eq!(d["summary"]["ok"], 2);
        assert!(d["summary"].get("stale").is_none(), "{d}");
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

    /// Silence is about the streams a person reads. The document a machine
    /// reads still carries the warning, because dropping it there would be
    /// losing it rather than quieting it.
    #[test]
    fn a_silent_run_still_parks_its_warnings_for_the_document() {
        drain_parked();
        let out = Out::silent(true);
        assert!(
            out.quiet,
            "silence implies quiet, so nothing checks for both"
        );
        out.warn("token stored unencrypted");
        assert_eq!(drain_parked(), vec!["token stored unencrypted".to_string()]);

        // Outside JSON mode there is nowhere to park it, and nowhere it should
        // go: `--silent` was asked for.
        let out = Out::silent(false);
        out.warn("token stored unencrypted");
        assert!(drain_parked().is_empty());
    }
}
