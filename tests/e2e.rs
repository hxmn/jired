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
            Deployment::Cloud => "PROJ-0142. Fix login flow.md",
            Deployment::DataCenter => "DC-0077. Rotate the LDAP bind credential.md",
        }
    }

    fn key(self) -> &'static str {
        match self {
            Deployment::Cloud => "PROJ-142",
            Deployment::DataCenter => "DC-77",
        }
    }

    /// Attachment contents the fixture server should serve, and their sizes.
    ///
    /// The sizes match the metadata in `issues.json`, because the download path
    /// verifies that what arrived is what was advertised.
    fn attachment_bodies(self) -> &'static [(&'static str, usize)] {
        match self {
            Deployment::Cloud => &[("login-trace.har", 2_200_000)],
            Deployment::DataCenter => &[("ldap-audit.log", 48_210)],
        }
    }

    /// The one attachment each fixture issue carries.
    fn attachment(self) -> &'static str {
        match self {
            Deployment::Cloud => "login-trace.har",
            Deployment::DataCenter => "ldap-audit.log",
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

        // Attachment bodies are generated rather than committed: they are large
        // and carry no information beyond their size, which is exactly what the
        // download path verifies.
        let bodies = fixtures.join("attachments");
        std::fs::create_dir_all(&bodies).unwrap();
        for (name, size) in deployment.attachment_bodies() {
            std::fs::write(bodies.join(name), vec![b'x'; *size]).unwrap();
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

    /// Run a command expecting a specific exit code, returning the JSON result
    /// document from stdout.
    ///
    /// For the outcomes a command reports rather than errors on: a push where
    /// one issue failed still produces a full report, and the exit code names
    /// the kind of failure.
    fn json_code(&self, args: &[&str], code: i32) -> Value {
        let out = self.cmd(args).arg("--json").output().expect("run");
        assert_eq!(
            out.status.code(),
            Some(code),
            "`jired {}` exit code\nstdout: {}\nstderr: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stdout),
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
    ///
    /// The `updated` stamp is set to now rather than to a date written into the
    /// test: a fetch is incremental against the last one, so a fixed timestamp
    /// stops being "after the last fetch" the moment the calendar passes it, and
    /// the test quietly starts asserting nothing.
    fn mutate_remote(&self, key: &str, f: impl Fn(&mut Value)) {
        let path = self.fixtures.join("issues.json");
        let mut issues: Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        for issue in issues.as_array_mut().unwrap() {
            if issue["key"] == key {
                f(issue);
                issue["fields"]["updated"] = jired::store::now_rfc3339().into();
            }
        }
        std::fs::write(&path, serde_json::to_string_pretty(&issues).unwrap()).unwrap();
    }
}

const ISSUE_FILE: &str = "PROJ-0142. Fix login flow.md";

// ---------------------------------------------------------------------------
// version
// ---------------------------------------------------------------------------

/// A bug report needs to name the exact binary, so `--version` carries the
/// commit and the build time, not just the release number.
#[test]
fn version_names_the_commit_and_when_it_was_built() {
    let short = Command::cargo_bin("jired")
        .unwrap()
        .arg("-V")
        .output()
        .unwrap();
    let line = String::from_utf8(short.stdout).unwrap();
    let line = line.trim();

    // jired <semver> (<commit> <timestamp>)
    let (name_version, rest) = line.split_once(" (").unwrap_or_else(|| panic!("{line}"));
    assert!(name_version.starts_with("jired "), "{line}");
    let (commit, timestamp) = rest
        .trim_end_matches(')')
        .split_once(' ')
        .unwrap_or_else(|| panic!("{line}"));
    assert!(!commit.is_empty(), "{line}");
    assert_timestamp(timestamp);

    let long = Command::cargo_bin("jired")
        .unwrap()
        .arg("--version")
        .output()
        .unwrap();
    let text = String::from_utf8(long.stdout).unwrap();
    for expected in ["commit:", "built:", "rustc:", "target:"] {
        assert!(text.contains(expected), "{expected} missing from\n{text}");
    }
    assert!(
        text.contains(commit),
        "the two forms disagree:\n{line}\n{text}"
    );
}

/// `2026-08-21T10:49:39Z`, checked by shape rather than by parsing: the point is
/// that it is a machine-readable instant, not that today is a particular day.
fn assert_timestamp(value: &str) {
    let bytes = value.as_bytes();
    assert_eq!(value.len(), 20, "not an RFC 3339 second: {value}");
    for (i, b) in bytes.iter().enumerate() {
        let ok = match i {
            4 | 7 => *b == b'-',
            10 => *b == b'T',
            13 | 16 => *b == b':',
            19 => *b == b'Z',
            _ => b.is_ascii_digit(),
        };
        assert!(ok, "unexpected byte at {i} in {value}");
    }
    let year: u32 = value[..4].parse().unwrap();
    assert!((2020..2100).contains(&year), "{value}");
}

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

    // The config file is the whole picture, and explains itself.
    let config = ws.read(".jired/config.toml");
    for key in [
        "url =",
        "deployment =",
        "kind =",
        "project =",
        "mode =",
        "insecure =",
        "auto_download_max =",
    ] {
        assert!(config.contains(key), "{key} missing from\n{config}");
    }
    assert!(
        config.contains("# Base URL of the Jira instance"),
        "settings are written with their documentation:\n{config}"
    );
    assert_eq!(report["account"]["tls"]["insecure"], false);
    assert!(report["account"]["tls"]["ca_path"].is_null());
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
        "PROJ-0143. Add integration test.md",
        "PROJ-0150. Rotate IdP certificate.md",
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
    assert!(text.contains("\n## Worklogs\n"));
    assert!(text.contains("\n## Attachments\n"));

    assert!(text.contains("- [ ] PROJ-143 Add integration test"));
    assert!(text.contains("- [x] PROJ-144 Update runbook"));
    assert!(text.contains("### [c-10812] Jane D — 2026-08-15T10:32:00Z"));
    assert!(text.contains("Repro confirmed on staging."));
    assert!(text.contains("### [w-10500] 2h 30m — Jane D — 2026-08-15T09:00:00Z"));
    assert!(text.contains("Chased the redirect loop."));
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

/// A pull has to say what it brought in, not just that something moved.
#[test]
fn pull_reports_what_changed_field_by_field() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);

    ws.mutate_remote("PROJ-142", |issue| {
        issue["fields"]["status"] = serde_json::json!({"id": "4", "name": "In Review"});
        issue["fields"]["labels"] = serde_json::json!(["auth", "backend", "sso"]);
        issue["fields"]["customfield_10456"] = serde_json::json!({"value": "Engineering"});
        issue["fields"]["description"]["content"][0]["content"][0]["text"] = "Rewritten.".into();
    });

    let report = ws.json(&["pull"]);
    let updated = &report["updated"][0];
    assert_eq!(updated["key"], "PROJ-142");

    let changes: std::collections::HashMap<&str, &Value> = updated["changes"]
        .as_array()
        .expect("per-field changes")
        .iter()
        .map(|c| (c["field"].as_str().unwrap(), c))
        .collect();

    let status = changes["status"];
    assert_eq!(status["from"], "In Progress");
    assert_eq!(status["to"], "In Review");

    let labels = changes["labels"];
    assert_eq!(labels["from"], "auth, backend");
    assert_eq!(labels["to"], "auth, backend, sso");

    // Custom fields are named individually, with the instance's own label.
    let team = changes["custom.customfield_10456"];
    assert_eq!(team["label"], "Team");
    assert_eq!(team["from"], "Platform");
    assert_eq!(team["to"], "Engineering");

    // Body text is counted, not quoted.
    let body = changes["description"];
    assert!(body["added"].as_u64().unwrap() > 0, "{body}");
    assert!(
        body["from"].is_null(),
        "a body diff carries no values: {body}"
    );

    // The coarse field list is still there for anything that reads it.
    let names: Vec<&str> = updated["fields"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f.as_str().unwrap())
        .collect();
    assert!(names.contains(&"status"), "{names:?}");
    assert!(names.contains(&"custom"), "{names:?}");

    // And the human output shows the same thing.
    let out = ws.cmd(&["pull"]).output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("unchanged"), "{text}");
}

/// A new comment is a change worth reporting, without printing the comment.
#[test]
fn pull_counts_new_comments_rather_than_quoting_them() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);

    ws.mutate_remote("PROJ-142", |issue| {
        let comments = issue["fields"]["comment"]["comments"]
            .as_array_mut()
            .unwrap();
        comments.push(serde_json::json!({
            "id": "10999",
            "author": {"accountId": "5b1af9", "displayName": "Jane D"},
            "created": "2026-08-20T10:00:00.000+0000",
            "updated": "2026-08-20T10:00:00.000+0000",
            "body": {"type": "doc", "version": 1, "content": [
                {"type": "paragraph", "content": [{"type": "text", "text": "One more thing."}]}
            ]}
        }));
    });

    let report = ws.json(&["pull"]);
    let change = report["updated"][0]["changes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["field"] == "comments")
        .expect("a comments change")
        .clone();
    assert_eq!(change["added"], 1);
    assert_eq!(change["removed"], 0);
}

