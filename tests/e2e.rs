//! End-to-end CLI tests [M1.12, M2.4, M2.9].
//!
//! Every test drives the real `jired` binary against the recorded fixture
//! client, so what is exercised is exactly what a user or an agent would run —
//! including exit codes, stdout/stderr separation, and the JSON contract.

use assert_cmd::Command;
use serde_json::Value;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

/// Which product a workspace talks to. The same suite runs against both so a
/// change cannot pass on Cloud while quietly breaking Data Center.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Deployment {
    Cloud,
    DataCenter,
}

impl Deployment {
    fn dir(self) -> &'static str {
        match self {
            Deployment::Cloud => "cloud",
            Deployment::DataCenter => "datacenter",
        }
    }

    fn project(self) -> &'static str {
        match self {
            Deployment::Cloud => "PROJ",
            Deployment::DataCenter => "DC",
        }
    }

    /// The issue every shared test edits.
    fn issue_file(self) -> &'static str {
        match self {
            Deployment::Cloud => "PROJ-142. Fix login flow.md",
            Deployment::DataCenter => "DC-77. Rotate the LDAP bind credential.md",
        }
    }

    fn key(self) -> &'static str {
        match self {
            Deployment::Cloud => "PROJ-142",
            Deployment::DataCenter => "DC-77",
        }
    }

    /// A line of the description that no lossy region covers, safe to edit.
    fn editable_line(self) -> &'static str {
        match self {
            Deployment::Cloud => "Steps to reproduce are below.",
            Deployment::DataCenter => "See DC-78 for the rollback plan.",
        }
    }
}

fn fixtures_for(deployment: Deployment) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(deployment.dir())
}

/// A workspace directory plus a `jired` invoker wired to the fixture server.
struct Ws {
    dir: TempDir,
    fixtures: PathBuf,
    deployment: Deployment,
}

impl Ws {
    /// Each workspace gets its own copy of the fixtures so a test that mutates
    /// the "server" cannot leak into another test.
    fn of(deployment: Deployment) -> Self {
        let dir = TempDir::new().expect("tempdir");
        let fixtures = dir.path().join(".fixtures");
        std::fs::create_dir_all(&fixtures).unwrap();
        for entry in std::fs::read_dir(fixtures_for(deployment))
            .unwrap()
            .flatten()
        {
            std::fs::copy(entry.path(), fixtures.join(entry.file_name())).unwrap();
        }
        Self {
            dir,
            fixtures,
            deployment,
        }
    }

    fn new() -> Self {
        Self::of(Deployment::Cloud)
    }

    fn path(&self) -> &Path {
        self.dir.path()
    }

    fn issue_file(&self) -> &'static str {
        self.deployment.issue_file()
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut cmd = Command::cargo_bin("jired").expect("binary");
        cmd.current_dir(self.path())
            .env("JIRED_FIXTURES", &self.fixtures)
            .env("JIRED_NO_KEYRING", "1")
            .env("JIRED_SECRET_KEY", "test-passphrase")
            .env("JIRED_TOKEN", "fixture-token")
            .env("JIRED_PROJECT", self.deployment.project())
            .env_remove("JIRED_LOG");

        match self.deployment {
            // Cloud authenticates as email + API token.
            Deployment::Cloud => {
                cmd.env("JIRED_URL", "https://fixture.invalid")
                    .env("JIRED_EMAIL", "ilya@example.com");
            }
            // Data Center uses a bearer PAT and no email at all.
            Deployment::DataCenter => {
                cmd.env("JIRED_URL", "https://jira.corp.example/jira")
                    .env("JIRED_AUTH", "pat")
                    .env_remove("JIRED_EMAIL");
            }
        }
        cmd.args(args).arg("--no-input");
        cmd
    }

    /// Run a command expecting success, returning parsed JSON stdout.
    fn json(&self, args: &[&str]) -> Value {
        let out = self.cmd(args).arg("--json").output().expect("run");
        assert!(
            out.status.success(),
            "`jired {}` failed with {:?}\nstderr: {}",
            args.join(" "),
            out.status.code(),
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
            panic!(
                "stdout of `jired {}` is not JSON: {e}\n{}",
                args.join(" "),
                String::from_utf8_lossy(&out.stdout)
            )
        })
    }

    /// Run a command expecting a specific exit code, returning the JSON error.
    fn json_err(&self, args: &[&str], code: i32) -> Value {
        let out = self.cmd(args).arg("--json").output().expect("run");
        assert_eq!(
            out.status.code(),
            Some(code),
            "`jired {}` exit code\nstdout: {}\nstderr: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_slice(&out.stderr).unwrap_or_else(|e| {
            panic!(
                "stderr of `jired {}` is not JSON: {e}\n{}",
                args.join(" "),
                String::from_utf8_lossy(&out.stderr)
            )
        })
    }

    fn init(&self) -> Value {
        self.json(&["init", "--no-pull"])
    }

    fn read(&self, name: &str) -> String {
        std::fs::read_to_string(self.path().join(name)).unwrap_or_else(|e| {
            let listing: Vec<String> = std::fs::read_dir(self.path())
                .map(|d| {
                    d.flatten()
                        .map(|e| e.file_name().to_string_lossy().into_owned())
                        .collect()
                })
                .unwrap_or_default();
            panic!("cannot read {name}: {e}\nworkspace contains: {listing:?}")
        })
    }

    fn write(&self, name: &str, text: &str) {
        std::fs::write(self.path().join(name), text).unwrap();
    }

    /// Change an issue on the "server" so the next fetch sees it move.
    fn mutate_remote(&self, key: &str, f: impl Fn(&mut Value)) {
        let path = self.fixtures.join("issues.json");
        let mut issues: Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        for issue in issues.as_array_mut().unwrap() {
            if issue["key"] == key {
                f(issue);
            }
        }
        std::fs::write(&path, serde_json::to_string_pretty(&issues).unwrap()).unwrap();
    }
}

