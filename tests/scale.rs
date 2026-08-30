//! Large-workspace behaviour [M5.6].
//!
//! The plan claims jired copes with projects of ten thousand issues and that a
//! dirty scan of a thousand files stays under a second. Claims like that are
//! usually wrong, so this measures them instead of asserting them by assumption.
//!
//! Ignored by default because it takes tens of seconds and writes thousands of
//! files. Run it with `cargo test --test scale -- --ignored --nocapture`.

use assert_cmd::Command;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tempfile::TempDir;

/// How many issues the scale run uses.
///
/// Chosen so the run is a few tens of seconds rather than minutes; the numbers
/// scale linearly and are reported per operation, so a regression shows up as a
/// changed rate rather than a changed total.
const ISSUES: usize = 2000;

fn generate(dir: &Path, count: usize) {
    let mut issues = Vec::with_capacity(count);
    for i in 1..=count {
        issues.push(json!({
            "id": (10000 + i).to_string(),
            "key": format!("BIG-{i}"),
            "fields": {
                "summary": format!("Issue number {i} with a reasonably long summary line"),
                "issuetype": {"name": "Task"},
                "status": {"id": "1", "name": "To Do"},
                "priority": {"name": "Medium"},
                "labels": ["generated", "scale"],
                "components": [],
                "created": "2026-01-01T09:00:00.000+0000",
                // Spread over days so an incremental fetch has a real watermark
                // to filter on rather than one indistinguishable instant.
                "updated": format!(
                    "2026-08-{:02}T{:02}:00:00.000+0000",
                    1 + (i % 17),
                    i % 24
                ),
                "description": {
                    "type": "doc",
                    "version": 1,
                    "content": [{
                        "type": "paragraph",
                        "content": [{
                            "type": "text",
                            "text": format!(
                                "Body for issue {i}. It has a few sentences so the \
                                 conversion and hashing work is not trivial. \
                                 See BIG-{} for context.",
                                i.saturating_sub(1).max(1)
                            )
                        }]
                    }]
                },
                "subtasks": [],
                "comment": {"comments": []},
                "attachment": []
            }
        }));
    }
    std::fs::write(
        dir.join("issues.json"),
        serde_json::to_string(&issues).unwrap(),
    )
    .unwrap();

    for (name, body) in [
        (
            "myself.json",
            json!({"accountId": "u1", "displayName": "Scale User"}),
        ),
        (
            "serverInfo.json",
            json!({"version": "1001.0.0", "deploymentType": "Cloud"}),
        ),
        (
            "projects.json",
            json!([{"id": "1", "key": "BIG", "name": "Big"}]),
        ),
        (
            "fields.json",
            json!([{"id": "summary", "name": "Summary", "custom": false}]),
        ),
    ] {
        std::fs::write(dir.join(name), serde_json::to_string(&body).unwrap()).unwrap();
    }
}

struct Timed {
    label: &'static str,
    elapsed: Duration,
    doc: Value,
}

fn run(dir: &Path, fixtures: &Path, label: &'static str, args: &[&str]) -> Timed {
    let start = Instant::now();
    let out = Command::cargo_bin("jired")
        .unwrap()
        .current_dir(dir)
        .env("JIRED_FIXTURES", fixtures)
        .env("JIRED_NO_KEYRING", "1")
        .env("JIRED_SECRET_KEY", "passphrase")
        .env("JIRED_URL", "https://fixture.invalid")
        .env("JIRED_EMAIL", "scale@example.invalid")
        .env("JIRED_TOKEN", "token")
        .env("JIRED_PROJECT", "BIG")
        .args(args)
        .args(["--json", "--no-input"])
        .output()
        .unwrap();
    let elapsed = start.elapsed();
    assert!(
        out.status.success(),
        "`jired {}` failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&out.stderr)
    );
    Timed {
        label,
        elapsed,
        doc: serde_json::from_slice(&out.stdout).unwrap_or(Value::Null),
    }
}