#[test]
fn a_remote_summary_change_renames_the_file_and_keeps_the_key() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);

    ws.mutate_remote("PROJ-142", |issue| {
        issue["fields"]["summary"] = "Fix the SSO login flow".into();
    });

    let report = ws.json(&["pull"]);
    let renamed = report["renamed"].as_array().unwrap();
    assert_eq!(renamed.len(), 1);
    assert_eq!(renamed[0]["key"], "PROJ-142");
    assert_eq!(renamed[0]["from"], ISSUE_FILE);
    assert_eq!(renamed[0]["to"], "PROJ-0142. Fix the SSO login flow.md");

    assert!(!ws.path().join(ISSUE_FILE).exists());
    assert!(ws
        .path()
        .join("PROJ-0142. Fix the SSO login flow.md")
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
        "## Worklogs",
        "### [new]\n\nFixed by rotating the IdP cert.\n\n## Worklogs",
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

    // The block is reported per issue, not as a whole-run error: another issue
    // in the same push is unaffected and must still go out. The exit code stays
    // 6 because that is the only kind of failure in the run.
    let report = ws.json_code(&["push"], 6);
    let failed = &report["results"][0];
    assert_eq!(failed["key"], "PROJ-142");
    assert_eq!(failed["ok"], false);
    assert_eq!(failed["error_kind"], "lossy");
    assert!(failed["hint"].as_str().unwrap().contains("--allow-lossy"));
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

/// In `--json` mode *both* streams must be machine-readable: stdout carries the
/// result document and stderr carries the structured error, nothing else.
/// Narration mixed into stderr would sit alongside the error and make it
/// unparseable, which defeats the point of the flag.
#[test]
fn json_mode_keeps_both_streams_machine_readable() {
    let ws = Ws::new();
    ws.init();

    let out = ws.cmd(&["pull", "--json"]).output().unwrap();
    serde_json::from_slice::<Value>(&out.stdout).expect("stdout is pure JSON");
    assert!(
        out.stderr.is_empty(),
        "a successful --json run must say nothing on stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // On failure, stderr is exactly one error document.
    let out = ws
        .cmd(&["transition", "PROJ-142", "--to", "Nowhere", "--json"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let err: Value = serde_json::from_slice(&out.stderr).expect("stderr is pure JSON");
    assert_eq!(err["schema"], "jired/error/1");
}

/// Without `--json` the narration is the whole point, and it goes to stdout.
#[test]
fn human_mode_narrates_on_stdout() {
    let ws = Ws::new();
    ws.init();
    let out = ws.cmd(&["pull"]).output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("new"), "expected a summary: {stdout}");
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
    assert!(ws
        .read("PROJ-0142. Retried.md")
        .contains("summary: Retried"));
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

        let report = ws.json_code(&["push"], 6);
        let failed = &report["results"][0];
        assert_eq!(failed["error_kind"], "lossy", "{:?}", ws.deployment);
        assert_eq!(failed["key"], ws.deployment.key());
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
fn logwork_stages_time_that_push_then_books() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);

    let report = ws.json(&[
        "logwork",
        "PROJ-142",
        "2h 30m",
        "-m",
        "Traced the redirect loop.",
        "--started",
        "2026-08-20T09:00:00Z",
    ]);
    assert_eq!(report["key"], "PROJ-142");
    assert_eq!(report["time_spent"], "2h 30m");
    assert_eq!(report["started"], "2026-08-20T09:00:00Z");
    assert_eq!(report["pushed"], false);

    let file = ws.read(ISSUE_FILE);
    assert!(
        file.contains("### [new] 2h 30m — 2026-08-20T09:00:00Z"),
        "{file}"
    );
    assert!(file.contains("Traced the redirect loop."));

    let status = ws.json(&["status"]);
    assert!(status["dirty"][0]["fields"]
        .as_array()
        .unwrap()
        .contains(&Value::from("worklogs")));

    let push = ws.json(&["push"]);
    let ops = push["results"][0]["ops"].as_array().unwrap();
    let booked = ops
        .iter()
        .find(|o| o["kind"] == "create_worklog")
        .unwrap_or_else(|| panic!("no create_worklog op in {ops:?}"));
    assert_eq!(booked["status"], "done");

    // The staged entry comes back with the id the server gave it, and the
    // duration the file asked for.
    let after = ws.read(ISSUE_FILE);
    assert!(
        !after.contains("### [new]"),
        "the placeholder must be replaced:\n{after}"
    );
    assert!(after.contains("2h 30m — Fixture User"), "{after}");
    assert!(ws.json(&["status"])["dirty"].as_array().unwrap().is_empty());
}

#[test]
fn logwork_with_push_sends_immediately() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);

    let report = ws.json(&["logwork", "PROJ-142", "45m", "--push"]);
    assert_eq!(report["pushed"], true);
    assert_eq!(report["push"]["results"][0]["ok"], true);

    assert!(ws.json(&["status"])["dirty"].as_array().unwrap().is_empty());
}

/// A duration Jira would reject costs nothing to catch here, and catching it
/// later would take the field edits planned alongside it down too.
#[test]
fn a_duration_that_is_not_one_is_refused_before_anything_is_written() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);

    let before = ws.read(ISSUE_FILE);
    let err = ws.json_err(&["logwork", "PROJ-142", "a while"], 2);
    assert_eq!(err["kind"], "usage");
    assert!(err["hint"].as_str().unwrap().contains("2h 30m"), "{err}");
    assert_eq!(ws.read(ISSUE_FILE), before, "nothing may have been written");

    // A bare number means minutes only because of an instance setting.
    ws.json_err(&["logwork", "PROJ-142", "90"], 2);
}

/// Time booked on the server is the file's to edit, and push carries the whole
/// entry — duration, start time and note — not just the note.
#[test]
fn editing_a_booked_worklog_sends_the_whole_entry() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);

    let text = ws
        .read(ISSUE_FILE)
        .replace("### [w-10500] 2h 30m", "### [w-10500] 3h");
    ws.write(ISSUE_FILE, &text);

    let push = ws.json(&["push"]);
    let ops = push["results"][0]["ops"].as_array().unwrap();
    assert!(
        ops.iter()
            .any(|o| o["kind"] == "update_worklog" && o["status"] == "done"),
        "{ops:?}"
    );
    assert!(ws.read(ISSUE_FILE).contains("### [w-10500] 3h"));
}

/// Deleting time somebody booked cannot be undone from the working copy, so it
/// takes the same explicit opt-in a comment does.
#[test]
fn removing_a_worklog_is_reported_rather_than_deleted_by_default() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);

    let text = ws.read(ISSUE_FILE);
    let start = text.find("### [w-10500]").unwrap();
    let end = text.find("### [w-10501]").unwrap();
    ws.write(ISSUE_FILE, &format!("{}{}", &text[..start], &text[end..]));

    let push = ws.json(&["push"]);
    let warnings = push["warnings"].as_array().unwrap();
    assert!(
        warnings
            .iter()
            .any(|w| w.as_str().unwrap().contains("--allow-worklog-delete")),
        "{warnings:?}"
    );
    assert!(
        push["results"].as_array().unwrap().is_empty(),
        "nothing may have been sent: {push}"
    );

    // With the flag, it goes.
    let push = ws.json(&["push", "--allow-worklog-delete"]);
    let ops = push["results"][0]["ops"].as_array().unwrap();
    assert!(
        ops.iter()
            .any(|o| o["kind"] == "delete_worklog" && o["status"] == "done"),
        "{ops:?}"
    );
    assert!(!ws.read(ISSUE_FILE).contains("### [w-10500]"));
}

/// The two values can be asked for, so leaving them off is not a parse error:
/// with no terminal to ask on it is exit 7 naming them, the same rung `create`
/// lands on.
#[test]
fn logwork_with_nothing_to_go_on_asks_rather_than_failing_to_parse() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);

    for args in [vec!["logwork"], vec!["logwork", "PROJ-142"]] {
        let err = ws.json_err(&args, 7);
        assert_eq!(err["kind"], "missing_value");
        assert!(
            err["hint"].as_str().unwrap().contains("in a terminal"),
            "{err}"
        );
    }

    // And the form cannot be demanded where there is nothing to draw it on.
    let err = ws.json_err(&["logwork", "-i", "PROJ-142", "1h"], 7);
    assert!(err["message"].as_str().unwrap().contains("--interactive"));
}

#[test]
fn logging_work_on_an_unknown_issue_is_a_usage_error() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);
    let err = ws.json_err(&["logwork", "PROJ-999", "1h"], 2);
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

    // The file carries the padded spelling of the key; the frontmatter carries
    // the key itself.
    let number = new_key.rsplit('-').next().unwrap();
    let created = ws.read(&format!("PROJ-{number:0>4}. Audit the SSO config.md"));
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

#[test]
fn the_create_form_is_refused_rather_than_skipped_when_it_cannot_be_shown() {
    // `--interactive` asks for the form; `--no-input` forbids asking. Silently
    // creating something from the defaults would be the wrong resolution.
    let ws = Ws::new();
    ws.init();
    let err = ws.json_err(&["create", "-i", "--summary", "Anything"], 7);
    assert_eq!(err["kind"], "missing_value");
    assert!(
        err["message"].as_str().unwrap().contains("terminal"),
        "{err}"
    );
}

#[test]
fn a_template_supplies_every_field_a_flag_did_not() {
    let ws = Ws::new();
    ws.init();
    std::fs::write(
        ws.path().join(".jired/templates/incident.md"),
        "---\nkey: NEW-000000\nid: ''\ntype: Bug\npriority: High\nlabels:\n- incident\n- auth\njired: {}\n---\n\n\
         ## Description\n\nWhat happened, and what it affected.\n",
    )
    .unwrap();

    let report = ws.json(&["create", "--template", "incident", "-s", "Login is down"]);
    assert_eq!(report["type"], "Bug");
    let file = ws.read(report["file"].as_str().unwrap());
    assert!(file.contains("priority: High"), "{file}");
    assert!(file.contains("- incident"), "{file}");
    assert!(file.contains("What happened"), "{file}");

    // An explicit flag still wins over what the template says.
    let overridden = ws.json(&[
        "create",
        "--template",
        "incident",
        "-s",
        "Login is slow",
        "--type",
        "Task",
    ]);
    assert_eq!(overridden["type"], "Task");
}

// ---------------------------------------------------------------------------
// Attachments [M3.3]
// ---------------------------------------------------------------------------

#[test]
fn a_large_attachment_becomes_a_stub_until_asked_for() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);

    let folder = ws.path().join(".PROJ-0142. Fix login flow");
    let name = ws.deployment.attachment();
    let stub = folder.join(format!("{name}.jiredstub"));

    assert!(stub.is_file(), "a 2.1 MiB file must not download eagerly");
    assert!(!folder.join(name).exists());

    // The stub says what the file is, so the folder still tells the truth.
    let meta: Value = serde_json::from_str(&std::fs::read_to_string(&stub).unwrap()).unwrap();
    assert_eq!(meta["filename"], name);
    assert_eq!(meta["size"], 2_200_000u64);
    assert!(meta["note"].as_str().unwrap().contains("attachments pull"));

    let listed = ws.json(&["attachments", "list"]);
    let entry = &listed["attachments"][0];
    assert_eq!(entry["filename"], name);
    assert_eq!(entry["downloaded"], false);
}

#[test]
fn attachments_pull_replaces_the_stub_with_real_content() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);

    let folder = ws.path().join(".PROJ-0142. Fix login flow");
    let name = ws.deployment.attachment();

    let report = ws.json(&["attachments", "pull"]);
    assert_eq!(report["downloaded"][0]["filename"], name);

    assert!(folder.join(name).is_file());
    assert_eq!(
        std::fs::metadata(folder.join(name)).unwrap().len(),
        2_200_000
    );
    assert!(
        !folder.join(format!("{name}.jiredstub")).exists(),
        "the stub must go once the content is there"
    );
    assert_eq!(
        ws.json(&["attachments", "list"])["attachments"][0]["downloaded"],
        true
    );
}

#[test]
fn attachments_pull_honours_max_size() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);

    let report = ws.json(&["attachments", "pull", "--max-size", "1024"]);
    assert!(report["downloaded"].as_array().unwrap().is_empty());
    assert_eq!(report["skipped"][0]["filename"], ws.deployment.attachment());
}

#[test]
fn a_small_attachment_is_downloaded_eagerly_on_pull() {
    // Data Center's fixture attachment is small enough for the default
    // threshold, so it arrives without being asked for.
    let ws = Ws::of(Deployment::DataCenter);
    ws.init();
    ws.json(&["pull"]);

    let folder = ws.path().join(".DC-0077. Rotate the LDAP bind credential");
    assert!(
        folder.join("ldap-audit.log").is_file(),
        "a 47 KiB file is under the default auto-download threshold"
    );
    assert!(!folder.join("ldap-audit.log.jiredstub").exists());
}

#[test]
fn attach_stages_a_file_that_push_uploads() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);

    let source = ws.path().join("evidence.txt");
    std::fs::write(&source, b"captured during the incident").unwrap();

    let report = ws.json(&["attach", "PROJ-142", source.to_str().unwrap()]);
    assert_eq!(report["staged"][0], "evidence.txt");
    assert!(ws
        .path()
        .join(".PROJ-0142. Fix login flow/evidence.txt")
        .is_file());

    let dry = ws.json(&["push", "--dry-run"]);
    let ops = dry["results"][0]["ops"].as_array().unwrap();
    let upload = ops
        .iter()
        .find(|o| o["kind"] == "upload_attachment")
        .expect("an upload was planned");
    assert_eq!(upload["request"]["body"]["filename"], "evidence.txt");

    let push = ws.json(&["push"]);
    let ops = push["results"][0]["ops"].as_array().unwrap();
    assert!(ops
        .iter()
        .any(|o| o["kind"] == "upload_attachment" && o["status"] == "done"));
}