const ISSUE_FILE: &str = "PROJ-142. Fix login flow.md";

// ---------------------------------------------------------------------------
// init
// ---------------------------------------------------------------------------

#[test]
fn init_creates_the_workspace_and_agent_docs() {
    let ws = Ws::new();
    let report = ws.init();

    assert_eq!(report["schema"], "jired/init/1");
    assert_eq!(report["account"]["deployment"], "cloud");
    assert_eq!(report["account"]["auth_kind"], "api_token");
    assert_eq!(report["account"]["display_name"], "Ilya A");
    assert_eq!(report["workspace"]["project"], "PROJ");

    assert!(ws.path().join(".jired/config.toml").is_file());
    assert!(ws.path().join(".jired/session.db").is_file());
    assert!(ws.path().join(".jired/state.db").is_file());

    let gitignore = ws.read(".gitignore");
    assert!(gitignore.contains("/.jired/"), "{gitignore}");

    for doc in ["CLAUDE.md", "AGENTS.md"] {
        let text = ws.read(doc);
        assert!(
            text.contains("jired push --dry-run"),
            "{doc} lacks the recipe"
        );
        assert!(
            text.contains("exit code 6"),
            "{doc} lacks the exit-code table"
        );
    }
}

#[test]
fn init_never_writes_the_token_to_disk_in_readable_form() {
    let ws = Ws::new();
    ws.init();
    let config = ws.read(".jired/config.toml");
    assert!(
        !config.contains("fixture-token"),
        "token leaked into config.toml"
    );

    // The session database holds it only as an encrypted blob.
    let db = std::fs::read(ws.path().join(".jired/session.db")).unwrap();
    assert!(
        !db.windows(14)
            .any(|w| w == b"fixture-token\0"[..13].to_vec().as_slice()),
        "token appears verbatim in session.db"
    );
}

#[test]
fn a_second_init_without_force_is_a_usage_error() {
    let ws = Ws::new();
    ws.init();
    let err = ws.json_err(&["init", "--no-pull"], 2);
    assert_eq!(err["schema"], "jired/error/1");
    assert_eq!(err["kind"], "usage");
}

#[test]
fn a_missing_required_value_exits_7_and_names_it() {
    let ws = Ws::new();
    let mut cmd = Command::cargo_bin("jired").unwrap();
    let out = cmd
        .current_dir(ws.path())
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("JIRED_FIXTURES", &ws.fixtures)
        .args(["init", "--no-input", "--json"])
        .output()
        .unwrap();

    assert_eq!(out.status.code(), Some(7));
    let err: Value = serde_json::from_slice(&out.stderr).unwrap();
    assert_eq!(err["kind"], "missing_value");
    assert_eq!(err["details"]["key"], "server.url");
    assert_eq!(err["details"]["env"], "JIRED_URL");
}

// ---------------------------------------------------------------------------
// fetch and pull
// ---------------------------------------------------------------------------

#[test]
fn pull_writes_one_file_per_issue() {
    let ws = Ws::new();
    ws.init();
    let report = ws.json(&["pull"]);

    assert_eq!(report["schema"], "jired/pull/1");
    assert_eq!(report["created"].as_array().unwrap().len(), 3);
    assert!(report["conflicts"].as_array().unwrap().is_empty());

    for name in [
        ISSUE_FILE,
        "PROJ-143. Add integration test.md",
        "PROJ-150. Rotate IdP certificate.md",
    ] {
        assert!(ws.path().join(name).is_file(), "missing {name}");
    }
}

#[test]
fn the_written_file_has_the_documented_structure() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);
    let text = ws.read(ISSUE_FILE);

    assert!(text.starts_with("---\n"));
    assert!(text.contains("key: PROJ-142"));
    assert!(text.contains("summary: Fix login flow"));
    assert!(
        text.contains("assignee: '@[Ilya A](5b10a2ilya)'")
            || text.contains("assignee: \"@[Ilya A](5b10a2ilya)\"")
    );
    assert!(text.contains("story_points: 5"));
    assert!(text.contains("customfield_10456"));

    assert!(text.contains("\n# Fix login flow\n"));
    assert!(text.contains("\n## Description\n"));
    assert!(text.contains("\n## Checklist\n"));
    assert!(text.contains("\n## Comments\n"));
    assert!(text.contains("\n## Attachments\n"));

    assert!(text.contains("- [ ] PROJ-143 Add integration test"));
    assert!(text.contains("- [x] PROJ-144 Update runbook"));
    assert!(text.contains("### [c-10812] Jane D — 2026-08-15T10:32:00Z"));
    assert!(text.contains("Repro confirmed on staging."));
    assert!(text.contains("login-trace.har"));
}