fn db_size(dir: &Path) -> u64 {
    ["state.db", "state.db-wal", "session.db"]
        .iter()
        .filter_map(|n| std::fs::metadata(dir.join(".jired").join(n)).ok())
        .map(|m| m.len())
        .sum()
}

#[test]
#[ignore = "slow: generates thousands of files"]
fn a_large_workspace_stays_workable() {
    let dir = TempDir::new().unwrap();
    let fixtures: PathBuf = dir.path().join(".fixtures");
    std::fs::create_dir_all(&fixtures).unwrap();
    generate(&fixtures, ISSUES);

    let mut timings = Vec::new();
    timings.push(run(dir.path(), &fixtures, "init", &["init", "--no-pull"]));

    let pull = run(dir.path(), &fixtures, "pull (cold)", &["pull"]);
    assert_eq!(
        pull.doc["created"].as_array().unwrap().len(),
        ISSUES,
        "every issue should have been written"
    );
    timings.push(pull);

    // The interesting case: a pull with nothing to do must not rewrite
    // anything, and must not take materially longer than reading the files.
    let repull = run(dir.path(), &fixtures, "pull (no changes)", &["pull"]);
    assert_eq!(repull.doc["unchanged"], ISSUES);
    assert_eq!(repull.doc["updated"].as_array().unwrap().len(), 0);
    timings.push(repull);

    timings.push(run(dir.path(), &fixtures, "status (clean)", &["status"]));
    timings.push(run(dir.path(), &fixtures, "diff (clean)", &["diff"]));

    // Now dirty a tenth of the workspace and time the scan that has to notice.
    let dirty = ISSUES / 10;
    for i in 1..=dirty {
        // Built by the same function the projector names files with. Spelling
        // it out here is how this stopped finding them: the number in a
        // filename is zero-padded, and a literal `BIG-1.` has not matched a
        // written file since that convention landed.
        let name = jired::project::naming::file_name(
            &format!("BIG-{i}"),
            &format!("Issue number {i} with a reasonably long summary line"),
        );
        let path = dir.path().join(&name);
        let text = std::fs::read_to_string(&path).unwrap();
        std::fs::write(&path, text.replace("- scale", "- scale\n- touched")).unwrap();
    }

    let status = run(dir.path(), &fixtures, "status (10% dirty)", &["status"]);
    assert_eq!(status.doc["dirty"].as_array().unwrap().len(), dirty);
    timings.push(status);

    timings.push(run(
        dir.path(),
        &fixtures,
        "push --dry-run (10% dirty)",
        &["push", "--dry-run"],
    ));

    let bytes = db_size(dir.path());
    println!("\n--- {ISSUES} issues ---");
    for t in &timings {
        println!(
            "{:<28} {:>7.2}s  ({:.1} ms/issue)",
            t.label,
            t.elapsed.as_secs_f64(),
            t.elapsed.as_secs_f64() * 1000.0 / ISSUES as f64
        );
    }
    println!(
        "state.db + wal                {:>7.1} MiB ({:.2} KiB/issue)",
        bytes as f64 / (1 << 20) as f64,
        bytes as f64 / 1024.0 / ISSUES as f64
    );

    // The plan's claim: a dirty scan of a thousand files inside a second. Scale
    // the budget to the size actually used, with headroom for slow CI.
    let scan = timings
        .iter()
        .find(|t| t.label == "status (10% dirty)")
        .unwrap();
    let budget = Duration::from_secs_f64(4.0 * ISSUES as f64 / 1000.0);
    assert!(
        scan.elapsed < budget,
        "a dirty scan of {ISSUES} issues took {:.2}s, over the {:.2}s budget",
        scan.elapsed.as_secs_f64(),
        budget.as_secs_f64()
    );

    // A no-op pull must not cost dramatically more than a status.
    let repull = timings
        .iter()
        .find(|t| t.label == "pull (no changes)")
        .unwrap();
    assert!(
        repull.elapsed < Duration::from_secs_f64(10.0 * ISSUES as f64 / 1000.0),
        "an unchanged pull of {ISSUES} issues took {:.2}s",
        repull.elapsed.as_secs_f64()
    );
}