#[test]
fn an_oversized_attachment_is_refused_before_anything_is_copied() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);

    let source = ws.path().join("huge.bin");
    std::fs::write(&source, vec![0u8; 4096]).unwrap();

    let out = ws
        .cmd(&["attach", "PROJ-142", source.to_str().unwrap(), "--json"])
        .env("JIRED_ATTACH_MAX_UPLOAD", "1024")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));

    let err: Value = serde_json::from_slice(&out.stderr).unwrap();
    assert_eq!(err["details"]["size"], 4096);
    assert_eq!(err["details"]["limit"], 1024);
    assert!(
        !ws.path()
            .join(".PROJ-0142. Fix login flow/huge.bin")
            .exists(),
        "nothing should have been copied"
    );
}

#[test]
fn a_staged_attachment_makes_the_issue_dirty() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);
    assert!(ws.json(&["status"])["dirty"].as_array().unwrap().is_empty());

    let source = ws.path().join("note.txt");
    std::fs::write(&source, b"evidence").unwrap();
    ws.json(&["attach", "PROJ-142", source.to_str().unwrap()]);

    // The `.md` file is untouched, so only the folder scan can reveal this.
    let status = ws.json(&["status"]);
    let dirty = &status["dirty"][0];
    assert_eq!(dirty["key"], "PROJ-142");
    assert!(dirty["fields"]
        .as_array()
        .unwrap()
        .contains(&Value::from("attachments")));

    ws.json(&["push"]);
    assert!(ws.json(&["status"])["dirty"].as_array().unwrap().is_empty());
}

// ---------------------------------------------------------------------------
// Conflict resolution [M3.8]
// ---------------------------------------------------------------------------

/// Put the workspace into the state a conflicting pull leaves behind.
fn conflicted(ws: &Ws) {
    let text = ws
        .read(ISSUE_FILE)
        .replace("summary: Fix login flow", "summary: Ours");
    ws.write(ISSUE_FILE, &text);

    ws.mutate_remote("PROJ-142", |issue| {
        issue["fields"]["summary"] = "Theirs".into();
    });

    let out = ws.cmd(&["pull", "--json"]).output().unwrap();
    assert_eq!(out.status.code(), Some(5), "the setup must conflict");
}

#[test]
fn resolve_lists_what_is_stuck() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);
    conflicted(&ws);

    let report = ws.json(&["resolve", "--list"]);
    let c = &report["conflicts"][0];
    assert_eq!(c["issue_key"], "PROJ-142");
    assert_eq!(c["field"], "summary");
}

#[test]
fn resolve_without_a_choice_refuses_rather_than_guessing() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);
    conflicted(&ws);

    let err = ws.json_err(&["resolve"], 7);
    assert_eq!(err["kind"], "missing_value");
    assert!(err["hint"].as_str().unwrap().contains("--ours"));
    // Still stuck, deliberately.
    assert_eq!(
        ws.json(&["status"])["conflicts"].as_array().unwrap().len(),
        1
    );
}

#[test]
fn resolve_theirs_takes_the_server_value_and_clears_the_conflict() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);
    conflicted(&ws);

    let report = ws.json(&["resolve", "--theirs"]);
    assert_eq!(report["strategy"], "theirs");
    assert_eq!(report["resolved"][0], "PROJ-142");

    assert!(ws.read("PROJ-0142. Theirs.md").contains("summary: Theirs"));
    let status = ws.json(&["status"]);
    assert!(status["conflicts"].as_array().unwrap().is_empty());
    assert!(status["dirty"].as_array().unwrap().is_empty());
}

#[test]
fn resolve_ours_keeps_the_local_value_and_leaves_it_to_push() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);
    conflicted(&ws);

    ws.json(&["resolve", "--ours"]);

    assert!(ws.read("PROJ-0142. Ours.md").contains("summary: Ours"));
    let status = ws.json(&["status"]);
    assert!(status["conflicts"].as_array().unwrap().is_empty());
    // Keeping the local value means there is something to send.
    assert_eq!(status["dirty"][0]["key"], "PROJ-142");
    assert!(status["dirty"][0]["fields"]
        .as_array()
        .unwrap()
        .contains(&Value::from("summary")));
}

#[test]
fn resolve_done_refuses_a_file_that_still_has_markers() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);

    // Conflict in the body so `--strategy merge` writes markers.
    let text = ws
        .read(ISSUE_FILE)
        .replace("Steps to reproduce are below.", "Our steps.");
    ws.write(ISSUE_FILE, &text);
    ws.mutate_remote("PROJ-142", |issue| {
        let content = issue["fields"]["description"]["content"]
            .as_array_mut()
            .unwrap();
        content[2]["content"][0]["text"] = "Their steps.".into();
    });
    let out = ws
        .cmd(&["pull", "--strategy", "merge", "--json"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(5));
    assert!(ws.read(ISSUE_FILE).contains("<<<<<<< LOCAL"));

    // Pushing markers into the issue would be worse than the conflict.
    let err = ws.json_err(&["resolve", "--done"], 5);
    assert_eq!(err["kind"], "conflict");
    assert!(err["hint"].as_str().unwrap().contains("<<<<<<<"));

    // Edit them out by hand, then accept.
    let cleaned: String = ws
        .read(ISSUE_FILE)
        .lines()
        .filter(|l| !l.starts_with("<<<<<<<") && !l.starts_with(">>>>>>>") && *l != "=======")
        .filter(|l| !l.contains("Their steps."))
        .collect::<Vec<_>>()
        .join("\n");
    ws.write(ISSUE_FILE, &format!("{cleaned}\n"));

    let report = ws.json(&["resolve", "--done"]);
    assert_eq!(report["strategy"], "manual");
    assert!(ws.json(&["status"])["conflicts"]
        .as_array()
        .unwrap()
        .is_empty());
}

// ---------------------------------------------------------------------------
// History and housekeeping [M3.7, M3.9]
// ---------------------------------------------------------------------------

#[test]
fn log_shows_the_changelog_newest_first_and_caches_it() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);

    let report = ws.json(&["log", "PROJ-142"]);
    let entries = report["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0]["id"], "70002", "newest first");
    assert_eq!(entries[0]["items"][0]["field"], "priority");
    assert_eq!(entries[1]["items"][0]["from"], "To Do");

    // Cached, so a second run needs no server at all.
    let out = ws
        .cmd(&["log", "PROJ-142", "--json"])
        .env_remove("JIRED_FIXTURES")
        .env("JIRED_FIXTURES", "")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "a cached changelog must not need the network: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let cached: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(cached["entries"].as_array().unwrap().len(), 2);
}

#[test]
fn log_honours_the_limit() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);
    let report = ws.json(&["log", "PROJ-142", "--limit", "1"]);
    assert_eq!(report["entries"].as_array().unwrap().len(), 1);
}

#[test]
fn log_on_an_unknown_issue_is_a_usage_error() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);
    let err = ws.json_err(&["log", "PROJ-999"], 2);
    assert_eq!(err["kind"], "usage");
}

#[test]
fn clean_removes_a_file_whose_issue_left_the_server() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);

    // Drop the issue from the fixture "server" entirely.
    let path = ws.fixtures.join("issues.json");
    let mut issues: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    issues
        .as_array_mut()
        .unwrap()
        .retain(|i| i["key"] != "PROJ-150");
    std::fs::write(&path, serde_json::to_string_pretty(&issues).unwrap()).unwrap();

    ws.json(&["fetch", "--full"]);
    let orphan = "PROJ-0150. Rotate IdP certificate.md";
    assert!(ws.path().join(orphan).is_file());

    let dry = ws.json(&["clean", "--dry-run"]);
    assert_eq!(dry["kept"][0]["key"], "PROJ-150");
    assert_eq!(dry["kept"][0]["reason"], "gone from the server");
    assert!(
        ws.path().join(orphan).is_file(),
        "a dry run removes nothing"
    );

    let report = ws.json(&["clean", "--yes"]);
    assert_eq!(report["removed"][0]["key"], "PROJ-150");
    assert!(!ws.path().join(orphan).exists());
}

#[test]
fn clean_keeps_an_orphan_that_still_holds_unpushed_edits() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);

    let orphan = "PROJ-0150. Rotate IdP certificate.md";
    let text = ws.read(orphan).replace("- ops\n", "- ops\n- unpushed\n");
    ws.write(orphan, &text);

    let path = ws.fixtures.join("issues.json");
    let mut issues: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    issues
        .as_array_mut()
        .unwrap()
        .retain(|i| i["key"] != "PROJ-150");
    std::fs::write(&path, serde_json::to_string_pretty(&issues).unwrap()).unwrap();
    ws.json(&["fetch", "--full"]);

    // Deleting it would lose work the server has never seen.
    let out = ws.cmd(&["clean", "--yes", "--json"]).output().unwrap();
    assert_eq!(out.status.code(), Some(1));
    let report: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(report["removed"].as_array().unwrap().is_empty());
    assert_eq!(report["kept"][0]["dirty"], true);
    assert!(ws.path().join(orphan).is_file());

    // --force says "yes, I mean it".
    let report = ws.json(&["clean", "--yes", "--force"]);
    assert_eq!(report["removed"][0]["key"], "PROJ-150");
    assert!(!ws.path().join(orphan).exists());
}

#[test]
fn clean_leaves_a_healthy_workspace_alone() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);
    let report = ws.json(&["clean", "--yes"]);
    assert!(report["removed"].as_array().unwrap().is_empty());
    assert!(ws.path().join(ISSUE_FILE).is_file());
}

// ---------------------------------------------------------------------------
// reset
// ---------------------------------------------------------------------------

/// The whole point: an edited file goes back to exactly what the last pull
/// wrote, and the workspace has nothing left to send.
#[test]
fn reset_puts_an_edited_file_back_and_leaves_the_workspace_clean() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);
    let original = ws.read(ISSUE_FILE);

    let edited = original
        .replace(
            "summary: Fix login flow",
            "summary: Fix login flow properly",
        )
        .replace(
            "Steps to reproduce are below.",
            "Steps to reproduce are below. And a line nobody asked for.",
        );
    // The summary drives the file name, so `jired edit` renames on save; doing
    // it here too is what makes reset put the name back as well as the content.
    let renamed = "PROJ-0142. Fix login flow properly.md";
    ws.write(renamed, &edited);
    std::fs::remove_file(ws.path().join(ISSUE_FILE)).unwrap();

    let report = ws.json(&["reset", "--yes"]);
    assert_eq!(report["schema"], "jired/reset/1");
    let restored = &report["restored"][0];
    assert_eq!(restored["key"], "PROJ-142");
    let dropped = restored["dropped"].as_array().unwrap();
    assert!(dropped.contains(&Value::from("summary")), "{dropped:?}");
    assert!(dropped.contains(&Value::from("description")), "{dropped:?}");

    assert_eq!(
        ws.read(ISSUE_FILE),
        original,
        "the file must come back byte for byte"
    );
    assert!(
        !ws.path().join(renamed).exists(),
        "an edited summary renamed the file; reset renames it back"
    );
    assert!(ws.json(&["status"])["dirty"].as_array().unwrap().is_empty());
    assert!(ws.json(&["diff"])["issues"].as_array().unwrap().is_empty());
}