#[test]
fn a_lossy_region_is_marked_and_flagged_in_frontmatter() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);
    let text = ws.read(ISSUE_FILE);

    assert!(
        text.contains("lossy: true"),
        "the panel must set the lossy flag"
    );
    assert!(
        text.contains("<!--jired:raw:"),
        "the panel must render as a marker pair"
    );
    assert!(text.contains("<!--/jired:raw:"));
    assert!(
        text.contains("Do not deploy before the cert rotation."),
        "the approximation must show the panel's text"
    );
}

#[test]
fn re_pulling_an_unchanged_project_touches_nothing() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);
    let before = std::fs::metadata(ws.path().join(ISSUE_FILE))
        .unwrap()
        .modified()
        .unwrap();
    let text_before = ws.read(ISSUE_FILE);

    let report = ws.json(&["pull"]);
    assert_eq!(report["created"].as_array().unwrap().len(), 0);
    assert_eq!(report["updated"].as_array().unwrap().len(), 0);
    assert_eq!(report["unchanged"], 3);

    assert_eq!(text_before, ws.read(ISSUE_FILE));
    assert_eq!(
        before,
        std::fs::metadata(ws.path().join(ISSUE_FILE))
            .unwrap()
            .modified()
            .unwrap(),
        "an unchanged issue must not be rewritten"
    );
}

#[test]
fn a_remote_summary_change_renames_the_file_and_keeps_the_key() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);

    ws.mutate_remote("PROJ-142", |issue| {
        issue["fields"]["summary"] = "Fix the SSO login flow".into();
        issue["fields"]["updated"] = "2026-08-18T09:00:00.000+0000".into();
    });

    let report = ws.json(&["pull"]);
    let renamed = report["renamed"].as_array().unwrap();
    assert_eq!(renamed.len(), 1);
    assert_eq!(renamed[0]["key"], "PROJ-142");
    assert_eq!(renamed[0]["from"], ISSUE_FILE);
    assert_eq!(renamed[0]["to"], "PROJ-142. Fix the SSO login flow.md");

    assert!(!ws.path().join(ISSUE_FILE).exists());
    assert!(ws
        .path()
        .join("PROJ-142. Fix the SSO login flow.md")
        .is_file());
}

// ---------------------------------------------------------------------------
// status and diff
// ---------------------------------------------------------------------------

#[test]
fn status_and_diff_see_a_local_edit_without_the_network() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);

    let text = ws.read(ISSUE_FILE).replace(
        "summary: Fix login flow",
        "summary: Fix login flow properly",
    );
    ws.write(ISSUE_FILE, &text);

    let status = ws.json(&["status"]);
    assert_eq!(status["schema"], "jired/status/1");
    let dirty = status["dirty"].as_array().unwrap();
    assert_eq!(dirty.len(), 1);
    assert_eq!(dirty[0]["key"], "PROJ-142");
    assert!(dirty[0]["fields"]
        .as_array()
        .unwrap()
        .contains(&Value::from("summary")));

    let diff = ws.json(&["diff"]);
    assert_eq!(diff["schema"], "jired/diff/1");
    assert_eq!(diff["mode"], "working-base");
    let issue = &diff["issues"][0];
    assert_eq!(issue["key"], "PROJ-142");
    let field = &issue["fields"][0];
    assert_eq!(field["field"], "summary");
    assert_eq!(field["from"], "Fix login flow");
    assert_eq!(field["to"], "Fix login flow properly");
}

#[test]
fn a_clean_workspace_reports_nothing_to_push() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);

    let status = ws.json(&["status"]);
    assert!(status["dirty"].as_array().unwrap().is_empty());
    assert!(status["conflicts"].as_array().unwrap().is_empty());
    assert!(status["pending_ops"].as_array().unwrap().is_empty());

    let diff = ws.json(&["diff"]);
    assert!(diff["issues"].as_array().unwrap().is_empty());
}

// ---------------------------------------------------------------------------
// push
// ---------------------------------------------------------------------------

#[test]
fn dry_run_shows_the_exact_calls_and_sends_nothing() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);

    let text = ws
        .read(ISSUE_FILE)
        .replace("summary: Fix login flow", "summary: Renamed locally");
    ws.write(ISSUE_FILE, &text);

    let report = ws.json(&["push", "--dry-run"]);
    assert_eq!(report["schema"], "jired/push/1");
    assert_eq!(report["dry_run"], true);

    let result = &report["results"][0];
    assert_eq!(result["key"], "PROJ-142");
    let op = &result["ops"][0];
    assert_eq!(op["kind"], "update_fields");
    assert_eq!(op["status"], "planned");
    assert_eq!(op["request"]["method"], "PUT");
    assert_eq!(
        op["request"]["body"]["fields"]["summary"],
        "Renamed locally"
    );

    // Nothing was sent, so the issue is still dirty afterwards.
    let status = ws.json(&["status"]);
    assert_eq!(status["dirty"].as_array().unwrap().len(), 1);
}

#[test]
fn push_applies_a_field_edit_and_leaves_the_workspace_clean() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);

    let text = ws
        .read(ISSUE_FILE)
        .replace("- backend\n", "- backend\n- sso\n");
    ws.write(ISSUE_FILE, &text);

    let report = ws.json(&["push"]);
    let result = &report["results"][0];
    assert_eq!(result["ok"], true);
    assert_eq!(result["ops"][0]["kind"], "update_fields");
    assert_eq!(result["ops"][0]["status"], "done");

    let status = ws.json(&["status"]);
    assert!(
        status["dirty"].as_array().unwrap().is_empty(),
        "base must advance after a confirmed push: {status}"
    );
    assert!(ws.read(ISSUE_FILE).contains("- sso"));
}

#[test]
fn push_creates_a_new_comment_and_rewrites_its_heading_with_the_real_id() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);

    let text = ws.read(ISSUE_FILE).replace(
        "## Attachments",
        "### [new]\n\nFixed by rotating the IdP cert.\n\n## Attachments",
    );
    ws.write(ISSUE_FILE, &text);

    let report = ws.json(&["push"]);
    let ops = report["results"][0]["ops"].as_array().unwrap();
    assert!(ops
        .iter()
        .any(|o| o["kind"] == "create_comment" && o["status"] == "done"));

    let after = ws.read(ISSUE_FILE);
    assert!(
        !after.contains("### [new]"),
        "the placeholder must be replaced"
    );
    assert!(after.contains("Fixed by rotating the IdP cert."));
    assert!(
        after.contains("### [c-9"),
        "the comment must come back with a server id:\n{after}"
    );
}

#[test]
fn editing_a_read_only_region_blocks_the_push_with_exit_6() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);

    let text = ws.read(ISSUE_FILE).replace(
        "Do not deploy before the cert rotation.",
        "Do not deploy before the cert rotation. EDITED BY HAND.",
    );
    ws.write(ISSUE_FILE, &text);

    let err = ws.json_err(&["push"], 6);
    assert_eq!(err["kind"], "lossy");
    assert_eq!(err["details"]["issue"], "PROJ-142");
    assert!(err["details"]["regions"].as_array().unwrap().len() == 1);
    assert!(err["hint"].as_str().unwrap().contains("--allow-lossy"));
}

#[test]
fn an_untouched_lossy_region_pushes_back_byte_identically() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);

    // Edit only the description text *outside* the panel.
    let text = ws.read(ISSUE_FILE).replace(
        "Steps to reproduce are below.",
        "Steps to reproduce are listed below.",
    );
    ws.write(ISSUE_FILE, &text);

    let journal = ws.path().join("calls.jsonl");
    let out = ws
        .cmd(&["push"])
        .arg("--json")
        .env("JIRED_FIXTURE_JOURNAL", &journal)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let calls: Vec<Value> = std::fs::read_to_string(&journal)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let description = calls
        .iter()
        .find_map(|c| c["body"]["fields"]["description"].as_object().cloned())
        .expect("a description update was sent");

    let panel = description["content"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["type"] == "panel")
        .expect("the panel survived the round trip");

    // Byte-identical to the fixture's original node, attributes and all.
    assert_eq!(
        serde_json::to_string(panel).unwrap(),
        serde_json::to_string(&serde_json::json!({
            "type": "panel",
            "attrs": { "panelType": "warning" },
            "content": [{
                "type": "paragraph",
                "content": [{ "type": "text", "text": "Do not deploy before the cert rotation." }]
            }]
        }))
        .unwrap()
    );
}

#[test]
fn push_refuses_a_stale_issue_with_exit_5() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);

    let text = ws
        .read(ISSUE_FILE)
        .replace("summary: Fix login flow", "summary: Local rename");
    ws.write(ISSUE_FILE, &text);

    // Someone else edits the same issue on the server.
    ws.mutate_remote("PROJ-142", |issue| {
        issue["fields"]["priority"] = serde_json::json!({ "name": "Low" });
        issue["fields"]["updated"] = "2026-08-19T09:00:00.000+0000".into();
    });

    let out = ws.cmd(&["push", "--json"]).output().unwrap();
    assert_eq!(out.status.code(), Some(5));

    let report: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(report["stale"][0], "PROJ-142");
    assert_eq!(report["results"][0]["ok"], false);
    assert_eq!(report["results"][0]["error_kind"], "conflict");
    assert_eq!(report["results"][0]["ops"][0]["status"], "skipped");

    // The local edit is untouched, so nothing was lost.
    assert!(ws.read(ISSUE_FILE).contains("summary: Local rename"));
}

// ---------------------------------------------------------------------------
// conflicts
// ---------------------------------------------------------------------------

#[test]
fn a_field_changed_on_both_sides_makes_pull_exit_5_and_write_nothing() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);

    let text = ws
        .read(ISSUE_FILE)
        .replace("summary: Fix login flow", "summary: Ours");
    ws.write(ISSUE_FILE, &text);
    let before = ws.read(ISSUE_FILE);

    ws.mutate_remote("PROJ-142", |issue| {
        issue["fields"]["summary"] = "Theirs".into();
        issue["fields"]["updated"] = "2026-08-19T09:00:00.000+0000".into();
    });

    let out = ws.cmd(&["pull", "--json"]).output().unwrap();
    assert_eq!(out.status.code(), Some(5));

    let report: Value = serde_json::from_slice(&out.stdout).unwrap();
    let conflicts = report["conflicts"].as_array().unwrap();
    assert_eq!(conflicts.len(), 1);
    assert_eq!(conflicts[0]["key"], "PROJ-142");
    assert!(conflicts[0]["fields"]
        .as_array()
        .unwrap()
        .contains(&Value::from("summary")));

    assert_eq!(
        before,
        ws.read(ISSUE_FILE),
        "an aborted pull must write nothing"
    );
    assert!(report["updated"].as_array().unwrap().is_empty());
}