/// Nothing is dropped without saying what, and `--dry-run` says it without
/// dropping anything.
#[test]
fn reset_dry_run_names_what_would_go_and_writes_nothing() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);

    let edited = ws
        .read(ISSUE_FILE)
        .replace("summary: Fix login flow", "summary: Ours");
    ws.write(ISSUE_FILE, &edited);

    let report = ws.json(&["reset", "--dry-run"]);
    assert_eq!(report["dry_run"], true);
    assert_eq!(report["restored"][0]["key"], "PROJ-142");
    assert!(ws.read(ISSUE_FILE).contains("summary: Ours"));
    assert!(!ws.json(&["status"])["dirty"].as_array().unwrap().is_empty());
}

/// Resetting is not a blanket operation when a key is given, and a key nobody
/// here has is a typo rather than a licence to reset the rest.
#[test]
fn reset_takes_a_scope_and_refuses_a_key_it_does_not_know() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);

    let other = "PROJ-0150. Rotate IdP certificate.md";
    ws.write(
        ISSUE_FILE,
        &ws.read(ISSUE_FILE)
            .replace("summary: Fix login flow", "summary: Ours"),
    );
    ws.write(other, &ws.read(other).replace("- ops\n", "- ops\n- mine\n"));

    let err = ws.json_err(&["reset", "PROJ-999", "--yes"], 2);
    assert_eq!(err["kind"], "usage");
    assert!(ws.read(ISSUE_FILE).contains("summary: Ours"));

    let report = ws.json(&["reset", "PROJ-142", "--yes"]);
    assert_eq!(report["restored"].as_array().unwrap().len(), 1);
    assert_eq!(report["restored"][0]["key"], "PROJ-142");
    assert!(!ws.read(ISSUE_FILE).contains("summary: Ours"));
    assert!(
        ws.read(other).contains("- mine"),
        "an issue outside the scope keeps its edits"
    );
}

/// An issue named that has nothing to drop is said out loud: a command that
/// prints nothing leaves the question of whether it looked.
#[test]
fn reset_says_when_a_named_issue_has_no_local_changes() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);

    let report = ws.json(&["reset", "PROJ-142", "--yes"]);
    assert_eq!(report["clean"][0], "PROJ-142");
    assert!(report["restored"].as_array().unwrap().is_empty());
}

/// A conflicting pull leaves a recorded conflict behind. The conflict is a
/// disagreement between a local edit and the server; drop the edit and there is
/// nothing left to disagree about.
#[test]
fn reset_clears_the_conflict_the_dropped_edit_caused() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);
    conflicted(&ws);
    assert!(!ws.json(&["status"])["conflicts"]
        .as_array()
        .unwrap()
        .is_empty());

    let report = ws.json(&["reset", "--yes"]);
    assert_eq!(report["conflicts_cleared"], 1);
    let status = ws.json(&["status"]);
    assert!(status["conflicts"].as_array().unwrap().is_empty());
    assert!(status["dirty"].as_array().unwrap().is_empty());
    // The server moved on while this was going round, so the issue is behind —
    // which is a pull's business, not a reset's.
    assert_eq!(status["behind"][0], "PROJ-142");
}

/// A locally created issue has no earlier state, so "drop its changes" can only
/// mean deleting it. That is not something to do because someone typed `reset`.
#[test]
fn reset_keeps_a_locally_created_issue_unless_asked_to_delete_it() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);

    let created = ws.json(&["create", "--summary", "Draft nobody pushed"]);
    let file = created["file"].as_str().expect("file").to_string();
    let key = created["key"].as_str().expect("key").to_string();
    assert!(ws.path().join(&file).is_file());

    let report = ws.json(&["reset", "--yes"]);
    assert_eq!(report["kept"][0]["key"], key);
    assert!(report["removed"].as_array().unwrap().is_empty());
    assert!(
        ws.path().join(&file).is_file(),
        "unpushed work is not deleted by a plain reset"
    );

    let report = ws.json(&["reset", "--include-new", "--yes"]);
    assert_eq!(report["removed"][0]["key"], key);
    assert!(!ws.path().join(&file).exists());
    assert!(ws.json(&["status"])["new_issues"]
        .as_array()
        .unwrap()
        .is_empty());
}

/// A staged upload is the only copy of that file anywhere. Reset restores from
/// the last sync, and the last sync cannot bring bytes back that never left.
#[test]
fn reset_leaves_a_staged_attachment_where_it_is() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);

    let source = ws.path().join("note.txt");
    std::fs::write(&source, b"evidence").unwrap();
    ws.json(&["attach", "PROJ-142", source.to_str().unwrap()]);
    let staged = ws
        .path()
        .join(".PROJ-0142. Fix login flow")
        .join("note.txt");
    assert!(staged.is_file());

    let report = ws.json(&["reset", "--yes"]);
    assert_eq!(report["kept"][0]["key"], "PROJ-142");
    assert!(report["kept"][0]["reason"]
        .as_str()
        .unwrap()
        .contains("attachment"));
    assert!(staged.is_file(), "the only copy of that file must survive");
}

/// A file somebody broke is exactly what reset is for, so it must not need the
/// file to parse before it can replace it.
#[test]
fn reset_rescues_a_file_that_no_longer_parses() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);
    let original = ws.read(ISSUE_FILE);

    ws.write(ISSUE_FILE, "this is not an issue file at all\n");
    let report = ws.json(&["reset", "--yes"]);
    assert_eq!(report["restored"][0]["key"], "PROJ-142");
    assert_eq!(ws.read(ISSUE_FILE), original);
    assert!(ws.json(&["status"])["dirty"].as_array().unwrap().is_empty());
}

/// Dropping other people's work by accident is the failure mode here, so a
/// script has to say `--yes` rather than have it assumed.
#[test]
fn reset_refuses_to_run_unconfirmed_without_a_terminal() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);
    ws.write(
        ISSUE_FILE,
        &ws.read(ISSUE_FILE)
            .replace("summary: Fix login flow", "summary: Ours"),
    );

    let err = ws.json_err(&["reset"], 7);
    assert_eq!(err["kind"], "missing_value");
    assert!(ws.read(ISSUE_FILE).contains("summary: Ours"));
}

/// Time booked but not yet pushed lives only in the file, so a reset takes it
/// with everything else — and has to say so before it does.
#[test]
fn reset_drops_time_that_was_booked_but_never_pushed() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);
    let original = ws.read(ISSUE_FILE);

    ws.json(&["logwork", "PROJ-142", "2h 30m", "-m", "Traced it."]);
    assert!(ws.read(ISSUE_FILE).contains("2h 30m"));

    let report = ws.json(&["reset", "--dry-run"]);
    assert!(report["restored"][0]["dropped"]
        .as_array()
        .unwrap()
        .contains(&Value::from("worklogs")));

    ws.json(&["reset", "--yes"]);
    assert_eq!(ws.read(ISSUE_FILE), original);
    assert!(ws.json(&["status"])["dirty"].as_array().unwrap().is_empty());
}

/// A card dragged between board columns is a staged change no `.md` file
/// records, so reset has to reach the folder tree to undo it.
#[test]
fn reset_puts_a_dragged_card_back_in_its_column() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);

    let card = "PROJ-0142. Fix login flow.md";
    let from = ws
        .path()
        .join("boards/Platform Board/In Progress")
        .join(card);
    let to_dir = ws.path().join("boards/Platform Board/Done");
    std::fs::create_dir_all(&to_dir).unwrap();
    std::fs::rename(&from, to_dir.join(card)).unwrap();
    assert_eq!(
        ws.json(&["status"])["staged_transitions"][0]["origin"],
        "board-move"
    );

    let report = ws.json(&["reset", "--yes"]);
    assert_eq!(report["moves_undone"][0], "PROJ-142");
    assert!(from.exists(), "the card belongs back in In Progress");
    assert!(!to_dir.join(card).exists());
    assert!(ws.json(&["status"])["staged_transitions"]
        .as_array()
        .unwrap()
        .is_empty());
}

#[test]
fn reset_on_a_clean_workspace_says_so_and_does_nothing() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);
    let report = ws.json(&["reset", "--yes"]);
    assert!(report["restored"].as_array().unwrap().is_empty());
    assert!(report["removed"].as_array().unwrap().is_empty());
    assert!(ws.path().join(ISSUE_FILE).is_file());
}

// ---------------------------------------------------------------------------
// edit [M4.7]
// ---------------------------------------------------------------------------

/// The form needs a terminal. Without one the command has to say so rather
/// than half-work, because there is no flags-only path to fall back to.
#[test]
fn edit_without_a_terminal_says_what_to_use_instead() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);

    let err = ws.json_err(&["edit", "PROJ-142"], 7);
    assert_eq!(err["kind"], "missing_value");
    assert!(err["hint"].as_str().unwrap().contains("comment"), "{err}");
}

#[test]
fn editing_an_issue_the_workspace_does_not_have_is_a_usage_error() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);

    let err = ws.json_err(&["edit", "PROJ-999"], 2);
    assert_eq!(err["kind"], "usage");
    assert_eq!(err["details"]["issue"], "PROJ-999");
}

/// With no issue named the command searches, which also needs a terminal.
#[test]
fn edit_with_nothing_named_and_no_terminal_says_to_name_one() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);

    let err = ws.json_err(&["edit"], 7);
    assert_eq!(err["kind"], "missing_value");
    assert!(err["hint"].as_str().unwrap().contains("PROJ-142"), "{err}");
}

/// A locally created issue's key is lowercase hex, and the file name is the
/// thing tab-completion produces: both have to reach the same issue.
#[test]
fn an_issue_is_found_by_key_in_any_case_or_by_its_file_name() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);

    let created = ws.json(&[
        "create",
        "--summary",
        "[CARE] Забота о сотрудниках",
        "--type",
        "Task",
    ]);
    let key = created["key"].as_str().unwrap().to_string();
    let file = created["file"].as_str().unwrap().to_string();
    assert!(key.starts_with("NEW-"), "{key}");

    // `edit` resolves each spelling before deciding it needs a terminal, so the
    // key it reports back is the proof the lookup worked.
    for spelling in [
        key.clone(),
        key.to_uppercase(),
        file.clone(),
        format!("./{file}"),
    ] {
        let err = ws.json_err(&["edit", &spelling], 7);
        assert_eq!(
            err["details"]["issue"], key,
            "`{spelling}` did not resolve to {key}"
        );
    }

    // And the commands that stage a single change resolve them the same way.
    let commented = ws.json(&["comment", &key.to_uppercase(), "-m", "works"]);
    assert_eq!(commented["key"], key);
}

/// Story points are a custom field whose id differs per instance, which is why
/// the form can offer them at all: push looks the id up in the field map.
#[test]
fn a_story_point_change_is_pushed_to_the_instances_own_field() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);

    let text = ws
        .read(ISSUE_FILE)
        .replace("story_points: 5.0", "story_points: 8.0");
    ws.write(ISSUE_FILE, &text);

    let dry = ws.json(&["push", "--dry-run"]);
    let body = &dry["results"][0]["ops"][0]["request"]["body"]["fields"];
    assert_eq!(body["customfield_10016"], 8.0, "{body}");
}

// ---------------------------------------------------------------------------
// Sprints
// ---------------------------------------------------------------------------

/// The Sprint field is read-only on most instances, so a move is a board call
/// rather than a field update.
#[test]
fn changing_the_sprint_moves_the_issue_between_boards() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);
    assert!(ws.read(ISSUE_FILE).contains("sprint: Sprint 42"));

    let text = ws
        .read(ISSUE_FILE)
        .replace("sprint: Sprint 42", "sprint: Sprint 43");
    ws.write(ISSUE_FILE, &text);

    let dry = ws.json(&["push", "--dry-run"]);
    let op = &dry["results"][0]["ops"][0];
    assert_eq!(op["kind"], "move_sprint");
    assert_eq!(op["request"]["path"], "/rest/agile/1.0/sprint/43/issue");

    ws.json(&["push"]);
    ws.json(&["pull"]);
    assert!(
        ws.read(ISSUE_FILE).contains("sprint: Sprint 43"),
        "the server moved it and the pull saw it"
    );
    assert!(ws.json(&["status"])["dirty"].as_array().unwrap().is_empty());
}