#[test]
fn a_non_conflicting_remote_change_merges_into_a_dirty_file() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);

    // Local: add a label. Remote: change the priority. Disjoint fields.
    let text = ws
        .read(ISSUE_FILE)
        .replace("- backend\n", "- backend\n- local-only\n");
    ws.write(ISSUE_FILE, &text);

    ws.mutate_remote("PROJ-142", |issue| {
        issue["fields"]["priority"] = serde_json::json!({ "name": "Lowest" });
        issue["fields"]["updated"] = "2026-08-19T09:00:00.000+0000".into();
    });

    let report = ws.json(&["pull"]);
    assert!(
        report["conflicts"].as_array().unwrap().is_empty(),
        "disjoint edits must not conflict: {report}"
    );

    let after = ws.read(ISSUE_FILE);
    assert!(after.contains("- local-only"), "the local edit survived");
    assert!(
        after.contains("priority: Lowest"),
        "the remote edit applied"
    );
}

#[test]
fn strategy_merge_writes_conflict_markers() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);

    let text = ws
        .read(ISSUE_FILE)
        .replace("Steps to reproduce are below.", "Our version of the steps.");
    ws.write(ISSUE_FILE, &text);

    ws.mutate_remote("PROJ-142", |issue| {
        let content = issue["fields"]["description"]["content"]
            .as_array_mut()
            .unwrap();
        content[2]["content"][0]["text"] = "Their version of the steps.".into();
        issue["fields"]["updated"] = "2026-08-19T09:00:00.000+0000".into();
    });

    let out = ws
        .cmd(&["pull", "--strategy", "merge", "--json"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(5));

    let after = ws.read(ISSUE_FILE);
    assert!(after.contains("<<<<<<< LOCAL"), "{after}");
    assert!(after.contains("Our version of the steps."));
    assert!(after.contains("======="));
    assert!(after.contains("Their version of the steps."));
    assert!(after.contains(">>>>>>> REMOTE (PROJ-142@"));
}

// ---------------------------------------------------------------------------
// contracts
// ---------------------------------------------------------------------------

#[test]
fn json_mode_keeps_stdout_parseable_and_prose_on_stderr() {
    let ws = Ws::new();
    ws.init();
    let out = ws.cmd(&["pull", "--json"]).output().unwrap();

    serde_json::from_slice::<Value>(&out.stdout).expect("stdout is pure JSON");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("new"),
        "human summary belongs on stderr: {stderr}"
    );
}

#[test]
fn every_json_output_validates_against_its_published_schema() {
    let ws = Ws::new();
    let init = ws.init();
    let pull = ws.json(&["pull"]);
    let status = ws.json(&["status"]);
    let diff = ws.json(&["diff"]);
    let push = ws.json(&["push", "--dry-run"]);

    for (name, doc) in [
        ("init", &init),
        ("pull", &pull),
        ("status", &status),
        ("diff", &diff),
        ("push", &push),
    ] {
        let schema_text = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("schemas/{name}.json")),
        )
        .unwrap();
        let schema: Value = serde_json::from_str(&schema_text).unwrap();
        let validator = jsonschema::validator_for(&schema).unwrap();
        let errors: Vec<String> = validator.iter_errors(doc).map(|e| e.to_string()).collect();
        assert!(
            errors.is_empty(),
            "jired/{name}/1 violated: {errors:?}\n{doc}"
        );
    }
}

#[test]
fn the_schema_command_lists_and_prints_schemas() {
    let ws = Ws::new();
    let mut cmd = Command::cargo_bin("jired").unwrap();
    let out = cmd.arg("schema").output().unwrap();
    assert!(out.status.success());
    let listing = String::from_utf8_lossy(&out.stdout);
    assert!(listing.contains("frontmatter"));
    assert!(listing.contains("pull"));

    let mut cmd = Command::cargo_bin("jired").unwrap();
    let out = cmd.args(["schema", "pull"]).output().unwrap();
    let doc: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(doc["$id"], "jired/pull/1");
    let _ = ws;
}

#[test]
fn the_frontmatter_of_a_written_file_matches_its_schema() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);

    let text = ws.read(ISSUE_FILE);
    let yaml = text
        .strip_prefix("---\n")
        .and_then(|r| r.split_once("\n---\n"))
        .expect("frontmatter block")
        .0;
    let value: Value = serde_yaml_ng::from_str(yaml).expect("frontmatter is valid YAML");

    let schema: Value = serde_json::from_str(
        &std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("schemas/frontmatter.json"),
        )
        .unwrap(),
    )
    .unwrap();
    let validator = jsonschema::validator_for(&schema).unwrap();
    let errors: Vec<String> = validator
        .iter_errors(&value)
        .map(|e| e.to_string())
        .collect();
    assert!(
        errors.is_empty(),
        "frontmatter violated its schema: {errors:?}"
    );
}

#[test]
fn concurrent_invocations_are_serialized_by_the_lock() {
    let ws = Ws::new();
    ws.init();

    // Hold the lock from outside by locking the file the same way jired does.
    let lock_path = ws.path().join(".jired/lock");
    let file = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
        .unwrap();
    file.try_lock().unwrap();

    let err = ws.json_err(&["fetch"], 8);
    assert_eq!(err["kind"], "locked");

    file.unlock().unwrap();
    ws.json(&["fetch"]);
}