/// Removing the line is how a file says "put it back in the backlog".
#[test]
fn clearing_the_sprint_moves_the_issue_to_the_backlog() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);

    let text = ws.read(ISSUE_FILE).replace("sprint: Sprint 42\n", "");
    ws.write(ISSUE_FILE, &text);

    let dry = ws.json(&["push", "--dry-run"]);
    let op = &dry["results"][0]["ops"][0];
    assert_eq!(op["kind"], "move_sprint");
    assert_eq!(op["request"]["path"], "/rest/agile/1.0/backlog/issue");

    ws.json(&["push"]);
    ws.json(&["pull"]);
    assert!(
        !ws.read(ISSUE_FILE).contains("sprint:"),
        "{}",
        ws.read(ISSUE_FILE)
    );
}

/// Jira's Sprint field is a list, and an issue that rolls over stays in the
/// sprint it came from as well as the one it moved to.
#[test]
fn an_issue_in_several_sprints_keeps_all_of_them() {
    let ws = Ws::new();
    ws.mutate_remote("PROJ-142", |issue| {
        issue["fields"]["customfield_10020"] = serde_json::json!([
            {"id": 41, "name": "Sprint 41", "state": "closed"},
            {"id": 42, "name": "Sprint 42", "state": "active"},
        ]);
    });
    ws.init();
    ws.json(&["pull"]);

    let text = ws.read(ISSUE_FILE);
    assert!(text.contains("sprint:\n- Sprint 41\n- Sprint 42"), "{text}");

    // Both folders exist once closed sprints are asked for; only the open one
    // before that.
    assert!(ws.path().join("sprints/Sprint 42").is_dir());
    assert!(!ws.path().join("sprints/Sprint 41").exists());
    ws.json(&["config", "set", "sprints.include_closed", "true"]);
    ws.json(&["doctor", "--regroup"]);
    assert!(
        ws.path()
            .join("sprints/Sprint 41")
            .join(ISSUE_FILE)
            .exists(),
        "a rolled-over issue belongs in the sprint it came from too"
    );

    // Moving it on acts on the last sprint, which is where it is now.
    let text = text.replace(
        "sprint:\n- Sprint 41\n- Sprint 42",
        "sprint:\n- Sprint 41\n- Sprint 42\n- Sprint 43",
    );
    ws.write(ISSUE_FILE, &text);
    let dry = ws.json(&["push", "--dry-run"]);
    let op = &dry["results"][0]["ops"][0];
    assert_eq!(op["kind"], "move_sprint");
    assert_eq!(op["request"]["path"], "/rest/agile/1.0/sprint/43/issue");
    assert_eq!(op["summary"], "Sprint 42 → Sprint 43");
}

/// Which sprints an issue has been through is the server's record of what
/// happened; no API rewrites it, so nothing is sent and it is said out loud.
#[test]
fn editing_sprint_history_is_reported_rather_than_attempted() {
    let ws = Ws::new();
    ws.mutate_remote("PROJ-142", |issue| {
        issue["fields"]["customfield_10020"] = serde_json::json!([
            {"id": 41, "name": "Sprint 41", "state": "closed"},
            {"id": 42, "name": "Sprint 42", "state": "active"},
        ]);
    });
    ws.init();
    ws.json(&["pull"]);

    let text = ws.read(ISSUE_FILE).replace("- Sprint 41\n", "");
    ws.write(ISSUE_FILE, &text);

    let report = ws.json(&["push"]);
    assert!(
        report["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|w| w.as_str().unwrap().contains("history")),
        "{report}"
    );
    assert!(report["results"].as_array().unwrap().is_empty());
}

/// A name no board knows cannot be resolved to an id, and guessing would move
/// the issue somewhere nobody asked for.
#[test]
fn a_sprint_the_workspace_does_not_know_is_reported_rather_than_guessed() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);

    let text = ws
        .read(ISSUE_FILE)
        .replace("sprint: Sprint 42", "sprint: Sprint 99");
    ws.write(ISSUE_FILE, &text);

    let report = ws.json(&["push"]);
    let warnings = report["warnings"].as_array().unwrap();
    assert!(
        warnings
            .iter()
            .any(|w| w.as_str().unwrap().contains("Sprint 99")),
        "{warnings:?}"
    );
    assert!(
        report["results"].as_array().unwrap().is_empty(),
        "nothing was sent"
    );
}

// ---------------------------------------------------------------------------
// Custom fields [M2.3]
// ---------------------------------------------------------------------------

/// A select list arrives as `{"value": "Platform"}`; the file shows `Platform`,
/// because nobody should have to write Jira's wire format into YAML.
#[test]
fn custom_fields_are_written_in_a_form_a_person_can_edit() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);

    let text = ws.read(ISSUE_FILE);
    assert!(text.contains("customfield_10456: Platform"), "{text}");
    assert!(
        text.contains("customfield_10205:\n  - Payments"),
        "a multi-select becomes a list:\n{text}"
    );
    // And an untouched file is not an edit.
    let push = ws.json(&["push", "--dry-run"]);
    assert!(push["results"].as_array().unwrap().is_empty(), "{push}");
}

/// The other half: what the file says has to go back as the shape Jira wants.
#[test]
fn editing_a_custom_field_pushes_the_shape_jira_expects() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);

    let text = ws
        .read(ISSUE_FILE)
        .replace(
            "customfield_10456: Platform",
            "customfield_10456: Engineering",
        )
        .replace(
            "customfield_10205:\n  - Payments",
            "customfield_10205:\n  - Payments\n  - Identity",
        );
    ws.write(ISSUE_FILE, &text);

    let dry = ws.json(&["push", "--dry-run"]);
    let body = &dry["results"][0]["ops"][0]["request"]["body"]["fields"];
    assert_eq!(
        body["customfield_10456"],
        serde_json::json!({"value": "Engineering"})
    );
    assert_eq!(
        body["customfield_10205"],
        serde_json::json!([{"value": "Payments"}, {"value": "Identity"}]),
        "a multi-select goes back as a list of options"
    );

    // A real push leaves the workspace clean.
    ws.json(&["push"]);
    assert!(ws.json(&["status"])["dirty"].as_array().unwrap().is_empty());
}

/// Deleting the entry is how a file says "clear this field".
#[test]
fn removing_a_custom_field_entry_clears_it_on_the_server() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);

    let text = ws
        .read(ISSUE_FILE)
        .replace("  customfield_10456: Platform\n", "");
    ws.write(ISSUE_FILE, &text);

    let dry = ws.json(&["push", "--dry-run"]);
    let body = &dry["results"][0]["ops"][0]["request"]["body"]["fields"];
    assert!(body["customfield_10456"].is_null(), "{body}");
}

/// Someone who keeps the raw Jira shape must not have it rewritten under them.
#[test]
fn a_custom_field_left_in_its_raw_shape_is_sent_verbatim() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);

    let text = ws.read(ISSUE_FILE).replace(
        "customfield_10456: Platform",
        "customfield_10456:\n    value: Engineering\n    id: '99'",
    );
    ws.write(ISSUE_FILE, &text);

    let dry = ws.json(&["push", "--dry-run"]);
    let body = &dry["results"][0]["ops"][0]["request"]["body"]["fields"];
    assert_eq!(
        body["customfield_10456"],
        serde_json::json!({"value": "Engineering", "id": "99"})
    );
}

// ---------------------------------------------------------------------------
// Credential storage [M1.5]
// ---------------------------------------------------------------------------