#[test]
fn an_interrupted_push_is_replayed_by_resume() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);

    // The server rejects the update, so base is not advanced and the journal
    // keeps the operation — exactly the state a crash would leave behind.
    let faults = serde_json::json!({
        "PUT /issue/PROJ-142": { "kind": "generic", "message": "boom" }
    });
    std::fs::write(
        ws.fixtures.join("faults.json"),
        serde_json::to_string_pretty(&faults).unwrap(),
    )
    .unwrap();

    let text = ws
        .read(ISSUE_FILE)
        .replace("summary: Fix login flow", "summary: Retried");
    ws.write(ISSUE_FILE, &text);

    let out = ws.cmd(&["push", "--json"]).output().unwrap();
    assert_eq!(out.status.code(), Some(1), "the push must report failure");

    let status = ws.json(&["status"]);
    let pending = status["pending_ops"].as_array().unwrap();
    assert_eq!(pending.len(), 1, "the journal records the unfinished work");
    assert_eq!(pending[0]["status"], "failed");
    assert_eq!(
        status["dirty"].as_array().unwrap().len(),
        1,
        "base must not advance for a failed push"
    );

    // With the server healthy again, --resume completes the same work.
    std::fs::remove_file(ws.fixtures.join("faults.json")).unwrap();
    let report = ws.json(&["push", "--resume"]);
    assert_eq!(report["results"][0]["key"], "PROJ-142");
    assert_eq!(report["results"][0]["ok"], true);

    let status = ws.json(&["status"]);
    assert!(status["dirty"].as_array().unwrap().is_empty());
    assert!(status["pending_ops"].as_array().unwrap().is_empty());

    // A confirmed summary change renames the file, so the old name is gone.
    assert!(!ws.path().join(ISSUE_FILE).exists());
    assert!(ws.read("PROJ-142. Retried.md").contains("summary: Retried"));
}

#[test]
fn resume_with_nothing_pending_is_a_no_op() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);
    let report = ws.json(&["push", "--resume"]);
    assert!(report["results"].as_array().unwrap().is_empty());
}

// ---------------------------------------------------------------------------
// Both deployments
//
// Cloud and Data Center differ in API version, rich-text format and user
// identifiers. These run the same workflow against both so a change cannot
// pass on one product while quietly breaking the other.
// ---------------------------------------------------------------------------

/// Run one scenario against each deployment, labelling any failure with which.
fn for_each_deployment(scenario: impl Fn(&Ws)) {
    for deployment in [Deployment::Cloud, Deployment::DataCenter] {
        let ws = Ws::of(deployment);
        ws.init();
        ws.json(&["pull"]);
        scenario(&ws);
    }
}

#[test]
fn init_detects_the_deployment_from_the_server_not_the_url() {
    let cloud = Ws::of(Deployment::Cloud);
    assert_eq!(cloud.init()["account"]["deployment"], "cloud");

    let dc = Ws::of(Deployment::DataCenter);
    let report = dc.init();
    assert_eq!(report["account"]["deployment"], "datacenter");
    assert_eq!(report["account"]["auth_kind"], "pat");
    assert_eq!(report["workspace"]["project"], "DC");
}

#[test]
fn data_center_wiki_markup_becomes_markdown() {
    let ws = Ws::of(Deployment::DataCenter);
    ws.init();
    ws.json(&["pull"]);
    let text = ws.read(ws.issue_file());

    // Structure survives the wiki → Markdown conversion.
    assert!(text.contains("### Steps"), "h3 heading:\n{text}");
    assert!(text.contains("- Generate the replacement secret"));
    assert!(
        text.contains("```java"),
        "fenced code with a language:\n{text}"
    );
    assert!(text.contains("| Environment | Owner |"), "table:\n{text}");
    assert!(text.contains("@[jane](jane)"), "mention:\n{text}");

    // Data Center identifies users by username, not an account id.
    assert!(text.contains("assignee: '@[Ilya A](ilya)'"), "{text}");

    // A `{panel}` macro has no Markdown form, so it is preserved read-only.
    assert!(text.contains("lossy: true"));
    assert!(text.contains("<!--jired:raw:"));
    assert!(text.contains("No directory changes during the release window."));
}

#[test]
fn data_center_custom_fields_are_mapped_by_name() {
    let ws = Ws::of(Deployment::DataCenter);
    ws.init();
    ws.json(&["pull"]);
    let text = ws.read(ws.issue_file());

    // Ids differ per instance, so these are resolved through /field.
    assert!(text.contains("story_points: 3"), "{text}");
    assert!(text.contains("epic: DC-50"), "{text}");
    // Data Center returns sprints as a stringified Java object.
    assert!(text.contains("sprint: Ops Sprint 12"), "{text}");
    // An unmapped custom field round-trips under its raw id.
    assert!(text.contains("customfield_10300"), "{text}");
}

#[test]
fn a_pull_is_a_no_op_on_both_deployments() {
    for_each_deployment(|ws| {
        let before = ws.read(ws.issue_file());
        let report = ws.json(&["pull"]);
        assert_eq!(
            report["updated"].as_array().unwrap().len(),
            0,
            "{:?} rewrote an unchanged issue: {report}",
            ws.deployment
        );
        assert_eq!(before, ws.read(ws.issue_file()));
    });
}