/// Run a command with extra environment, returning its parsed JSON stdout.
fn json_with(ws: &Ws, env: &[(&str, &str)], args: &[&str]) -> Value {
    let mut cmd = ws.cmd(args);
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = cmd.arg("--json").output().expect("run");
    assert!(
        out.status.success(),
        "`jired {}` failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).expect("json")
}

/// With no keychain reachable, the credential belongs to the workspace — and
/// `init` writes down which store it actually used, so nothing has to guess.
#[test]
fn credentials_default_to_the_workspace_when_there_is_no_keychain() {
    let ws = Ws::new();
    let report = ws.init();
    assert_eq!(report["account"]["secret_location"], "local_db");
    assert_eq!(report["account"]["secret_storage"], "encrypted_blob");
    assert!(ws
        .read(".jired/config.toml")
        .contains("storage = \"local_db\""));
    assert_eq!(ws.json(&["whoami"])["secret_location"], "local_db");
}

/// `creds.storage` is a decision, not a description: changing it moves the
/// credential rather than making the user authenticate again.
#[test]
fn doctor_moves_the_credential_when_creds_storage_changes() {
    let ws = Ws::new();
    let home = TempDir::new().unwrap();
    let xdg = home.path().to_str().unwrap();
    let credentials = home.path().join("jired/credentials.toml");
    ws.init();

    json_with(
        &ws,
        &[("XDG_CONFIG_HOME", xdg)],
        &["config", "set", "creds.storage", "user-config"],
    );
    let report = json_with(&ws, &[("XDG_CONFIG_HOME", xdg)], &["doctor"]);
    assert_eq!(report["problems"], 0);
    let check = report["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "credentials")
        .unwrap()
        .clone();
    assert_eq!(check["status"], "fixed");

    let file = std::fs::read_to_string(&credentials).expect("the credential file was written");
    assert!(file.contains("fixture.invalid"), "{file}");
    assert!(
        !file.contains("fixture-token"),
        "the token is not in the clear"
    );

    let who = json_with(&ws, &[("XDG_CONFIG_HOME", xdg)], &["whoami"]);
    assert_eq!(who["secret_location"], "user_config");

    // And a command that needs the token still finds it.
    json_with(&ws, &[("XDG_CONFIG_HOME", xdg)], &["pull"]);

    // Moving it back leaves nothing behind in the shared file.
    json_with(
        &ws,
        &[("XDG_CONFIG_HOME", xdg)],
        &["config", "set", "creds.storage", "local_db"],
    );
    json_with(&ws, &[("XDG_CONFIG_HOME", xdg)], &["doctor"]);
    let file = std::fs::read_to_string(&credentials).unwrap();
    assert!(!file.contains("fixture.invalid"), "{file}");
    assert_eq!(
        json_with(&ws, &[("XDG_CONFIG_HOME", xdg)], &["whoami"])["secret_location"],
        "local_db"
    );
}

/// The point of the user-config store: one credential, many workspaces.
#[test]
fn a_second_workspace_reuses_a_credential_from_the_user_config_store() {
    let first = Ws::new();
    let home = TempDir::new().unwrap();
    let xdg = home.path().to_str().unwrap();
    let env: &[(&str, &str)] = &[
        ("XDG_CONFIG_HOME", xdg),
        ("JIRED_CREDS_STORAGE", "user_config"),
    ];

    let report = json_with(&first, env, &["init", "--no-pull"]);
    assert_eq!(report["account"]["secret_location"], "user_config");

    // A fresh workspace with no token anywhere in its environment.
    let second = Ws::new();
    let mut cmd = second.cmd(&["init", "--no-pull", "--json"]);
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = cmd.env_remove("JIRED_TOKEN").output().unwrap();
    assert!(
        out.status.success(),
        "the shared credential should have been reused: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let report: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(report["account"]["secret_location"], "user_config");
    assert_eq!(report["account"]["display_name"], "Ilya A");
}

/// Asking for a store that cannot work has to fail, not quietly use another one:
/// `creds.storage = keychain` that silently wrote a file would be a lie.
#[test]
fn an_unavailable_keychain_is_refused_rather_than_downgraded() {
    let ws = Ws::new();
    let mut cmd = ws.cmd(&["init", "--no-pull", "--json"]);
    let out = cmd.env("JIRED_CREDS_STORAGE", "keychain").output().unwrap();
    assert_eq!(out.status.code(), Some(3));
    let err: Value = serde_json::from_slice(&out.stderr).unwrap();
    assert_eq!(err["kind"], "auth");
    assert!(err["hint"].as_str().unwrap().contains("creds.storage"));
}

#[test]
fn an_unknown_credential_store_is_rejected_when_it_is_set() {
    let ws = Ws::new();
    ws.init();
    let out = ws
        .cmd(&["config", "set", "creds.storage", "somewhere-else"])
        .arg("--json")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
}

// ---------------------------------------------------------------------------
// doctor
// ---------------------------------------------------------------------------

/// The migration path for the padded filenames: a workspace written by an older
/// jired has `PROJ-142. …md`, and doctor has to move it without losing the
/// attachment folder, the database row, or the grouping links.
#[test]
fn doctor_migrates_filenames_written_before_the_padding_convention() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);

    let legacy = "PROJ-142. Fix login flow.md";
    std::fs::rename(ws.path().join(ISSUE_FILE), ws.path().join(legacy)).unwrap();
    std::fs::rename(
        ws.path().join(".PROJ-0142. Fix login flow"),
        ws.path().join(".PROJ-142. Fix login flow"),
    )
    .unwrap();

    let dry = ws.json(&["doctor", "--dry-run"]);
    assert_eq!(dry["schema"], "jired/doctor/1");
    assert_eq!(dry["dry_run"], true);
    // The rename and the grouping links it invalidates.
    assert_eq!(dry["pending"], 2);
    assert_eq!(dry["fixed"], 0);
    assert!(
        ws.path().join(legacy).is_file(),
        "a dry run changes nothing"
    );

    let report = ws.json(&["doctor"]);
    let names = &report["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "filenames")
        .expect("a filenames check")
        .clone();
    assert_eq!(names["status"], "fixed");
    assert_eq!(names["changes"][0]["from"], legacy);
    assert_eq!(names["changes"][0]["to"], ISSUE_FILE);
    assert_eq!(report["problems"], 0);

    assert!(ws.path().join(ISSUE_FILE).is_file());
    assert!(!ws.path().join(legacy).exists());
    assert!(
        ws.path().join(".PROJ-0142. Fix login flow").is_dir(),
        "the attachment folder follows the rename"
    );
    // The grouping links point at the new name rather than dangling.
    let linked = ws
        .path()
        .join("by-parent/PROJ-0142. Fix login flow")
        .join("PROJ-0143. Add integration test.md");
    assert!(linked.exists(), "grouping links were rebuilt");

    // Everything jired command touches still resolves to the real key.
    let status = ws.json(&["status"]);
    assert!(status["dirty"].as_array().unwrap().is_empty());

    // Idempotent: a second run has nothing left to do.
    let again = ws.json(&["doctor"]);
    assert_eq!(again["fixed"], 0);
    assert_eq!(again["pending"], 0);
    assert_eq!(again["problems"], 0);
}

#[test]
fn doctor_restores_config_defaults_and_the_gitignore_block() {
    let ws = Ws::new();
    ws.init();

    ws.write(".gitignore", "target/\n");
    std::fs::write(
        ws.path().join(".jired/config.toml"),
        "[server]\nurl = \"https://acme.atlassian.net\"\n",
    )
    .unwrap();

    let report = ws.json(&["doctor"]);
    assert_eq!(report["problems"], 0);
    assert!(ws.read(".gitignore").contains("/.jired/"));
    let config = ws.read(".jired/config.toml");
    for key in ["links", "auto_download_max", "insecure"] {
        assert!(config.contains(key), "{key} missing from {config}");
    }
    // The rewritten file explains itself.
    assert!(config.contains("# "), "{config}");
}

/// Without an account there is nothing doctor can repair on its own, and saying
/// "healthy" would be a lie.
#[test]
fn doctor_reports_a_workspace_that_was_never_initialized() {
    let ws = Ws::new();
    ws.init();
    std::fs::remove_file(ws.path().join(".jired/session.db")).unwrap();

    let out = ws.cmd(&["doctor", "--json"]).output().unwrap();
    assert_eq!(out.status.code(), Some(1));
    let report: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(report["problems"], 1);
    let credentials = report["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "credentials")
        .unwrap()
        .clone();
    assert_eq!(credentials["status"], "problem");
}

// ---------------------------------------------------------------------------
// Children and checklists [M3.4, M5.4]
// ---------------------------------------------------------------------------

#[test]
fn subtasks_render_as_children_with_their_done_state() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);
    let text = ws.read(ISSUE_FILE);
    assert!(text.contains("- [ ] PROJ-143 Add integration test"));
    assert!(text.contains("- [x] PROJ-144 Update runbook"));
}

#[test]
fn ticking_a_box_does_nothing_until_a_done_transition_is_configured() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);

    let text = ws
        .read(ISSUE_FILE)
        .replace("- [ ] PROJ-143", "- [x] PROJ-143");
    ws.write(ISSUE_FILE, &text);

    // The edit is visible, but transitioning an issue because a box was ticked
    // is too surprising to do without being asked (decision D12).
    let status = ws.json(&["status"]);
    assert!(status["dirty"][0]["fields"]
        .as_array()
        .unwrap()
        .contains(&Value::from("children")));

    // Nothing is planned, and push says why rather than leaving the file
    // mysteriously dirty.
    let dry = ws.json(&["push", "--dry-run"]);
    let planned: Vec<&Value> = dry["results"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|r| r["ops"].as_array().unwrap())
        .collect();
    assert!(
        !planned.iter().any(|o| o["kind"] == "transition"),
        "nothing should be transitioned: {planned:?}"
    );
    let warning = dry["warnings"][0].as_str().unwrap_or_default();
    assert!(warning.contains("PROJ-143"), "{warning}");
    assert!(warning.contains("children.done_transition"), "{warning}");
}

#[test]
fn ticking_a_box_transitions_the_subtask_when_configured() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);
    ws.json(&["config", "set", "children.done_transition", "Done"]);

    let text = ws
        .read(ISSUE_FILE)
        .replace("- [ ] PROJ-143", "- [x] PROJ-143");
    ws.write(ISSUE_FILE, &text);

    let dry = ws.json(&["push", "--dry-run"]);
    let op = dry["results"][0]["ops"]
        .as_array()
        .unwrap()
        .iter()
        .find(|o| o["kind"] == "transition")
        .expect("the subtask transition was planned");
    // The transition targets the subtask, not the issue listing it.
    assert_eq!(op["request"]["path"], "/issue/PROJ-143/transitions");
    assert_eq!(op["request"]["body"]["to"], "Done");

    let push = ws.json(&["push"]);
    let ops = push["results"][0]["ops"].as_array().unwrap();
    assert!(ops
        .iter()
        .any(|o| o["kind"] == "transition" && o["status"] == "done"));

    // The subtask really moved on the server, so pulling brings its new status
    // back into the working copy.
    assert!(ws
        .read("PROJ-0143. Add integration test.md")
        .contains("status: To Do"));

    ws.json(&["fetch", "--full"]);
    let pulled = ws.json(&["pull", "--no-fetch"]);
    assert!(
        pulled["updated"]
            .as_array()
            .unwrap()
            .iter()
            .any(|u| u["key"] == "PROJ-143"),
        "the subtask's new status should come back: {pulled}"
    );
    assert!(ws
        .read("PROJ-0143. Add integration test.md")
        .contains("status: Done"));
}

/// The Data Center fixture carries a Smart Checklist, which the REST API hands
/// back as the plugin's Java `toString()`. What the file shows is the checklist,
/// not the dump — and not in `custom:`, where nobody could read it.
#[test]
fn a_plugin_checklist_renders_as_checkboxes_in_its_own_section() {
    let ws = Ws::of(Deployment::DataCenter);
    ws.init();
    ws.json(&["pull"]);
    let text = ws.read(Deployment::DataCenter.issue_file());

    let checklist = section(&text, "## Checklist");
    assert_eq!(
        checklist.trim(),
        "- [ ] `IN PROGRESS` Generate the replacement secret\n  \
         Coordinate the window with @jane\n\
         - [ ] Update the connector config\n\
         - [x] Announce the freeze",
        "{text}"
    );

    // Subtasks are a different list and live under their own heading.
    assert_eq!(
        section(&text, "## Children").trim(),
        "- [ ] DC-79 Update the connector config"
    );

    assert!(
        !text.contains("checklistId="),
        "the raw dump leaked: {text}"
    );
    assert!(!text.contains("customfield_10205"), "{text}");
    assert!(!text.contains("customfield_10207"), "{text}");
}

/// Read-only means the edit is refused out loud rather than silently kept or
/// silently sent.
#[test]
fn editing_the_checklist_section_is_reported_and_then_undone_by_a_pull() {
    let ws = Ws::of(Deployment::DataCenter);
    ws.init();
    ws.json(&["pull"]);
    let file = Deployment::DataCenter.issue_file();

    let text = ws.read(file).replace(
        "- [ ] Update the connector config",
        "- [x] I did this myself",
    );
    ws.write(file, &text);

    // Push says why it is not sending it, and plans nothing.
    let dry = ws.json(&["push", "--dry-run"]);
    let warning = dry["warnings"][0].as_str().unwrap_or_default();
    assert!(warning.contains("Checklist"), "{dry}");
    assert!(warning.contains("read-only"), "{dry}");

    // And status does not call the issue dirty for it, or the file would stay
    // dirty for ever: there is nothing push could do to clean it.
    let status = ws.json(&["status"]);
    assert!(
        !status["dirty"]
            .as_array()
            .unwrap()
            .iter()
            .any(|d| d["key"] == "DC-77"),
        "{status}"
    );

    // A real edit alongside it still goes through, and the rewrite that follows
    // the push puts the checklist back to what the server holds.
    let text = ws.read(file).replace(
        "summary: Rotate the LDAP bind credential",
        "summary: Rotate the LDAP bind credential now",
    );
    ws.write(file, &text);
    let push = ws.json(&["push"]);
    assert!(
        push["results"][0]["ok"].as_bool().unwrap_or(false),
        "{push}"
    );

    let after = ws.read("DC-0077. Rotate the LDAP bind credential now.md");
    assert!(!after.contains("I did this myself"), "{after}");
    assert!(
        after.contains("- [ ] Update the connector config"),
        "{after}"
    );
}

/// A workspace pulled before the split has its subtasks under `## Checklist`
/// and no `## Children` at all. Doctor is the only thing that can tell the two
/// layouts apart, so it is where the migration lives.
#[test]
fn doctor_moves_an_old_checklist_section_into_children() {
    let ws = Ws::of(Deployment::DataCenter);
    ws.init();
    ws.json(&["pull"]);
    let file = Deployment::DataCenter.issue_file();
    let current = ws.read(file);

    // What the same issue looked like when `## Checklist` meant the subtasks
    // and the plugin's own field was just another opaque `custom:` entry.
    let head = current.find("## Checklist").unwrap();
    let tail = current.find("## Comments").unwrap();
    let old = format!(
        "{}## Checklist\n\n- [ ] DC-79 Update the connector config\n\n{}",
        &current[..head],
        &current[tail..]
    )
    .replace(
        "custom:\n",
        "custom:\n  customfield_10205: Checklist(id=793, issueId=20501, _items=[])\n",
    );
    ws.write(file, &old);

    // A dry run says what it would do and leaves the file alone.
    let dry = ws.json(&["doctor", "--dry-run"]);
    let check = dry["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "sections")
        .expect("doctor checks the sections");
    assert_eq!(check["status"], "would_fix", "{dry}");
    assert_eq!(check["changes"][0]["from"], file);
    assert!(
        check["changes"][0]["to"]
            .as_str()
            .unwrap_or_default()
            .contains("Children"),
        "{check}"
    );
    assert_eq!(ws.read(file), old);

    // The repair reproduces exactly what this build writes: the subtask under
    // `## Children`, the plugin's checklist back from the last snapshot, and no
    // `custom:` entry for a field that now has a section of its own.
    ws.json(&["doctor"]);
    assert!(!ws.read(file).contains("customfield_10205"));
    assert_eq!(ws.read(file), current);

    // Byte-identical means in sync, so nothing is left looking edited.
    let status = ws.json(&["status"]);
    assert!(
        status["dirty"].as_array().unwrap().is_empty(),
        "the migrated file should not read as edited: {status}"
    );

    // And running it again is a no-op.
    let again = ws.json(&["doctor"]);
    let check = again["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "sections")
        .unwrap();
    assert_eq!(check["status"], "ok", "{again}");
}

/// A field nobody in this workspace cares about is noise in every file and a
/// value every push has to reason about. `ignored_custom_fields` takes it out.
#[test]
fn an_ignored_custom_field_leaves_the_files_and_stays_out() {
    let ws = Ws::of(Deployment::DataCenter);
    ws.init();
    ws.json(&["pull"]);
    let file = Deployment::DataCenter.issue_file();
    assert!(ws.read(file).contains("customfield_10300"));

    let added = ws.json(&[
        "config",
        "add",
        "ignored_custom_fields",
        "customfield_10300",
    ]);
    assert_eq!(added["values"][0], "customfield_10300");
    assert_eq!(added["changed"], true);

    // The config file keeps it as a list, above the first table where a
    // root-level key has to live.
    let config = std::fs::read_to_string(ws.path().join(".jired").join("config.toml")).unwrap();
    assert!(
        config.contains("ignored_custom_fields = [\"customfield_10300\"]"),
        "{config}"
    );

    // Doctor takes it out of the file that already carries it.
    let report = ws.json(&["doctor"]);
    let check = report["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "custom-fields")
        .expect("doctor checks the ignored fields");
    assert_eq!(check["status"], "fixed", "{report}");
    assert_eq!(check["changes"][0]["from"], file);
    assert!(!ws.read(file).contains("customfield_10300"));

    // Removing it from the file is not an edit anyone has to push, and the
    // field does not come back on the next pull either.
    let status = ws.json(&["status"]);
    assert!(status["dirty"].as_array().unwrap().is_empty(), "{status}");
    ws.json(&["pull"]);
    assert!(!ws.read(file).contains("customfield_10300"));

    // Un-ignoring brings it back, because nothing was thrown away: the value
    // was in the database the whole time. Doctor has to be the one to do it —
    // otherwise the file would read as having deleted the field, and push
    // would clear it on the server.
    let removed = ws.json(&[
        "config",
        "remove",
        "ignored_custom_fields",
        "customfield_10300",
    ]);
    assert_eq!(removed["values"].as_array().unwrap().len(), 0);
    ws.json(&["doctor"]);
    assert!(
        ws.read(file).contains("customfield_10300"),
        "{}",
        ws.read(file)
    );
    let status = ws.json(&["status"]);
    assert!(status["dirty"].as_array().unwrap().is_empty(), "{status}");
}

/// Between ignoring a field and running doctor, the file still holds a copy
/// that base no longer has. Push must not read that as an edit to send.
#[test]
fn an_ignored_field_left_in_a_file_is_reported_rather_than_pushed() {
    let ws = Ws::of(Deployment::DataCenter);
    ws.init();
    ws.json(&["pull"]);
    let file = Deployment::DataCenter.issue_file();

    ws.json(&[
        "config",
        "add",
        "ignored_custom_fields",
        "customfield_10300",
    ]);

    // Push only looks at a file it has a reason to look at, so the leftover
    // field only matters alongside a real edit — which is exactly when it would
    // have been sent.
    let text = ws.read(file).replace("priority: High", "priority: Low");
    ws.write(file, &text);

    let dry = ws.json(&["push", "--dry-run"]);
    let warning = dry["warnings"][0].as_str().unwrap_or_default();
    assert!(warning.contains("customfield_10300"), "{dry}");
    assert!(warning.contains("ignored_custom_fields"), "{dry}");
    let sent = dry["results"][0]["ops"][0]["request"]["body"]["fields"].clone();
    assert!(sent.get("priority").is_some(), "{sent}");
    assert!(sent.get("customfield_10300").is_none(), "{sent}");

    // And once doctor has taken it out of the file there is nothing to warn
    // about; the real edit still goes.
    ws.json(&["doctor"]);
    assert!(!ws.read(file).contains("customfield_10300"));
    let dry = ws.json(&["push", "--dry-run"]);
    assert!(dry["warnings"].as_array().unwrap().is_empty(), "{dry}");
    let sent = dry["results"][0]["ops"][0]["request"]["body"]["fields"].clone();
    assert!(sent.get("priority").is_some(), "{sent}");
}

/// `add` is for lists; saying it to a single-valued setting is a mistake worth
/// catching where the typo is still on screen.
#[test]
fn config_add_refuses_a_setting_that_holds_one_value() {
    let ws = Ws::new();
    ws.init();
    let err = ws.json_err(&["config", "add", "server.url", "https://x"], 2);
    assert!(err["message"].as_str().unwrap().contains("single value"));
    assert!(err["hint"].as_str().unwrap().contains("config set"));

    let err = ws.json_err(&["config", "add", "nonsense.key", "x"], 2);
    assert!(err["message"]
        .as_str()
        .unwrap()
        .contains("unknown config key"));
}

/// A setting that changed name is worse than a missing one: it reads as
/// configured and does nothing.
#[test]
fn doctor_renames_the_old_done_transition_setting() {
    let ws = Ws::new();
    ws.init();
    let config = ws.path().join(".jired").join("config.toml");
    let text = std::fs::read_to_string(&config).unwrap();
    std::fs::write(
        &config,
        format!("{text}\n[checklist]\ndone_transition = \"Done\"\n"),
    )
    .unwrap();

    ws.json(&["doctor"]);

    let text = std::fs::read_to_string(&config).unwrap();
    assert!(!text.contains("[checklist]"), "{text}");
    assert!(text.contains("[children]"), "{text}");

    let listed = ws.json(&["config", "list", "--all"]);
    let value = listed["settings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["key"] == "children.done_transition")
        .expect("the renamed setting is resolvable");
    assert_eq!(value["value"], "Done", "{listed}");
}

/// Text between one heading and the next.
fn section<'a>(text: &'a str, heading: &str) -> &'a str {
    let body = text
        .split_once(&format!("{heading}\n"))
        .unwrap_or_else(|| panic!("{heading} is missing from:\n{text}"))
        .1;
    body.split_once("\n## ").map(|(s, _)| s).unwrap_or(body)
}

// ---------------------------------------------------------------------------
// Grouping folders [M4.2, M4.3, M4.4, M4.5]
//
// These are views over state.db, never the source of truth, which is what lets
// `doctor --regroup` throws them away and rebuilds.
// ---------------------------------------------------------------------------

/// Resolve a link of whichever kind this platform produced.
/// The grouping check inside a `jired doctor` report.
fn groups_check(report: &Value) -> &Value {
    report["checks"]
        .as_array()
        .expect("doctor reports a list of checks")
        .iter()
        .find(|c| c["name"] == "groups")
        .expect("a --regroup run includes the groups check")
}

fn resolves_to_issue(link: &Path, key: &str) -> bool {
    match std::fs::read_to_string(link) {
        // A symlink or hard link reads as the issue itself.
        Ok(text) if text.contains(&format!("key: {key}")) => true,
        // A stub points at it instead.
        Ok(text) => text.contains("link: '") && text.contains(key),
        Err(_) => false,
    }
}

#[test]
fn pull_builds_the_parent_and_sprint_folders() {
    let ws = Ws::new();
    ws.init();
    let report = ws.json(&["pull"]);

    let groups = &report["groups"];
    assert!(groups["links"].as_u64().unwrap() > 0, "{report}");

    // PROJ-143 is a subtask of PROJ-142, so it groups under the parent's file.
    let parent = ws
        .path()
        .join("by-parent/PROJ-0142. Fix login flow/PROJ-0143. Add integration test.md");
    assert!(
        std::fs::symlink_metadata(&parent).is_ok(),
        "expected a link at {}",
        parent.display()
    );
    assert!(resolves_to_issue(&parent, "PROJ-143"));

    // PROJ-142 is in Sprint 42.
    let sprint = ws
        .path()
        .join("sprints/Sprint 42/PROJ-0142. Fix login flow.md");
    assert!(std::fs::symlink_metadata(&sprint).is_ok());
    assert!(resolves_to_issue(&sprint, "PROJ-142"));
}

#[test]
fn a_closed_sprint_is_skipped_unless_asked_for() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);
    // Sprint 41 is closed in the fixture and no issue references it, so the
    // folder must not appear.
    assert!(!ws.path().join("sprints/Sprint 41").exists());
}

#[test]
fn board_columns_group_issues_by_status() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);

    // PROJ-142 is In Progress (status id 3), PROJ-143 and PROJ-150 are To Do.
    let in_progress = ws
        .path()
        .join("boards/Platform Board/In Progress/PROJ-0142. Fix login flow.md");
    assert!(
        std::fs::symlink_metadata(&in_progress).is_ok(),
        "expected {}",
        in_progress.display()
    );
    let todo = ws
        .path()
        .join("boards/Platform Board/To Do/PROJ-0150. Rotate IdP certificate.md");
    assert!(std::fs::symlink_metadata(&todo).is_ok());
}

#[test]
fn grouping_folders_are_never_mistaken_for_issues() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);

    // Three issues, however many links exist beneath the grouping roots.
    let status = ws.json(&["status"]);
    assert_eq!(status["workspace"]["issues"], 3);
    assert!(status["dirty"].as_array().unwrap().is_empty());

    // And a second pull still sees nothing to do.
    let again = ws.json(&["pull"]);
    assert_eq!(again["updated"].as_array().unwrap().len(), 0);
    assert_eq!(again["unchanged"], 3);
}

#[test]
fn doctor_regroup_rebuilds_everything_from_the_database() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);

    let sprint = ws
        .path()
        .join("sprints/Sprint 42/PROJ-0142. Fix login flow.md");
    assert!(std::fs::symlink_metadata(&sprint).is_ok());

    // Throw the whole lot away, as a confused user or a bad merge might.
    for root in ["by-parent", "sprints", "boards"] {
        std::fs::remove_dir_all(ws.path().join(root)).unwrap();
    }
    assert!(!ws.path().join("sprints").exists());

    let report = ws.json(&["doctor", "--regroup"]);
    assert!(
        groups_check(&report)["regrouped"]["links"]
            .as_u64()
            .unwrap()
            > 0
    );
    assert!(
        std::fs::symlink_metadata(&sprint).is_ok(),
        "regroup must restore the link"
    );
    assert!(resolves_to_issue(&sprint, "PROJ-142"));
}