#[test]
fn a_field_edit_pushes_on_both_deployments() {
    for_each_deployment(|ws| {
        let file = ws.issue_file();
        let text = ws
            .read(file)
            .replace("- security\n", "- security\n- audited\n");
        let text = text.replace("- auth\n", "- auth\n- audited\n");
        ws.write(file, &text);

        let report = ws.json(&["push"]);
        let result = &report["results"][0];
        assert_eq!(result["ok"], true, "{:?}: {report}", ws.deployment);
        assert_eq!(result["ops"][0]["kind"], "update_fields");

        let status = ws.json(&["status"]);
        assert!(
            status["dirty"].as_array().unwrap().is_empty(),
            "{:?} left the workspace dirty: {status}",
            ws.deployment
        );
    });
}

#[test]
fn an_untouched_lossy_region_survives_a_push_on_both_deployments() {
    for_each_deployment(|ws| {
        let file = ws.issue_file();
        let line = ws.deployment.editable_line();
        let text = ws
            .read(file)
            .replace(line, "Edited outside the read-only block.");
        assert!(
            !text.contains(line),
            "{:?}: edit did not apply",
            ws.deployment
        );
        ws.write(file, &text);

        let journal = ws.path().join("calls.jsonl");
        let out = ws
            .cmd(&["push", "--json"])
            .env("JIRED_FIXTURE_JOURNAL", &journal)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{:?}: {}",
            ws.deployment,
            String::from_utf8_lossy(&out.stderr)
        );

        let calls: Vec<Value> = std::fs::read_to_string(&journal)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        let description = calls
            .iter()
            .find_map(|c| {
                c["body"]["fields"]["description"]
                    .as_str()
                    .map(str::to_string)
            })
            .or_else(|| {
                calls.iter().find_map(|c| {
                    c["body"]["fields"]["description"]
                        .as_object()
                        .map(|o| serde_json::to_string(o).unwrap())
                })
            })
            .unwrap_or_else(|| panic!("{:?}: no description was sent", ws.deployment));

        match ws.deployment {
            Deployment::Cloud => assert!(
                description.contains("\"type\":\"panel\""),
                "the panel node must be rebuilt verbatim: {description}"
            ),
            Deployment::DataCenter => assert!(
                description.contains("{panel:title=Change freeze}")
                    && description
                        .contains("No directory changes during the release window.\n{panel}"),
                "the panel macro must be rebuilt verbatim: {description}"
            ),
        }
        assert!(description.contains("Edited outside the read-only block."));
    });
}

#[test]
fn editing_a_read_only_region_is_blocked_on_both_deployments() {
    for_each_deployment(|ws| {
        let file = ws.issue_file();
        let needle = match ws.deployment {
            Deployment::Cloud => "Do not deploy before the cert rotation.",
            Deployment::DataCenter => "No directory changes during the release window.",
        };
        let text = ws.read(file).replace(needle, "Rewritten by hand.");
        ws.write(file, &text);

        let err = ws.json_err(&["push"], 6);
        assert_eq!(err["kind"], "lossy", "{:?}", ws.deployment);
        assert_eq!(err["details"]["issue"], ws.deployment.key());
    });
}

#[test]
fn a_conflict_aborts_the_pull_on_both_deployments() {
    for_each_deployment(|ws| {
        let file = ws.issue_file();
        let text = ws
            .read(file)
            .replace("priority: High\n", "priority: Lowest\n")
            .replace("priority: Medium\n", "priority: Lowest\n");
        ws.write(file, &text);
        let before = ws.read(file);

        ws.mutate_remote(ws.deployment.key(), |issue| {
            issue["fields"]["priority"] = serde_json::json!({ "name": "Highest" });
            issue["fields"]["updated"] = "2026-08-20T09:00:00.000+0000".into();
        });

        let out = ws.cmd(&["pull", "--json"]).output().unwrap();
        assert_eq!(out.status.code(), Some(5), "{:?}", ws.deployment);
        let report: Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(report["conflicts"][0]["key"], ws.deployment.key());
        assert_eq!(before, ws.read(file), "an aborted pull must write nothing");
    });
}

// ---------------------------------------------------------------------------
// Issue lifecycle: comment, transition, assign, create [M3]
//
// These commands stage their change in the working copy rather than talking to
// the server directly, so `status` and `diff` see them and `push` remains the
// single mutation point. `--push` runs that same path immediately.
// ---------------------------------------------------------------------------

#[test]
fn comment_stages_a_new_comment_that_push_then_creates() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);

    let report = ws.json(&["comment", "PROJ-142", "-m", "Rotated the cert."]);
    assert_eq!(report["key"], "PROJ-142");
    assert_eq!(report["pushed"], false);

    let file = ws.read(ISSUE_FILE);
    assert!(file.contains("### [new]"), "{file}");
    assert!(file.contains("Rotated the cert."));

    let status = ws.json(&["status"]);
    assert!(status["dirty"][0]["fields"]
        .as_array()
        .unwrap()
        .contains(&Value::from("comments")));

    let push = ws.json(&["push"]);
    let ops = push["results"][0]["ops"].as_array().unwrap();
    assert!(ops
        .iter()
        .any(|o| o["kind"] == "create_comment" && o["status"] == "done"));

    let after = ws.read(ISSUE_FILE);
    assert!(
        !after.contains("### [new]"),
        "the placeholder must be replaced"
    );
    assert!(after.contains("Rotated the cert."));
}