#[test]
fn regroup_clears_a_stale_group_rather_than_leaving_it() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);

    // A group folder from an earlier sprint the issue has since left.
    let stale = ws.path().join("sprints/Sprint 40");
    std::fs::create_dir_all(&stale).unwrap();
    std::fs::write(stale.join("PROJ-0142. Fix login flow.md"), "stale").unwrap();

    ws.json(&["doctor", "--regroup"]);
    assert!(
        !stale.exists(),
        "a group nothing belongs to must not survive a rebuild"
    );
    assert!(ws
        .path()
        .join("sprints/Sprint 42/PROJ-0142. Fix login flow.md")
        .exists());
}

#[test]
fn stub_mode_produces_followable_files_where_links_are_unavailable() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);
    ws.json(&["config", "set", "links.mode", "stub"]);

    let report = ws.json(&["doctor", "--regroup"]);
    assert_eq!(groups_check(&report)["regrouped"]["kind"], "stub");

    let stub = ws
        .path()
        .join("sprints/Sprint 42/PROJ-0142. Fix login flow.md");
    let text = std::fs::read_to_string(&stub).unwrap();
    assert!(text.starts_with("---\nlink: '"), "{text}");
    assert!(
        text.contains("../../PROJ-0142. Fix login flow.md"),
        "{text}"
    );
    // Readable by a human in any editor, not just resolvable by the filesystem.
    assert!(text.contains("[Open the issue]"), "{text}");
}

#[test]
fn dragging_a_card_between_columns_stages_a_transition() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);

    // Move PROJ-142's link from "In Progress" to "Done", as dragging the card
    // on a board would.
    let from = ws
        .path()
        .join("boards/Platform Board/In Progress/PROJ-0142. Fix login flow.md");
    let to_dir = ws.path().join("boards/Platform Board/Done");
    std::fs::create_dir_all(&to_dir).unwrap();
    std::fs::rename(&from, to_dir.join("PROJ-0142. Fix login flow.md")).unwrap();

    // The `.md` file is untouched, so only the board folders reveal this.
    let status = ws.json(&["status"]);
    let staged = &status["staged_transitions"][0];
    assert_eq!(staged["key"], "PROJ-142");
    assert_eq!(staged["from"], "In Progress");
    assert_eq!(staged["to"], "Done");
    assert_eq!(staged["origin"], "board-move");

    // `push --dry-run` accounts for it, so nothing happens unannounced.
    let dry = ws.json(&["push", "--dry-run"]);
    let op = dry["results"][0]["ops"]
        .as_array()
        .unwrap()
        .iter()
        .find(|o| o["kind"] == "transition")
        .expect("the move was planned");
    assert_eq!(op["request"]["body"]["to"], "Done");

    let push = ws.json(&["push"]);
    assert!(push["results"][0]["ops"]
        .as_array()
        .unwrap()
        .iter()
        .any(|o| o["kind"] == "transition" && o["status"] == "done"));

    // The issue really moved, and the rebuilt board reflects it.
    let after = ws.read(ISSUE_FILE);
    assert!(after.contains("status: Done"), "{after}");
    assert!(ws.json(&["status"])["staged_transitions"]
        .as_array()
        .unwrap()
        .is_empty());
}

#[test]
fn a_card_left_where_it_belongs_stages_nothing() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);
    let status = ws.json(&["status"]);
    assert!(
        status["staged_transitions"].as_array().unwrap().is_empty(),
        "{status}"
    );
}

#[test]
fn an_explicit_status_edit_wins_over_a_board_move() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);

    // Say one thing in the file and another on the board.
    let text = ws
        .read(ISSUE_FILE)
        .replace("status: In Progress", "status: Done");
    ws.write(ISSUE_FILE, &text);

    let from = ws
        .path()
        .join("boards/Platform Board/In Progress/PROJ-0142. Fix login flow.md");
    let to_dir = ws.path().join("boards/Platform Board/To Do");
    std::fs::rename(&from, to_dir.join("PROJ-0142. Fix login flow.md")).unwrap();

    // The file is the more explicit statement of intent, so it wins.
    let dry = ws.json(&["push", "--dry-run"]);
    let op = dry["results"][0]["ops"]
        .as_array()
        .unwrap()
        .iter()
        .find(|o| o["kind"] == "transition")
        .expect("a transition was planned");
    assert_eq!(op["request"]["body"]["to"], "Done");
}

#[test]
fn a_card_dropped_somewhere_that_is_not_a_column_says_so() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);

    // "In Review" is a workflow status but not a column on this board, so a
    // link left there means nothing — and saying nothing would leave the user
    // believing they had moved the card.
    let from = ws
        .path()
        .join("boards/Platform Board/In Progress/PROJ-0142. Fix login flow.md");
    let to_dir = ws.path().join("boards/Platform Board/In Review");
    std::fs::create_dir_all(&to_dir).unwrap();
    std::fs::rename(&from, to_dir.join("PROJ-0142. Fix login flow.md")).unwrap();

    let status = ws.json(&["status"]);
    assert!(status["staged_transitions"].as_array().unwrap().is_empty());
    let warning = status["warnings"][0]["message"]
        .as_str()
        .unwrap_or_default();
    assert!(warning.contains("PROJ-142"), "{status}");
    assert!(warning.contains("not a column"), "{warning}");
}

// ---------------------------------------------------------------------------
// usability contracts
// ---------------------------------------------------------------------------

/// Issues are files, so a path has to work wherever a key does — that is what
/// makes shell completion, `xargs` and editor integration possible.
#[test]
fn an_issue_can_be_named_by_its_file_as_well_as_its_key() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);

    let by_path = ws.json(&["comment", ISSUE_FILE, "-m", "from a path"]);
    assert_eq!(by_path["key"], "PROJ-142");

    let nested = format!("./sprints/Sprint 42/{ISSUE_FILE}");
    let by_nested_path = ws.json(&["log", &nested]);
    assert_eq!(by_nested_path["key"], "PROJ-142");
}

/// `jired pull PROJ-1` used to fail while `jired push PROJ-1` worked.
#[test]
fn pull_and_fetch_take_issues_positionally() {
    let ws = Ws::new();
    ws.init();
    let report = ws.json(&["pull", "PROJ-142"]);
    assert_eq!(report["schema"], "jired/pull/1");
    assert!(ws.path().join(ISSUE_FILE).exists());
}

/// A preview that writes nothing is the only safe way to inspect what a pull
/// would do to a working copy that has edits in it.
#[test]
fn pull_dry_run_writes_nothing() {
    let ws = Ws::new();
    ws.init();

    let report = ws.json(&["pull", "--dry-run"]);
    assert_eq!(report["dry_run"], true);
    assert!(!report["created"].as_array().unwrap().is_empty());
    assert!(
        !ws.path().join(ISSUE_FILE).exists(),
        "a dry run must not write files"
    );

    // And the real pull still does the work afterwards.
    ws.json(&["pull"]);
    assert!(ws.path().join(ISSUE_FILE).exists());
}

/// `--all` was declared, parsed, and silently ignored.
#[test]
fn status_all_lists_unchanged_issues() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);

    let plain = ws.json(&["status"]);
    assert!(plain.get("unchanged").is_none() || plain["unchanged"].as_array().unwrap().is_empty());

    let all = ws.json(&["status", "--all"]);
    let unchanged = all["unchanged"].as_array().expect("--all lists unchanged");
    assert!(!unchanged.is_empty(), "{all}");
    assert!(unchanged.iter().any(|u| u["key"] == "PROJ-142"), "{all}");
}

/// Config has to work before `init`, or the documented way to choose a
/// credential store for a machine cannot be followed.
#[test]
fn config_works_before_a_workspace_exists() {
    let dir = TempDir::new().unwrap();
    let home = dir.path().join("home");
    std::fs::create_dir_all(&home).unwrap();

    let out = Command::cargo_bin("jired")
        .unwrap()
        .current_dir(dir.path())
        .env("XDG_CONFIG_HOME", &home)
        .args(["config", "set", "creds.storage", "user_config", "--global"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let path = Command::cargo_bin("jired")
        .unwrap()
        .current_dir(dir.path())
        .env("XDG_CONFIG_HOME", &home)
        .args(["config", "path", "--global"])
        .output()
        .unwrap();
    let shown = String::from_utf8_lossy(&path.stdout);
    assert!(shown.contains("config.toml"), "{shown}");
}

/// A bad value used to be accepted and only surface at the next command that
/// happened to read it.
#[test]
fn config_set_rejects_a_value_the_setting_cannot_take() {
    let ws = Ws::new();
    ws.init();

    let err = ws.json_err(&["config", "set", "attachments.max_upload", "lots"], 2);
    assert_eq!(err["kind"], "usage");
    assert_eq!(err["details"]["key"], "attachments.max_upload");

    // A mistyped key is caught by clap itself now that the key list is a value
    // parser, which also means it is offered by shell completion.
    let out = ws.cmd(&["config", "get", "server.urls"]).output().unwrap();
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("server.url"), "{stderr}");
}

/// Piping into `head` and quitting early must not look like a crash.
#[test]
fn a_closed_pipe_is_not_an_error() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);

    // `sh -c '… | head -1'` is the real shape of the problem: head exits after
    // one line and the writer gets EPIPE on the next println.
    let shell = format!(
        "'{}' status --all --no-input | head -1",
        assert_cmd::cargo::cargo_bin("jired").display()
    );
    let out = std::process::Command::new("sh")
        .arg("-c")
        .arg(&shell)
        .current_dir(ws.path())
        .env("JIRED_FIXTURES", &ws.fixtures)
        .env("JIRED_NO_KEYRING", "1")
        .env("JIRED_SECRET_KEY", "test-passphrase")
        .env("JIRED_TOKEN", "fixture-token")
        .env("JIRED_PROJECT", "PROJ")
        .env("JIRED_URL", "https://fixture.invalid")
        .env("JIRED_EMAIL", "ilya@example.com")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("panicked") && !stderr.contains("failed printing to stdout"),
        "a closed pipe must not panic: {stderr}"
    );
}

/// The connection flags were global and silently ignored everywhere but init,
/// which reads like switching servers when it does nothing of the kind.
#[test]
fn connection_flags_are_rejected_outside_init() {
    let ws = Ws::new();
    ws.init();

    let out = ws
        .cmd(&["status", "--url", "https://elsewhere.invalid"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("--url"), "{stderr}");
}

/// One issue that cannot be pushed must not hide the ones that went out.
#[test]
fn a_lossy_block_does_not_discard_the_rest_of_the_push() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);

    // PROJ-142 has a read-only region; edit it, and edit a *different* issue in
    // a way that pushes cleanly.
    let text = ws.read(ISSUE_FILE).replace(
        "Do not deploy before the cert rotation.",
        "Do not deploy before the cert rotation. EDITED.",
    );
    ws.write(ISSUE_FILE, &text);
    ws.json(&["comment", "PROJ-143", "-m", "this one is fine"]);

    let report = ws.json_code(&["push"], 6);
    let results = report["results"].as_array().unwrap();
    assert!(
        results
            .iter()
            .any(|r| r["key"] == "PROJ-143" && r["ok"] == true),
        "the clean issue must still be pushed: {report}"
    );
    assert!(
        results
            .iter()
            .any(|r| r["key"] == "PROJ-142" && r["error_kind"] == "lossy"),
        "{report}"
    );
}