#[test]
fn comment_with_push_sends_immediately() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);

    let report = ws.json(&["comment", "PROJ-142", "-m", "Sent now.", "--push"]);
    assert_eq!(report["pushed"], true);
    assert_eq!(report["push"]["results"][0]["ok"], true);

    assert!(ws.json(&["status"])["dirty"].as_array().unwrap().is_empty());
}

#[test]
fn commenting_on_an_unknown_issue_is_a_usage_error() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);
    let err = ws.json_err(&["comment", "PROJ-999", "-m", "x"], 2);
    assert_eq!(err["kind"], "usage");
    assert_eq!(err["details"]["issue"], "PROJ-999");
}

#[test]
fn transition_lists_what_the_workflow_allows() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);

    let report = ws.json(&["transition", "PROJ-142", "--list"]);
    let names: Vec<&str> = report["transitions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["to_status"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"In Review"), "{names:?}");
    assert!(names.contains(&"Done"));
}

#[test]
fn transition_stages_a_status_change_and_push_applies_it() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);

    let report = ws.json(&["transition", "PROJ-142", "--to", "In Review"]);
    assert_eq!(report["to"], "In Review");
    assert_eq!(report["transition_id"], "31");
    assert!(ws.read(ISSUE_FILE).contains("status: In Review"));

    let status = ws.json(&["status"]);
    let staged = &status["staged_transitions"][0];
    assert_eq!(staged["key"], "PROJ-142");
    assert_eq!(staged["to"], "In Review");
    assert_eq!(staged["from"], "In Progress");

    let push = ws.json(&["push"]);
    let ops = push["results"][0]["ops"].as_array().unwrap();
    assert!(ops
        .iter()
        .any(|o| o["kind"] == "transition" && o["status"] == "done"));
    assert!(ws.json(&["status"])["dirty"].as_array().unwrap().is_empty());
}

#[test]
fn an_impossible_transition_is_refused_with_the_available_ones() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);

    let err = ws.json_err(&["transition", "PROJ-142", "--to", "Teleported"], 2);
    assert_eq!(err["kind"], "usage");
    let available = err["details"]["available"].as_array().unwrap();
    assert!(available.contains(&Value::from("Done")), "{available:?}");
    // Nothing was staged.
    assert!(ws.read(ISSUE_FILE).contains("status: In Progress"));
}

#[test]
fn a_transition_needing_fields_says_which_ones() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);

    let err = ws.json_err(&["transition", "PROJ-142", "--to", "Closed"], 2);
    assert_eq!(
        err["details"]["required"].as_array().unwrap(),
        &vec![Value::from("resolution")]
    );
    assert!(err["hint"]
        .as_str()
        .unwrap()
        .contains("--field resolution="));
}

#[test]
fn assign_me_uses_the_authenticated_account() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);

    let report = ws.json(&["assign", "PROJ-142", "--me"]);
    assert_eq!(report["assignee"], "@[Ilya A](5b10a2ilya)");
    assert!(ws.read(ISSUE_FILE).contains("5b10a2ilya"));
}

#[test]
fn assign_none_clears_the_field_and_pushes_a_null() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);

    ws.json(&["assign", "PROJ-142", "--none"]);
    assert!(!ws.read(ISSUE_FILE).contains("assignee:"));

    let push = ws.json(&["push", "--dry-run"]);
    let body = &push["results"][0]["ops"][0]["request"]["body"]["fields"];
    assert!(body.get("assignee").is_some());
    assert_eq!(body["assignee"], Value::Null);
}

#[test]
fn create_makes_a_local_issue_that_push_turns_into_a_real_one() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);

    let report = ws.json(&[
        "create",
        "--summary",
        "Audit the SSO config",
        "--type",
        "Task",
        "-d",
        "Check every tenant.",
    ]);
    let temp_key = report["key"].as_str().unwrap().to_string();
    assert!(temp_key.starts_with("NEW-"), "{temp_key}");
    assert!(ws.path().join(report["file"].as_str().unwrap()).is_file());

    let status = ws.json(&["status"]);
    assert_eq!(status["new_issues"][0]["key"], temp_key);

    let dry = ws.json(&["push", "--dry-run"]);
    let op = &dry["results"][0]["ops"][0];
    assert_eq!(op["kind"], "create_issue");
    assert_eq!(op["request"]["body"]["fields"]["project"]["key"], "PROJ");
    assert_eq!(
        op["request"]["body"]["fields"]["summary"],
        "Audit the SSO config"
    );

    let push = ws.json(&["push"]);
    let result = &push["results"][0];
    assert_eq!(result["key"], temp_key);
    assert_eq!(result["ok"], true);
    let new_key = result["new_key"].as_str().expect("a real key was assigned");
    assert!(new_key.starts_with("PROJ-"), "{new_key}");

    // The temporary key is gone from the workspace and from state.
    assert!(!ws.path().join(report["file"].as_str().unwrap()).exists());
    let status = ws.json(&["status"]);
    assert!(status["new_issues"].as_array().unwrap().is_empty());
    assert!(status["dirty"].as_array().unwrap().is_empty());

    let created = ws.read(&format!("{new_key}. Audit the SSO config.md"));
    assert!(created.contains(&format!("key: {new_key}")));
    assert!(created.contains("Check every tenant."));
}

#[test]
fn create_requires_a_summary() {
    let ws = Ws::new();
    ws.init();
    let err = ws.json_err(&["create"], 7);
    assert_eq!(err["kind"], "missing_value");
}
