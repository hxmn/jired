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
    /// How the workspace authenticates. Data Center defaults to a personal
    /// access token, which is what `init` recommends there — and which reaches
    /// the REST API only, so the one operation that needs a web page says so.
    auth: &'static str,
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
            auth: "pat",
        }
    }

    /// The same workspace signed in another way. Only Data Center has a choice
    /// worth making: basic auth is what authenticates its web pages.
    fn with_auth(mut self, auth: &'static str) -> Self {
        self.auth = auth;
        self
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
            // Data Center uses a bearer PAT and no email at all — unless the
            // test asked for basic auth, which needs a username to go with it.
            Deployment::DataCenter => {
                cmd.env("JIRED_URL", "https://jira.corp.example/jira")
                    .env("JIRED_AUTH", self.auth);
                match self.auth {
                    "basic" => cmd.env("JIRED_EMAIL", "ilya"),
                    _ => cmd.env_remove("JIRED_EMAIL"),
                };
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

    /// Replace one of the "server's" files, for a test that needs a shape the
    /// shared fixture does not have — three boards, say. Each workspace has its
    /// own copy, so this cannot reach another test.
    fn write_fixture(&self, name: &str, value: Value) {
        std::fs::write(
            self.fixtures.join(name),
            serde_json::to_string_pretty(&value).unwrap(),
        )
        .unwrap();
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

    /// Stamp an issue's `updated` on the "server" to now, milliseconds and all.
    ///
    /// Which is how a real Jira reports it, and the precision `mutate_remote`
    /// does not reproduce because it writes whole seconds.
    fn touch_remote_with_millis(&self, key: &str) -> String {
        let stamp = jired::store::now_rfc3339().replace('Z', ".480Z");
        let path = self.fixtures.join("issues.json");
        let mut issues: Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        for issue in issues.as_array_mut().unwrap() {
            if issue["key"] == key {
                issue["fields"]["updated"] = stamp.clone().into();
            }
        }
        std::fs::write(&path, serde_json::to_string_pretty(&issues).unwrap()).unwrap();
        stamp
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

/// The changelog is compiled in, so an installed binary can say what changed in
/// it without a checkout to read `CHANGELOG.md` out of — and says the same thing
/// whatever the checkout next to it has moved on to.
#[test]
fn the_binary_carries_the_changelog_it_was_built_from() {
    let run = |args: &[&str]| {
        let out = Command::cargo_bin("jired")
            .unwrap()
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "`jired {}`", args.join(" "));
        String::from_utf8(out.stdout).unwrap()
    };

    // Plain `version` is what `--version` says, and points at the changelog.
    let plain = run(&["version"]);
    for expected in ["commit:", "built:", "rustc:", "target:"] {
        assert!(plain.contains(expected), "{expected} missing from\n{plain}");
    }
    assert!(plain.contains("--changelog"), "{plain}");
    assert!(!plain.contains("# Changelog"), "not unless asked:\n{plain}");

    // And it is the file, not a summary of it.
    let printed = run(&["version", "--changelog"]);
    let repo = std::fs::read_to_string(repo_root().join("CHANGELOG.md")).unwrap();
    assert_eq!(printed.trim_end(), repo.trim_end());

    let doc: Value = serde_json::from_str(&run(&["version", "--json"])).unwrap();
    assert_eq!(doc["schema"], "jired/version/1");
    assert_eq!(doc["version"], env!("CARGO_PKG_VERSION"));
    // 19 KB of prose in every document would be paid for by agents that never
    // read it, so it arrives only when asked for.
    assert_eq!(doc["changelog"], Value::Null);

    let asked: Value = serde_json::from_str(&run(&["version", "--changelog", "--json"])).unwrap();
    assert_eq!(
        asked["changelog"].as_str().unwrap().trim_end(),
        repo.trim_end()
    );
}

/// What an agent whose docs an older jired wrote has to catch up on, and
/// nothing it already knows.
#[test]
fn the_changelog_can_be_read_from_a_version_on() {
    let out = Command::cargo_bin("jired")
        .unwrap()
        .args(["version", "--changelog", "--since", "0.20.0"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(text.contains("## [0.21.0]"), "{text}");
    assert!(!text.contains("## [0.20.0]"), "{text}");
    assert!(!text.contains("# Changelog"), "{text}");
}

fn repo_root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
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
fn init_creates_the_workspace_and_agent_files() {
    let ws = Ws::new();
    let report = ws.init();

    assert_eq!(report["schema"], "jired/init/1");
    assert_eq!(report["account"]["deployment"], "cloud");
    assert_eq!(report["account"]["auth_kind"], "api_token");
    assert_eq!(report["account"]["display_name"], "Ilya A");
    assert_eq!(report["workspace"]["project"], "PROJ");
    // It ends on the workspace's state, so nobody has to ask `status`.
    assert_eq!(report["state"]["clean"], true, "{report}");

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

    // An agent that has to ask before every `jired status` is an agent nobody
    // uses, so setup grants the commands that cannot change anything — and
    // only those.
    let files = report["workspace"]["agent_files"].as_array().unwrap();
    let files: Vec<&str> = files.iter().map(|v| v.as_str().unwrap()).collect();
    assert_eq!(
        files,
        [
            "CLAUDE.md",
            "AGENTS.md",
            ".claude/settings.json",
            ".codex/rules/default.rules"
        ]
    );

    let settings: Value = serde_json::from_str(&ws.read(".claude/settings.json")).unwrap();
    let allow = settings["permissions"]["allow"].as_array().unwrap();
    let allow: Vec<&str> = allow.iter().map(|v| v.as_str().unwrap()).collect();
    assert!(allow.contains(&"Bash(jired status:*)"), "{allow:?}");
    assert!(allow.contains(&"Bash(jired push --dry-run:*)"), "{allow:?}");
    assert!(!allow.contains(&"Bash(jired push:*)"), "{allow:?}");
    assert!(!allow.contains(&"Bash(jired pull:*)"), "{allow:?}");

    let rules = ws.read(".codex/rules/default.rules");
    assert!(
        rules.contains(r#"pattern = ["jired", "status"]"#),
        "{rules}"
    );
    assert!(
        rules.contains(r#"pattern = ["jired", "push", "--dry-run"]"#),
        "{rules}"
    );
    assert!(!rules.contains(r#"pattern = ["jired", "push"]"#), "{rules}");
    // Every rule grants; a `prompt` or `forbidden` here would be jired
    // overruling a decision that is the user's to make.
    assert_eq!(
        rules.matches("decision = \"allow\"").count(),
        rules.matches("prefix_rule(").count()
    );
}

/// A stale agent doc is worse than no agent doc: it reads as authoritative and
/// it is what an agent plans against. So the generated block carries the
/// version that wrote it, `status` says when that is not this binary, and both
/// `init-agents` and `doctor` put it right — in place, without stacking a
/// second block under the first or losing the user's own notes around it.
#[test]
fn a_doc_written_by_another_jired_is_reported_and_replaced() {
    let ws = Ws::new();
    ws.init();

    // Fresh from `init`, nothing is stale and the block names its version.
    let status = ws.json(&["status"]);
    assert!(
        status["stale_agent_docs"].as_array().unwrap().is_empty(),
        "{status:#}"
    );
    let version = env!("CARGO_PKG_VERSION");
    assert!(
        ws.read("CLAUDE.md")
            .contains(&format!("<!-- BEGIN jired v{version} -->")),
        "the block records which jired wrote it"
    );

    // What an upgrade leaves behind: one file stamped by an older jired, one
    // from before the stamp existed, and a note of the user's around each.
    ws.write(
        "CLAUDE.md",
        "# My notes\n\nkeep me\n\n<!-- BEGIN jired v0.0.1 -->\nancient instructions\n<!-- END jired -->\n",
    );
    ws.write(
        "AGENTS.md",
        "<!-- BEGIN jired -->\nunstamped instructions\n<!-- END jired -->\n\nkeep me too\n",
    );

    let status = ws.json(&["status"]);
    let stale = status["stale_agent_docs"].as_array().unwrap();
    assert_eq!(stale.len(), 2, "{status:#}");
    assert_eq!(stale[0]["file"], "CLAUDE.md");
    assert_eq!(stale[0]["generated_by"], "0.0.1");
    assert_eq!(stale[0]["expected"], version);
    assert_eq!(stale[1]["file"], "AGENTS.md");
    assert!(
        stale[1]["generated_by"].is_null(),
        "a block from before the stamp has no version to name"
    );

    // Doctor names the version rather than only reporting a difference.
    let dry = ws.json(&["doctor", "--dry-run"]);
    let agents = dry["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "agents")
        .expect("an agents check")
        .clone();
    assert_eq!(agents["status"], "would_fix");
    let detail = agents["detail"].as_str().unwrap();
    assert!(
        detail.contains("CLAUDE.md was written by jired 0.0.1"),
        "{detail}"
    );
    assert!(
        detail.contains("AGENTS.md was written by jired an earlier version"),
        "{detail}"
    );
    assert!(
        ws.read("CLAUDE.md").contains("ancient instructions"),
        "a dry run changes nothing"
    );

    ws.json(&["doctor", "--yes"]);
    for (name, note) in [("CLAUDE.md", "keep me"), ("AGENTS.md", "keep me too")] {
        let text = ws.read(name);
        assert!(
            text.contains(note),
            "{name} lost the user's own notes:\n{text}"
        );
        assert!(
            !text.contains("instructions\n<!-- END"),
            "{name} kept the old body"
        );
        assert_eq!(
            text.matches("<!-- BEGIN jired").count(),
            1,
            "{name} has more than one block:\n{text}"
        );
        assert!(
            text.contains(&format!("<!-- BEGIN jired v{version} -->")),
            "{text}"
        );
    }
    assert!(ws.json(&["status"])["stale_agent_docs"]
        .as_array()
        .unwrap()
        .is_empty());
}

/// The flags a scripted caller reaches for first were in `--help` and in no
/// generated table, which left parsing the whole `--json` document by hand
/// looking like the only way through.
#[test]
fn the_generated_docs_carry_the_flags_an_agent_scripts_with() {
    let ws = Ws::new();
    ws.init();
    let doc = ws.read("CLAUDE.md");
    for needle in [
        "--fields",
        "--json-compact",
        "jired diff --stat",
        "--exit-code",
        "--summary-contains",
        "--issue-type",
        "jired transition KEY[,KEY…] --to STATUS",
        "jired set --field FIELD --value VALUE",
        "jired push --resume",
        "--refresh-stale",
        "stale_agent_docs",
    ] {
        assert!(doc.contains(needle), "CLAUDE.md never mentions {needle:?}");
    }
    assert_eq!(doc, ws.read("AGENTS.md"), "both files carry the same block");
}

/// Adding a project beside the ones already set up is one call: the server
/// and the account come from the workspaces next to it, the directory is
/// made, and the output ends on the state rather than on "run status".
#[test]
fn init_beside_a_workspace_needs_only_the_project() {
    let ws = Ws::new();
    let first = ws
        .cmd(&["init", "--dir", "ALPHA", "--no-pull"])
        .output()
        .unwrap();
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );

    let out = ws
        .cmd(&["init", "--dir", "DELTA", "--json"])
        .env_remove("JIRED_URL")
        .env_remove("JIRED_EMAIL")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let report: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(report["account"]["base_url"], "https://fixture.invalid");
    assert_eq!(report["account"]["identity"], "ilya@example.com");
    assert_eq!(report["state"]["clean"], true, "{report}");
    assert!(report["state"]["staged_transitions"]
        .as_array()
        .unwrap()
        .is_empty());

    let text = ws
        .cmd(&["status", "--dir", "DELTA"])
        .env_remove("JIRED_URL")
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&text.stdout);
    assert!(text.contains("nothing to push, no conflicts"), "{text}");
}

#[test]
fn init_lists_the_servers_it_cannot_choose_between() {
    let ws = Ws::new();
    let first = ws
        .cmd(&["init", "--dir", "ALPHA", "--no-pull"])
        .output()
        .unwrap();
    assert!(first.status.success());
    std::fs::create_dir_all(ws.path().join("OTHER/.jired")).unwrap();
    std::fs::write(
        ws.path().join("OTHER/.jired/config.toml"),
        "[server]\nurl = \"https://other.example/\"\n",
    )
    .unwrap();

    let out = ws
        .cmd(&["init", "--dir", "DELTA", "--json"])
        .env_remove("JIRED_URL")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(7));
    let err: Value = serde_json::from_slice(&out.stderr).unwrap();
    let servers: Vec<&str> = err["details"]["servers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["url"].as_str().unwrap())
        .collect();
    assert_eq!(
        servers,
        ["https://fixture.invalid", "https://other.example"]
    );
    assert!(
        !ws.path().join("DELTA").exists(),
        "nothing is made for a refused init"
    );
}

/// A failed init used to leave `.jired/logs` and `.jired/templates`, which
/// every later command then took for a workspace.
#[test]
fn a_failed_init_leaves_the_directory_as_it_was() {
    let ws = Ws::new();
    // Refused before anything is written: no server to sign in to.
    let out = ws
        .cmd(&["init", "--dir", "NEW/DELTA"])
        .env_remove("JIRED_URL")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(7));
    assert!(!ws.path().join("NEW").exists());

    // Refused after the workspace was made: the token cannot be stored
    // where it was asked to go. An existing directory stays, `.jired/` goes.
    std::fs::create_dir_all(ws.path().join("KEEP")).unwrap();
    let out = ws
        .cmd(&["init", "--dir", "KEEP"])
        .env("JIRED_CREDS_STORAGE", "local_db")
        .env_remove("JIRED_SECRET_KEY")
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(ws.path().join("KEEP").is_dir());
    assert!(
        !ws.path().join("KEEP/.jired").exists(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    // And the same directory then initializes cleanly.
    let out = ws
        .cmd(&["init", "--dir", "KEEP", "--no-pull"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn init_agents_is_idempotent_and_leaves_other_permissions_alone() {
    let ws = Ws::new();
    ws.init();
    // Something the user put there themselves, in both files.
    ws.write(
        ".claude/settings.json",
        r#"{"permissions": {"allow": ["Bash(make test:*)"]}}"#,
    );
    ws.write(
        ".codex/rules/default.rules",
        "prefix_rule(pattern = [\"make\", \"test\"])\n",
    );

    ws.json(&["init-agents"]);
    let first = (
        ws.read(".claude/settings.json"),
        ws.read(".codex/rules/default.rules"),
    );
    ws.json(&["init-agents"]);
    assert_eq!(
        first,
        (
            ws.read(".claude/settings.json"),
            ws.read(".codex/rules/default.rules")
        ),
        "regenerating twice must not keep changing the files"
    );
    assert!(first.0.contains("Bash(make test:*)"), "{}", first.0);
    assert!(first.0.contains("Bash(jired status:*)"), "{}", first.0);
    assert!(first.1.contains(r#"["make", "test"]"#), "{}", first.1);
    assert!(first.1.contains(r#"["jired", "status"]"#), "{}", first.1);
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
        // A directory of its own: beside the other tests' workspaces in the
        // temp directory, init would take their server and ask for a token.
        .args(["init", "--dir", "fresh", "--no-input", "--json"])
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

/// A corrected worklog is `1 edited`, not `1 entries → 1 entries`. An issue
/// the server moved without changing anything the file shows says what did
/// move — its `updated` — rather than `updated PROJ-142 []`.
#[test]
fn pull_names_an_edited_worklog() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);
    let change = |report: &Value, field: &str| -> Value {
        report["updated"][0]["changes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["field"] == field)
            .unwrap_or_else(|| panic!("no {field} change: {report}"))
            .clone()
    };

    ws.mutate_remote("PROJ-142", |issue| {
        issue["fields"]["worklog"]["worklogs"][0]["timeSpent"] = "3h".into();
        issue["fields"]["worklog"]["worklogs"][0]["timeSpentSeconds"] = 10800.into();
    });
    let report = ws.json(&["pull"]);
    let worklogs = change(&report, "worklogs");
    assert_eq!(worklogs["to"], "1 edited", "{report}");
    assert_eq!(worklogs["added"], 0);
    assert!(worklogs["from"].is_null(), "{worklogs}");
}

/// One server change per test: `mutate_remote` stamps whole seconds, and a
/// second change inside the same second is not one an incremental fetch sees.
#[test]
fn pull_names_a_bare_timestamp_move_rather_than_an_empty_list() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);
    // Nothing a file shows; only the stamp.
    ws.mutate_remote("PROJ-142", |_| {});
    let report = ws.json(&["pull"]);
    let changes = report["updated"][0]["changes"].as_array().unwrap();
    assert_eq!(changes.len(), 1, "{report}");
    assert_eq!(changes[0]["field"], "updated", "{report}");
    assert_eq!(
        report["updated"][0]["fields"],
        serde_json::json!(["updated"])
    );
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

/// A comment carries regions of its own, and the gate used to look only at the
/// description. The edit was dropped on the way out — the payload is rebuilt
/// from the store — and push reported success having sent the original text
/// back over somebody else's comment.
#[test]
fn editing_a_region_inside_a_comment_blocks_the_push_too() {
    let ws = Ws::of(Deployment::DataCenter);
    ws.init();
    ws.json(&["pull"]);

    let file = ws.issue_file();
    let text = ws.read(file);
    assert!(
        text.contains("Checked against the audit log"),
        "the fixture comment must carry a region:\n{text}"
    );
    ws.write(
        file,
        &text.replace(
            "Checked against the audit log",
            "Checked against the audit log twice",
        ),
    );

    // `status` has to agree with the push it precedes.
    let dirty = &ws.json(&["status"])["dirty"][0];
    assert_eq!(dirty["key"], "DC-77");
    assert_eq!(dirty["lossy_touched"], true, "{dirty}");

    let report = ws.json_code(&["push", "DC-77"], 6);
    let failed = &report["results"][0];
    assert_eq!(failed["ok"], false);
    assert_eq!(failed["error_kind"], "lossy");
    assert!(
        failed["error"]
            .as_str()
            .unwrap()
            .contains("a read-only region was edited"),
        "{failed}"
    );
    assert!(failed["hint"].as_str().unwrap().contains("--allow-lossy"));
}

/// `status` counted new comments and nothing else, so editing one already on
/// the server left the workspace looking clean while `push` went on planning
/// the `update_comment` for it. Nobody reads a diff for work they have been
/// told they do not have.
#[test]
fn editing_a_comment_already_on_the_server_is_reported_as_dirty() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);

    let text = ws
        .read(ISSUE_FILE)
        .replace("Repro confirmed on staging.", "Repro confirmed on prod.");
    ws.write(ISSUE_FILE, &text);

    let dirty = &ws.json(&["status"])["dirty"][0];
    assert_eq!(dirty["key"], "PROJ-142");
    assert!(
        dirty["fields"]
            .as_array()
            .unwrap()
            .contains(&Value::from("comments")),
        "{dirty}"
    );

    // What `status` reports and what `push` sends are the same change.
    let ops = ws.json(&["push", "PROJ-142"])["results"][0]["ops"]
        .as_array()
        .unwrap()
        .clone();
    assert!(ops.iter().any(|o| o["kind"] == "update_comment"), "{ops:?}");
    assert!(ws.json(&["status"])["dirty"].as_array().unwrap().is_empty());
}

/// The flag the error points at has to do what the error says it does. It used
/// to skip the gate and nothing else, so the conversion still rebuilt the
/// region from the store: the edit that the flag was reached for was dropped,
/// and the original markup — the thing `--allow-lossy` says it replaces — was
/// sent straight back.
#[test]
fn allow_lossy_sends_the_edited_text_rather_than_the_original_markup() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);

    let text = ws.read(ISSUE_FILE).replace(
        "Do not deploy before the cert rotation.",
        "Do not deploy until the cert rotation lands.",
    );
    ws.write(ISSUE_FILE, &text);

    let journal = ws.path().join("calls.jsonl");
    let out = ws
        .cmd(&["push", "PROJ-142", "--json", "--allow-lossy"])
        .env("JIRED_FIXTURE_JOURNAL", &journal)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let sent = std::fs::read_to_string(&journal)
        .unwrap()
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .find_map(|c| c["body"]["fields"]["description"].as_object().cloned())
        .expect("a description update was sent");
    let sent = serde_json::to_string(&sent).unwrap();

    assert!(
        sent.contains("Do not deploy until the cert rotation lands."),
        "the edit is what the flag exists to send:\n{sent}"
    );
    assert!(
        !sent.contains("\"panel\""),
        "the panel is what the flag says it replaces:\n{sent}"
    );
}

/// PROJ-150 keeps a media node inside a bullet, so its markers are written at
/// the list's indent rather than at the left margin. The gate used to read that
/// indent as an edit and refuse the issue for anything at all — booking time
/// against it, labelling it — while `status` and `diff` said the description
/// was clean.
#[test]
fn an_indented_region_does_not_block_a_change_elsewhere_in_the_issue() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);

    let file = "PROJ-0150. Rotate IdP certificate.md";
    assert!(
        ws.read(file).contains("  <!--jired:raw:"),
        "the fixture must carry a region indented under a bullet:\n{}",
        ws.read(file)
    );

    ws.json(&[
        "logwork",
        "PROJ-150",
        "1h",
        "--started",
        "2026-08-24T09:00:00Z",
    ]);
    ws.json(&["label", "add", "probe-label", "PROJ-150"]);

    // Neither change is anywhere near the description.
    let dirty = &ws.json(&["status"])["dirty"][0];
    assert_eq!(dirty["key"], "PROJ-150");
    assert_eq!(
        dirty["lossy_touched"], false,
        "nothing touched the region: {dirty}"
    );

    let report = ws.json(&["push"]);
    let result = report["results"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["key"] == "PROJ-150")
        .expect("PROJ-150 was pushed");
    assert_eq!(result["ok"], true, "{result}");
    assert!(ws.json(&["status"])["dirty"].as_array().unwrap().is_empty());
}

/// The indent is not only on the marker's line. DC-78 carries a colour macro
/// inside a numbered item and another inside a nested bullet: both open
/// mid-line, and their approximation runs onto a second line that the list
/// indents and the store never saw.
#[test]
fn a_region_running_past_its_first_line_does_not_block_the_issue() {
    let ws = Ws::of(Deployment::DataCenter);
    ws.init();
    ws.json(&["pull"]);

    let file = "DC-0078. Document the LDAP rollback plan.md";
    let text = ws.read(file);
    // `1. …<!--jired:raw:…-->` opens mid-line, and the line under it is indented
    // to sit inside the item.
    assert!(
        text.contains("1. Restore the previous secret <!--jired:raw:"),
        "the fixture must open a region mid-item:\n{text}"
    );
    assert!(
        text.contains("\n   verified on staging<!--/jired:raw:"),
        "the region must run onto an indented line:\n{text}"
    );
    assert!(
        text.contains("\n    connection count<!--/jired:raw:"),
        "the nested bullet must indent further still:\n{text}"
    );

    ws.json(&[
        "logwork",
        "DC-78",
        "1h",
        "--started",
        "2026-08-24T09:00:00Z",
    ]);

    let dirty = &ws.json(&["status"])["dirty"][0];
    assert_eq!(dirty["key"], "DC-78");
    assert_eq!(
        dirty["lossy_touched"], false,
        "nothing touched either region: {dirty}"
    );

    let report = ws.json(&["push"]);
    let result = report["results"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["key"] == "DC-78")
        .expect("DC-78 was pushed");
    assert_eq!(result["ok"], true, "{result}");

    // Still a gate: an edit *inside* one of them is refused, at the indent it
    // is written at.
    let text = ws.read(file).replace(
        "\n   verified on staging",
        "\n   verified on staging and in production",
    );
    ws.write(file, &text);
    let blocked = ws.json_code(&["push", "DC-78"], 6);
    assert_eq!(blocked["results"][0]["error_kind"], "lossy");
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
    // And the one-line summary says so without walking the results.
    assert_eq!(report["summary"]["stale"][0], "PROJ-142");
    assert_eq!(report["summary"]["ok"], 0);
    assert_eq!(report["summary"]["failed"], 1);
    assert_eq!(report["results"][0]["ok"], false);
    assert_eq!(report["results"][0]["error_kind"], "conflict");
    assert_eq!(report["results"][0]["ops"][0]["status"], "skipped");

    // The local edit is untouched, so nothing was lost.
    assert!(ws.read(ISSUE_FILE).contains("summary: Local rename"));
}

/// Jira reports `updated` to the millisecond; the snapshot keeps whole seconds.
/// The same instant spelled two ways used to read as "the server changed",
/// refusing every push against an untouched issue — and `pull` could not clear
/// it, because snapshot and server really did agree.
#[test]
fn an_issue_the_server_has_not_touched_is_not_refused_as_stale() {
    let ws = Ws::new();
    ws.init();
    let stamp = ws.touch_remote_with_millis("PROJ-142");
    ws.json(&["pull"]);

    // What the file records is the truncation: whole seconds, no milliseconds.
    let file = ws.read(ISSUE_FILE);
    assert!(
        file.contains(&format!("base_updated: {}", stamp.replace(".480Z", "Z"))),
        "the snapshot should hold the truncated stamp:\n{file}"
    );

    let text = file.replace("sprint: Sprint 42", "sprint: Sprint 43");
    ws.write(ISSUE_FILE, &text);

    let report = ws.json(&["push"]);
    assert!(
        report["stale"].as_array().unwrap().is_empty(),
        "nothing on the server moved: {report}"
    );
    assert_eq!(report["results"][0]["ok"], true, "{report}");
    assert_eq!(report["results"][0]["ops"][0]["kind"], "move_sprint");
    assert_eq!(report["results"][0]["ops"][0]["status"], "done");
}

/// The two timestamps are checked against `jired.base_updated` in the file,
/// which is UTC, so the message has to name one instant rather than leave the
/// reader subtracting an offset it never printed.
#[test]
fn the_stale_message_names_the_zone_its_timestamps_are_in() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);

    let text = ws
        .read(ISSUE_FILE)
        .replace("summary: Fix login flow", "summary: Local rename");
    ws.write(ISSUE_FILE, &text);
    ws.mutate_remote("PROJ-142", |issue| {
        issue["fields"]["priority"] = serde_json::json!({ "name": "Low" });
    });

    let report = ws.json_code(&["push"], 5);
    let message = report["results"][0]["error"].as_str().unwrap();
    // An offset, or `Z` where the reader is on UTC — either way, a zone.
    let zoned = |stamp: &str| {
        stamp.ends_with('Z') || stamp[10..].contains('+') || stamp[10..].contains('-')
    };
    for stamp in message.split_whitespace().filter(|w| w.contains('T')) {
        assert!(zoned(stamp), "`{stamp}` names no timezone in: {message}");
    }
    assert_eq!(
        message
            .split_whitespace()
            .filter(|w| w.contains('T'))
            .count(),
        2,
        "both timestamps are in the message: {message}"
    );
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
    let sprint = ws.json(&[
        "sprint",
        "create",
        "Sprint 44",
        "--start",
        "2026-09-01",
        "--end",
        "2026-09-15",
    ]);

    for (name, doc) in [
        ("init", &init),
        ("pull", &pull),
        ("status", &status),
        ("diff", &diff),
        ("push", &push),
        ("sprint-create", &sprint),
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

/// An issue payload carries only the first page of comments — 50 on a stock
/// instance — and says how many there are. A file that shows the page and not
/// the count reads as a discussion that stopped, which is exactly wrong on the
/// long-running issues where it happens.
#[test]
fn every_comment_is_pulled_when_the_issue_payload_pages_them() {
    for deployment in [Deployment::Cloud, Deployment::DataCenter] {
        let ws = Ws::of(deployment);
        ws.init();
        ws.json(&["pull"]);
        let key = deployment.key();

        // The server now says there are three, and still sends one.
        ws.mutate_remote(key, |issue| {
            issue["fields"]["comment"]["total"] = 3.into();
        });
        let body = |n: u32| match deployment {
            Deployment::Cloud => serde_json::json!({
                "type": "doc", "version": 1,
                "content": [{"type": "paragraph",
                             "content": [{"type": "text", "text": format!("page comment {n}")}]}]
            }),
            Deployment::DataCenter => serde_json::json!(format!("page comment {n}")),
        };
        ws.write_fixture(
            &format!("comments-{key}.json"),
            serde_json::json!({
                "startAt": 0, "maxResults": 100, "total": 3,
                "comments": [
                    {"id": "90001", "body": body(1), "created": "2026-08-15T10:00:00.000+0000",
                     "updated": "2026-08-15T10:00:00.000+0000",
                     "author": {"displayName": "Jane D", "name": "jane", "accountId": "5b1af9"}},
                    {"id": "90002", "body": body(2), "created": "2026-08-15T11:00:00.000+0000",
                     "updated": "2026-08-15T11:00:00.000+0000",
                     "author": {"displayName": "Jane D", "name": "jane", "accountId": "5b1af9"}},
                    {"id": "90003", "body": body(3), "created": "2026-08-15T12:00:00.000+0000",
                     "updated": "2026-08-15T12:00:00.000+0000",
                     "author": {"displayName": "Jane D", "name": "jane", "accountId": "5b1af9"}},
                ]
            }),
        );

        ws.json(&["pull"]);
        let text = ws.read(deployment.issue_file());
        for n in 1..=3 {
            assert!(
                text.contains(&format!("page comment {n}")),
                "{deployment:?}: comment {n} of 3 is missing from:\n{text}"
            );
        }
    }
}

/// The support floor is a promise that has been tested, and what is tested is
/// the 10.7.4 description every Data Center route is checked against. A server
/// below that is refused where it costs nothing — before a workspace exists —
/// rather than at the first call that happens to differ.
#[test]
fn a_data_center_below_ten_is_refused_before_anything_is_stored() {
    let ws = Ws::of(Deployment::DataCenter);
    ws.write_fixture(
        "serverInfo.json",
        serde_json::json!({
            "version": "9.12.4",
            "versionNumbers": [9, 12, 4],
            "deploymentType": "Server",
            "serverTitle": "Fixture Data Center",
        }),
    );

    let err = ws.json_err(&["init", "--no-pull"], 10);
    assert_eq!(err["kind"], "unsupported_server");
    assert_eq!(err["details"]["version"], "9.12.4");
    assert_eq!(err["details"]["minimum"], "10.0");

    // Nothing was written for a server jired will not talk to: no credential,
    // and no workspace to mislead the next command.
    assert!(!ws.path().join(".jired/session.db").exists(), "{err}");
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

/// With no issue named `comment` searches for one, which needs a terminal to
/// draw the list on. Without one it says what to type, rather than clap's
/// "required arguments were not provided".
#[test]
fn comment_with_nothing_named_and_no_terminal_says_to_name_one() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);

    let err = ws.json_err(&["comment"], 7);
    assert_eq!(err["kind"], "missing_value");
    assert!(err["hint"].as_str().unwrap().contains("PROJ-142"), "{err}");

    // And the body is never asked for first: nothing to comment on means
    // nothing to write, and $EDITOR must not open over a question that failed.
    let err = ws.json_err(&["comment", "-m", "on it"], 7);
    assert_eq!(err["kind"], "missing_value");
}

/// The picker's sort and filter are settings like any other, which is what
/// lets them be persisted at all — and what makes a typo in one a message.
#[test]
fn the_issue_picker_settings_round_trip_and_reject_nonsense() {
    let ws = Ws::new();
    ws.init();

    ws.json(&["config", "set", "ui.issues.sort", "priority:asc"]);
    assert_eq!(
        ws.json(&["config", "get", "ui.issues.sort"])["value"],
        "priority:asc"
    );
    ws.json(&["config", "set", "ui.issues.filter", "category:done"]);
    assert_eq!(
        ws.json(&["config", "get", "ui.issues.filter"])["value"],
        "category:done"
    );

    // A bare column takes the direction that column is usually read in.
    ws.json(&["config", "set", "ui.issues.sort", "updated"]);
    assert_eq!(
        ws.json(&["config", "get", "ui.issues.sort"])["value"],
        "updated:desc"
    );

    for (key, value) in [
        ("ui.issues.sort", "sideways"),
        ("ui.issues.sort", "key:diagonally"),
        ("ui.issues.filter", "category:nonesuch"),
        ("ui.issues.filter", "colour:blue"),
    ] {
        let err = ws.json_err(&["config", "set", key, value], 2);
        assert_eq!(err["kind"], "usage", "`{value}` was accepted for {key}");
        assert_eq!(err["details"]["value"], value);
    }
}

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
    let staged = &report["issues"][0];
    assert_eq!(staged["key"], "PROJ-142");
    assert_eq!(staged["transition_id"], "31");
    assert_eq!(staged["changed"], true);
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

/// Closing out a sprint is a dozen tickets moving to one status, which is why
/// `list --keys` documents itself as something to pipe — into this, mostly.
/// One command, one push, and an issue whose workflow cannot get there is a
/// warning beside the ones that moved rather than a reason none of them did.
#[test]
fn transition_moves_several_issues_and_a_stuck_one_does_not_stop_the_rest() {
    let ws = Ws::new();
    // PROJ-143's workflow position offers no route to Done.
    ws.write_fixture(
        "transitions-PROJ-143.json",
        serde_json::json!([
            { "id": "21", "name": "Start Progress", "to_status": "In Progress",
              "to_status_id": "3", "required_fields": [] },
        ]),
    );
    ws.init();
    ws.json(&["pull"]);

    let report = ws.json(&[
        "transition",
        "PROJ-142",
        "PROJ-143",
        "PROJ-150",
        "--to",
        "Done",
    ]);
    let staged = report["issues"].as_array().unwrap();
    let keys: Vec<&str> = staged.iter().map(|s| s["key"].as_str().unwrap()).collect();
    assert_eq!(keys, ["PROJ-142", "PROJ-150"], "{report}");
    assert!(staged.iter().all(|s| s["changed"] == true), "{report}");

    let push = ws.json(&["push"]);
    assert_eq!(push["summary"]["ok"], 2, "{push}");
    for result in push["results"].as_array().unwrap() {
        assert!(
            result["ops"]
                .as_array()
                .unwrap()
                .iter()
                .any(|o| o["kind"] == "transition" && o["status"] == "done"),
            "{push}"
        );
    }
    // The stuck one kept its status: nothing was staged for it.
    assert!(ws
        .read("PROJ-0143. Add integration test.md")
        .contains("status: To Do"));
}

/// With every issue named unable to move, there is nothing to stage and the
/// command fails the way the single-issue refusal always has.
#[test]
fn a_transition_no_issue_can_make_is_an_error_not_a_quiet_success() {
    let ws = Ws::new();
    ws.write_fixture(
        "transitions.json",
        serde_json::json!([
            { "id": "21", "name": "Start Progress", "to_status": "In Progress",
              "to_status_id": "3", "required_fields": [] },
        ]),
    );
    ws.init();
    ws.json(&["pull"]);

    let err = ws.json_err(&["transition", "PROJ-143", "PROJ-150", "--to", "Done"], 2);
    assert_eq!(err["kind"], "usage", "{err}");
    assert!(
        err["message"].as_str().unwrap().contains("no transition"),
        "{err}"
    );
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
    assert_eq!(report["issues"][0]["key"], "PROJ-142");
    assert!(ws.read(ISSUE_FILE).contains("5b10a2ilya"));
}

/// Handing work over is rarely about one ticket, and `list --keys` pipes here.
/// Staged like `label` across the lot: one entry per file written, an issue
/// that already has the assignee reported rather than staged.
#[test]
fn assign_stages_several_issues_at_once() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);

    let report = ws.json(&["assign", "PROJ-142", "PROJ-143", "--me"]);
    let staged = report["issues"].as_array().unwrap();
    assert_eq!(staged.len(), 2, "{report}");
    // PROJ-142 is already Ilya's in the fixture: reported, not staged.
    assert_eq!(staged[0]["key"], "PROJ-142");
    assert_eq!(staged[0]["changed"], false, "{report}");
    assert_eq!(staged[1]["key"], "PROJ-143");
    assert_eq!(staged[1]["changed"], true, "{report}");
    assert!(ws
        .read("PROJ-0143. Add integration test.md")
        .contains("5b10a2ilya"));

    // Only the issue that moved is dirty.
    let status = ws.json(&["status"]);
    let dirty: Vec<&str> = status["dirty"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["key"].as_str().unwrap())
        .collect();
    assert_eq!(dirty, ["PROJ-143"], "{status}");
}

/// Jira's field configuration is per project, per issue type and per screen, so
/// an issue can have a priority while the screen you edit it through offers no
/// way to set one. That `PUT` is a 400 naming a screen, arriving after the push
/// has sent everything else. Asked first, it is a refusal at plan time that
/// names the field.
#[test]
fn a_field_the_edit_screen_lacks_is_refused_before_the_server_sees_it() {
    let ws = Ws::of(Deployment::DataCenter);
    // What this instance's edit screen carries — and priority is not on it.
    ws.write_fixture(
        "editmeta-DC-77.json",
        serde_json::json!({"fields": {
            "summary": {"name": "Summary", "required": true, "operations": ["set"]},
            "description": {"name": "Description", "required": false, "operations": ["set"]},
            "labels": {"name": "Labels", "required": false, "operations": ["add", "remove", "set"]},
        }}),
    );
    ws.init();
    ws.json(&["pull"]);

    let file = Deployment::DataCenter.issue_file();
    let text = ws.read(file);
    ws.write(file, &text.replace("priority: High", "priority: Low"));

    let dry = ws.json_code(&["push", "--dry-run"], 2);
    let result = &dry["results"][0];
    assert_eq!(result["ok"], false, "{dry}");
    let shown = serde_json::to_string(result).unwrap();
    // Named as the workspace knows it. This fixture's `/field` lists no system
    // fields, so the id is the best name there is — which is the fallback
    // working, not the mapping failing.
    assert!(
        shown.contains("priority is not on the edit screen"),
        "the refusal has to name the field: {shown}"
    );
    assert_eq!(result["error_kind"], "usage", "{dry}");

    // Nothing was sent, and the edit is still there to send once the screen
    // carries it.
    let journal = ws.path().join("calls.jsonl");
    ws.cmd(&["push", "--json"])
        .env("JIRED_FIXTURE_JOURNAL", &journal)
        .output()
        .unwrap();
    let sent = std::fs::read_to_string(&journal).unwrap_or_default();
    assert!(
        !sent.contains("\"PUT\""),
        "no field update should have gone out:\n{sent}"
    );
    assert!(ws.read(file).contains("priority: Low"));
}

/// The description travels in the same `PUT` as the fields, so it is asked
/// about on the same screen.
#[test]
fn a_description_edit_is_checked_against_the_edit_screen() {
    let ws = Ws::of(Deployment::DataCenter);
    ws.write_fixture(
        "editmeta-DC-77.json",
        serde_json::json!({"fields": {
            "summary": {"name": "Summary", "required": true, "operations": ["set"]},
        }}),
    );
    ws.init();
    ws.json(&["pull"]);

    let file = Deployment::DataCenter.issue_file();
    let text = ws.read(file);
    ws.write(
        file,
        &text.replace(
            Deployment::DataCenter.editable_line(),
            "Rolled back on the standby node instead.",
        ),
    );

    let dry = ws.json_code(&["push", "--dry-run"], 2);
    let shown = serde_json::to_string(&dry["results"][0]).unwrap();
    assert!(shown.contains("escription"), "{shown}");
}

/// The same question asked of a create screen, where the failure would
/// otherwise be an issue that does not get made at all.
#[test]
fn a_field_the_create_screen_lacks_is_refused_before_the_issue_is_made() {
    let ws = Ws::of(Deployment::DataCenter);
    ws.write_fixture(
        "createmeta-DC-Task.json",
        serde_json::json!({"fields": {
            "summary": {"name": "Summary", "required": true, "operations": ["set"]},
            "description": {"name": "Description", "required": false, "operations": ["set"]},
        }}),
    );
    ws.init();
    ws.json(&["pull"]);

    ws.json(&[
        "create",
        "--summary",
        "Audit the bind account",
        "--type",
        "Task",
        "--priority",
        "High",
    ]);

    let dry = ws.json_code(&["push", "--dry-run"], 2);
    let shown = serde_json::to_string(&dry["results"][0]).unwrap();
    assert!(
        shown.contains("priority is not on the create screen"),
        "{shown}"
    );

    let journal = ws.path().join("calls.jsonl");
    ws.cmd(&["push", "--json"])
        .env("JIRED_FIXTURE_JOURNAL", &journal)
        .output()
        .unwrap();
    let sent = std::fs::read_to_string(&journal).unwrap_or_default();
    assert!(
        !sent.contains("\"/issue\""),
        "no issue should have been created:\n{sent}"
    );
}

/// Assigning is not editing: Jira gives the assignee a route of its own because
/// it is its own permission, and an instance whose edit screen does not carry
/// `assignee` — a normal configuration — refuses the field update while
/// accepting the route. So the change travels as its own call on both
/// deployments, and never inside a `fields` payload.
#[test]
fn an_assignee_edit_is_its_own_call_on_the_assignee_route() {
    for deployment in [Deployment::Cloud, Deployment::DataCenter] {
        let ws = Ws::of(deployment);
        let (who, field) = match deployment {
            Deployment::Cloud => ("5b1af9jane", "accountId"),
            Deployment::DataCenter => ("jane", "name"),
        };
        ws.write_fixture(
            "users.json",
            serde_json::json!([{"id": who, "display_name": "Jane D"}]),
        );
        ws.init();
        ws.json(&["pull"]);
        let key = deployment.key();

        ws.json(&["assign", key, "--user", who]);

        let dry = ws.json(&["push", "--dry-run"]);
        let op = &dry["results"][0]["ops"][0];
        assert_eq!(op["kind"], "assign", "{deployment:?}: {dry}");
        assert_eq!(op["request"]["method"], "PUT");
        assert_eq!(op["request"]["path"], "/issue/{key}/assignee");
        assert_eq!(op["request"]["body"][field], who);

        let journal = ws.path().join("calls.jsonl");
        let out = ws
            .cmd(&["push", "--json"])
            .env("JIRED_FIXTURE_JOURNAL", &journal)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{deployment:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );

        let calls: Vec<Value> = std::fs::read_to_string(&journal)
            .unwrap()
            .lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect();
        assert!(
            calls.iter().any(|c| c["method"] == "PUT"
                && c["path"] == format!("/issue/{key}/assignee")
                && c["body"][field] == who),
            "{deployment:?}: no call to the assignee route in {calls:#?}"
        );
        // And it did not also travel as a field, which is the thing that fails
        // on an instance whose edit screen has no assignee on it.
        assert!(
            !calls
                .iter()
                .any(|c| c["body"]["fields"].get("assignee").is_some()),
            "{deployment:?}: the assignee was smuggled into a field payload: {calls:#?}"
        );
    }
}

/// A 2xx is not evidence. Jira answers success to writes it does not make, and
/// an assignee is refused for a permission as readily as a parent is — so the
/// re-read is asked, and a push that did not happen fails rather than reporting
/// a change the file no longer shows.
#[test]
fn an_assignee_the_server_did_not_apply_fails_the_push() {
    let ws = Ws::of(Deployment::DataCenter);
    ws.write_fixture(
        "ignored.json",
        serde_json::json!(["PUT /issue/DC-77/assignee"]),
    );
    ws.write_fixture(
        "users.json",
        serde_json::json!([{"id": "jane", "display_name": "Jane D"}]),
    );
    ws.init();
    ws.json(&["pull"]);

    ws.json(&["assign", "DC-77", "--user", "jane"]);
    let report = ws.json_code(&["push"], 10);

    let result = &report["results"][0];
    assert_eq!(result["ok"], false, "{report}");
    let error = serde_json::to_string(result).unwrap();
    assert!(
        error.contains("the assignee is still"),
        "the failure has to name what the server did not do: {error}"
    );
    // The edit stays in the file: it was never sent anywhere that kept it.
    assert!(
        ws.read(Deployment::DataCenter.issue_file())
            .contains("jane"),
        "{report}"
    );
}

/// A failed push leaves its journal for `--resume`; a retry re-plans the same
/// work from the same working copy. The retry's journal has to *replace* the
/// old one — appending gave every attempt a fresh set of byte-identical
/// `failed` rows for `status` to report, multiplying one stuck operation into
/// a pile.
#[test]
fn a_retried_failure_replaces_its_journal_rather_than_duplicating_it() {
    let ws = Ws::of(Deployment::DataCenter);
    ws.write_fixture(
        "ignored.json",
        serde_json::json!(["PUT /issue/DC-77/assignee"]),
    );
    ws.write_fixture(
        "users.json",
        serde_json::json!([{"id": "jane", "display_name": "Jane D"}]),
    );
    ws.init();
    ws.json(&["pull"]);
    ws.json(&["assign", "DC-77", "--user", "jane"]);

    ws.json_code(&["push"], 10);
    ws.json_code(&["push"], 10);

    let ops = ws.json(&["status"])["pending_ops"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(
        ops.len(),
        1,
        "two failed attempts at one operation are one unfinished operation: {ops:#?}"
    );
    assert_eq!(ops[0]["issue_key"], "DC-77");
}

/// `summary.failed: 0` is true of what a push attempted, and silent about the
/// queue: an operation an *earlier* push left unfinished sat invisible in the
/// report a caller actually reads, and finding it took `jired status`. It is a
/// warning now — for issues this push did not touch, since an issue in
/// `results` already tells its own story.
#[test]
fn push_warns_about_unfinished_work_it_did_not_touch() {
    let ws = Ws::of(Deployment::DataCenter);
    ws.write_fixture(
        "ignored.json",
        serde_json::json!(["PUT /issue/DC-77/assignee"]),
    );
    ws.write_fixture(
        "users.json",
        serde_json::json!([{"id": "jane", "display_name": "Jane D"}]),
    );
    ws.init();
    ws.json(&["pull"]);
    ws.json(&["assign", "DC-77", "--user", "jane"]);
    ws.json_code(&["push"], 10);

    // A push aimed elsewhere: DC-77 is not in its results.
    let report = ws.json(&["push", "DC-78", "--dry-run"]);
    assert!(report["results"].as_array().unwrap().is_empty(), "{report}");
    let warning = report["warnings"][0].as_str().unwrap_or_default();
    assert!(warning.contains("DC-77"), "{report}");
    assert!(warning.contains("assign"), "{report}");
    assert!(warning.contains("--resume"), "{report}");

    // A push that re-plans the issue says it in `results`, not twice.
    let report = ws.json_code(&["push"], 10);
    assert!(
        report["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .all(|w| !w.as_str().unwrap_or_default().contains("DC-77")),
        "{report}"
    );
}

/// The fields go first and the assignee follows, because they are two calls and
/// the order is the one `push` promises: everything an ordinary field update
/// can carry is already sent when the separate routes start.
#[test]
fn a_mixed_edit_sends_the_fields_then_the_assignee() {
    let ws = Ws::of(Deployment::DataCenter);
    ws.write_fixture(
        "users.json",
        serde_json::json!([{"id": "jane", "display_name": "Jane D"}]),
    );
    ws.init();
    ws.json(&["pull"]);

    let file = Deployment::DataCenter.issue_file();
    let text = ws.read(file);
    assert!(text.contains("priority: High"), "{text}");
    ws.write(file, &text.replace("priority: High", "priority: Low"));
    ws.json(&["assign", "DC-77", "--user", "jane"]);

    let dry = ws.json(&["push", "--dry-run"]);
    let kinds: Vec<&str> = dry["results"][0]["ops"]
        .as_array()
        .unwrap()
        .iter()
        .map(|o| o["kind"].as_str().unwrap())
        .collect();
    assert_eq!(kinds, ["update_fields", "assign"], "{dry}");
}

/// Assignment is a permission, held per project and per issue-security level,
/// so the users who can be assigned an issue are fewer than the users who
/// exist. Searching the whole directory offered names the server would then
/// refuse — a failure that arrives one push later, on someone else's screen.
#[test]
fn assign_searches_the_users_who_can_be_assigned_the_issue() {
    let ws = Ws::of(Deployment::DataCenter);
    ws.write_fixture(
        "users.json",
        serde_json::json!([
            {"id": "jane", "display_name": "Jane D"},
            {"id": "bob", "display_name": "Bob R"},
        ]),
    );
    // Bob exists on the instance and cannot be assigned this issue.
    ws.write_fixture(
        "assignable-DC-77.json",
        serde_json::json!([{"id": "jane", "display_name": "Jane D"}]),
    );
    ws.init();
    ws.json(&["pull"]);

    let err = ws.json_err(&["assign", "DC-77", "--user", "bo"], 2);
    assert!(
        err["message"].as_str().unwrap().contains("no user matches"),
        "{err}"
    );

    let report = ws.json(&["assign", "DC-77", "--user", "ja"]);
    assert_eq!(report["assignee"], "@[Jane D](jane)");
}

#[test]
fn assign_none_clears_the_field_and_pushes_a_null() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);

    ws.json(&["assign", "PROJ-142", "--none"]);
    assert!(!ws.read(ISSUE_FILE).contains("assignee:"));

    let push = ws.json(&["push", "--dry-run"]);
    let op = &push["results"][0]["ops"][0];
    // Its own route, and the null goes on the id rather than on the field: a
    // bare `null` is what a `fields` payload clears with, and this is not one.
    assert_eq!(op["kind"], "assign", "{push}");
    assert_eq!(op["request"]["path"], "/issue/{key}/assignee");
    assert_eq!(
        op["request"]["body"],
        serde_json::json!({"accountId": null})
    );
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

/// Everything the file says has to reach the server or be complained about.
/// `assignee` was handled for updates and simply absent from the create call,
/// so `diff` promised to send it, `push` reported clean success, and the value
/// was gone from the rewritten file with nothing saying so.
#[test]
fn creating_an_issue_sends_every_field_the_file_carries() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);

    let report = ws.json(&["create", "--summary", "Audit the SSO config"]);
    let file = report["file"].as_str().unwrap().to_string();
    let text = ws
        .read(&file)
        .replace("labels: []", "labels:\n- regular")
        .replace("components: []", "components:\n- Auth")
        .replace("fix_versions: []", "fix_versions:\n- '2.1'")
        .replace(
            "url: ''",
            "url: ''\npriority: High\nassignee: '@[Ilya A](5b10a2ilya)'\nsprint: Sprint 43",
        );
    ws.write(&file, &text);

    // What diff promises is what push has to send.
    let diff = ws.json(&["diff"]);
    let promised: Vec<&str> = diff["issues"][0]["fields"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|f| f["sendable"] == true)
        .map(|f| f["field"].as_str().unwrap())
        .collect();
    for field in ["priority", "assignee", "components", "labels"] {
        assert!(promised.contains(&field), "diff promised: {promised:?}");
    }

    // The plan shows the follow-ups too, not just the create.
    let dry = ws.json(&["push", "--dry-run"]);
    let sent = &dry["results"][0]["ops"][0]["request"]["body"]["fields"];
    // Cloud identifies a user by account id, Data Center by name.
    assert_eq!(sent["assignee"]["accountId"], "5b10a2ilya", "{sent}");
    assert_eq!(sent["priority"]["name"], "High");
    assert_eq!(sent["components"][0]["name"], "Auth");
    assert_eq!(sent["labels"][0], "regular");
    assert_eq!(sent["fixVersions"][0]["name"], "2.1");
    let kinds: Vec<&str> = dry["results"][0]["ops"]
        .as_array()
        .unwrap()
        .iter()
        .map(|o| o["kind"].as_str().unwrap())
        .collect();
    assert_eq!(
        kinds,
        ["create_issue", "move_sprint"],
        "the sprint move is part of the plan"
    );

    let push = ws.json(&["push"]);
    assert_eq!(push["results"][0]["ok"], true, "{push}");
    let new_key = push["results"][0]["new_key"].as_str().unwrap().to_string();
    let ops: Vec<&str> = push["results"][0]["ops"]
        .as_array()
        .unwrap()
        .iter()
        .map(|o| o["kind"].as_str().unwrap())
        .collect();
    assert_eq!(ops, ["create_issue", "move_sprint"]);

    // And the issue that came back really has them.
    let number = new_key.rsplit('-').next().unwrap();
    let created = ws.read(&format!("PROJ-{number:0>4}. Audit the SSO config.md"));
    assert!(
        created.contains("assignee:"),
        "assignee is gone:\n{created}"
    );
    assert!(created.contains("priority: High"), "{created}");
    assert!(created.contains("Auth"), "{created}");
    // The sprint came from the `move_sprint` op rather than from the create
    // payload, and the file proves the move actually landed — an op reported
    // done that the server did not act on now fails the push.
    assert!(created.contains("sprint: Sprint 43"), "{created}");
}

/// A field jired cannot send must not be reported as a clean success. `epic`
/// is in the format and in `diff`, and push has no call for it; saying so is
/// the difference between a known gap and a lost value.
#[test]
fn a_field_push_cannot_send_is_warned_about_rather_than_dropped() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);

    let text = ws
        .read(ISSUE_FILE)
        .replace("epic: PROJ-90", "epic: PROJ-100");
    ws.write(ISSUE_FILE, &text);

    // `diff` calls it a change, so a push that says nothing has misreported.
    let diff = ws.json(&["diff"]);
    assert!(
        diff["issues"][0]["fields"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f["field"] == "epic"),
        "{diff}"
    );

    let push = ws.json(&["push"]);
    let warnings = push["warnings"].as_array().unwrap();
    assert!(
        warnings
            .iter()
            .any(|w| w.as_str().unwrap().contains("`epic`")),
        "no warning about the field that was not sent: {push}"
    );
}

/// The field report's case: `links:` is editable and now has calls behind it.
/// An added line becomes `POST /issueLink`, a removed one `DELETE
/// /issueLink/{id}`, and the sides follow Jira's backwards convention — the
/// issue that "is blocked by" another stands at the *outward* end, because the
/// inward issue is the one that bears the type's outward description.
#[test]
fn a_link_edit_becomes_its_own_calls_on_push() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);

    let text = ws
        .read(ISSUE_FILE)
        .replace("- blocks: PROJ-150", "- is blocked by: PROJ-143");
    ws.write(ISSUE_FILE, &text);

    let dry = ws.json(&["push", "--dry-run"]);
    let ops = dry["results"][0]["ops"].as_array().unwrap().clone();
    let create = ops
        .iter()
        .find(|o| o["kind"] == "create_link")
        .expect("a create_link op");
    assert_eq!(create["request"]["method"], "POST");
    assert_eq!(create["request"]["path"], "/issueLink");
    assert_eq!(create["request"]["body"]["type"]["name"], "Blocks");
    // PROJ-142 is blocked by PROJ-143: the blocker is the inward issue.
    assert_eq!(create["request"]["body"]["inwardIssue"]["key"], "PROJ-143");
    assert_eq!(create["request"]["body"]["outwardIssue"]["key"], "PROJ-142");
    let delete = ops
        .iter()
        .find(|o| o["kind"] == "delete_link")
        .expect("a delete_link op");
    // By the link's own id, read out of the base snapshot.
    assert_eq!(delete["request"]["path"], "/issueLink/10500");

    let push = ws.json(&["push"]);
    assert_eq!(push["results"][0]["ok"], true, "{push}");

    // The re-read shows the link from this issue's side, and base advanced.
    let after = ws.read(ISSUE_FILE);
    assert!(after.contains("is blocked by: PROJ-143"), "{after}");
    assert!(!after.contains("blocks: PROJ-150"), "{after}");
    let status = ws.json(&["status"]);
    assert!(status["dirty"].as_array().unwrap().is_empty(), "{status}");
}

/// The field report's data loss, step for step. `A blocks B` changes B on the
/// server, so B used to be refused as stale in the same push; the merging pull
/// that followed kept B's own new link and dropped the inverse one, and the
/// next push of B deleted `is blocked by: A` from the server. Now the push
/// takes B's inverse link in before B's turn, and both go out together.
#[test]
fn a_chain_of_links_goes_out_in_one_push_and_deletes_nothing() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);
    let a = "PROJ-0143. Add integration test.md";
    let b = "PROJ-0150. Rotate IdP certificate.md";
    ws.write(
        a,
        &ws.read(a)
            .replace("links: []", "links:\n- blocks: PROJ-150"),
    );
    ws.write(
        b,
        &ws.read(b)
            .replace("links: []", "links:\n- relates to: PROJ-142"),
    );

    let push = ws.json(&["push", "PROJ-143", "PROJ-150"]);
    assert_eq!(push["summary"]["failed"], 0, "{push}");
    assert!(push["stale"].as_array().unwrap().is_empty(), "{push}");
    let kinds: Vec<&str> = push["results"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|r| r["ops"].as_array().unwrap())
        .map(|o| o["kind"].as_str().unwrap())
        .collect();
    assert_eq!(kinds, ["create_link", "create_link"], "{push}");

    let b_now = ws.read(b);
    assert!(b_now.contains("- is blocked by: PROJ-143"), "{b_now}");
    assert!(b_now.contains("- relates to: PROJ-142"), "{b_now}");
    let status = ws.json(&["status"]);
    assert!(status["dirty"].as_array().unwrap().is_empty(), "{status}");
}

/// The other half of the same report: a link the server gained while the file
/// gained a different one is not a conflict, under any strategy, and the push
/// after the pull sends the new link and deletes nothing.
#[test]
fn a_pull_keeps_links_added_on_both_sides() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);
    let b = "PROJ-0150. Rotate IdP certificate.md";
    ws.write(
        b,
        &ws.read(b)
            .replace("links: []", "links:\n- relates to: PROJ-142"),
    );
    ws.mutate_remote("PROJ-150", |issue| {
        issue["fields"]["issuelinks"] = serde_json::json!([{
            "id": "10777",
            "type": {"id": "10000", "name": "Blocks", "inward": "is blocked by", "outward": "blocks"},
            "inwardIssue": {"key": "PROJ-143"},
        }]);
    });

    let pull = ws.json(&["pull", "--strategy", "merge"]);
    assert!(pull["conflicts"].as_array().unwrap().is_empty(), "{pull}");
    let text = ws.read(b);
    assert!(text.contains("- is blocked by: PROJ-143"), "{text}");
    assert!(text.contains("- relates to: PROJ-142"), "{text}");

    let dry = ws.json(&["push", "--dry-run", "PROJ-150"]);
    let kinds: Vec<&str> = dry["results"][0]["ops"]
        .as_array()
        .unwrap()
        .iter()
        .map(|o| o["kind"].as_str().unwrap())
        .collect();
    assert_eq!(kinds, ["create_link"], "{dry}");
}

/// A direction the instance does not define is refused while nothing has been
/// sent: exit 2, in the plan, the hint naming the directions it does offer —
/// the spelling is per instance, not a fixed vocabulary.
#[test]
fn an_unknown_link_direction_is_refused_at_plan_time() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);

    let text = ws
        .read(ISSUE_FILE)
        .replace("- blocks: PROJ-150", "- depends on: PROJ-143");
    ws.write(ISSUE_FILE, &text);

    let dry = ws.json_code(&["push", "--dry-run"], 2);
    let result = &dry["results"][0];
    assert_eq!(result["ok"], false);
    assert!(
        result["error"].as_str().unwrap().contains("depends on"),
        "{dry}"
    );
    assert!(
        result["hint"].as_str().unwrap().contains("is blocked by"),
        "{dry}"
    );

    // Refused for real too, with the edit still in the file.
    ws.json_code(&["push"], 2);
    let status = ws.json(&["status"]);
    assert_eq!(status["dirty"].as_array().unwrap().len(), 1, "{status}");
    assert!(ws.read(ISSUE_FILE).contains("- depends on: PROJ-143"));
}

/// The live case from the field report: an issue created with `is blocked by`
/// lines was created and the links never followed. They are follow-ups the
/// create call cannot carry — planned before it so `--dry-run` shows them,
/// aimed at the key the create hands back.
#[test]
fn a_new_issue_pushes_its_links_after_the_create() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);

    let report = ws.json(&["create", "--summary", "Wire the audit log"]);
    let file = report["file"].as_str().unwrap().to_string();
    let text = ws.read(&file).replace(
        "links: []",
        "links:\n- is blocked by: PROJ-143\n- relates to: PROJ-150",
    );
    ws.write(&file, &text);

    let dry = ws.json(&["push", "--dry-run"]);
    let kinds: Vec<&str> = dry["results"][0]["ops"]
        .as_array()
        .unwrap()
        .iter()
        .map(|o| o["kind"].as_str().unwrap())
        .collect();
    assert_eq!(kinds, ["create_issue", "create_link", "create_link"]);

    let push = ws.json(&["push"]);
    let result = &push["results"][0];
    assert_eq!(result["ok"], true, "{push}");
    let new_key = result["new_key"].as_str().unwrap();
    let number = new_key.rsplit('-').next().unwrap();
    let created = ws.read(&format!("PROJ-{number:0>4}. Wire the audit log.md"));
    assert!(created.contains("is blocked by: PROJ-143"), "{created}");
    assert!(created.contains("relates to: PROJ-150"), "{created}");
}

/// The calls a command made, out of the fixture journal it wrote.
fn journaled(ws: &Ws, args: &[&str]) -> (Value, Vec<Value>) {
    let journal = ws.path().join("calls.jsonl");
    let _ = std::fs::remove_file(&journal);
    let out = ws
        .cmd(args)
        .arg("--json")
        .env("JIRED_FIXTURE_JOURNAL", &journal)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "`jired {}` failed\nstdout: {}\nstderr: {}",
        args.join(" "),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let calls = std::fs::read_to_string(&journal)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .collect();
    (serde_json::from_slice(&out.stdout).unwrap(), calls)
}

/// `jired link` writes the `links:` line an agent used to write with a
/// script, spelled the instance's way and checked against the link types the
/// last fetch listed; `jired unlink` takes it back out. Both stage like
/// `label` does, and say so.
#[test]
fn link_and_unlink_stage_the_lines_push_sends() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);
    let file = "PROJ-0143. Add integration test.md";

    let staged = ws.json(&["link", "PROJ-143", "Blocks", "PROJ-150"]);
    assert_eq!(staged["schema"], "jired/link/1");
    assert_eq!(staged["staged"], true, "{staged}");
    assert_eq!(staged["next"], "jired push PROJ-143");
    assert!(
        ws.read(file).contains("- blocks: PROJ-150"),
        "{}",
        ws.read(file)
    );
    // Again is not a change.
    let again = ws.json(&["link", "PROJ-143", "blocks", "PROJ-150"]);
    assert_eq!(again["issues"][0]["changed"], false);

    let err = ws.json_err(&["link", "PROJ-143", "depends", "on", "PROJ-150"], 2);
    assert!(
        err["hint"].as_str().unwrap().contains("is blocked by"),
        "{err}"
    );
    ws.json_err(&["link", "PROJ-143", "blocks", "PROJ-143"], 2);

    let (pushed, calls) = journaled(&ws, &["push", "PROJ-143", "--verify"]);
    assert_eq!(pushed["summary"]["failed"], 0, "{pushed}");
    assert_eq!(pushed["verify"], serde_json::json!([]), "{pushed}");
    assert!(calls.iter().any(|c| c["path"] == "/issueLink"), "{calls:?}");

    let unlinked = ws.json(&["unlink", "PROJ-143", "PROJ-150", "--push"]);
    assert_eq!(unlinked["staged"], false, "{unlinked}");
    let ops = &unlinked["push"]["results"][0]["ops"];
    assert_eq!(ops[0]["kind"], "delete_link", "{unlinked}");
    assert!(!ws.read(file).contains("PROJ-150"), "{}", ws.read(file));
}

/// A web link is a `links:` line like any other — `- web: URL "title"` —
/// read from the server's remote-link resource and written back through it.
#[test]
fn web_links_round_trip_through_links() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);
    let file = "PROJ-0143. Add integration test.md";
    let url = "https://wiki.example/display/TSK/Release+1.0.0";

    ws.json(&["link", "PROJ-143", "--web", url, "--title", "Release 1.0.0"]);
    let line = format!("- web: {url} \"Release 1.0.0\"");
    assert!(ws.read(file).contains(&line), "{}", ws.read(file));

    let (pushed, calls) = journaled(&ws, &["push", "--verify"]);
    assert_eq!(pushed["verify"], serde_json::json!([]), "{pushed}");
    let create = calls
        .iter()
        .find(|c| c["path"] == "/issue/PROJ-143/remotelink")
        .expect("the web link was created");
    assert_eq!(create["body"]["object"]["url"], url);
    assert_eq!(create["body"]["object"]["title"], "Release 1.0.0");
    // Read back into the file from the server, and nothing left to push.
    assert!(ws.read(file).contains(&line), "{}", ws.read(file));
    assert!(ws.json(&["status"])["dirty"].as_array().unwrap().is_empty());

    // One the server gained arrives with the next pull.
    ws.mutate_remote("PROJ-150", |issue| {
        issue["_fixture_remotelinks"] = serde_json::json!([
            {"id": 501, "object": {"url": "https://ci.example/run/7", "title": "CI run"}}
        ]);
    });
    ws.json(&["pull"]);
    let other = ws.read("PROJ-0150. Rotate IdP certificate.md");
    assert!(
        other.contains("- web: https://ci.example/run/7 \"CI run\""),
        "{other}"
    );

    // And removing the line deletes it by the id the server gave it.
    let removed = ws.json(&[
        "unlink",
        "PROJ-150",
        "--web",
        "https://ci.example/run/7",
        "--push",
    ]);
    let op = &removed["push"]["results"][0]["ops"][0];
    assert_eq!(op["kind"], "delete_remote_link", "{removed}");
    assert_eq!(op["status"], "done", "{removed}");
    let other = ws.read("PROJ-0150. Rotate IdP certificate.md");
    assert!(!other.contains("ci.example"), "{other}");
}

/// The field report's last task, small: release tasks created with a label, a
/// Fix Version and a sprint, chained by `blocks` links, the first carrying the
/// release page — all with jired, and checked against the server afterwards.
#[test]
fn linked_release_tasks_are_created_and_verified_with_jired_alone() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);

    let mut keys = Vec::new();
    for summary in ["Freeze 2.5.0", "Tag 2.5.0", "Announce 2.5.0"] {
        let created = ws.json(&[
            "create",
            "--summary",
            summary,
            "--labels",
            "release",
            "--set",
            "fix_versions=2.5.0",
            "--sprint",
            "Sprint 43",
            "--push",
            "--verify",
        ]);
        assert_eq!(
            created["push"]["verify"],
            serde_json::json!([]),
            "{created}"
        );
        keys.push(created["new_key"].as_str().unwrap().to_string());
    }
    ws.json(&["link", &keys[0], "blocks", &keys[1]]);
    ws.json(&["link", &keys[1], "blocks", &keys[2]]);
    ws.json(&[
        "link",
        &keys[0],
        "--web",
        "https://wiki.example/release-2.5.0",
        "--title",
        "Release 2.5.0",
    ]);

    // The chain goes out in one push, and the server holds what the files say.
    let pushed = ws.json(&["push", "--verify"]);
    assert_eq!(pushed["summary"]["failed"], 0, "{pushed}");
    assert!(pushed["stale"].as_array().unwrap().is_empty(), "{pushed}");
    assert_eq!(pushed["verify"], serde_json::json!([]), "{pushed}");

    let middle = ws.json(&["show", &keys[1]]);
    let links = middle["frontmatter"]["links"].to_string();
    assert!(
        links.contains(&format!("\"is blocked by\":\"{}\"", keys[0])),
        "{links}"
    );
    assert!(
        links.contains(&format!("\"blocks\":\"{}\"", keys[2])),
        "{links}"
    );
    assert!(ws.json(&["status"])["dirty"].as_array().unwrap().is_empty());
}

/// A chain written from its far end — `B is blocked by A` in B's file — makes
/// its inverse on an issue the push has already sent. `--verify` held each
/// issue to exactly the links its file had, called that inverse a failure, and
/// exited 10 on a push that had done everything asked.
#[test]
fn verify_does_not_count_a_link_another_issue_made_as_a_failure() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);
    // PROJ-143 has an edit of its own, so it is sent — and held to its file —
    // before PROJ-150's link reaches it.
    ws.json(&["label", "add", "release", "PROJ-143"]);
    ws.json(&["link", "PROJ-150", "is blocked by", "PROJ-143"]);

    let pushed = ws.json(&["push", "--verify"]);
    assert_eq!(pushed["summary"]["failed"], 0, "{pushed}");
    assert_eq!(pushed["verify"], serde_json::json!([]), "{pushed}");
    let a = ws.read("PROJ-0143. Add integration test.md");
    assert!(a.contains("- blocks: PROJ-150"), "{a}");
}

/// The same on Data Center, whose remote-link routes are the ones its own
/// description declares (`tests/dc_spec_contract.rs` holds them to it).
#[test]
fn web_links_go_out_on_data_center_too() {
    let ws = Ws::of(Deployment::DataCenter);
    ws.init();
    ws.json(&["pull"]);
    ws.json(&["link", "DC-77", "--web", "https://wiki.corp.example/x"]);
    let (pushed, calls) = journaled(&ws, &["push", "--verify"]);
    assert_eq!(pushed["verify"], serde_json::json!([]), "{pushed}");
    let create = calls
        .iter()
        .find(|c| c["path"] == "/issue/DC-77/remotelink")
        .expect("the web link was created");
    // No title given: the server needs one, and the URL is what Jira uses.
    assert_eq!(
        create["body"]["object"]["title"],
        "https://wiki.corp.example/x"
    );
    let text = ws.read(ws.issue_file());
    assert!(
        text.contains("- web: https://wiki.corp.example/x\n"),
        "{text}"
    );
}

/// A write the server accepts and does not apply passes a plain push; under
/// `--verify` the field is named with what was asked and what is there, and
/// the push exits 10.
#[test]
fn verify_names_a_field_the_server_did_not_take() {
    let ws = Ws::new();
    ws.write_fixture("ignored.json", serde_json::json!(["PUT /issue/PROJ-142"]));
    ws.init();
    ws.json(&["pull"]);
    ws.json(&["label", "add", "verified-label", "PROJ-142"]);

    let report = ws.json_code(&["push", "--verify"], 10);
    let found = report["verify"].as_array().unwrap();
    let labels = found
        .iter()
        .find(|m| m["field"] == "labels")
        .unwrap_or_else(|| panic!("labels not reported: {report}"));
    assert_eq!(labels["key"], "PROJ-142");
    assert!(
        labels["expected"].to_string().contains("verified-label"),
        "{report}"
    );
    assert!(
        !labels["actual"].to_string().contains("verified-label"),
        "{report}"
    );
}

/// The field report: `create --sprint 17 … --push` sent no sprint, the plan
/// showed no move, and the issue landed in the backlog — `17` was written into
/// the file as typed and named no sprint. It now resolves to the sprint whose
/// name ends in it, the dry run shows the move, and the dry run leaves no
/// draft behind for a later push to create.
#[test]
fn create_resolves_its_sprint_and_a_dry_run_leaves_nothing() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);
    ws.write_fixture(
        "sprints.json",
        serde_json::json!([
            {"id": "917", "name": "TSK Sprint 17", "state": "future", "board_id": "12"},
            {"id": "42", "name": "Sprint 42", "state": "active", "board_id": "12"},
        ]),
    );
    ws.json(&["fetch"]);

    let dry = ws.json(&[
        "create",
        "--summary",
        "Ship 1.0.0",
        "--sprint",
        "17",
        "--push",
        "--dry-run",
    ]);
    assert_eq!(dry["staged"], false, "{dry}");
    let ops = dry["push"]["results"][0]["ops"].as_array().unwrap();
    let kinds: Vec<&str> = ops.iter().map(|o| o["kind"].as_str().unwrap()).collect();
    assert_eq!(kinds, ["create_issue", "move_sprint"], "{dry}");
    assert_eq!(
        ops[1]["request"]["path"], "/rest/agile/1.0/sprint/917/issue",
        "{dry}"
    );
    assert!(
        !ws.path().join(dry["file"].as_str().unwrap()).exists(),
        "the draft the dry run planned from is gone"
    );
    let status = ws.json(&["status"]);
    assert!(
        status["new_issues"].as_array().unwrap().is_empty(),
        "{status}"
    );

    // Staged for real, the file carries the sprint's name, not the number.
    let staged = ws.json(&["create", "--summary", "Ship 1.0.0", "--sprint", "17"]);
    assert_eq!(staged["staged"], true);
    let text = ws.read(staged["file"].as_str().unwrap());
    assert!(text.contains("sprint: TSK Sprint 17"), "{text}");

    // A sprint nobody has is refused before anything is written.
    let err = ws.json_err(&["create", "--summary", "Nope", "--sprint", "99"], 2);
    assert!(err["message"].as_str().unwrap().contains("99"), "{err}");

    // And one written into a file by hand is refused at plan time, not warned
    // about while the issue goes to the backlog.
    let file = staged["file"].as_str().unwrap();
    ws.write(file, &ws.read(file).replace("TSK Sprint 17", "Sprint 99"));
    let refused = ws.json_code(&["push", "--dry-run"], 2);
    assert_eq!(refused["results"][0]["ok"], false, "{refused}");
    assert!(
        refused["results"][0]["hint"]
            .as_str()
            .unwrap()
            .contains("TSK Sprint 17"),
        "{refused}"
    );
}

/// The field report: `link … --push --dry-run` wrote the link into the file
/// and left it staged, so the next plain `push` would have sent it; and
/// `--dry-run` without `--push` was refused. Every staging command now puts the
/// file back after a dry run — what it held before, not base, so edits already
/// waiting there survive — and `--dry-run` alone is the dry run of the push.
#[test]
fn a_dry_run_of_a_staging_command_leaves_the_files_as_they_were() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);
    // An edit already waiting in the file, which the dry run must not lose.
    let waiting = ws
        .read(ISSUE_FILE)
        .replace("summary: Fix login flow", "summary: Fix the login flow");
    ws.write(ISSUE_FILE, &waiting);
    let renamed = "PROJ-0142. Fix the login flow.md";
    std::fs::rename(ws.path().join(ISSUE_FILE), ws.path().join(renamed)).unwrap();
    let before = ws.read(renamed);

    for args in [
        &[
            "link",
            "PROJ-142",
            "is blocked by",
            "PROJ-143",
            "--push",
            "--dry-run",
        ][..],
        &["link", "PROJ-142", "is blocked by", "PROJ-143", "--dry-run"],
        &["label", "add", "release", "PROJ-142", "--dry-run"],
        &["sprint", "add", "PROJ-142", "--sprint", "43", "--dry-run"],
        &[
            "set",
            "--field",
            "summary",
            "--value",
            "Renamed",
            "PROJ-142",
            "--dry-run",
        ],
        &["comment", "PROJ-142", "-m", "a note", "--dry-run"],
        &["logwork", "PROJ-142", "1h", "--dry-run"],
    ] {
        let out = ws.json(args);
        assert_eq!(out["push"]["dry_run"], true, "{args:?}: {out}");
        assert_ne!(out["staged"], true, "{args:?}: {out}");
        assert_eq!(ws.read(renamed), before, "{args:?} left the file changed");
    }
    let status = ws.json(&["status"]);
    let dirty = status["dirty"].as_array().unwrap();
    assert_eq!(
        dirty.len(),
        1,
        "only the edit that was already there: {status}"
    );
}

/// Unlinking a link that was only ever staged puts the file back to what the
/// server has. That is not a change waiting for push, and saying "staged"
/// about it sent a reader looking for one.
#[test]
fn undoing_a_staged_link_is_not_reported_as_staged() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);
    let linked = ws.json(&["link", "PROJ-142", "is blocked by", "PROJ-143"]);
    assert_eq!(linked["staged"], true, "{linked}");
    let undone = ws.json(&["unlink", "PROJ-142", "is blocked by", "PROJ-143"]);
    assert_eq!(undone["staged"], false, "{undone}");
    assert_eq!(undone["next"], Value::Null, "{undone}");
    assert_eq!(undone["issues"][0]["differs"], false, "{undone}");
}

/// The field report: sprint ids ran two ahead of the names, so `--sprint 17`
/// was id 17, `TSK Спринт 15` — and would still have been once `TSK Спринт
/// 17` existed. A number that names one sprint by id and another by name is
/// refused, naming both; `id:17` and the full name say which.
#[test]
fn a_sprint_number_that_is_one_sprint_s_id_and_another_s_name_is_refused() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);
    ws.write_fixture(
        "sprints.json",
        serde_json::json!([
            {"id": "17", "name": "TSK Спринт 15", "state": "active", "board_id": "12"},
            {"id": "19", "name": "TSK Спринт 17", "state": "future", "board_id": "12"},
        ]),
    );
    ws.json(&["fetch"]);

    let err = ws.json_err(&["sprint", "add", "PROJ-142", "--sprint", "17"], 2);
    let known = err["details"]["known"].to_string();
    assert!(
        known.contains("TSK Спринт 15") && known.contains("TSK Спринт 17"),
        "{err}"
    );
    assert!(err["hint"].as_str().unwrap().contains("id:17"), "{err}");

    let by_id = ws.json(&["sprint", "add", "PROJ-142", "--sprint", "id:17"]);
    assert_eq!(by_id["sprint"]["name"], "TSK Спринт 15", "{by_id}");
    let by_name = ws.json(&["sprint", "add", "PROJ-150", "--sprint", "TSK Спринт 17"]);
    assert_eq!(by_name["sprint"]["id"], "19", "{by_name}");
    // A number only one reading of finds is still taken.
    let one = ws.json(&["sprint", "add", "PROJ-143", "--sprint", "15"]);
    assert_eq!(one["sprint"]["id"], "17", "{one}");
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
// Reports
// ---------------------------------------------------------------------------

/// The field report's chart — issues per sprint and type — took 75 searches
/// and a `jq` loop. One command answers it, from the working copy offline or
/// from the server with `--jql`, and the two agree.
#[test]
fn report_issues_counts_a_matrix_from_the_files_or_the_server() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);

    let local = ws.json(&["report", "issues", "--by", "sprint,type"]);
    assert_eq!(local["schema"], "jired/report-issues/1");
    assert_eq!(local["source"], "workspace");
    assert_eq!(local["by"], serde_json::json!(["sprint", "type"]));
    assert_eq!(local["total"], 3);
    assert_eq!(
        local["columns"],
        serde_json::json!(["Story", "Sub-task", "Task"])
    );
    let rows = local["rows"].as_array().unwrap();
    assert_eq!(rows[0]["value"], "Sprint 42", "{local}");
    assert_eq!(rows[0]["counts"]["Story"], 1);
    assert_eq!(rows[0]["total"], 1);
    // An issue in no sprint is counted, under a name a chart can show.
    assert_eq!(rows[1]["value"], "(none)", "{local}");
    assert_eq!(rows[1]["total"], 2);
    assert_eq!(local["totals"]["Task"], 1);

    let remote = ws.json(&[
        "report",
        "issues",
        "--by",
        "sprint,type",
        "--jql",
        "project = PROJ",
    ]);
    assert_eq!(remote["source"], "server");
    assert_eq!(remote["jql"], "project = PROJ");
    assert_eq!(remote["rows"], local["rows"], "{remote}");

    // Markdown to paste into a page: header, alignment, a total row last.
    let md = ws
        .cmd(&["report", "issues", "--by", "sprint,type", "--format", "md"])
        .output()
        .unwrap();
    let md = String::from_utf8(md.stdout).unwrap();
    assert!(
        md.starts_with("| sprint | Story | Sub-task | Task | total |\n| --- | ---: |"),
        "{md}"
    );
    assert!(md.contains("| Sprint 42 | 1 | 0 | 0 | 1 |"), "{md}");
    assert!(md.trim_end().ends_with("| total | 1 | 1 | 1 | 3 |"), "{md}");

    let one = ws.json(&["report", "issues", "--by", "label"]);
    let values: Vec<&str> = one["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["value"].as_str().unwrap())
        .collect();
    assert_eq!(values, ["auth", "backend", "ops", "(none)"], "{one}");

    let err = ws.json_err(&["report", "issues", "--by", "sprint,type,label"], 2);
    assert!(err["message"].as_str().unwrap().contains("--by"), "{err}");
}

/// The report is read off the working copy, so it counts everybody's time —
/// not only the account's — and breaks each person down by issue.
///
/// The window is asked for explicitly: the fixture bookings carry fixed dates,
/// and a default seven days would stop reaching them the week after next.
#[test]
fn report_time_totals_what_everyone_booked() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);

    let report = ws.json(&["report", "time", "--days", "3650"]);
    assert_eq!(report["days"], 3650);
    assert_eq!(report["project"], "PROJ");
    assert_eq!(report["seconds"], 9000 + 3600);
    assert_eq!(report["entries"], 2);
    assert_eq!(report["skipped"], 0);

    // Most time first: two and a half hours from Jane, one from the account.
    let people = report["people"].as_array().unwrap();
    assert_eq!(people.len(), 2, "{report}");
    assert_eq!(people[0]["name"], "Jane D");
    assert_eq!(people[0]["seconds"], 9000);
    assert_eq!(people[1]["name"], "Ilya A");
    assert_eq!(people[1]["seconds"], 3600);

    // And under each of them, where the time went.
    assert_eq!(people[0]["issues"][0]["key"], "PROJ-142");
    assert_eq!(people[0]["issues"][0]["summary"], "Fix login flow");
    assert_eq!(people[0]["issues"][0]["seconds"], 9000);

    // A column per day of the window, and every entry in the column of the day
    // it was booked on: Jane's two and a half hours on the 15th, the account's
    // hour on the 16th.
    let dates = report["dates"].as_array().unwrap();
    assert_eq!(dates.len(), 3650);
    let column = |day: &str| {
        dates
            .iter()
            .position(|d| d == day)
            .unwrap_or_else(|| panic!("{day} is not a column"))
    };
    let by_day = |who: &Value| {
        who["by_day"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s.as_u64().unwrap())
            .collect::<Vec<_>>()
    };
    assert_eq!(by_day(&people[0])[column("2026-08-15")], 9000);
    assert_eq!(by_day(&people[1])[column("2026-08-16")], 3600);
    assert_eq!(by_day(&people[0]), by_day(&people[0]["issues"][0]));
    // The column totals are the same figures, added up the other way.
    assert_eq!(by_day(&report).iter().sum::<u64>(), 9000 + 3600);
}

/// Time staged locally has no author yet — the author is whoever pushes it — so
/// it counts towards the account, flagged as not yet on the server.
#[test]
fn report_time_counts_staged_bookings_and_says_they_are_staged() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);
    ws.json(&["logwork", "PROJ-142", "45m", "-m", "triage"]);

    let report = ws.json(&["report", "time", "--days", "3650"]);
    assert_eq!(report["staged_seconds"], 2700);
    let mine = report["people"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["name"] == "Ilya A")
        .unwrap_or_else(|| panic!("no row for the account: {report}"));
    assert_eq!(mine["seconds"], 3600 + 2700);
    assert_eq!(mine["staged_seconds"], 2700);
}

/// `--days` is the whole question the report answers, so it has to bite: an
/// entry booked now lands in a one-day window, and the older ones do not.
#[test]
fn report_time_counts_only_the_days_asked_for() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);

    let seconds = |doc: &Value| doc["seconds"].as_u64().unwrap();
    let before = seconds(&ws.json(&["report", "time", "--days", "1"]));
    ws.json(&["logwork", "PROJ-142", "45m"]);
    let after = ws.json(&["report", "time", "--days", "1"]);

    assert_eq!(seconds(&after) - before, 2700, "{after}");
    assert_eq!(after["days"], 1);

    // A window has to have days in it.
    let out = ws.cmd(&["report", "time", "--days", "0"]).output().unwrap();
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("--days"), "{stderr}");
}

/// Without a terminal the report is a table on stdout, and `--show-issues` is
/// what puts the second level into it.
#[test]
fn report_time_prints_a_table_when_there_is_no_terminal() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);

    let out = ws
        .cmd(&["report", "time", "--days", "3650"])
        .output()
        .unwrap();
    let flat = String::from_utf8_lossy(&out.stdout);
    assert!(flat.contains("Jane D"), "{flat}");
    assert!(flat.contains("2h 30m"), "{flat}");
    assert!(!flat.contains("Fix login flow"), "{flat}");

    let out = ws
        .cmd(&["report", "time", "--days", "3650", "--show-issues"])
        .output()
        .unwrap();
    let deep = String::from_utf8_lossy(&out.stdout);
    assert!(deep.contains("PROJ-142"), "{deep}");
    assert!(deep.contains("Fix login flow"), "{deep}");
}

/// The field report's worklog table, in one command: a fixed Monday-to-Friday
/// style window by its dates, sub-task time folded into the parent, a column
/// per person, as Markdown with a total row — what took `--days`, jq and sixty
/// lines of Python. And a person renamed in Jira is one column, not two: the
/// file pulled before the rename says `Jane D`, the one after says `Jane Doe`,
/// and the snapshots say both are `5b1af9jane`.
#[test]
fn report_time_draws_the_parent_level_table_for_a_window_of_dates() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);
    ws.mutate_remote("PROJ-150", |issue| {
        issue["fields"]["worklog"] = serde_json::json!({
            "startAt": 0, "maxResults": 20, "total": 1,
            "worklogs": [{
                "id": "10600",
                "author": {"accountId": "5b1af9jane", "displayName": "Jane Doe"},
                "started": "2026-08-18T09:00:00.000+0000",
                "timeSpent": "1h", "timeSpentSeconds": 3600,
            }],
        });
    });
    ws.json(&["pull"]);
    // Staged on the sub-task, so it is the account's and rolls up into PROJ-142.
    ws.json(&[
        "logwork",
        "PROJ-143",
        "45m",
        "--started",
        "2026-08-17T09:00:00Z",
    ]);

    let args = [
        "report",
        "time",
        "--since",
        "2026-08-15",
        "--until",
        "2026-08-19",
        "--rollup",
        "parent",
        "--columns",
        "people",
        "--format",
        "md",
    ];
    let out = ws.cmd(&args).output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim_end(),
        "| Issue | Jane Doe | Ilya A | Total |\n\
         |---|---:|---:|---:|\n\
         | PROJ-142 Fix login flow | 2h 30m | 1h 45m | 4h 15m |\n\
         | PROJ-150 Rotate IdP certificate | 1h |  | 1h |\n\
         | Total | 3h 30m | 1h 45m | 5h 15m |"
    );

    let report = ws.json(&args);
    assert_eq!(report["days"], 5);
    assert_eq!(report["dates"][0], "2026-08-15");
    assert_eq!(report["dates"][4], "2026-08-19");
    assert_eq!(report["rollup"], "parent");
    let jane = &report["people"][0];
    assert_eq!(jane["account_id"], "5b1af9jane", "{report}");
    assert_eq!(jane["name"], "Jane Doe");
    assert_eq!(jane["also_known_as"], serde_json::json!(["Jane D"]));
    assert_eq!(report["people"][1]["account_id"], "5b10a2ilya", "{report}");
    let top = &report["issues"][0];
    assert_eq!(top["key"], "PROJ-142");
    assert_eq!(top["type"], "Story");
    assert_eq!(top["rolled_up"], serde_json::json!(["PROJ-143"]));

    // Without the rollup the sub-task is its own row, saying where it hangs.
    let flat = ws.json(&[
        "report",
        "time",
        "--since",
        "2026-08-15",
        "--until",
        "2026-08-19",
    ]);
    let sub = flat["issues"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["key"] == "PROJ-143")
        .unwrap_or_else(|| panic!("{flat}"));
    assert_eq!(sub["parent"], "PROJ-142");

    // Dates the wrong way round are a usage error, naming both.
    let err = ws.json_err(
        &[
            "report",
            "time",
            "--since",
            "2026-08-19",
            "--until",
            "2026-08-15",
        ],
        2,
    );
    assert!(
        err["message"].as_str().unwrap().contains("2026-08-15"),
        "{err}"
    );
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
    // Settled, so not a conflict: the pull it ran says what it decided rather
    // than listing the issue as still standing.
    assert!(
        report["pull"]["conflicts"].as_array().unwrap().is_empty(),
        "{report}"
    );
    assert_eq!(report["pull"]["decided"][0]["key"], "PROJ-142", "{report}");

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

/// A merging pull: the summary conflicts in the frontmatter and a line of
/// the description conflicts in the body, so both kinds are recorded.
fn merged_with_markers(ws: &Ws) {
    let text = ws
        .read(ISSUE_FILE)
        .replace("summary: Fix login flow", "summary: Ours")
        .replace("Steps to reproduce are below.", "Our steps.");
    ws.write(ISSUE_FILE, &text);
    ws.mutate_remote("PROJ-142", |issue| {
        issue["fields"]["summary"] = "Theirs".into();
        let content = issue["fields"]["description"]["content"]
            .as_array_mut()
            .unwrap();
        content[2]["content"][0]["text"] = "Their steps.".into();
    });
    let out = ws
        .cmd(&["pull", "--strategy", "merge", "--json"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(5), "the setup must conflict");
}

/// The field report: after `pull --strategy merge`, `resolve --theirs` said
/// it had resolved the issue, the file kept the local values, and `resolve
/// --list` still listed both conflicts with every value null. The merge had
/// already moved base to the server's copy, so the pull `resolve` re-ran found
/// nothing to write — and only an issue it wrote had its record cleared.
#[test]
fn resolve_theirs_after_a_merging_pull_takes_the_server_side() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);
    merged_with_markers(&ws);

    // What there is to decide between is on the record.
    let listed = ws.json(&["resolve", "--list"]);
    let summary = listed["conflicts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["field"] == "summary")
        .expect("summary is recorded");
    assert_eq!(summary["ours_val"], "Ours", "{listed}");
    assert_eq!(summary["theirs_val"], "Theirs", "{listed}");

    ws.json(&["resolve", "--theirs", "--yes"]);
    let text = ws.read("PROJ-0142. Theirs.md");
    assert!(text.contains("summary: Theirs"), "{text}");
    assert!(text.contains("Their steps."), "{text}");
    assert!(!text.contains("Our steps."), "{text}");
    assert!(!text.contains("<<<<<<<"), "{text}");

    assert!(ws.json(&["resolve", "--list"])["conflicts"]
        .as_array()
        .unwrap()
        .is_empty());
    let status = ws.json(&["status"]);
    assert!(
        status["conflicts"].as_array().unwrap().is_empty(),
        "{status}"
    );
    assert!(status["dirty"].as_array().unwrap().is_empty(), "{status}");
}

#[test]
fn resolve_ours_after_a_merging_pull_keeps_the_local_side() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);
    merged_with_markers(&ws);

    ws.json(&["resolve", "--ours"]);
    let text = ws.read("PROJ-0142. Ours.md");
    assert!(text.contains("Our steps."), "{text}");
    assert!(!text.contains("Their steps."), "{text}");
    assert!(!text.contains("<<<<<<<"), "{text}");
    let status = ws.json(&["status"]);
    assert!(
        status["conflicts"].as_array().unwrap().is_empty(),
        "{status}"
    );
    assert_eq!(status["dirty"][0]["key"], "PROJ-142", "{status}");
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
    // Two changelog entries, one comment and two worklogs: the log is the
    // issue's history, and Jira's changelog is only part of that.
    assert_eq!(entries.len(), 5);
    assert_eq!(entries[0]["id"], "70002", "newest first");
    assert_eq!(entries[0]["kind"], "change");
    assert_eq!(entries[0]["items"][0]["field"], "priority");
    let changes: Vec<&Value> = entries.iter().filter(|e| e["kind"] == "change").collect();
    assert_eq!(changes.len(), 2);
    assert_eq!(changes[1]["items"][0]["from"], "To Do");

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
    assert_eq!(cached["entries"].as_array().unwrap().len(), 5);
}

/// Colour is on by default, and both ways of turning it off work.
///
/// The two spellings are not interchangeable to whoever is typing them:
/// `--no-color` is what every other tool calls it, and `--color never` is what
/// this one always called it.
#[test]
fn colour_is_on_by_default_and_can_be_turned_off_either_way() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);

    let printed = |args: &[&str], no_color_env: bool| -> String {
        let mut cmd = ws.cmd(args);
        if no_color_env {
            cmd.env("NO_COLOR", "1");
        }
        let out = cmd.output().expect("run");
        assert!(out.status.success(), "`jired {}` failed", args.join(" "));
        String::from_utf8_lossy(&out.stdout).into_owned()
    };

    // `always` because the test's stdout is a pipe, where `auto` correctly
    // stays plain — that is the default doing its job, not colour being off.
    assert!(
        printed(&["log", "--color", "always"], false).contains('\u{1b}'),
        "colour was asked for and not delivered"
    );

    // Asking for colour outright beats the environment saying no: `NO_COLOR`
    // is how someone states a preference once, not a veto over the flag they
    // just typed.
    assert!(
        printed(&["log", "--color", "always"], true).contains('\u{1b}'),
        "an explicit --color always should outrank NO_COLOR"
    );

    for args in [
        vec!["log", "--no-color"],
        vec!["log", "--color", "never"],
        vec!["log"], // a pipe, where auto means plain
    ] {
        let text = printed(&args, false);
        assert!(
            !text.contains('\u{1b}'),
            "`jired {}` should print no escapes:\n{text}",
            args.join(" ")
        );
    }
}

/// Naming both is a mistake worth catching, not a precedence rule to remember.
#[test]
fn the_two_colour_flags_refuse_to_be_used_together() {
    let ws = Ws::new();
    ws.init();
    let out = ws
        .cmd(&["log", "--no-color", "--color", "always"])
        .output()
        .expect("run");
    assert_eq!(out.status.code(), Some(2), "usage error");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("--no-color"), "{stderr}");
}

/// How many, in one request: no issues come back, only the server's total.
#[test]
fn search_count_prints_the_total_alone() {
    let ws = Ws::new();
    ws.init();

    let doc = ws.json(&["search", "project = PROJ", "--count"]);
    assert_eq!(doc["schema"], "jired/search-count/1");
    assert_eq!(doc["total"], 3);
    assert!(doc.get("issues").is_none(), "{doc}");

    let out = ws
        .cmd(&["search", "project = PROJ", "--count"])
        .output()
        .unwrap();
    assert_eq!(String::from_utf8(out.stdout).unwrap(), "3\n");
}

/// Every row carries what the next question needs, so checking a type or a
/// parent is not a second lookup per issue; `--columns` prints any of it the
/// way `list --columns` does, tab-separated into a pipe.
#[test]
fn search_rows_carry_type_parent_sprint_and_versions() {
    let ws = Ws::new();
    ws.init();

    let doc = ws.json(&["search", "project = PROJ"]);
    let issues = doc["issues"].as_array().unwrap();
    let story = issues.iter().find(|i| i["key"] == "PROJ-142").unwrap();
    assert_eq!(story["type"], "Story");
    assert_eq!(story["sprint"], serde_json::json!(["Sprint 42"]));
    assert_eq!(story["fix_versions"], serde_json::json!(["2.4.0"]));
    assert_eq!(story["labels"], serde_json::json!(["auth", "backend"]));
    let sub = issues.iter().find(|i| i["key"] == "PROJ-143").unwrap();
    assert_eq!(sub["parent"], "PROJ-142", "{sub}");

    let out = ws
        .cmd(&[
            "search",
            "project = PROJ",
            "--columns",
            "key,type,parent,sprint",
        ])
        .output()
        .unwrap();
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(text.contains("PROJ-142\tStory\t\tSprint 42\n"), "{text:?}");
    assert!(
        text.contains("PROJ-143\tSub-task\tPROJ-142\t\n"),
        "{text:?}"
    );

    let keys = ws
        .cmd(&["search", "project = PROJ", "--keys"])
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8(keys.stdout).unwrap(),
        "PROJ-142\nPROJ-143\nPROJ-150\n"
    );

    // Fewer than were asked for is said, with the total the server reported.
    let two = ws.json(&["search", "project = PROJ", "--limit", "2"]);
    assert_eq!(two["issues"].as_array().unwrap().len(), 2);
    assert_eq!(two["total"], 3);
}

/// A status is coloured by the category the server files it under, so an
/// instance that renamed its workflow still reads correctly.
#[test]
fn a_status_is_coloured_by_its_category() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);

    let out = ws
        .cmd(&["search", "--jql", "project = PROJ", "--color", "always"])
        .output()
        .expect("run");
    let text = String::from_utf8_lossy(&out.stdout);
    // In Progress is `indeterminate` (yellow); To Do is `new` (dim).
    assert!(text.contains("\u{1b}[33mIn Progress"), "{text}");
    assert!(text.contains("\u{1b}[2mTo Do"), "{text}");
}

/// What jired prints follows the reader's clock; what it records does not.
///
/// PROJ-142's newest changelog entry is 2026-08-16T12:03:00Z. Read from Tokyo
/// that is the 16th at 21:03, and from New York the 16th at 08:03 — but the
/// JSON says the same thing from either chair, because agents and issue files
/// need one instant rather than one per machine.
#[test]
fn printed_times_follow_the_readers_timezone_and_json_does_not() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);

    let printed = |zone: &str| -> String {
        let out = ws
            .cmd(&["log", "PROJ-142"])
            .env("TZ", zone)
            .output()
            .expect("run");
        assert!(out.status.success(), "`jired log` failed in {zone}");
        String::from_utf8_lossy(&out.stdout).into_owned()
    };

    // The fixture's history is months old, so every heading carries its date;
    // the year is this one, which the reader knows, so it is not printed. Ilya
    // touched the issue twice that day, so the heading spans both — one date,
    // two clock readings, and it is the readings that move with the chair.
    assert!(printed("Asia/Tokyo").contains("08-16 18:05–21:03"));
    assert!(printed("America/New_York").contains("08-16 05:05–08:03"));
    assert!(printed("UTC").contains("08-16 09:05–12:03"));
    // The zone marker goes with the zone: what is left is the reader's clock.
    assert!(!printed("Asia/Tokyo").contains('Z'));

    for zone in ["Asia/Tokyo", "America/New_York", "UTC"] {
        let out = ws
            .cmd(&["log", "PROJ-142", "--json"])
            .env("TZ", zone)
            .output()
            .expect("run");
        let doc: Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(
            doc["entries"][0]["created"], "2026-08-16T12:03:00Z",
            "the record must not move with whoever reads it ({zone})"
        );
    }
}

/// A start time with no zone means the one the reader was just shown; one that
/// names its zone is kept in the words it was written in. Either way what
/// reaches Jira is the same instant.
#[test]
fn a_worklog_start_time_is_read_in_the_readers_timezone() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);

    let booked = ws
        .cmd(&[
            "logwork",
            "PROJ-142",
            "2h",
            "--started",
            "2026-08-21 09:00",
            "--json",
        ])
        .env("TZ", "Asia/Tokyo")
        .output()
        .expect("run");
    let doc: Value = serde_json::from_slice(&booked.stdout).unwrap();
    assert_eq!(
        doc["started"], "2026-08-21T09:00:00+09:00",
        "read in the reader's zone, and written down with it spelled out"
    );

    // 09:00 in Tokyo is midnight UTC, which is what the server is told.
    let push = ws.json(&["push", "--dry-run"]);
    let sent = push.to_string();
    assert!(
        sent.contains("2026-08-21T00:00:00.000+0000"),
        "the worklog should reach Jira as one instant: {sent}"
    );
}

/// With no issue named, `log` reads like `git log`: one merged list over every
/// issue in the workspace, each entry saying which issue it belongs to.
#[test]
fn log_with_no_issue_covers_the_whole_workspace() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);

    let report = ws.json(&["log"]);
    assert_eq!(report["scope"], "workspace");
    assert!(report["key"].is_null(), "no single issue was asked for");

    let entries = report["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 5);
    assert_eq!(entries[0]["key"], "PROJ-142");
    assert_eq!(entries[0]["id"], "70002", "newest first");
    assert_eq!(entries[0]["summary"], "Fix login flow", "what the key is");
    assert_eq!(entries.last().unwrap()["id"], "70001", "oldest last");

    // The kinds are told apart, and each carries what it is: a change has the
    // fields it touched, a comment and a worklog have their text.
    let kinds: Vec<&str> = entries
        .iter()
        .map(|e| e["kind"].as_str().unwrap())
        .collect();
    assert_eq!(kinds, ["change", "worklog", "worklog", "comment", "change"]);
    let comment = entries.iter().find(|e| e["kind"] == "comment").unwrap();
    assert_eq!(comment["text"], "Repro confirmed on staging.");
    assert!(comment["items"].as_array().unwrap().is_empty());
    let worklog = entries.iter().find(|e| e["kind"] == "worklog").unwrap();
    assert!(worklog["time_spent"].is_string());

    // The narrow form still works, and still says which issue it read.
    let one = ws.json(&["log", "PROJ-142"]);
    assert_eq!(one["scope"], "issue");
    assert_eq!(one["key"], "PROJ-142");
    assert_eq!(one["entries"][0]["key"], "PROJ-142");
}

/// PROJ-143 and PROJ-150 have no history at all. An empty changelog is a real
/// answer, so the second run must not go back to the server to hear it again.
#[test]
fn log_over_the_workspace_is_offline_after_the_first_run() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);
    ws.json(&["log"]);

    let out = ws
        .cmd(&["log", "--json"])
        .env("JIRED_FIXTURES", "")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "a second workspace log must not need the network: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let cached: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(cached["entries"].as_array().unwrap().len(), 5);
}

/// The field report: a sprint history meant `jq '[.. | objects |
/// select(.field? == "Sprint")]'` over the timeline, which kept each change
/// and dropped the date it was made on. `--field` is the history of one field,
/// flat, each row carrying its issue, time and author.
#[test]
fn log_field_is_a_flat_history_of_the_fields_named() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);

    let sprint = ws.json(&["log", "PROJ-142", "--field", "sprint"]);
    assert_eq!(sprint["schema"], "jired/log-field/1");
    assert_eq!(sprint["scope"], "issue");
    assert_eq!(sprint["total"], 1);
    let change = &sprint["changes"][0];
    assert_eq!(change["key"], "PROJ-142");
    assert_eq!(change["field"], "Sprint", "the changelog's own spelling");
    assert_eq!(change["from"], "Sprint 11");
    assert_eq!(change["to"], Value::Null);
    assert_eq!(change["author"], "Jane D");
    assert_eq!(change["created"], "2026-08-14T09:00:00Z");

    // Several fields, newest first; reversed on request.
    let two = ws.json(&["log", "--field", "STATUS,priority"]);
    assert_eq!(two["scope"], "workspace");
    let fields: Vec<&str> = two["changes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["field"].as_str().unwrap())
        .collect();
    assert_eq!(fields, ["priority", "status"]);
    let reversed = ws.json(&[
        "log",
        "--field",
        "status",
        "--field",
        "priority",
        "--reverse",
    ]);
    assert_eq!(reversed["changes"][0]["field"], "status");

    // One line per change on a terminal or off one.
    let out = ws
        .cmd(&["log", "PROJ-142", "--field", "status", "--color", "never"])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    let line = text.lines().find(|l| l.contains("status:")).expect("a row");
    assert!(line.starts_with("PROJ-142"), "{text}");
    assert!(line.contains("Jane D"), "{text}");
    assert!(line.contains("status: To Do → In Progress"), "{text}");

    // A field nobody changed is an answer, not an error.
    let none = ws.json(&["log", "PROJ-142", "--field", "Fix Version"]);
    assert_eq!(none["total"], 0);
}

#[test]
fn log_honours_the_limit() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);
    let report = ws.json(&["log", "PROJ-142", "--limit", "1"]);
    assert_eq!(report["entries"].as_array().unwrap().len(), 1);
}

/// `--limit 1d` is a window, not a count: the fixture's history was recorded in
/// August 2026 and falls outside a day of any run, and inside a wide enough
/// span. The entries themselves are the same ones the count form shows.
#[test]
fn log_takes_a_span_as_a_limit() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);

    let recent = ws.json(&["log", "PROJ-142", "--limit", "1d"]);
    assert_eq!(recent["scope"], "issue");
    assert!(
        recent["entries"].as_array().unwrap().is_empty(),
        "nothing was edited in the last day"
    );

    let ever = ws.json(&["log", "PROJ-142", "--limit", "999w"]);
    let entries = ever["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 5);
    assert_eq!(entries[0]["id"], "70002", "newest first, as ever");

    // History exists, it is just older than the window — the difference is
    // said out loud rather than read as an empty changelog.
    let out = ws
        .cmd(&["log", "PROJ-142", "--limit", "1d"])
        .output()
        .unwrap();
    let printed = String::from_utf8_lossy(&out.stdout);
    assert!(
        printed.contains("nothing happened in the last 1d"),
        "expected the window in the message, got: {printed}"
    );
}

/// A description change carries the whole old and new text on both sides. Put
/// through the `from → to` row it is a page of prose with an arrow somewhere in
/// it; as a diff it is the line that moved, with the paragraph around it shown
/// once. Short changes keep the row, which is what the row is good at.
#[test]
fn log_diffs_a_text_field_and_leaves_short_changes_alone() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);

    let out = ws.cmd(&["log", "PROJ-142"]).output().unwrap();
    let printed = String::from_utf8_lossy(&out.stdout);

    assert!(
        printed.contains("-2. Log out") && printed.contains("+2. Wait five minutes"),
        "the edited line should be the only one signed:\n{printed}"
    );
    assert!(
        printed.contains(" 3. Log in again"),
        "the line after it is context:\n{printed}"
    );
    assert!(
        printed.contains("description") && printed.contains("+1") && printed.contains("-1"),
        "the field is headed with what it gained and lost:\n{printed}"
    );
    assert!(
        !printed.contains("1. Log in\n2. Log out\n3. Log in again → "),
        "the arrow row must not be used for prose:\n{printed}"
    );
    // The short changes in the same entry are untouched.
    assert!(
        printed.contains("Medium → High") && printed.contains("To Do → In Progress"),
        "a status or a priority still reads as one line:\n{printed}"
    );

    // The JSON is the record: it keeps both texts whole, diffed by nobody.
    let report = ws.json(&["log", "PROJ-142"]);
    let items = report["entries"][0]["items"].as_array().unwrap();
    let description = items.iter().find(|i| i["field"] == "description").unwrap();
    assert!(description["from"]
        .as_str()
        .unwrap()
        .contains("2. Log out\n3. Log in again"));
}

/// Each row says when, which issue, what the issue is and who did it; a field
/// that gained or lost a value says so with a sign rather than with `(none)`.
#[test]
fn log_rows_carry_the_issue_and_sign_one_sided_changes() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);

    let out = ws.cmd(&["log"]).output().unwrap();
    let printed = String::from_utf8_lossy(&out.stdout);

    assert!(
        printed.contains("PROJ-142  Fix login flow"),
        "the key alone does not say what the issue is:\n{printed}"
    );
    assert!(
        printed.contains("(Ilya A)") && printed.contains("(Jane D)"),
        "who changed it is what a log is for:\n{printed}"
    );
    assert!(
        printed.contains("+labels: auth backend"),
        "a value where there was none is a `+`:\n{printed}"
    );
    assert!(
        printed.contains("-Sprint: Sprint 11"),
        "a value taken away is a `-`:\n{printed}"
    );
    assert!(
        !printed.contains("(none)"),
        "which is the point of the signs:\n{printed}"
    );
}

/// Jira's changelog records field changes and nothing else. A comment posted
/// and time booked are as much of the issue's history, and jired already has
/// both in the snapshot, so they belong on the same timeline.
#[test]
fn log_merges_comments_and_worklogs_into_the_timeline() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);

    let out = ws.cmd(&["log", "PROJ-142"]).output().unwrap();
    let printed = String::from_utf8_lossy(&out.stdout);

    assert!(
        printed.contains("+comment: Repro confirmed on staging."),
        "a comment is part of what happened:\n{printed}"
    );
    assert!(
        printed.contains("+worklog: 2h 30m — Chased the redirect loop."),
        "so is time booked, with the note it was booked with:\n{printed}"
    );
    // Ordered against the field changes rather than listed after them.
    let comment = printed.find("+comment:").expect("a comment");
    let status = printed.find("status: To Do").expect("the status change");
    let priority = printed
        .find("priority: Medium")
        .expect("the priority change");
    assert!(
        priority < comment && comment < status,
        "one timeline, newest first:\n{printed}"
    );
}

/// A blank line means "the next block starts here" and nothing else, so a block
/// of six rows is as easy to see as a block of one — and a description with a
/// blank paragraph in it does not split its own block in two.
#[test]
fn log_entries_are_separated_by_a_blank_line_and_nothing_else_is() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);

    let out = ws.cmd(&["log"]).output().unwrap();
    let printed = String::from_utf8_lossy(&out.stdout);
    let lines: Vec<&str> = printed.lines().collect();

    // The fixture's five events are two people's, so two blocks and one gap —
    // and the description diff carries a blank line of its own, which must not
    // read as a second.
    assert_eq!(
        lines.iter().filter(|l| l.trim().is_empty()).count(),
        1,
        "expected one blank line per block boundary:\n{printed}"
    );
    for (i, line) in lines.iter().enumerate() {
        if line.trim().is_empty() {
            assert!(
                lines[i + 1].starts_with(char::is_numeric),
                "a blank line must be followed by a block, got: {}",
                lines[i + 1]
            );
        }
    }
    assert!(
        printed.contains("    │") && printed.contains("│ -2. Log out"),
        "quoted text is marked as quoted:\n{printed}"
    );
}

/// Jira records a transition, the comment explaining it and the time booked
/// against it as three separate things. Printing three identical headings makes
/// one person's afternoon look like three, so neighbouring events by the same
/// person on the same issue share a heading — and the heading says the span
/// they covered rather than picking the newest and calling it the moment.
#[test]
fn log_gives_one_hand_on_one_issue_a_single_heading() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);

    let out = ws
        .cmd(&["log", "PROJ-142"])
        .env("TZ", "UTC")
        .output()
        .unwrap();
    let printed = String::from_utf8_lossy(&out.stdout);

    // Ilya's change and worklog are hours apart on one day, so the date is
    // printed once and the clock twice; the time column pads the shorter
    // heading out to the longer, which is why the row is matched in pieces.
    assert!(
        printed.contains("08-16 09:05–12:03")
            && printed.contains("PROJ-142  Fix login flow  (Ilya A)"),
        "one heading over the pair, spanning both:\n{printed}"
    );
    // Jane's three run over two days, and both ends carry their date.
    assert!(
        printed.contains("08-14 09:00–08-15 11:30  PROJ-142  Fix login flow  (Jane D)"),
        "a block that is not one afternoon must not read as one:\n{printed}"
    );
    assert_eq!(
        printed.matches("PROJ-142").count(),
        2,
        "five events, two hands, two headings:\n{printed}"
    );
    // The events themselves are all still there, in the order they happened.
    let order = [
        "priority: Medium",
        "+worklog: 1h",
        "+comment:",
        "status: To Do",
    ];
    let mut cursor = 0;
    for row in order {
        let found = printed[cursor..]
            .find(row)
            .unwrap_or_else(|| panic!("{row} missing after byte {cursor}:\n{printed}"));
        cursor += found;
    }

    // Grouping is a rendering; the record stays one document per event.
    let report = ws.json(&["log", "PROJ-142"]);
    assert_eq!(report["entries"].as_array().unwrap().len(), 5);
}

/// `--links` into a pipe cannot rely on an escape sequence nobody will render,
/// so it prints the URL. On a terminal the key itself carries it.
#[test]
fn log_links_print_a_url_when_nothing_can_click_them() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);

    let plain = String::from_utf8_lossy(&ws.cmd(&["log"]).output().unwrap().stdout).into_owned();
    assert!(!plain.contains("browse/PROJ-142"), "not without --links");

    let linked =
        String::from_utf8_lossy(&ws.cmd(&["log", "--links"]).output().unwrap().stdout).into_owned();
    assert!(
        linked.contains("https://fixture.invalid/browse/PROJ-142"),
        "expected the URL in a piped log, got:\n{linked}"
    );
    assert!(
        !linked.contains("\x1b]8;;"),
        "and no hyperlink escapes into a pipe:\n{linked:?}"
    );
}

#[test]
fn log_refuses_a_limit_that_is_neither_a_count_nor_a_span() {
    let ws = Ws::new();
    ws.init();
    let out = ws.cmd(&["log", "--limit", "1x"]).output().unwrap();
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("neither a count nor a span"),
        "the error should say what a limit may look like"
    );
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
// open [M3.7]
// ---------------------------------------------------------------------------

/// The URL is built from the stored server and the key, however the key was
/// spelled; nothing is launched when the answer is going into a pipeline.
#[test]
fn open_prints_the_browse_url_for_any_spelling_of_the_key() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);

    for spelling in ["PROJ-142", "proj-142", ISSUE_FILE] {
        let doc = ws.json(&["open", spelling, "--print"]);
        assert_eq!(doc["key"], "PROJ-142");
        assert_eq!(doc["url"], "https://fixture.invalid/browse/PROJ-142");
        assert_eq!(doc["launched"], false, "{doc}");
    }
}

/// With no key the command searches, which needs a terminal to draw the list
/// on. Without one it says what to type instead of half-working.
#[test]
fn open_with_nothing_named_and_no_terminal_says_to_name_one() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);

    let err = ws.json_err(&["open"], 7);
    assert_eq!(err["kind"], "missing_value");
    assert!(err["hint"].as_str().unwrap().contains("PROJ-142"), "{err}");
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
/// the issue somewhere nobody asked for. Refused at plan time, exit 2: a
/// warning beside exit 0 was read as the move having happened.
#[test]
fn a_sprint_the_workspace_does_not_know_is_refused_rather_than_guessed() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);

    let text = ws
        .read(ISSUE_FILE)
        .replace("sprint: Sprint 42", "sprint: Sprint 99");
    ws.write(ISSUE_FILE, &text);

    let report = ws.json_code(&["push"], 2);
    let result = &report["results"][0];
    assert_eq!(result["ok"], false, "{report}");
    assert!(
        result["error"].as_str().unwrap().contains("Sprint 99"),
        "{report}"
    );
    assert!(
        result["ops"].as_array().unwrap().is_empty(),
        "nothing was sent"
    );
    assert!(ws.read(ISSUE_FILE).contains("sprint: Sprint 99"));
}

/// A sub-task is not a board item: it takes its sprint from its parent, and the
/// board call for one is accepted and ignored. Reported as a `push` that failed
/// with the edit still in the file, because the alternative — what actually
/// happened — was `"status": "done"`, exit 0, and the line quietly rewritten
/// back to the parent's sprint on the way out.
#[test]
fn a_sub_task_cannot_be_moved_into_a_sprint_of_its_own() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);

    const SUB_TASK: &str = "PROJ-0143. Add integration test.md";
    let text = ws.read(SUB_TASK);
    assert!(!text.contains("sprint:"), "{text}");
    ws.write(
        SUB_TASK,
        &text.replace("type: Sub-task", "type: Sub-task\nsprint: Sprint 43"),
    );

    // The dry run is where this has to show: a clean green plan is what made
    // the push worth trusting.
    for args in [&["push", "--dry-run"][..], &["push", "--yes"][..]] {
        let report = ws.json_code(args, 2);
        let result = &report["results"][0];
        assert_eq!(result["key"], "PROJ-143", "{report}");
        assert_eq!(result["ok"], false, "{report}");
        assert!(
            result["error"].as_str().unwrap().contains("its parent"),
            "{report}"
        );
        assert!(
            result["hint"].as_str().unwrap().contains("PROJ-142"),
            "the hint names the issue to move instead: {report}"
        );
        assert!(result["ops"].as_array().unwrap().is_empty(), "{report}");
    }

    // Nothing was sent and nothing was overwritten: the edit is where the user
    // left it, and `status` still says so.
    assert!(ws.read(SUB_TASK).contains("sprint: Sprint 43"));
    assert_eq!(ws.json(&["status"])["dirty"][0]["key"], "PROJ-143");
}

const DC_SUB_TASK: &str = "DC-0079. Update the connector config.md";

/// Data Center re-parents a sub-task through the Move wizard, which is a web
/// action rather than a REST endpoint — `PUT /issue` takes `parent`, answers
/// success and leaves it where it was. The plan has to say which of the two it
/// is sending, and the push has to send it.
///
/// Basic auth throughout: the wizard is a Jira *page*, and only a credential
/// Seraph authenticates pages with reaches one.
#[test]
fn a_sub_task_moves_to_another_parent_on_data_center() {
    let ws = Ws::of(Deployment::DataCenter).with_auth("basic");
    ws.init();
    ws.json(&["pull"]);

    let text = ws.read(DC_SUB_TASK);
    assert!(text.contains("parent: DC-77"), "{text}");
    ws.write(DC_SUB_TASK, &text.replace("parent: DC-77", "parent: DC-78"));

    // The dry run names the wizard, not `PUT /issue`: the whole point is that
    // the call jired would otherwise make is the one that does nothing.
    let planned = ws.json(&["push", "--dry-run"]);
    let op = &planned["results"][0]["ops"][0];
    assert_eq!(op["kind"], "move_parent", "{planned}");
    assert_eq!(op["summary"], "DC-77 → DC-78", "{planned}");
    assert_eq!(
        op["request"]["path"], "/secure/MoveSubTaskParent.jspa",
        "{planned}"
    );

    let journal = ws.path().join("calls.jsonl");
    let out = ws
        .cmd(&["push", "--yes"])
        .arg("--json")
        .env("JIRED_FIXTURE_JOURNAL", &journal)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    // The parent left the field payload: sending it there is the silence this
    // whole path exists to avoid.
    let calls: Vec<Value> = std::fs::read_to_string(&journal)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert!(
        calls
            .iter()
            .any(|c| c["path"] == "/secure/MoveSubTaskParent.jspa"
                && c["body"]["parentIssue"] == "DC-78"),
        "the wizard was driven: {calls:?}"
    );
    assert!(
        !calls
            .iter()
            .any(|c| c["body"]["fields"]["parent"].is_object()),
        "no PUT carried the parent: {calls:?}"
    );

    // And the file now reads back from the server, which is what a push that
    // worked looks like.
    assert!(ws.read(DC_SUB_TASK).contains("parent: DC-78"));
    assert!(ws.json(&["status"])["dirty"].as_array().unwrap().is_empty());
}

/// Jira does not touch a parent when a sub-task is created under it, so the
/// parent is never part of the next incremental fetch, and `## Children` stayed
/// empty however many pulls followed. The push knows where each new sub-task
/// went, and the parent's file says so before the command returns.
#[test]
fn a_pushed_sub_task_is_listed_under_its_parent_at_once() {
    for_each_deployment(|ws| {
        let parent = ws.issue_file();
        let before = ws.read(parent);
        ws.json(&[
            "create",
            "--type",
            "Sub-task",
            "--parent",
            ws.deployment.key(),
            "-s",
            "Write the migration note",
        ]);
        let push = ws.json(&["push"]);
        let new_key = push["results"][0]["new_key"].as_str().expect("created");

        let after = ws.read(parent);
        assert!(
            after.contains(&format!("- [ ] {new_key} Write the migration note")),
            "{:?}: {after}",
            ws.deployment
        );
        // What was listed before is still listed, in the same place.
        let listed = |text: &str| -> Vec<String> {
            text.split("## Children")
                .nth(1)
                .and_then(|rest| rest.split("\n## ").next())
                .unwrap_or_default()
                .lines()
                .filter(|l| l.starts_with("- ["))
                .map(str::to_string)
                .collect()
        };
        assert_eq!(listed(&after)[..listed(&before).len()], listed(&before)[..]);

        // Rewritten as a sync, not as an edit: nothing to push, nothing to
        // pull, and the next pull leaves the file alone.
        let status = ws.json(&["status"]);
        assert!(status["dirty"].as_array().unwrap().is_empty(), "{status}");
        assert!(status["behind"].as_array().unwrap().is_empty(), "{status}");
        let pulled = ws.json(&["pull"]);
        assert!(pulled["updated"].as_array().unwrap().is_empty(), "{pulled}");
        assert_eq!(ws.read(parent), after);
    });
}

/// A parent with unpushed edits of its own is merged into, not overwritten,
/// when a sub-task pushed on its own lands under it.
#[test]
fn a_pushed_sub_task_merges_into_a_parent_with_local_edits() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);

    let text = ws
        .read(ISSUE_FILE)
        .replace("- backend\n", "- backend\n- local-only\n");
    ws.write(ISSUE_FILE, &text);
    let created = ws.json(&[
        "create",
        "--type",
        "Sub-task",
        "--parent",
        "PROJ-142",
        "-s",
        "Write the migration note",
    ]);
    let temp_key = created["key"].as_str().unwrap();
    let push = ws.json(&["push", temp_key]);
    let new_key = push["results"][0]["new_key"].as_str().expect("created");

    let after = ws.read(ISSUE_FILE);
    assert!(after.contains("- local-only"), "the local edit survived");
    assert!(after.contains(&format!("{new_key} Write the migration note")));
    assert_eq!(ws.json(&["status"])["dirty"][0]["key"], "PROJ-142");
}

/// The same staleness from the other side: a sub-task created or moved in the
/// web UI arrives in the next fetch on its own, and its parent — old and new —
/// has to follow it in that one pull.
#[test]
fn a_sub_task_created_or_moved_on_the_server_reaches_its_parent_in_one_pull() {
    let ws = Ws::of(Deployment::DataCenter);
    ws.init();
    ws.json(&["pull"]);
    const DC_ROLLBACK: &str = "DC-0078. Document the LDAP rollback plan.md";

    let path = ws.fixtures.join("issues.json");
    let mut issues: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let mut created = issues
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["key"] == "DC-79")
        .unwrap()
        .clone();
    created["id"] = "20504".into();
    created["key"] = "DC-80".into();
    created["fields"]["summary"] = "Rehearse the rollback".into();
    created["fields"]["parent"] = serde_json::json!({"key": "DC-78"});
    created["fields"]["updated"] = jired::store::now_rfc3339().into();
    issues.as_array_mut().unwrap().push(created);
    std::fs::write(&path, serde_json::to_string_pretty(&issues).unwrap()).unwrap();

    let report = ws.json(&["pull"]);
    let updated: Vec<&str> = report["updated"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["key"].as_str().unwrap())
        .collect();
    assert_eq!(updated, ["DC-78"], "{report}");
    assert!(ws
        .read(DC_ROLLBACK)
        .contains("- [ ] DC-80 Rehearse the rollback"));

    ws.mutate_remote("DC-79", |issue| {
        issue["fields"]["parent"] = serde_json::json!({"key": "DC-78"});
    });
    ws.json(&["pull"]);
    let rollback = ws.read(DC_ROLLBACK);
    assert!(
        rollback.contains("- [ ] DC-79 Update the connector config")
            && rollback.contains("- [ ] DC-80 Rehearse the rollback"),
        "{rollback}"
    );
    assert!(
        !ws.read(Deployment::DataCenter.issue_file())
            .contains("DC-79 "),
        "and gone from the old parent"
    );

    let status = ws.json(&["status"]);
    assert!(status["dirty"].as_array().unwrap().is_empty(), "{status}");
    assert!(status["behind"].as_array().unwrap().is_empty(), "{status}");
}

/// The wizard answers in HTML, so a step that renders is not a move that
/// happened. The push re-reads the issue, and a parent that did not move fails
/// the operation with the edit still in the file — the same protection the
/// swallowed field update gets.
#[test]
fn a_re_parenting_the_server_does_not_make_fails_the_push() {
    let ws = Ws::of(Deployment::DataCenter).with_auth("basic");
    ws.write_fixture(
        "ignored.json",
        serde_json::json!(["POST /secure/MoveSubTaskParent.jspa"]),
    );
    ws.init();
    ws.json(&["pull"]);

    let text = ws.read(DC_SUB_TASK);
    ws.write(DC_SUB_TASK, &text.replace("parent: DC-77", "parent: DC-78"));

    let report = ws.json_code(&["push", "--yes"], 10);
    let result = &report["results"][0];
    assert_eq!(result["ok"], false, "{report}");
    assert!(
        result["error"]
            .as_str()
            .unwrap()
            .contains("the parent is still DC-77"),
        "{report}"
    );

    assert!(ws.read(DC_SUB_TASK).contains("parent: DC-78"));
    assert_eq!(ws.json(&["status"])["dirty"][0]["key"], "DC-79");
}

/// The two moves Jira has no wizard page for. Refused while the edit is on
/// screen, rather than sent and answered with a page nobody can read.
#[test]
fn a_sub_task_cannot_lose_its_parent_or_change_project() {
    let ws = Ws::of(Deployment::DataCenter).with_auth("basic");
    ws.init();
    ws.json(&["pull"]);
    let original = ws.read(DC_SUB_TASK);

    ws.write(DC_SUB_TASK, &original.replace("parent: DC-77\n", ""));
    let report = ws.json_code(&["push", "--dry-run"], 2);
    assert!(
        report["results"][0]["error"]
            .as_str()
            .unwrap()
            .contains("without a parent"),
        "{report}"
    );

    ws.write(
        DC_SUB_TASK,
        &original.replace("parent: DC-77", "parent: OPS-12"),
    );
    let report = ws.json_code(&["push", "--dry-run"], 2);
    assert!(
        report["results"][0]["error"]
            .as_str()
            .unwrap()
            .contains("another project"),
        "{report}"
    );

    // Nothing was sent and nothing was rewritten, either time.
    assert!(ws.read(DC_SUB_TASK).contains("parent: OPS-12"));
}

/// `jired parent` is the same edit without opening the file, and stages like
/// every other write command.
#[test]
fn parent_stages_a_move_and_pushes_it() {
    let ws = Ws::of(Deployment::DataCenter).with_auth("basic");
    ws.init();
    ws.json(&["pull"]);

    let staged = ws.json(&["parent", "DC-79", "--to", "DC-78"]);
    assert_eq!(staged["schema"], "jired/parent/1", "{staged}");
    assert_eq!(staged["parent"], "DC-78", "{staged}");
    assert_eq!(staged["issues"][0]["key"], "DC-79", "{staged}");
    assert_eq!(staged["issues"][0]["changed"], true, "{staged}");
    assert_eq!(staged["pushed"], false, "{staged}");
    assert!(ws.read(DC_SUB_TASK).contains("parent: DC-78"));
    assert_eq!(ws.json(&["status"])["dirty"][0]["key"], "DC-79");

    // Staging the move it is already staged for changes nothing rather than
    // giving `push` an operation that does nothing.
    let again = ws.json(&["parent", "DC-79", "--to", "DC-78"]);
    assert_eq!(again["issues"][0]["changed"], false, "{again}");
    ws.json(&["push", "--yes"]);

    // And back again, sent in the one command this time.
    let pushed = ws.json(&["parent", "DC-79", "--to", "DC-77", "--push"]);
    assert_eq!(pushed["push"]["summary"]["ok"], 1, "{pushed}");
    assert_eq!(
        pushed["push"]["results"][0]["ops"][0]["kind"], "move_parent",
        "{pushed}"
    );
    assert!(ws.read(DC_SUB_TASK).contains("parent: DC-77"));
}

/// An issue cannot hang off itself, and a mistyped `--to` is what that usually
/// is. Caught before the file is written rather than by the server.
#[test]
fn parent_refuses_an_issue_as_its_own_parent() {
    let ws = Ws::of(Deployment::DataCenter).with_auth("basic");
    ws.init();
    ws.json(&["pull"]);

    let out = ws
        .cmd(&["parent", "DC-79", "--to", "DC-79"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(ws.read(DC_SUB_TASK).contains("parent: DC-77"));
}

/// The Move wizard is a Jira page, and a personal access token authenticates
/// the REST API and nothing else: the request is bounced to the session-timeout
/// page, which Jira serves as HTTP 200. 0.4.0 planned the move anyway and found
/// out afterwards. The credential is known before anything is sent, so the
/// refusal belongs in the plan — `--dry-run` has to show it, which is what
/// makes a green plan worth trusting.
#[test]
fn a_move_needing_a_web_page_is_refused_under_a_token_that_cannot_open_one() {
    let ws = Ws::of(Deployment::DataCenter); // a PAT, which is init's default here
    ws.init();
    ws.json(&["pull"]);

    let text = ws.read(DC_SUB_TASK);
    ws.write(DC_SUB_TASK, &text.replace("parent: DC-77", "parent: DC-78"));

    for args in [&["push", "--dry-run"][..], &["push", "--yes"][..]] {
        let report = ws.json_code(args, 2);
        let result = &report["results"][0];
        assert_eq!(result["ok"], false, "{report}");
        assert!(
            result["error"].as_str().unwrap().contains("web page"),
            "{report}"
        );
        assert!(
            result["hint"].as_str().unwrap().contains("--auth basic"),
            "the way out is named: {report}"
        );
        // Nothing was planned, so nothing was sent — least of all the `PUT`
        // that Jira would have accepted and ignored.
        assert!(result["ops"].as_array().unwrap().is_empty(), "{report}");
    }

    assert!(ws.read(DC_SUB_TASK).contains("parent: DC-78"));
    assert_eq!(ws.json(&["status"])["dirty"][0]["key"], "DC-79");
}

/// And `jired parent` says it while staging, rather than letting the push be
/// where it comes out. A warning, not a refusal: the line is what the file
/// should say once the sub-task is moved in Jira.
#[test]
fn parent_warns_when_the_credential_cannot_reach_the_wizard() {
    let ws = Ws::of(Deployment::DataCenter);
    ws.init();
    ws.json(&["pull"]);

    let out = ws
        .cmd(&["parent", "DC-79", "--to", "DC-78"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("DC-79"), "{stderr}");
    assert!(stderr.contains("web page"), "{stderr}");
    // The way out goes where every other hint goes.
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("--auth basic"), "{stdout}");
    assert!(ws.read(DC_SUB_TASK).contains("parent: DC-78"));

    // A standard issue's parent is an ordinary field update, which the same
    // token sends: warning about that one would only teach people to ignore it.
    let out = ws
        .cmd(&["parent", "DC-78", "--to", "DC-77"])
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(
        !String::from_utf8_lossy(&out.stderr).contains("web page"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A 2xx is not evidence. The reported case in one push: a `parent` and a sprint
/// the server accepted, applied neither of, and reported `"status": "done"` for
/// — after which the file was rewritten from the server and both edits were
/// gone, under exit 0.
#[test]
fn a_change_the_server_accepts_and_ignores_fails_the_push() {
    let ws = Ws::new();
    ws.write_fixture(
        "ignored.json",
        serde_json::json!([
            "PUT /issue/PROJ-142",
            "POST /rest/agile/1.0/sprint/43/issue",
        ]),
    );
    ws.init();
    ws.json(&["pull"]);

    let text = ws
        .read(ISSUE_FILE)
        .replace("sprint: Sprint 42", "sprint: Sprint 43")
        .replace("type: Story", "type: Story\nparent: PROJ-150");
    ws.write(ISSUE_FILE, &text);

    let report = ws.json_code(&["push", "--yes"], 10);
    assert_eq!(report["summary"]["ok"], 0, "{report}");
    assert_eq!(report["summary"]["failed"], 1, "{report}");

    let result = &report["results"][0];
    assert_eq!(result["ok"], false, "{report}");
    for op in result["ops"].as_array().unwrap() {
        assert_eq!(op["status"], "failed", "{report}");
        assert!(op["error"].is_string(), "{report}");
        // What was sent, which a real push used to drop on the floor.
        assert!(op["request"]["path"].is_string(), "{report}");
    }
    let errors = result["ops"].to_string();
    assert!(errors.contains("the parent is still unset"), "{errors}");
    assert!(errors.contains("still in Sprint 42"), "{errors}");

    // The point of all of it: the edit survived and base did not advance.
    let after = ws.read(ISSUE_FILE);
    assert!(after.contains("sprint: Sprint 43"), "{after}");
    assert!(after.contains("parent: PROJ-150"), "{after}");
    assert_eq!(ws.json(&["status"])["dirty"][0]["key"], "PROJ-142");
}

/// The same silence can meet a follow-up to a create, and the create cannot be
/// taken back — but the edit can still be kept. Rebuilding the file from the
/// server alone is how a checklist the plugin refused came back empty with a
/// success to show for it: base advanced over the edit and `status` called the
/// issue clean. What did not happen stays in the file, the issue is not `ok`,
/// and the op waits in the journal like a failed update's.
#[test]
fn a_swallowed_follow_up_to_a_create_fails_the_issue_and_keeps_the_edit() {
    let ws = Ws::new();
    ws.write_fixture(
        "ignored.json",
        serde_json::json!(["POST /rest/agile/1.0/sprint/43/issue"]),
    );
    ws.init();
    // The board move needs the sprint cache to resolve the name to an id.
    ws.json(&["pull"]);
    ws.json(&[
        "create",
        "--summary",
        "Audit the SSO config",
        "--sprint",
        "Sprint 43",
    ]);

    let report = ws.json_code(&["push", "--yes"], 10);
    assert_eq!(report["summary"]["ok"], 0, "{report}");
    assert_eq!(report["summary"]["failed"], 1, "{report}");
    let result = &report["results"][0];
    assert_eq!(result["ok"], false, "{report}");
    assert_eq!(result["error_kind"], "unsupported_server", "{report}");
    let new_key = result["new_key"].as_str().expect("the issue was created");
    assert!(
        result["error"].as_str().unwrap().contains(new_key),
        "{report}"
    );
    let ops = result["ops"].as_array().unwrap();
    assert_eq!(ops[0]["kind"], "create_issue");
    assert_eq!(ops[0]["status"], "done", "{report}");
    assert_eq!(ops[1]["kind"], "move_sprint");
    assert_eq!(ops[1]["status"], "failed", "{report}");
    assert!(
        ops[1]["error"].as_str().unwrap().contains("Sprint 43"),
        "{report}"
    );

    // Renamed to its real key, with the sprint it was not moved into still
    // written in it — an edit to an issue that exists, which `status` sees.
    let number = new_key.rsplit('-').next().unwrap();
    let file = format!("PROJ-{number:0>4}. Audit the SSO config.md");
    assert!(
        ws.read(&file).contains("sprint: Sprint 43"),
        "{}",
        ws.read(&file)
    );
    let status = ws.json(&["status"]);
    let dirty = status["dirty"]
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["key"] == new_key)
        .unwrap_or_else(|| panic!("{new_key} is not dirty: {status}"));
    assert!(
        dirty["fields"]
            .as_array()
            .unwrap()
            .contains(&Value::from("sprint")),
        "{status}"
    );
    let pending = status["pending_ops"].as_array().unwrap();
    assert!(
        pending
            .iter()
            .any(|op| op["issue_key"] == new_key && op["kind"] == "move_sprint"),
        "{status}"
    );

    // And `--resume` takes it up as the update it now is, rather than as a
    // second create.
    let dry = ws.json(&["push", "--resume", "--dry-run"]);
    let kinds: Vec<&str> = dry["results"][0]["ops"]
        .as_array()
        .unwrap()
        .iter()
        .map(|o| o["kind"].as_str().unwrap())
        .collect();
    assert_eq!(dry["results"][0]["key"], new_key, "{dry}");
    assert_eq!(kinds, ["move_sprint"], "{dry}");
}

/// A sprint has no file to stage it in, so `sprint create` is a write that goes
/// straight to the server — and the sprint it made is in the listing after.
#[test]
fn creating_a_sprint_puts_it_on_the_board() {
    let ws = Ws::new();
    ws.init();

    let out = ws.json(&[
        "sprint",
        "create",
        "Sprint 44",
        "--board",
        "12",
        "--start",
        "2026-09-01T09:00:00Z",
        "--end",
        "2026-09-15T09:00:00Z",
        "--goal",
        "Ship SSO.",
    ]);
    assert_eq!(out["schema"], "jired/sprint-create/1");
    assert_eq!(out["dry_run"], false);
    assert_eq!(out["sprint"]["name"], "Sprint 44");
    // Jira decides the state, and a sprint nobody has started yet is future.
    assert_eq!(out["sprint"]["state"], "future");
    assert_eq!(out["sprint"]["board_id"], "12");
    assert_eq!(out["board"]["name"], "Platform Board");
    assert_eq!(out["request"]["path"], "/rest/agile/1.0/sprint");
    // The board id goes out as a number, which is what the agile API wants.
    assert_eq!(out["request"]["body"]["originBoardId"], 12);
    assert_eq!(out["request"]["body"]["startDate"], "2026-09-01T09:00:00Z");
    assert_eq!(out["request"]["body"]["goal"], "Ship SSO.");

    let listed = ws.json(&["sprints", "--board", "12"]);
    let names: Vec<&str> = listed["sprints"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"Sprint 44"), "{names:?}");
}

/// One board is not a choice, so it does not have to be named — and the name is
/// then the only thing the command needs.
#[test]
fn a_project_with_one_board_needs_no_board_flag() {
    let ws = Ws::new();
    ws.init();

    let out = ws.json(&["sprint", "create", "Hardening week"]);
    assert_eq!(out["board"]["id"], "12");
    assert_eq!(out["sprint"]["name"], "Hardening week");
    // Neither date was given, so neither is sent: Jira takes both or neither.
    assert!(out["request"]["body"].get("startDate").is_none(), "{out}");
    assert_eq!(out["sprint"]["start_date"], Value::Null);
}

/// A board can be named the way a person has it rather than as an id.
#[test]
fn a_board_can_be_named_instead_of_numbered() {
    let ws = Ws::new();
    ws.init();

    let out = ws.json(&["sprint", "create", "Sprint 44", "--board", "platform board"]);
    assert_eq!(out["board"]["id"], "12");

    let err = ws.json_err(&["sprint", "create", "Sprint 45", "--board", "Nope"], 2);
    assert!(err["message"]
        .as_str()
        .unwrap()
        .contains("no board called `Nope`"));
    assert!(err["hint"]
        .as_str()
        .unwrap()
        .contains("Platform Board (12)"));
}

/// The same escape hatch `push` offers, for the same reason: this one cannot be
/// reviewed with `diff` first.
#[test]
fn a_dry_run_prints_the_call_and_sends_nothing() {
    let ws = Ws::new();
    ws.init();

    let out = ws.json(&["sprint", "create", "Sprint 44", "--dry-run"]);
    assert_eq!(out["dry_run"], true);
    assert_eq!(out["sprint"], Value::Null);
    assert_eq!(out["request"]["body"]["name"], "Sprint 44");

    let listed = ws.json(&["sprints", "--board", "12", "--all"]);
    assert_eq!(
        listed["sprints"].as_array().unwrap().len(),
        3,
        "the fixture's three sprints, and nothing new"
    );
}

/// Every complaint the form makes with the cursor on the field is one the
/// flags-only path has to make too.
#[test]
fn the_values_a_sprint_cannot_do_without_are_checked_without_a_terminal() {
    let ws = Ws::new();
    ws.init();

    let err = ws.json_err(&["sprint", "create"], 7);
    assert_eq!(err["kind"], "missing_value");
    assert!(err["message"]
        .as_str()
        .unwrap()
        .contains("a name is required"));

    let err = ws.json_err(&["sprint", "create", "Sprint 44", "--start", "tuesday"], 2);
    assert!(err["message"]
        .as_str()
        .unwrap()
        .contains("`tuesday` is not a date"));

    // Jira takes both dates or neither; an end on its own is silently dropped.
    let err = ws.json_err(&["sprint", "create", "S", "--end", "2026-09-15"], 2);
    assert!(err["message"]
        .as_str()
        .unwrap()
        .contains("an end date needs a start date"));

    let err = ws.json_err(
        &[
            "sprint",
            "create",
            "S",
            "--start",
            "2026-09-15",
            "--end",
            "2026-09-01",
        ],
        2,
    );
    assert!(err["message"]
        .as_str()
        .unwrap()
        .contains("ends before it starts"));
}

/// The sprint is in the local cache the moment it exists, so an issue can be
/// moved into it without a fetch in between.
#[test]
fn a_new_sprint_can_be_used_before_the_next_fetch() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);
    let created = ws.json(&["sprint", "create", "Sprint 44"]);
    let id = created["sprint"]["id"].as_str().unwrap().to_string();

    let text = ws
        .read(ISSUE_FILE)
        .replace("sprint: Sprint 42", "sprint: Sprint 44");
    ws.write(ISSUE_FILE, &text);

    let push = ws.json(&["push", "--dry-run"]);
    let op = &push["results"][0]["ops"][0];
    assert_eq!(op["kind"], "move_sprint");
    assert_eq!(
        op["request"]["path"],
        format!("/rest/agile/1.0/sprint/{id}/issue")
    );
}

/// The scriptable equivalent of editing `sprint:` by hand: it stages, the way
/// `transition` and `assign` do, so it shows up in `status` and `diff` and goes
/// out through the normal push.
#[test]
fn sprint_add_stages_the_move_on_every_issue_named() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);

    let out = ws.json(&[
        "sprint",
        "add",
        "PROJ-142",
        "PROJ-150",
        "--sprint",
        "Sprint 43",
    ]);
    assert_eq!(out["schema"], "jired/sprint-move/1");
    assert_eq!(out["sprint"]["id"], "43");
    assert_eq!(out["sprint"]["name"], "Sprint 43");
    assert_eq!(out["pushed"], false);
    // Said in the document, where an agent reading --json looks: the moves
    // are in the files and nothing has reached the server.
    assert_eq!(out["staged"], true);
    assert_eq!(out["next"], "jired push PROJ-142 PROJ-150");
    let keys: Vec<&str> = out["issues"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["key"].as_str().unwrap())
        .collect();
    assert_eq!(keys, ["PROJ-142", "PROJ-150"]);

    // The list is a history, so a move appends rather than overwrites.
    let text = ws.read(ISSUE_FILE);
    assert!(
        text.contains("sprint:\n- Sprint 42\n- Sprint 43"),
        "the sprint it came from is kept:\n{text}"
    );

    // Staged, not sent — and `push` turns it into the board call.
    assert_eq!(ws.json(&["status"])["dirty"].as_array().unwrap().len(), 2);
    let dry = ws.json(&["push", "--dry-run"]);
    let op = &dry["results"][0]["ops"][0];
    assert_eq!(op["kind"], "move_sprint");
    assert_eq!(op["request"]["path"], "/rest/agile/1.0/sprint/43/issue");
}

/// A sprint can be named the way the board has it or by the id the listing
/// shows, and a name no board knows is an error here rather than a warning
/// after the push has already skipped it.
#[test]
fn sprint_add_takes_a_name_or_an_id_and_refuses_neither() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);

    let by_id = ws.json(&["sprint", "add", "PROJ-142", "--sprint", "43"]);
    assert_eq!(by_id["sprint"]["name"], "Sprint 43");
    // Written in the board's own spelling, not the one that was typed.
    let lower = ws.json(&["sprint", "add", "PROJ-150", "--sprint", "sprint 43"]);
    assert_eq!(lower["sprint"]["name"], "Sprint 43");
    assert!(ws
        .read("PROJ-0150. Rotate IdP certificate.md")
        .contains("Sprint 43"));

    let err = ws.json_err(&["sprint", "add", "PROJ-142", "--sprint", "Sprint 99"], 2);
    assert!(err["message"]
        .as_str()
        .unwrap()
        .contains("`Sprint 99` is not a sprint this workspace knows"));
    assert!(err["hint"].as_str().unwrap().contains("Sprint 43"));
}

/// Already being where you are being sent is not a change, and must not stage
/// an edit that `push` would then have to explain away.
#[test]
fn sprint_add_says_so_when_the_issue_is_already_there() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);

    let out = ws.json(&["sprint", "add", "PROJ-142", "--sprint", "Sprint 42"]);
    assert_eq!(out["issues"][0]["changed"], false);
    assert!(
        ws.json(&["status"])["dirty"].as_array().unwrap().is_empty(),
        "nothing was staged"
    );
}

/// Removing them all is how a file says "the backlog".
#[test]
fn sprint_remove_sends_the_issue_back_to_the_backlog() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);

    let out = ws.json(&["sprint", "remove", "PROJ-142", "--push"]);
    assert_eq!(out["sprint"], Value::Null);
    assert_eq!(out["pushed"], true);
    assert_eq!(out["staged"], false);
    assert_eq!(out["next"], Value::Null);
    let op = &out["push"]["results"][0]["ops"][0];
    assert_eq!(op["kind"], "move_sprint");
    assert_eq!(op["status"], "done");
    assert!(!ws.read(ISSUE_FILE).contains("sprint:"));
}

/// Creating a sprint is almost always followed by filling it, so it can be done
/// in one command.
#[test]
fn a_sprint_can_be_created_and_seeded_in_one_go() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);

    let out = ws.json(&[
        "sprint",
        "create",
        "Sprint 44",
        "--issues",
        "PROJ-142,PROJ-150",
        "--push",
    ]);
    assert_eq!(out["issues"].as_array().unwrap().len(), 2);
    assert_eq!(out["push"]["results"][0]["ok"], true);
    assert_eq!(out["push"]["results"][0]["ops"][0]["kind"], "move_sprint");
    assert_eq!(out["push"]["results"][0]["ops"][0]["status"], "done");
    // The push wrote the server's answer back, which is the new sprint alone.
    assert!(ws.read(ISSUE_FILE).contains("sprint: Sprint 44"));

    // The issues really are on the new sprint, not merely staged for it.
    let id = out["sprint"]["id"].as_str().unwrap().to_string();
    ws.json(&["pull"]);
    assert!(
        ws.path().join("sprints/Sprint 44").is_dir(),
        "sprint {id} should have a grouping folder now"
    );
}

/// Starting a sprint is one merging POST, and the fields it does not name are
/// the ones the sprint keeps.
#[test]
fn starting_a_sprint_sets_its_state_and_leaves_the_rest_alone() {
    let ws = Ws::new();
    ws.init();
    // The sprint cache is what resolves a name to an id.
    ws.json(&["fetch"]);

    // Sprint 42 is the one running; Jira allows one per board.
    ws.json(&["sprint", "close", "Sprint 42"]);

    let out = ws.json(&["sprint", "start", "Sprint 43", "--goal", "Ship SSO."]);
    assert_eq!(out["schema"], "jired/sprint-update/1");
    assert_eq!(out["dry_run"], false);
    assert_eq!(out["request"]["path"], "/rest/agile/1.0/sprint/43");
    assert_eq!(out["request"]["body"]["state"], "active");
    assert_eq!(out["request"]["body"]["goal"], "Ship SSO.");
    // The dates were already on the sprint, so they are not resent: what is not
    // sent is what the sprint keeps.
    assert!(out["request"]["body"].get("startDate").is_none(), "{out}");
    assert_eq!(out["sprint"]["state"], "active");
    assert_eq!(out["sprint"]["goal"], "Ship SSO.");
    assert_eq!(out["sprint"]["start_date"], "2026-08-24T09:00:00Z");

    let listed = ws.json(&["sprints", "--board", "12"]);
    let started = listed["sprints"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["id"] == "43")
        .unwrap()
        .clone();
    assert_eq!(started["state"], "active");
}

/// Both of the 400s Jira answers without saying why, caught while the command
/// is still on screen.
#[test]
fn a_sprint_jira_would_refuse_to_start_is_refused_here() {
    let ws = Ws::new();
    ws.init();
    // The sprint cache is what resolves a name to an id.
    ws.json(&["fetch"]);

    // One running sprint per board, and the one in the way is named.
    let err = ws.json_err(&["sprint", "start", "Sprint 43"], 2);
    assert!(
        err["message"]
            .as_str()
            .unwrap()
            .contains("already running Sprint 42"),
        "{err}"
    );
    assert!(err["hint"].as_str().unwrap().contains("sprint close"));

    ws.json(&["sprint", "close", "Sprint 42"]);

    // A sprint with no dates cannot be started, and the missing half is named.
    let made = ws.json(&["sprint", "create", "Hardening week"]);
    let id = made["sprint"]["id"].as_str().unwrap().to_string();
    let err = ws.json_err(&["sprint", "start", &id], 2);
    assert!(
        err["message"].as_str().unwrap().contains("no start or end"),
        "{err}"
    );

    // Passing the dates sets them and starts it in the one call.
    let out = ws.json(&[
        "sprint",
        "start",
        &id,
        "--start",
        "2026-09-01T09:00:00Z",
        "--end",
        "2026-09-15T09:00:00Z",
    ]);
    assert_eq!(out["request"]["body"]["startDate"], "2026-09-01T09:00:00Z");
    assert_eq!(out["sprint"]["state"], "active");

    // And one that is already running says so rather than sending again.
    let err = ws.json_err(&["sprint", "start", &id], 2);
    assert!(err["message"].as_str().unwrap().contains("already running"));
    assert_eq!(err["details"]["sprint_id"], id);
}

/// The field the report could not reach at all: a goal on a sprint that exists.
#[test]
fn a_sprints_goal_and_dates_can_be_changed_after_it_is_made() {
    let ws = Ws::new();
    ws.init();
    // The sprint cache is what resolves a name to an id.
    ws.json(&["fetch"]);

    let out = ws.json(&["sprint", "edit", "Sprint 43", "--goal", "Ship the fluids."]);
    assert_eq!(out["schema"], "jired/sprint-update/1");
    assert_eq!(out["request"]["path"], "/rest/agile/1.0/sprint/43");
    assert_eq!(out["request"]["body"]["goal"], "Ship the fluids.");
    // A goal alone changes nothing else, because nothing else is sent.
    assert!(out["request"]["body"].get("state").is_none(), "{out}");
    assert_eq!(out["sprint"]["goal"], "Ship the fluids.");
    assert_eq!(
        out["sprint"]["state"], "future",
        "editing does not start it"
    );

    // An empty goal is how one is taken back off; `create` has no such reading.
    let cleared = ws.json(&["sprint", "edit", "43", "--goal", ""]);
    assert_eq!(cleared["request"]["body"]["goal"], "");
    assert_eq!(cleared["sprint"]["goal"], Value::Null);

    let renamed = ws.json(&["sprint", "edit", "43", "--name", "Sprint 43a"]);
    assert_eq!(renamed["sprint"]["name"], "Sprint 43a");

    // An empty name is refused where an empty goal is not: a sprint without a
    // goal is an ordinary sprint, one without a name is nothing to point at.
    let err = ws.json_err(&["sprint", "edit", "43", "--name", ""], 2);
    assert!(err["message"]
        .as_str()
        .unwrap()
        .contains("cannot be renamed to nothing"));

    // Nothing to change is a mistake about the command, made before any request.
    let err = ws.json_err(&["sprint", "edit", "43"], 7);
    assert_eq!(err["kind"], "missing_value");
    assert!(err["message"]
        .as_str()
        .unwrap()
        .contains("nothing to change"));

    // The same date checks `create` makes, against what the sprint would have.
    let err = ws.json_err(&["sprint", "edit", "43", "--end", "2026-01-01"], 2);
    assert!(err["message"]
        .as_str()
        .unwrap()
        .contains("ends before it starts"));
}

/// Closing a sprint in the UI left nine issues pointing at a closed sprint with
/// no carry-over, and no way to see it. This is both halves of that.
#[test]
fn closing_a_sprint_names_what_it_would_strand_and_can_carry_it_over() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);
    // Two issues in the running sprint, one of which gets finished.
    ws.json(&[
        "sprint",
        "add",
        "PROJ-150",
        "--sprint",
        "Sprint 42",
        "--push",
    ]);
    ws.json(&["transition", "PROJ-150", "--to", "Done", "--push"]);
    ws.json(&["pull"]);

    // Without the flag: closed, and the unfinished issue named rather than
    // quietly stranded.
    let seen = ws.json(&["sprint", "close", "Sprint 42", "--dry-run"]);
    assert_eq!(seen["schema"], "jired/sprint-close/1");
    assert_eq!(seen["dry_run"], true);
    assert_eq!(seen["closed"], false);
    assert_eq!(seen["carried_to"], Value::Null);
    let stranded: Vec<&str> = seen["incomplete"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["key"].as_str().unwrap())
        .collect();
    assert_eq!(
        stranded,
        ["PROJ-142"],
        "the issue that is Done is finished and stays put"
    );
    assert_eq!(seen["request"]["path"], "/rest/agile/1.0/sprint/42");
    assert_eq!(seen["request"]["body"]["state"], "closed");
    // A dry run sends nothing at all.
    assert_eq!(
        ws.json(&["sprints", "--board", "12"])["sprints"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["id"] == "42")
            .unwrap()["state"],
        "active"
    );

    let out = ws.json(&[
        "sprint",
        "close",
        "Sprint 42",
        "--move-incomplete-to",
        "Sprint 43",
    ]);
    assert_eq!(out["closed"], true);
    assert_eq!(out["sprint"]["state"], "closed");
    assert_eq!(out["carried_to"]["name"], "Sprint 43");
    assert_eq!(out["issues"][0]["key"], "PROJ-142");
    assert_eq!(out["push"]["results"][0]["ops"][0]["kind"], "move_sprint");
    assert_eq!(out["push"]["results"][0]["ops"][0]["status"], "done");

    // The carry-over is a real move, not a staged one: the file holds what the
    // push wrote back, which is where the "server" now says the issue is.
    let text = ws.read(ISSUE_FILE);
    assert_eq!(out["issues"][0]["changed"], true);
    assert!(text.contains("sprint: Sprint 43"), "{text}");
    // The finished one stayed where it was.
    assert!(ws
        .read("PROJ-0150. Rotate IdP certificate.md")
        .contains("sprint: Sprint 42"));
}

/// The states a close cannot be asked for, and the sprints it cannot carry into.
#[test]
fn a_sprint_that_cannot_be_closed_says_so_before_sending() {
    let ws = Ws::new();
    ws.init();
    // The sprint cache is what resolves a name to an id.
    ws.json(&["fetch"]);

    let err = ws.json_err(&["sprint", "close", "Sprint 43"], 2);
    assert!(
        err["message"]
            .as_str()
            .unwrap()
            .contains("has not been started"),
        "{err}"
    );
    let err = ws.json_err(&["sprint", "close", "Sprint 41"], 2);
    assert!(err["message"].as_str().unwrap().contains("already closed"));

    // Carrying into a closed sprint would strand the issues just as thoroughly.
    let err = ws.json_err(
        &[
            "sprint",
            "close",
            "Sprint 42",
            "--move-incomplete-to",
            "Sprint 41",
        ],
        2,
    );
    assert!(
        err["message"].as_str().unwrap().contains("is closed"),
        "{err}"
    );

    let err = ws.json_err(
        &[
            "sprint",
            "close",
            "Sprint 42",
            "--move-incomplete-to",
            "Sprint 42",
        ],
        2,
    );
    assert!(err["message"]
        .as_str()
        .unwrap()
        .contains("cannot carry its own issues over to itself"));
}

/// The same escape hatch `create` offers, for the same reason: neither of these
/// can be reviewed with `diff` first.
#[test]
fn start_and_edit_print_the_call_under_a_dry_run() {
    let ws = Ws::new();
    ws.init();
    // The sprint cache is what resolves a name to an id.
    ws.json(&["fetch"]);

    let out = ws.json(&["sprint", "edit", "43", "--goal", "Ship SSO.", "-n"]);
    assert_eq!(out["dry_run"], true);
    assert_eq!(out["sprint"], Value::Null);
    assert_eq!(out["request"]["body"]["goal"], "Ship SSO.");

    // Nothing was sent: the sprint still has no goal.
    let listed = ws.json(&["sprints", "--board", "12"]);
    let s = listed["sprints"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["id"] == "43")
        .unwrap()
        .clone();
    assert_eq!(s["goal"], Value::Null);
    assert_eq!(s["state"], "future");
}

/// One key that is not here does not cost the others their move.
#[test]
fn an_issue_the_workspace_does_not_have_is_reported_and_the_rest_still_move() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);

    let out = ws.json(&["sprint", "add", "PROJ-142", "PROJ-999", "--sprint", "43"]);
    let keys: Vec<&str> = out["issues"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["key"].as_str().unwrap())
        .collect();
    assert_eq!(keys, ["PROJ-142"]);
    assert!(
        out["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|w| w.as_str().unwrap().contains("PROJ-999")),
        "{out}"
    );

    // None of them being here is the command failing, not a quiet success.
    let err = ws.json_err(&["sprint", "add", "PROJ-998", "--sprint", "43"], 2);
    assert!(
        err["message"]
            .as_str()
            .unwrap()
            .contains("not in this workspace")
            || err["message"]
                .as_str()
                .unwrap()
                .contains("none of the 1 issue(s) named")
    );
}

/// Labelling six issues used to mean a script that rewrote `labels:` in the
/// YAML — the caller parsing jired's own file format, which breaks the first
/// time the format moves.
#[test]
fn label_add_and_remove_touch_one_label_and_leave_the_rest() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);

    let out = ws.json(&["label", "add", "regular", "PROJ-142", "PROJ-150"]);
    assert_eq!(out["schema"], "jired/label/1");
    assert_eq!(out["operation"], "add");
    assert_eq!(out["labels"][0], "regular");
    assert_eq!(out["issues"].as_array().unwrap().len(), 2);

    // The labels that were already there are still there.
    let text = ws.read(ISSUE_FILE);
    assert!(text.contains("- auth"), "{text}");
    assert!(text.contains("- regular"), "{text}");

    // Already labelled is reported, not staged a second time.
    let again = ws.json(&["label", "add", "regular", "PROJ-142"]);
    assert_eq!(again["issues"][0]["changed"], false);

    let off = ws.json(&["label", "remove", "regular", "PROJ-142"]);
    assert_eq!(off["issues"][0]["changed"], true);
    let text = ws.read(ISSUE_FILE);
    assert!(!text.contains("- regular"), "{text}");
    assert!(text.contains("- auth"), "the others are untouched:\n{text}");
}

/// `set` replaces the lot, and the change goes out as an ordinary field update.
#[test]
fn label_set_replaces_every_label_and_pushes() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);

    let out = ws.json(&["label", "set", "a,b", "PROJ-142", "--push"]);
    assert_eq!(out["operation"], "set");
    assert_eq!(out["pushed"], true);
    let op = &out["push"]["results"][0]["ops"][0];
    assert_eq!(op["kind"], "update_fields");
    assert_eq!(op["status"], "done");
    let text = ws.read(ISSUE_FILE);
    assert!(text.contains("- a\n- b"), "{text}");
    assert!(!text.contains("- auth"), "set replaces:\n{text}");
}

/// A label with a space in it is a list somebody separated the wrong way, and
/// sending it would make a label nobody meant.
#[test]
fn a_label_with_a_space_is_refused() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);

    let err = ws.json_err(&["label", "add", "two words", "PROJ-142"], 2);
    assert!(
        err["message"].as_str().unwrap().contains("no spaces"),
        "{err}"
    );
    let err = ws.json_err(&["label", "add", "a,b", "PROJ-142"], 2);
    assert!(
        err["message"]
            .as_str()
            .unwrap()
            .contains("one label at a time"),
        "{err}"
    );
}

/// The general form: any editable field, on any number of issues, without
/// anybody outside jired writing YAML.
#[test]
fn set_writes_any_editable_field_across_issues() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);

    let out = ws.json(&[
        "set", "--field", "priority", "--value", "Low", "PROJ-142", "PROJ-150",
    ]);
    assert_eq!(out["schema"], "jired/set/1");
    assert_eq!(out["field"], "priority");
    assert_eq!(out["issues"].as_array().unwrap().len(), 2);
    assert!(ws.read(ISSUE_FILE).contains("priority: Low"));

    // A list field takes commas, and a number is parsed as one.
    ws.json(&[
        "set",
        "--field",
        "components",
        "--value",
        "Auth,API",
        "PROJ-142",
    ]);
    let text = ws.read(ISSUE_FILE);
    assert!(text.contains("- Auth"), "{text}");
    assert!(text.contains("- API"), "{text}");
    ws.json(&["set", "--field", "story_points", "--value", "8", "PROJ-142"]);
    assert!(ws.read(ISSUE_FILE).contains("story_points: 8"));

    // Setting what is already there is reported rather than staged.
    let again = ws.json(&["set", "--field", "priority", "--value", "Low", "PROJ-142"]);
    assert_eq!(again["issues"][0]["changed"], false);

    // An empty value clears, which is what a push sends as null.
    ws.json(&["set", "--field", "priority", "--value", "", "PROJ-142"]);
    assert!(!ws.read(ISSUE_FILE).contains("priority:"));

    let push = ws.json(&["push", "--dry-run"]);
    let sent = &push["results"][0]["ops"][0]["request"]["body"]["fields"];
    assert_eq!(sent["priority"], Value::Null, "{sent}");
}

/// A field nobody can write is a mistake about the command, not about one of
/// the issues — so it is refused before any file is touched.
#[test]
fn set_refuses_a_field_it_cannot_write_before_editing_anything() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);
    let before = ws.read(ISSUE_FILE);

    let err = ws.json_err(
        &["set", "--field", "nonsense", "--value", "x", "PROJ-142"],
        2,
    );
    assert!(
        err["hint"].as_str().unwrap().contains("customfield_"),
        "{err}"
    );

    let err = ws.json_err(
        &["set", "--field", "key", "--value", "PROJ-1", "PROJ-142"],
        2,
    );
    assert!(
        err["message"]
            .as_str()
            .unwrap()
            .contains("identifies the issue"),
        "{err}"
    );

    let err = ws.json_err(
        &[
            "set",
            "--field",
            "story_points",
            "--value",
            "many",
            "PROJ-142",
        ],
        2,
    );
    assert!(
        err["message"].as_str().unwrap().contains("not a number"),
        "{err}"
    );

    assert_eq!(ws.read(ISSUE_FILE), before, "nothing was written");
}

/// Six of ten calls in a real session existed only to print a subset of the
/// JSON jired had just built. `--fields` is that subset, without the script.
#[test]
fn fields_projects_the_document_and_keeps_the_schema() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);

    let out = ws.json(&["status", "--fields", "dirty,new_issues"]);
    assert_eq!(out["schema"], "jired/status/1");
    assert_eq!(out.as_object().unwrap().len(), 3, "{out}");

    // A path maps over an array: how did each issue do, in one call.
    ws.json(&["label", "add", "regular", "PROJ-142", "PROJ-150"]);
    let out = ws.json(&["push", "--fields", "summary,results.key,results.ok"]);
    assert_eq!(out["summary"]["ok"], 2);
    assert_eq!(out["results"][0]["key"], "PROJ-142");
    assert_eq!(out["results"][0]["ok"], true);
    assert!(out["results"][0].get("ops").is_none(), "{out}");
    assert!(out.get("dry_run").is_none(), "{out}");
}

/// The nulls and empty lists a caller pays for on every call and never reads.
#[test]
fn json_compact_drops_nulls_and_empties() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);
    ws.json(&["label", "add", "regular", "PROJ-142"]);

    let full = ws.json(&["push", "--dry-run"]);
    assert!(full["results"][0]["new_key"].is_null());
    assert!(full["results"][0]
        .as_object()
        .unwrap()
        .contains_key("error"));

    ws.json(&["label", "add", "another", "PROJ-142"]);
    let lean = ws.json(&["push", "--dry-run", "--json-compact"]);
    let result = lean["results"][0].as_object().unwrap();
    assert!(!result.contains_key("new_key"), "{lean}");
    assert!(!result.contains_key("error"), "{lean}");
    // A value that is `false` is a value, not an absence.
    assert_eq!(lean["dry_run"], true);
    assert_eq!(lean["schema"], "jired/push/1", "the schema always survives");
}

/// Checking whether a push went through should not mean walking every
/// operation of every issue.
#[test]
fn push_leads_with_a_one_line_summary() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);
    ws.json(&["label", "add", "regular", "PROJ-142", "PROJ-150"]);

    let push = ws.json(&["push"]);
    assert_eq!(push["summary"]["ok"], 2);
    assert_eq!(push["summary"]["failed"], 0);
    assert!(push["summary"]["stale"].as_array().unwrap().is_empty());

    // The stale case is asserted where a stale push is set up properly:
    // see `push_refuses_a_stale_issue_with_exit_5`.
}

/// The other half of any bulk edit: choosing what to edit. Without it the
/// choice was `grep -l '^summary:.*\[ALL\]' *.md` — a regex over jired's file
/// format, which is what the format exists to spare people.
#[test]
fn list_selects_issues_from_the_working_copy() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);

    let all = ws.json(&["list"]);
    assert_eq!(all["schema"], "jired/list/1");
    let total = all["count"].as_u64().unwrap();
    assert!(total >= 2, "{all}");
    assert!(all["issues"][0]["file"].as_str().unwrap().ends_with(".md"));

    // A summary is a substring search, which is what looking for `[ALL]` means.
    let some = ws.json(&["list", "--summary-contains", "login"]);
    assert_eq!(some["count"], 1);
    assert_eq!(some["issues"][0]["key"], "PROJ-142");

    // The rest match whole values.
    assert_eq!(ws.json(&["list", "--status", "In Progress"])["count"], 1);
    assert_eq!(ws.json(&["list", "--status", "In"])["count"], 0);
    assert_eq!(ws.json(&["list", "--label", "auth"])["count"], 1);
    assert_eq!(ws.json(&["list", "--sprint", "Sprint 42"])["count"], 1);

    // Filters are a conjunction.
    let none = ws.json(&["list", "--label", "auth", "--status", "Done"]);
    assert_eq!(none["count"], 0);
}

/// `sprint` in the JSON is the whole history the file carries, not the last of
/// it: the difference between the two is what splits carry-over from new work,
/// and answering that used to mean parsing the `.md` files instead.
#[test]
fn list_reports_the_sprint_history_the_file_carries() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);

    let text = ws
        .read(ISSUE_FILE)
        .replace("sprint: Sprint 42", "sprint:\n- Sprint 41\n- Sprint 42");
    ws.write(ISSUE_FILE, &text);

    let listed = ws.json(&["list", "--sprint", "Sprint 42"]);
    assert_eq!(listed["count"], 1);
    assert_eq!(
        listed["issues"][0]["sprint"],
        serde_json::json!(["Sprint 41", "Sprint 42"])
    );
    // An issue in none of them gets an empty list rather than a null.
    let backlog = ws.json(&["list", "--summary-contains", "IdP"]);
    assert_eq!(backlog["issues"][0]["sprint"], serde_json::json!([]));

    // The column is where it is *now*, the way `assignee` shows a name out of
    // the `@[Name](id)` the JSON carries.
    let out = ws
        .cmd(&["list", "--columns", "key,sprint", "--sprint", "Sprint 42"])
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        "PROJ-142\tSprint 42"
    );
}

/// "Everything in this sprint that isn't finished yet" had no expression:
/// `--status` named one status, and naming the negation by hand breaks the day
/// a workflow gains a status. The category is Jira's own three-way reading of
/// any workflow, and `--not-status` is the exact-name complement.
#[test]
fn list_selects_by_category_and_by_negated_status() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);

    // The fixture: PROJ-142 In Progress (indeterminate), 143 and 150 To Do (new).
    let open = ws.json(&["list", "--category", "indeterminate"]);
    assert_eq!(open["count"], 1, "{open}");
    assert_eq!(open["issues"][0]["key"], "PROJ-142");
    assert_eq!(ws.json(&["list", "--category", "new"])["count"], 2);
    assert_eq!(ws.json(&["list", "--category", "done"])["count"], 0);

    // A repeated `--status` accepts any of them; `--not-status` excludes.
    let either = ws.json(&["list", "--status", "To Do", "--status", "In Progress"]);
    assert_eq!(either["count"], 3, "{either}");
    let not_started = ws.json(&["list", "--not-status", "In Progress"]);
    assert_eq!(not_started["count"], 2, "{not_started}");

    // A status the flag cannot name is refused where the typo is on screen.
    let out = ws
        .cmd(&["list", "--category", "unfinished"])
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(2),
        "clap rejects an unknown category"
    );
}

/// `--keys` exists to be piped into the commands that take issues.
#[test]
fn list_keys_pipes_into_the_commands_that_edit() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);

    let out = ws
        .cmd(&["list", "--keys", "--label", "auth"])
        .output()
        .unwrap();
    let keys: Vec<String> = String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .map(str::to_string)
        .collect();
    assert_eq!(keys, ["PROJ-142"], "nothing but the keys, one per line");

    // Which is exactly what `label add` takes.
    let mut args = vec!["label", "add", "picked"];
    args.extend(keys.iter().map(String::as_str));
    let labelled = ws.json(&args);
    assert_eq!(labelled["issues"][0]["key"], "PROJ-142");
    assert!(ws.read(ISSUE_FILE).contains("- picked"));
}

/// The default line is key, status and summary, which cannot answer "which of
/// these are Tasks, and who has them" on an instance whose type names differ by
/// a prefix. That question was being answered by piping `--json` through a
/// parser; `--columns` answers it with `cut`.
#[test]
fn list_columns_pick_the_fields_and_tab_separate_them_off_a_terminal() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);

    let out = ws
        .cmd(&["list", "--columns", "key,type,assignee", "--label", "auth"])
        .output()
        .unwrap();
    let text = String::from_utf8(out.stdout).unwrap();
    // stdout here is a pipe, so the row is a record: one tab per boundary and
    // no padding, which is what makes the second field addressable by number.
    let fields: Vec<&str> = text.trim_end().split('\t').collect();
    assert_eq!(fields[0], "PROJ-142", "{text:?}");
    assert_eq!(
        fields[1],
        ws.json(&["list", "--label", "auth"])["issues"][0]["type"]
    );
    // The name out of `@[Name](id)`: the id stays in `--json`.
    assert_eq!(fields[2], "Ilya A", "{text:?}");

    // Order is the caller's, and a column can be left out.
    let only = ws
        .cmd(&["list", "--columns", "status,key", "--label", "auth"])
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8(only.stdout).unwrap().trim_end(),
        "In Progress\tPROJ-142"
    );

    // A column jired has no field for is refused where the typo is on screen,
    // rather than printing a listing that quietly lacks it.
    let bad = ws.cmd(&["list", "--columns", "asignee"]).output().unwrap();
    assert_eq!(bad.status.code(), Some(2));
    // `--keys` is the same choice made differently, so asking for both is not a
    // question with an answer.
    let both = ws
        .cmd(&["list", "--keys", "--columns", "key"])
        .output()
        .unwrap();
    assert_eq!(both.status.code(), Some(2));
}

/// "Raise an issue in a sprint" was three steps and a YAML edit: create the
/// file, edit the frontmatter by hand, push. With the create call sending every
/// field, it is one command.
#[test]
fn create_takes_the_fields_an_issue_is_raised_with() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);

    let report = ws.json(&[
        "create",
        "-s",
        "Ship SSO",
        "--assignee",
        "@[Ilya A](5b10a2ilya)",
        "--sprint",
        "Sprint 43",
        "--labels",
        "regular,auth",
        "--priority",
        "High",
        "--component",
        "Auth",
        "--set",
        "story_points=5",
        "--set",
        "customfield_10456=Platform",
    ]);
    let file = report["file"].as_str().unwrap();
    let text = ws.read(file);
    for expected in [
        "priority: High",
        "- regular",
        "- Auth",
        "story_points: 5",
        "sprint: Sprint 43",
        "customfield_10456: Platform",
    ] {
        assert!(text.contains(expected), "{expected} missing from\n{text}");
    }

    // And every one of them reaches the server, not just the file.
    let dry = ws.json(&["push", "--dry-run"]);
    let sent = &dry["results"][0]["ops"][0]["request"]["body"]["fields"];
    assert_eq!(sent["priority"]["name"], "High");
    assert_eq!(sent["assignee"]["accountId"], "5b10a2ilya");
    assert_eq!(sent["components"][0]["name"], "Auth");
    assert_eq!(sent["labels"][0], "regular");
    let kinds: Vec<&str> = dry["results"][0]["ops"]
        .as_array()
        .unwrap()
        .iter()
        .map(|o| o["kind"].as_str().unwrap())
        .collect();
    assert_eq!(kinds, ["create_issue", "move_sprint"]);
}

/// A skeleton can live in the repository being worked on rather than having to
/// be copied into every workspace, and `--set` overrides what it said.
#[test]
fn a_template_can_be_a_path_and_set_overrides_it() {
    let ws = Ws::new();
    ws.init();

    ws.write(
        "skeleton.md",
        "---\ntype: Bug\npriority: Low\nlabels:\n- incident\njired: {}\n---\n\n         ## Description\n\nWhat happened?\n",
    );
    let report = ws.json(&[
        "create",
        "-s",
        "Login is down",
        "--template",
        "skeleton.md",
        "--set",
        "priority=Highest",
    ]);
    let text = ws.read(report["file"].as_str().unwrap());
    assert!(text.contains("type: Bug"), "the template's:\n{text}");
    assert!(text.contains("- incident"), "{text}");
    assert!(text.contains("What happened?"), "{text}");
    assert!(
        text.contains("priority: Highest"),
        "--set beats the template:\n{text}"
    );
}

#[test]
fn a_set_that_is_not_field_equals_value_is_refused() {
    let ws = Ws::new();
    ws.init();
    let err = ws.json_err(&["create", "-s", "x", "--set", "nonsense"], 2);
    assert!(
        err["message"].as_str().unwrap().contains("field=value"),
        "{err}"
    );
    let err = ws.json_err(&["create", "-s", "x", "--set", "nope=1"], 2);
    assert!(err["message"].as_str().unwrap().contains("`nope`"), "{err}");
}

/// A worklog somebody logged while you worked has nothing to do with the label
/// you are sending. Dropping the issue over it is stricter than it needs to be,
/// and recovering took three commands and a full pull of the workspace.
#[test]
fn refresh_stale_retries_an_issue_whose_incoming_change_is_unrelated() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);
    ws.json(&["label", "add", "regular", "PROJ-142"]);

    // Somebody else touches a field this push is not sending.
    ws.mutate_remote("PROJ-142", |issue| {
        issue["fields"]["description"] = "Someone else edited this.".into();
    });

    // Without the flag it is refused, exactly as before.
    let refused = ws.json_code(&["push"], 5);
    assert_eq!(refused["summary"]["stale"][0], "PROJ-142");

    // With it, the issue is re-fetched and sent.
    let out = ws.json(&["push", "--refresh-stale"]);
    assert!(
        out["summary"]["stale"].as_array().unwrap().is_empty(),
        "{out}"
    );
    assert_eq!(out["summary"]["ok"], 1, "{out}");
    assert_eq!(out["results"][0]["ops"][0]["kind"], "update_fields");
    // Both changes survive: theirs came in on the re-fetch, ours went out.
    let text = ws.read(ISSUE_FILE);
    assert!(text.contains("- regular"), "{text}");
    assert!(text.contains("Someone else edited this."), "{text}");
}

/// A change under a change is a real conflict, and stays refused.
#[test]
fn refresh_stale_leaves_a_real_conflict_alone() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);
    ws.json(&["set", "--field", "priority", "--value", "Low", "PROJ-142"]);

    // The same field, changed on the server.
    ws.mutate_remote("PROJ-142", |issue| {
        issue["fields"]["priority"] = serde_json::json!({ "name": "Highest" });
    });

    let out = ws.json_code(&["push", "--refresh-stale"], 5);
    assert_eq!(out["summary"]["stale"][0], "PROJ-142", "{out}");
    assert!(
        out["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|w| w.as_str().unwrap().contains("touches a field you changed")),
        "{out}"
    );
    // The local edit is untouched, so nothing was lost.
    assert!(ws.read(ISSUE_FILE).contains("priority: Low"));
}

/// "Create a task like PROJ-142" had no expression: the model had to be read
/// and its fields typed back one at a time.
#[test]
fn create_like_copies_the_shape_of_another_issue() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);
    ws.json(&["label", "add", "regular", "PROJ-142"]);

    let report = ws.json(&[
        "create",
        "-s",
        "[CARE] A new one",
        "--like",
        "PROJ-142",
        "--priority",
        "Low",
    ]);
    let text = ws.read(report["file"].as_str().unwrap());

    // What describes the kind of issue this is, copied.
    assert!(text.contains("type: Story"), "{text}");
    assert!(text.contains("- regular"), "{text}");
    assert!(text.contains("assignee:"), "{text}");
    // A flag beats the model.
    assert!(text.contains("priority: Low"), "{text}");
    // What belongs to the issue that already exists, not copied.
    assert!(
        text.contains("summary: '[CARE] A new one'")
            || text.contains("summary: \"[CARE] A new one\"")
            || text.contains("summary: [CARE] A new one"),
        "{text}"
    );
    assert!(!text.contains("key: PROJ-142"), "{text}");
    assert!(!text.contains("status: In Progress"), "{text}");
    assert!(!text.contains("Repro confirmed"), "no comments:\n{text}");
}

/// An unset field is not written into the file, so "is it empty, or does this
/// instance not have it?" was answerable only from `sqlite3 .jired/state.db`.
#[test]
fn show_answers_from_the_snapshot_including_the_raw_payload() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);

    let out = ws.json(&["show", "PROJ-142"]);
    assert_eq!(out["schema"], "jired/show/1");
    assert_eq!(out["key"], "PROJ-142");
    assert_eq!(out["source"], "file");
    assert_eq!(out["frontmatter"]["summary"], "Fix login flow");

    // The server's own payload, where a field jired does not model still is.
    let raw = ws.json(&["show", "PROJ-142", "--raw"]);
    assert!(
        raw["raw"]["fields"].as_object().unwrap().len() > 5,
        "the whole payload: {raw}"
    );
}

/// The field report: `ls TSK-710*.md` found nothing, because files are named
/// `TSK-0710. …`. `--path` prints the file, from the key however it is padded.
#[test]
fn show_path_names_the_file_whatever_the_key_looks_like() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);

    for spelling in ["PROJ-142", "proj-142", "PROJ-0142"] {
        let out = ws.cmd(&["show", spelling, "--path"]).output().unwrap();
        assert!(out.status.success(), "{spelling}");
        assert_eq!(
            String::from_utf8_lossy(&out.stdout).trim_end(),
            ISSUE_FILE,
            "{spelling}"
        );
    }
    let doc = ws.json(&["show", "PROJ-142", "--path"]);
    assert_eq!(doc["file"], ISSUE_FILE);
    // And `file` is in the ordinary document too.
    assert_eq!(ws.json(&["show", "PROJ-142"])["file"], ISSUE_FILE);

    // An issue with no file says so, rather than printing nothing.
    std::fs::remove_file(ws.path().join(ISSUE_FILE)).unwrap();
    let err = ws.json_err(&["show", "PROJ-142", "--path"], 2);
    assert!(
        err["message"].as_str().unwrap().contains("no file"),
        "{err}"
    );
    assert_eq!(ws.json(&["show", "PROJ-142"])["file"], Value::Null);
    ws.json_err(&["show", "PROJ-999", "--path"], 2);
}

/// The field report: a person's id came out of `grep -rhoE` over the files,
/// and a renamed person came out twice. `users` is one row per id, under the
/// newest name, with the mention to paste.
#[test]
fn users_lists_one_entry_per_person_under_their_newest_name() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);

    let all = ws.json(&["users"]);
    assert_eq!(all["schema"], "jired/users/1");
    let ilya = all["users"]
        .as_array()
        .unwrap()
        .iter()
        .find(|u| u["account_id"] == "5b10a2ilya")
        .expect("the assignee is known");
    assert_eq!(ilya["display_name"], "Ilya A");
    assert_eq!(ilya["mention"], "@[Ilya A](5b10a2ilya)");

    // Jane is renamed on the server; the newer name wins, the old one stays
    // findable.
    ws.mutate_remote("PROJ-150", |issue| {
        issue["fields"]["assignee"] = serde_json::json!({
            "accountId": "5b1af9jane", "displayName": "Jane Doe-Smith"
        });
    });
    ws.json(&["pull"]);
    let jane = ws.json(&["users", "jane d"]);
    assert_eq!(jane["count"], 1, "{jane}");
    let jane = &jane["users"][0];
    assert_eq!(jane["display_name"], "Jane Doe-Smith");
    assert_eq!(jane["mention"], "@[Jane Doe-Smith](5b1af9jane)");
    assert_eq!(jane["also_known_as"][0], "Jane D");

    assert_eq!(ws.json(&["users", "nobody-here"])["count"], 0);
}

/// Working out what `customfield_10015` is meant reading `field_meta` out of
/// the private state database.
#[test]
fn fields_names_what_the_instance_calls_its_fields() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);

    let out = ws.json(&["fields"]);
    assert_eq!(out["schema"], "jired/fields/1");
    assert!(out["count"].as_u64().unwrap() > 0, "{out}");

    let story = ws.json(&["fields", "story"]);
    let ids: Vec<&str> = story["fields"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["id"].as_str().unwrap())
        .collect();
    assert!(ids.contains(&"customfield_10016"), "{story}");
    assert_eq!(story["fields"][0]["name"], "Story Points");

    // And only the instance's own, when that is the question.
    let custom = ws.json(&["fields", "--custom"]);
    assert!(custom["fields"]
        .as_array()
        .unwrap()
        .iter()
        .all(|f| f["id"].as_str().unwrap().starts_with("customfield_")));
}

/// `duedate` is a Jira system field, and `custom:` only ever collected
/// `customfield_*` — so a due date could be written and never read back, and
/// the docs gave it no home at all.
#[test]
fn a_due_date_is_read_from_the_server_and_sent_back() {
    let ws = Ws::new();
    ws.mutate_remote("PROJ-142", |issue| {
        issue["fields"]["duedate"] = "2026-09-14".into();
    });
    ws.init();
    ws.json(&["pull"]);

    let text = ws.read(ISSUE_FILE);
    assert!(
        text.contains("due_date:"),
        "no due date in the file:\n{text}"
    );
    // And it reads back as the string it was written as, rather than YAML
    // quietly turning an unquoted date into something else.
    assert_eq!(
        ws.json(&["show", "PROJ-142"])["frontmatter"]["due_date"],
        "2026-09-14"
    );
    assert!(
        ws.json(&["diff"])["issues"].as_array().unwrap().is_empty(),
        "a pulled file is not a local edit"
    );

    ws.json(&[
        "set",
        "--field",
        "due_date",
        "--value",
        "2026-09-30",
        "PROJ-142",
    ]);
    let dry = ws.json(&["push", "--dry-run"]);
    assert_eq!(
        dry["results"][0]["ops"][0]["request"]["body"]["fields"]["duedate"],
        "2026-09-30"
    );

    // A date Jira would refuse is refused here, before the push.
    let err = ws.json_err(
        &[
            "set",
            "--field",
            "due_date",
            "--value",
            "next tuesday",
            "PROJ-142",
        ],
        2,
    );
    assert!(
        err["message"].as_str().unwrap().contains("not a date"),
        "{err}"
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

/// An issue whose checklist is empty has an empty section — not the plugin's
/// own `toString()` of an empty list, which is what reading the dump as text
/// used to put there, checkbox and all.
#[test]
fn an_empty_checklist_is_an_empty_section_rather_than_the_plugins_dump() {
    let ws = Ws::of(Deployment::DataCenter);
    ws.init();
    ws.json(&["pull"]);
    let text = ws.read("DC-0078. Document the LDAP rollback plan.md");

    assert_eq!(section(&text, "## Checklist").trim(), "", "{text}");
    assert!(!text.contains("_items="), "the dump leaked: {text}");
}

/// Ticking a box, adding an item, writing a note: the section is the plugin's
/// own syntax on the way out as well as on the way in.
#[test]
fn a_checklist_edit_is_pushed_as_the_plugins_own_syntax() {
    let ws = Ws::of(Deployment::DataCenter);
    ws.init();
    ws.json(&["pull"]);
    let file = Deployment::DataCenter.issue_file();

    let text = ws.read(file).replace(
        "- [ ] Update the connector config\n",
        "- [x] Update the connector config\n- [ ] `IN PROGRESS` Restart the pool\n  \
         after the connector\n",
    );
    ws.write(file, &text);

    // Unpushed work, and `status` says so — it used to call the file clean
    // because there was nothing push could do about it.
    let status = ws.json(&["status"]);
    let dirty = status["dirty"]
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["key"] == "DC-77")
        .expect("the checklist edit is unpushed work");
    assert!(
        dirty["fields"]
            .as_array()
            .unwrap()
            .contains(&Value::from("checklist")),
        "{status}"
    );

    // One write of the plugin's property, carrying the text and nothing
    // derived from it — and no `PUT /issue` at all.
    let dry = ws.json(&["push", "--dry-run"]);
    let ops = dry["results"][0]["ops"].as_array().unwrap();
    assert_eq!(ops.len(), 1, "{dry}");
    assert_eq!(ops[0]["kind"], "update_checklist", "{dry}");
    assert_eq!(
        ops[0]["request"]["path"], "/issue/{key}/properties/com.railsware.SmartChecklist.checklist",
        "{dry}"
    );
    assert_eq!(
        ops[0]["request"]["body"],
        Value::from(
            "~ Generate the replacement secret\n\
             > Coordinate the window with @jane\n\
             + Update the connector config\n\
             ~ Restart the pool\n\
             > after the connector\n\
             + Announce the freeze\n"
        ),
        "{dry}"
    );

    // And the file settles: what comes back is what was sent, so a second push
    // has nothing left to do.
    assert!(ws.json(&["push"])["results"][0]["ok"]
        .as_bool()
        .unwrap_or(false));
    let after = ws.read(file);
    assert!(
        after.contains("- [x] Update the connector config"),
        "{after}"
    );
    assert!(
        after.contains("- [ ] `IN PROGRESS` Restart the pool\n  after the connector"),
        "{after}"
    );
    assert!(ws.json(&["status"])["dirty"].as_array().unwrap().is_empty());
}

/// Items typed into an issue that has none — the round trip the plugin's web UI
/// used to be the only way to finish.
#[test]
fn a_checklist_can_be_written_onto_an_issue_that_had_none() {
    let ws = Ws::of(Deployment::DataCenter);
    ws.init();
    ws.json(&["pull"]);
    let file = "DC-0078. Document the LDAP rollback plan.md";

    let text = ws.read(file).replace(
        "## Checklist\n",
        "## Checklist\n\n### Analysis\n\n- [ ] Draft the plan\n- [x] Book the window\n",
    );
    ws.write(file, &text);

    let dry = ws.json(&["push", "DC-78", "--dry-run"]);
    assert_eq!(
        dry["results"][0]["ops"][0]["request"]["body"],
        Value::from("# Analysis\n- Draft the plan\n+ Book the window\n"),
        "{dry}"
    );

    ws.json(&["push", "DC-78"]);
    let after = ws.read(file);
    assert_eq!(
        section(&after, "## Checklist").trim(),
        "### Analysis\n\n- [ ] Draft the plan\n- [x] Book the window",
        "{after}"
    );

    // And emptying the section empties the checklist: the property is given
    // an empty text, which is a checklist with nothing in it.
    ws.write(
        file,
        &after.replace(
            "### Analysis\n\n- [ ] Draft the plan\n- [x] Book the window\n",
            "",
        ),
    );
    let dry = ws.json(&["push", "DC-78", "--dry-run"]);
    assert_eq!(dry["results"][0]["ops"][0]["kind"], "update_checklist");
    assert_eq!(
        dry["results"][0]["ops"][0]["request"]["body"],
        Value::from(""),
        "{dry}"
    );
    ws.json(&["push", "DC-78"]);
    assert_eq!(section(&ws.read(file), "## Checklist").trim(), "");
}

/// The round trip the report was about: an issue created from the CLI arrives
/// with its checklist. The create call cannot carry it — a plugin's field is not
/// on an instance's create screen — so it is a follow-up, listed in the plan,
/// and on Data Center it is the plugin's property, which no screen governs.
#[test]
fn a_new_issue_gets_its_checklist_after_the_create() {
    let ws = Ws::of(Deployment::DataCenter);
    ws.init();
    ws.json(&["pull"]);

    let report = ws.json(&["create", "--summary", "Wire the audit log"]);
    let file = report["file"].as_str().unwrap().to_string();
    let text = ws.read(&file).replace(
        "## Checklist\n",
        "## Checklist\n\n- [ ] Draft the plan\n- [ ] Review it\n",
    );
    ws.write(&file, &text);

    let dry = ws.json(&["push", "--dry-run"]);
    let ops = dry["results"][0]["ops"].as_array().unwrap();
    let kinds: Vec<&str> = ops.iter().map(|o| o["kind"].as_str().unwrap()).collect();
    assert_eq!(kinds, ["create_issue", "update_checklist"], "{dry}");
    assert!(
        ops[0]["request"]["body"]["fields"]
            .get("customfield_10206")
            .is_none(),
        "the create screen is not asked to carry it: {dry}"
    );
    assert_eq!(
        ops[1]["request"]["body"],
        Value::from("- Draft the plan\n- Review it\n"),
        "{dry}"
    );

    let push = ws.json(&["push"]);
    let new_key = push["results"][0]["new_key"].as_str().unwrap();
    let number = new_key.rsplit('-').next().unwrap();
    let created = ws.read(&format!("DC-{number:0>4}. Wire the audit log.md"));
    assert_eq!(
        section(&created, "## Checklist").trim(),
        "- [ ] Draft the plan\n- [ ] Review it",
        "{created}"
    );
}

/// The instance this was reported against has Smart Checklist's text field on
/// no edit screen at all, so `PUT /issue` refused every checklist with a 400 —
/// and, once screens were checked, `push` refused it before sending. The
/// property is not a field, and the screen has no say in it.
#[test]
fn a_checklist_is_written_where_no_edit_screen_carries_its_field() {
    let ws = Ws::of(Deployment::DataCenter);
    ws.write_fixture(
        "editmeta-DC-77.json",
        serde_json::json!({"fields": {
            "summary": {"name": "Summary", "required": true, "operations": ["set"]},
            "description": {"name": "Description", "required": false, "operations": ["set"]},
        }}),
    );
    ws.init();
    ws.json(&["pull"]);
    let file = Deployment::DataCenter.issue_file();
    ws.write(
        file,
        &ws.read(file).replace(
            "- [ ] Update the connector config\n",
            "- [x] Update the connector config\n",
        ),
    );

    let dry = ws.json(&["push", "--dry-run"]);
    assert_eq!(dry["results"][0]["ok"], true, "{dry}");
    assert_eq!(dry["results"][0]["ops"][0]["kind"], "update_checklist");

    let push = ws.json(&["push"]);
    assert_eq!(push["summary"]["ok"], 1, "{push}");
    assert!(
        ws.read(file).contains("- [x] Update the connector config"),
        "{}",
        ws.read(file)
    );
    assert!(ws.json(&["status"])["dirty"].as_array().unwrap().is_empty());
}

/// A property takes whatever it is given, read or not — another checklist app
/// behind the same field name, Smart Checklist switched off for the project. A
/// write the plugin did not take up is not reported as one it did.
#[test]
fn a_checklist_property_nothing_reads_fails_and_keeps_the_edit() {
    let ws = Ws::of(Deployment::DataCenter);
    ws.write_fixture(
        "ignored.json",
        serde_json::json!(["PUT /issue/DC-77/properties/com.railsware.SmartChecklist.checklist"]),
    );
    ws.init();
    // Nothing is coming, so waiting for it would only slow the test down.
    ws.json(&["config", "set", "checklist.apply_wait", "0"]);
    ws.json(&["pull"]);
    let file = Deployment::DataCenter.issue_file();
    let edited = ws.read(file).replace(
        "- [ ] Update the connector config\n",
        "- [x] Update the connector config\n",
    );
    ws.write(file, &edited);

    let push = ws.json_code(&["push"], 10);
    let result = &push["results"][0];
    assert_eq!(result["ok"], false, "{push}");
    assert_eq!(result["ops"][0]["status"], "failed", "{push}");
    assert!(
        result["ops"][0]["error"]
            .as_str()
            .unwrap()
            .contains("com.railsware.SmartChecklist.checklist"),
        "{push}"
    );
    assert_eq!(ws.read(file), edited, "the edit is still in the file");
}

/// Smart Checklist applies its property on its own schedule — a minute later,
/// on the instance that reported it. Push re-reads until the checklist shows,
/// rather than calling a write that is only late unapplied.
#[test]
fn a_checklist_the_plugin_applies_late_is_waited_for() {
    let ws = Ws::of(Deployment::DataCenter);
    ws.write_fixture(
        "late.json",
        serde_json::json!({"PUT /issue/DC-77/properties/com.railsware.SmartChecklist.checklist": 2}),
    );
    ws.init();
    ws.json(&["pull"]);
    let file = Deployment::DataCenter.issue_file();
    ws.write(
        file,
        &ws.read(file).replace(
            "- [ ] Update the connector config\n",
            "- [x] Update the connector config\n",
        ),
    );

    let push = ws.json(&["push"]);
    assert_eq!(push["summary"]["ok"], 1, "{push}");
    assert_eq!(push["results"][0]["ops"][0]["status"], "done", "{push}");
    let status = ws.json(&["status"]);
    assert!(status["dirty"].as_array().unwrap().is_empty(), "{status}");
    assert!(
        status["pending_ops"].as_array().unwrap().is_empty(),
        "{status}"
    );
}

/// Later than push waits for: the op fails, honestly, and the edit stays. When
/// the plugin does catch up, the pull that brings its copy in finds the same
/// checklist on both sides — no conflict — and the op it leaves behind is spent.
#[test]
fn a_checklist_applied_after_the_wait_is_taken_in_by_the_next_pull() {
    let ws = Ws::of(Deployment::DataCenter);
    ws.write_fixture(
        "late.json",
        serde_json::json!({"PUT /issue/DC-78/properties/com.railsware.SmartChecklist.checklist": 1}),
    );
    ws.init();
    ws.json(&["config", "set", "checklist.apply_wait", "0"]);
    ws.json(&["pull"]);
    let file = "DC-0078. Document the LDAP rollback plan.md";
    let edited = ws.read(file).replace(
        "## Checklist\n",
        "## Checklist\n\n- [ ] Draft the plan\n  Keep the window short\n",
    );
    ws.write(file, &edited);

    let push = ws.json_code(&["push", "DC-78"], 10);
    assert_eq!(push["results"][0]["ok"], false, "{push}");
    assert!(
        push["results"][0]["hint"]
            .as_str()
            .unwrap()
            .contains("jired pull"),
        "{push}"
    );
    assert_eq!(ws.read(file), edited);

    let pull = ws.json(&["pull"]);
    assert!(pull["conflicts"].as_array().unwrap().is_empty(), "{pull}");
    assert_eq!(
        section(&ws.read(file), "## Checklist"),
        section(&edited, "## Checklist"),
        "the server's copy is the file's"
    );
    let status = ws.json(&["status"]);
    assert!(status["dirty"].as_array().unwrap().is_empty(), "{status}");
    assert!(
        status["pending_ops"].as_array().unwrap().is_empty(),
        "{status}"
    );
}

/// A note written on an issue that has none comes back as it was written. With
/// nothing on the server to learn the spelling from, jired fell back to `>>`,
/// which the plugin reads as a nested quote and stores as `> > note` — every
/// note on a newly created issue grew a stray `> `.
#[test]
fn a_note_on_an_issue_with_none_round_trips() {
    let ws = Ws::of(Deployment::DataCenter);
    ws.init();
    ws.json(&["pull"]);
    let file = "DC-0078. Document the LDAP rollback plan.md";
    let edited = ws.read(file).replace(
        "## Checklist\n",
        "## Checklist\n\n- [ ] Draft the plan\n  Keep the window short\n",
    );
    ws.write(file, &edited);

    let dry = ws.json(&["push", "DC-78", "--dry-run"]);
    assert_eq!(
        dry["results"][0]["ops"][0]["request"]["body"],
        Value::from("- Draft the plan\n> Keep the window short\n"),
        "{dry}"
    );
    let push = ws.json(&["push", "DC-78"]);
    assert_eq!(push["summary"]["ok"], 1, "{push}");
    assert_eq!(
        section(&ws.read(file), "## Checklist").trim(),
        "- [ ] Draft the plan\n  Keep the window short"
    );
}

/// Item text that starts in brackets survives the round trip. Sent bare, the
/// plugin read `[03.11 18:00]` as the name of a custom status, had none such,
/// and dropped it: the server held `Check the release`, push failed waiting
/// for a checklist that was never coming, and the next pull met the plugin's
/// copy as a conflict.
#[test]
fn an_item_starting_in_brackets_keeps_them_on_the_server() {
    let ws = Ws::of(Deployment::DataCenter);
    ws.init();
    ws.json(&["config", "set", "checklist.apply_wait", "0"]);
    ws.json(&["pull"]);
    let file = "DC-0078. Document the LDAP rollback plan.md";
    let edited = ws.read(file).replace(
        "## Checklist\n",
        "## Checklist\n\n- [ ] [03.11 18:00] Check the release\n",
    );
    ws.write(file, &edited);

    let dry = ws.json(&["push", "DC-78", "--dry-run"]);
    assert_eq!(
        dry["results"][0]["ops"][0]["request"]["body"],
        Value::from("- \\[03.11 18:00\\] Check the release\n"),
        "{dry}"
    );
    let push = ws.json(&["push", "DC-78"]);
    assert_eq!(push["summary"]["ok"], 1, "{push}");
    assert_eq!(
        section(&ws.read(file), "## Checklist").trim(),
        "- [ ] [03.11 18:00] Check the release",
        "read back without the escape"
    );
    let pull = ws.json(&["pull"]);
    assert!(pull["conflicts"].as_array().unwrap().is_empty(), "{pull}");
    assert!(ws.json(&["status"])["dirty"].as_array().unwrap().is_empty());
}

/// The report's second half. A checklist the server would not take, on an
/// issue being created: the issue exists, so the create stands — but the
/// result is a failure, the checklist is still in the renamed file, `status`
/// sees it, and the op waits in `pending_ops`. It used to be `ok: true` with an
/// empty `## Checklist` and nothing anywhere to say what had been lost.
#[test]
fn a_new_issues_checklist_the_server_refused_stays_in_its_file() {
    let ws = Ws::of(Deployment::DataCenter);
    ws.init();
    ws.json(&["pull"]);
    let report = ws.json(&["create", "--summary", "Wire the audit log"]);
    let file = report["file"].as_str().unwrap().to_string();
    ws.write(
        &file,
        &ws.read(&file).replace(
            "## Checklist\n",
            "## Checklist\n\n- [ ] Draft the plan\n- [ ] Review it\n",
        ),
    );
    // The key the fixture will hand out, so the refusal can be scripted for
    // it: ids count from 90001 in a fresh fixture.
    ws.write_fixture(
        "faults.json",
        serde_json::json!({
            "PUT /issue/DC-90001/properties/com.railsware.SmartChecklist.checklist": {
                "kind": "generic",
                "message": "HTTP 400: Field 'customfield_10206' cannot be set."
            }
        }),
    );

    let push = ws.json_code(&["push"], 1);
    assert_eq!(push["summary"]["ok"], 0, "{push}");
    assert_eq!(push["summary"]["failed"], 1, "{push}");
    let result = &push["results"][0];
    assert_eq!(result["ok"], false, "{push}");
    assert_eq!(result["new_key"], "DC-90001", "{push}");
    assert_eq!(result["ops"][0]["status"], "done", "{push}");
    assert_eq!(result["ops"][1]["kind"], "update_checklist", "{push}");
    assert_eq!(result["ops"][1]["status"], "failed", "{push}");

    let created = ws.read("DC-90001. Wire the audit log.md");
    assert_eq!(
        section(&created, "## Checklist").trim(),
        "- [ ] Draft the plan\n- [ ] Review it",
        "{created}"
    );
    let status = ws.json(&["status"]);
    assert!(
        status["dirty"]
            .as_array()
            .unwrap()
            .iter()
            .any(|d| d["key"] == "DC-90001"
                && d["fields"]
                    .as_array()
                    .unwrap()
                    .contains(&Value::from("checklist"))),
        "{status}"
    );
    assert!(
        status["pending_ops"]
            .as_array()
            .unwrap()
            .iter()
            .any(|op| op["issue_key"] == "DC-90001" && op["kind"] == "update_checklist"),
        "{status}"
    );

    // Once the server takes it, `--resume` finishes the job as an update.
    ws.write_fixture("faults.json", serde_json::json!({}));
    let resumed = ws.json(&["push", "--resume"]);
    assert_eq!(resumed["summary"]["ok"], 1, "{resumed}");
    assert_eq!(resumed["results"][0]["key"], "DC-90001", "{resumed}");
    assert_eq!(resumed["results"][0]["ops"][0]["kind"], "update_checklist");
    let status = ws.json(&["status"]);
    assert!(status["dirty"].as_array().unwrap().is_empty(), "{status}");
    assert!(
        status["pending_ops"].as_array().unwrap().is_empty(),
        "{status}"
    );
}

/// The field belongs to a plugin, and jired writes it only where it can prove
/// it reads the whole of it: rendering the *unedited* checklist has to
/// reproduce the text the server holds. Here it cannot — the item is mandatory,
/// which the section has no way to show — so the edit is refused out loud
/// rather than sent with the flag quietly dropped.
#[test]
fn a_checklist_jired_cannot_reproduce_is_refused_rather_than_flattened() {
    let ws = Ws::of(Deployment::DataCenter);
    ws.init();
    ws.json(&["pull"]);
    let file = Deployment::DataCenter.issue_file();

    // The server grows a construct this jired does not model.
    ws.mutate_remote("DC-77", |issue| {
        issue["fields"]["customfield_10206"] =
            Value::from("- Update the connector config\n! Sign it off\n");
    });
    ws.json(&["pull"]);

    let text = ws
        .read(file)
        .replace("- [ ] Update the connector config", "- [x] Done it");
    ws.write(file, &text);

    let dry = ws.json(&["push", "--dry-run"]);
    let warning = dry["warnings"][0].as_str().unwrap_or_default();
    assert!(warning.contains("Checklist"), "{dry}");
    assert!(warning.contains("not modelled"), "{dry}");
    assert!(
        dry["results"].as_array().unwrap().is_empty(),
        "nothing else was planned: {dry}"
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

/// `diff`, `status` and `push` answer one question about one working copy, and
/// they have to answer it alike. A file whose bytes are exactly what the last
/// pull wrote holds no edit — but base re-rendered under today's rules can
/// differ from it anyway: a field the renderer has learned since the file was
/// written, or `ignored_custom_fields` changed underneath it. `diff` used to
/// report that drift as a sendable change while `status` said clean and `push`
/// sent nothing, and telling which of the three was lying took a dry run.
#[test]
fn an_untouched_file_diffs_clean_however_base_renders_today() {
    let ws = Ws::of(Deployment::DataCenter);
    ws.init();
    ws.json(&["pull"]);

    // The drift, without running `doctor` to reconcile the file: base now
    // renders without the field, the file still carries it, the hash is the
    // one the pull recorded.
    ws.json(&[
        "config",
        "add",
        "ignored_custom_fields",
        "customfield_10300",
    ]);

    let status = ws.json(&["status"]);
    assert!(status["dirty"].as_array().unwrap().is_empty(), "{status}");
    let diff = ws.json(&["diff"]);
    assert_eq!(
        diff["issues"].as_array().map(Vec::len),
        Some(0),
        "diff must not report a change no push will send: {diff}"
    );
    let dry = ws.json(&["push", "--dry-run"]);
    assert!(dry["results"].as_array().unwrap().is_empty(), "{dry}");

    // A real edit still shows: the hash is not a blindfold, it is the answer
    // to "did anyone touch this file".
    let file = Deployment::DataCenter.issue_file();
    let text = ws.read(file).replace("priority: High", "priority: Low");
    ws.write(file, &text);
    let diff = ws.json(&["diff"]);
    assert!(
        diff["issues"][0]["fields"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f["field"] == "priority"),
        "{diff}"
    );
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
    // And says so, the way `status` would, so nobody has to ask.
    assert_eq!(again["state"]["clean"], true, "{again}");
    let text = ws.cmd(&["pull"]).output().unwrap();
    let text = String::from_utf8_lossy(&text.stdout);
    assert!(
        text.trim_end().ends_with("nothing to push, no conflicts"),
        "{text}"
    );
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

    // `diff` reports the same change, so the two never disagree about
    // whether there is anything to push.
    let diff = ws.json(&["diff"]);
    let row = &diff["issues"][0]["fields"][0];
    assert_eq!(diff["issues"][0]["key"], "PROJ-142", "{diff}");
    assert_eq!(row["field"], "status");
    assert_eq!(row["from"], "In Progress");
    assert_eq!(row["to"], "Done");
    assert_eq!(row["origin"], "board-move");
    let text = ws.cmd(&["status"]).output().unwrap();
    let text = String::from_utf8_lossy(&text.stdout);
    assert!(text.contains("move the link back"), "{text}");

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

/// Jira names every board of a project after it — `DELTA board` for the Scrum
/// one and the Kanban one — and the two shared a folder, so each judged the
/// other's cards: a fresh pull reported `To Do → To Do` for every card the
/// second board maps elsewhere.
#[test]
fn two_boards_with_one_name_stage_nothing_after_a_pull() {
    let ws = Ws::new();
    std::fs::write(
        ws.fixtures.join("boards.json"),
        r#"[
          { "id": "12", "name": "Platform Board", "type": "scrum", "project_key": "PROJ" },
          { "id": "13", "name": "Platform Board", "type": "kanban", "project_key": "PROJ" }
        ]"#,
    )
    .unwrap();
    // The second board's `To Do` holds a status none of the issues has.
    std::fs::write(
        ws.fixtures.join("board-13-columns.json"),
        r#"[
          { "name": "To Do", "status_ids": ["10000"] },
          { "name": "In Progress", "status_ids": ["3"] },
          { "name": "Done", "status_ids": ["5"] }
        ]"#,
    )
    .unwrap();
    ws.init();
    ws.json(&["pull"]);

    let status = ws.json(&["status"]);
    assert!(
        status["staged_transitions"].as_array().unwrap().is_empty(),
        "{status}"
    );
    assert!(
        !ws.json(&["push", "--dry-run"])["results"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["ops"]
                .as_array()
                .unwrap()
                .iter()
                .any(|o| o["kind"] == "transition")),
        "push must not plan a transition nobody asked for"
    );
    // Each board has a folder of its own, and a card only on the board that
    // maps its status.
    let todo = "To Do/PROJ-0150. Rotate IdP certificate.md";
    assert!(
        std::fs::symlink_metadata(ws.path().join("boards/Platform Board (12)").join(todo)).is_ok()
    );
    assert!(
        std::fs::symlink_metadata(ws.path().join("boards/Platform Board (13)").join(todo)).is_err()
    );
}

/// A link in the column named after the issue's own status is where the user
/// expects it, whatever the column's status mapping says: staging a transition
/// from a status to itself only asks Jira for one the workflow does not offer.
#[test]
fn a_card_in_the_column_named_after_its_status_stages_nothing() {
    let ws = Ws::new();
    std::fs::write(
        ws.fixtures.join("board-12-columns.json"),
        r#"[
          { "name": "To Do", "status_ids": ["10000"] },
          { "name": "In Progress", "status_ids": ["1", "3"] },
          { "name": "Done", "status_ids": ["5"] }
        ]"#,
    )
    .unwrap();
    ws.init();
    ws.json(&["pull"]);

    let name = "PROJ-0150. Rotate IdP certificate.md";
    let from = ws
        .path()
        .join("boards/Platform Board/In Progress")
        .join(name);
    let to_dir = ws.path().join("boards/Platform Board/To Do");
    std::fs::create_dir_all(&to_dir).unwrap();
    std::fs::rename(&from, to_dir.join(name)).unwrap();

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

/// The issues named bound what the pull writes, not only what it fetches. An
/// issue an earlier fetch had left behind was written too: `pull TSK-793`
/// reported eighteen others as updated.
#[test]
fn pull_with_issues_named_writes_only_those() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);
    let other = ws.read(ISSUE_FILE);
    for key in ["PROJ-142", "PROJ-143"] {
        ws.mutate_remote(key, |issue| {
            issue["fields"]["summary"] = "Renamed on the server".into();
        });
    }
    // Both are behind now; only one is asked for.
    ws.json(&["fetch"]);

    let report = ws.json(&["pull", "PROJ-143"]);
    let updated: Vec<&str> = report["updated"]
        .as_array()
        .unwrap()
        .iter()
        .map(|u| u["key"].as_str().unwrap())
        .collect();
    assert_eq!(updated, ["PROJ-143"], "{report}");
    assert_eq!(ws.read(ISSUE_FILE), other, "PROJ-142 was not asked for");

    // And the plain pull still brings it in.
    let report = ws.json(&["pull"]);
    assert_eq!(report["updated"][0]["key"], "PROJ-142", "{report}");
}

/// A recorded conflict needs local edits to stand on. One whose file holds
/// none — given up by hand, or recorded by a version that conflicted where
/// this one merges — is cleared by the next pull instead of standing in
/// `status` for ever.
#[test]
fn a_conflict_over_a_file_with_no_local_edits_is_cleared_by_the_next_pull() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);
    let clean = ws.read(ISSUE_FILE);
    conflicted(&ws);
    assert_eq!(
        ws.json(&["status"])["conflicts"].as_array().unwrap().len(),
        1
    );

    ws.write(ISSUE_FILE, &clean);
    ws.json(&["pull"]);
    let status = ws.json(&["status"]);
    assert!(
        status["conflicts"].as_array().unwrap().is_empty(),
        "{status}"
    );
    assert!(ws.read("PROJ-0142. Theirs.md").contains("summary: Theirs"));
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

/// The safety check `jired diff` exists to be is only useful if it shows the
/// same set of changes the push will act on. Sprint history — the entries
/// before the last — is the server's record, so a file that differs only there
/// is not a difference: `status`, `push` and `diff` all have to agree.
#[test]
fn diff_leaves_out_sprint_history_it_could_never_push() {
    let ws = Ws::new();
    ws.mutate_remote("PROJ-142", |issue| {
        issue["fields"]["customfield_10020"] = serde_json::json!([
            {"id": 41, "name": "Sprint 41", "state": "closed"},
            {"id": 42, "name": "Sprint 42", "state": "active"},
        ]);
    });
    ws.init();
    ws.json(&["pull"]);

    // A file that names only the sprint the issue is in now — what every file
    // written before jired kept the whole list looks like.
    let text = ws
        .read(ISSUE_FILE)
        .replace("sprint:\n- Sprint 41\n- Sprint 42", "sprint: Sprint 42");
    ws.write(ISSUE_FILE, &text);

    let status = ws.json(&["status"]);
    assert_eq!(status["dirty"].as_array().map(|d| d.len()), Some(0));
    let diff = ws.json(&["diff"]);
    assert_eq!(
        diff["issues"].as_array().map(|i| i.len()),
        Some(0),
        "diff must not report what push cannot send: {diff}"
    );

    // With a real change on the same issue the row is kept, marked, and left
    // out of the counts.
    ws.write(
        ISSUE_FILE,
        &text.replace("summary: Fix login flow", "summary: Fix the login flow"),
    );
    let diff = ws.json(&["diff"]);
    let fields = diff["issues"][0]["fields"].as_array().unwrap();
    let sprint = fields
        .iter()
        .find(|f| f["field"] == "sprint")
        .expect("shown for context");
    assert_eq!(sprint["sendable"], false);
    let summary = fields.iter().find(|f| f["field"] == "summary").unwrap();
    assert_eq!(summary["sendable"], true);
    assert_eq!(diff["stat"]["fields"], 1, "only the sendable field counts");

    // And what push plans matches, field for field.
    let dry = ws.json(&["push", "--dry-run"]);
    let ops = dry["results"][0]["ops"].as_array().unwrap();
    assert_eq!(ops.len(), 1);
    assert_eq!(ops[0]["kind"], "update_fields");
}

/// Grouping folders are views over base, and a push moves base. The sprint
/// somebody just made is the one they want to look at, and it has to be there
/// as soon as the push that filled it returns — a pull that writes no file
/// used to leave the folder tree describing where the issue used to be.
#[test]
fn a_push_that_moves_an_issue_rebuilds_the_folder_it_moved_into() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);

    // A sprint nobody has started yet: Jira calls it future, and that is where
    // the next sprint's planning happens.
    let created = ws.json(&["sprint", "create", "--board", "12", "Sprint 44"]);
    assert_eq!(created["sprint"]["state"], "future");

    let text = ws
        .read(ISSUE_FILE)
        .replace("sprint: Sprint 42", "sprint: Sprint 44");
    ws.write(ISSUE_FILE, &text);
    let push = ws.json(&["push"]);
    assert_eq!(push["results"][0]["ops"][0]["kind"], "move_sprint");
    assert!(
        push["groups"].is_object(),
        "the push rebuilt the folders: {push}"
    );

    assert!(
        ws.path()
            .join("sprints/Sprint 44")
            .join(ISSUE_FILE)
            .exists(),
        "a future sprint is a folder like any other, and the push filled it"
    );
    assert!(
        !ws.path().join("sprints/Sprint 42").exists(),
        "and the sprint it left no longer claims it"
    );
}

/// A push that only says something about an issue leaves the folder tree alone,
/// so a card dragged between board columns and not yet pushed survives it.
#[test]
fn a_push_that_moves_nothing_leaves_the_folders_alone() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);
    let text = ws
        .read(ISSUE_FILE)
        .replace("## Comments", "## Comments\n\n### [new]\n\nOn it.\n");
    ws.write(ISSUE_FILE, &text);
    let push = ws.json(&["push"]);
    assert_eq!(push["results"][0]["ops"][0]["kind"], "create_comment");
    assert!(
        push["groups"].is_null(),
        "nothing moved, nothing rebuilt: {push}"
    );
}

/// A subtask whose summary ends in a space used to put `diff` and `status`
/// permanently at odds: `## Children` is line-based, so reading the file back
/// trimmed the title the snapshot still held, and every issue with such a child
/// was reported as modified while `status` called the workspace clean and
/// `push` had nothing to send. No user edit was involved, and the section is
/// not one a push can act on anyway.
#[test]
fn a_child_summary_ending_in_a_space_is_not_a_change() {
    let ws = Ws::new();
    // Both sides, as Jira holds it: the sub-task's own summary is what its
    // parent's list is kept in step with.
    ws.mutate_remote("PROJ-142", |issue| {
        issue["fields"]["subtasks"][0]["fields"]["summary"] =
            "Реализовать иерархию ".to_string().into();
    });
    ws.mutate_remote("PROJ-143", |issue| {
        issue["fields"]["summary"] = "Реализовать иерархию ".to_string().into();
    });
    ws.init();
    ws.json(&["pull"]);

    // What the three of them say has to be the same thing.
    assert!(
        ws.json(&["status"])["dirty"].as_array().unwrap().is_empty(),
        "nothing was edited"
    );
    let diff = ws.json(&["diff"]);
    assert!(
        diff["issues"].as_array().unwrap().is_empty(),
        "the working copy matches its own base:\n{diff}"
    );
    assert_eq!(diff["stat"]["issues"], 0);
    let push = ws.json(&["push", "--dry-run"]);
    assert!(push["results"].as_array().unwrap().is_empty(), "{push}");

    // The title is there, trimmed, and a second pull does not move it.
    let text = ws.read(ISSUE_FILE);
    assert!(
        text.contains("- [ ] PROJ-143 Реализовать иерархию\n"),
        "{text}"
    );
    ws.json(&["pull"]);
    assert_eq!(ws.read(ISSUE_FILE), text, "the second pull rewrote nothing");
}

/// Three boards over one project's backlog commonly share a sprint pool: every
/// one of them answers with the same sprints, and the sprints name the board
/// that owns them rather than the board that was asked. Making somebody choose
/// between three identical answers is a question not worth asking.
#[test]
fn sprints_needs_no_board_when_every_board_gives_the_same_answer() {
    let ws = Ws::new();
    ws.write_fixture(
        "boards.json",
        serde_json::json!([
            {"id": "11", "name": "TSK board", "type": "scrum"},
            {"id": "12", "name": "sprints", "type": "scrum"},
            {"id": "13", "name": "TSK", "type": "scrum"},
        ]),
    );
    ws.init();

    let out = ws.json(&["sprints"]);
    let names: Vec<&str> = out["sprints"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"Sprint 42"), "{out}");
}

/// When the boards really do have different sprints there is a choice to make,
/// and the error makes it: it names every value that would work.
#[test]
fn sprints_asks_which_board_when_the_boards_disagree() {
    let ws = Ws::new();
    ws.write_fixture(
        "boards.json",
        serde_json::json!([
            {"id": "11", "name": "TSK board", "type": "scrum"},
            {"id": "12", "name": "sprints", "type": "scrum"},
        ]),
    );
    ws.write_fixture(
        "sprints-11.json",
        serde_json::json!([
            {"id": "77", "name": "Ops Sprint 7", "state": "active", "board_id": "11"},
        ]),
    );
    ws.init();

    let err = ws.json_err(&["sprints"], 7);
    assert_eq!(err["kind"], "missing_value");
    let hint = err["hint"].as_str().unwrap_or_default();
    assert!(
        hint.contains("TSK board (11)") && hint.contains("sprints (12)"),
        "{err}"
    );

    // Naming one answers it.
    let out = ws.json(&["sprints", "--board", "11"]);
    assert_eq!(out["sprints"][0]["name"], "Ops Sprint 7");
}

/// Data Center leaves the project out of the board listing, so a script cannot
/// tell which board belongs to the project it is working in. The board's own
/// configuration knows, and `jired boards` asks it.
#[test]
fn boards_says_which_project_each_board_belongs_to() {
    let ws = Ws::of(Deployment::DataCenter);
    ws.write_fixture(
        "boards.json",
        serde_json::json!([
            {"id": "11", "name": "TSK board", "type": "scrum"},
            {"id": "13", "name": "TSK", "type": "kanban"},
        ]),
    );
    ws.write_fixture(
        "board-11-config.json",
        serde_json::json!({"location": {"type": "project", "key": "DC", "name": "Data Centre"}}),
    );
    // A board that will not say stays null rather than failing the listing.
    ws.init();

    let out = ws.json(&["boards"]);
    assert_eq!(out["boards"][0]["project_key"], "DC");
    assert!(out["boards"][1]["project_key"].is_null(), "{out}");
}

/// The `location` in a board's configuration is a Cloud field: Data Center
/// leaves it out of that payload entirely and answers on the board's project
/// listing instead. Reading only the Cloud shape is why every board on a real
/// DC instance came back `project_key: null`, leaving a script no way to pick
/// one — and why the column never appeared on screen either.
#[test]
fn a_data_center_board_is_placed_from_its_project_listing() {
    let ws = Ws::of(Deployment::DataCenter);
    ws.write_fixture(
        "boards.json",
        serde_json::json!([
            {"id": "11", "name": "TSK board", "type": "scrum"},
            {"id": "12", "name": "sprints", "type": "scrum"},
            {"id": "13", "name": "TSK", "type": "kanban"},
        ]),
    );
    // What Data Center actually returns for a board configuration: the filter
    // and the columns, and no `location` anywhere in it.
    for id in ["11", "12", "13"] {
        ws.write_fixture(
            &format!("board-{id}-config.json"),
            serde_json::json!({
                "id": id,
                "name": "a board",
                "type": "scrum",
                "filter": {"id": "10100"},
            }),
        );
    }
    for id in ["11", "12"] {
        ws.write_fixture(
            &format!("board-{id}-projects.json"),
            serde_json::json!({"values": [{"id": "10000", "key": "DC", "name": "Data Centre"}]}),
        );
    }
    // A filter spanning two projects has no single answer, and naming the first
    // would be a guess a script would then act on.
    ws.write_fixture(
        "board-13-projects.json",
        serde_json::json!({"values": [{"key": "DC"}, {"key": "OPS"}]}),
    );
    ws.init();

    let out = ws.json(&["boards"]);
    assert_eq!(out["boards"][0]["project_key"], "DC");
    assert_eq!(out["boards"][1]["project_key"], "DC");
    assert!(out["boards"][2]["project_key"].is_null(), "{out}");

    // And on screen it is a column, which is what tells `TSK` from `TSK board`.
    let text = String::from_utf8(ws.cmd(&["boards"]).output().unwrap().stdout).unwrap();
    assert!(text.contains("DC"), "no project column in:\n{text}");
}

/// `sprint create` puts the new sprint in the local cache on the way out, so a
/// `sprint:` naming it resolves on the very next push — no fetch in between.
/// Without that, creating a sprint and using it would need a round trip nobody
/// would think to make.
#[test]
fn a_sprint_just_created_can_be_pushed_into_without_a_fetch() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);

    let created = ws.json(&["sprint", "create", "--board", "12", "Sprint 44"]);
    let id = created["sprint"]["id"].as_str().unwrap().to_string();

    let text = ws
        .read(ISSUE_FILE)
        .replace("sprint: Sprint 42", "sprint: Sprint 44");
    ws.write(ISSUE_FILE, &text);

    // The name resolves to the id the create returned, with nothing fetched in
    // between — the plan proves it before anything is sent.
    let dry = ws.json(&["push", "--dry-run"]);
    let op = &dry["results"][0]["ops"][0];
    assert_eq!(op["kind"], "move_sprint");
    assert_eq!(
        op["request"]["path"],
        format!("/rest/agile/1.0/sprint/{id}/issue")
    );
    assert_eq!(op["summary"], "Sprint 42 → Sprint 44");
    assert!(dry["warnings"].as_array().unwrap().is_empty(), "{dry}");

    let push = ws.json(&["push"]);
    assert_eq!(push["results"][0]["ops"][0]["status"], "done");
}

/// `fetch-spec` writes what the server says about its own API, under a name
/// that says which server and which version said it.
#[test]
fn fetch_spec_writes_one_file_per_described_rest_module() {
    let ws = Ws::of(Deployment::DataCenter);
    ws.init();

    let out = ws.path().join("specs");
    let report = ws.json(&["fetch-spec", "--output", out.to_str().unwrap()]);

    assert_eq!(report["version"], "10.7.4");
    assert_eq!(report["deployment"], "datacenter");

    let names: Vec<&str> = report["specs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["module"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["api-2", "agile-1.0"]);

    // The board and sprint commands live on the Agile API, which Atlassian
    // publishes no reference for at all: fetching only the platform WADL would
    // leave exactly the half that is hardest to look up.
    let agile = &report["specs"][1];
    assert_eq!(agile["path"], "/rest/agile/1.0/application.wadl");

    let file = out.join("jira.corp.example-datacenter-10.7.4-api-2.wadl");
    let text = std::fs::read_to_string(&file).unwrap_or_else(|e| {
        panic!("{} was not written: {e}", file.display());
    });
    assert!(text.starts_with("<?xml"), "not a WADL:\n{text}");
    // Named by the method rather than by its path: Jersey 2 puts the module in
    // the `base` URL and nests the resources under it, so the flat
    // `path="api/2/search"` an older Jira wrote does not occur here.
    assert!(text.contains(r#"id="searchUsingSearchRequest""#), "{text}");
    assert_eq!(report["specs"][0]["file"], file.display().to_string());
    assert_eq!(report["specs"][0]["bytes"], text.len());

    // A module this Jira does not describe is reported, not invented.
    assert_eq!(report["skipped"][0]["module"], "auth-1");
    assert!(!out
        .join("jira.corp.example-datacenter-10.7.4-auth-1.wadl")
        .exists());
}

/// Cloud serves no WADL, and the answer worth giving is where the document it
/// does have lives — not a bare refusal.
#[test]
fn fetch_spec_on_cloud_points_at_the_published_openapi() {
    let ws = Ws::new();
    ws.init();

    let err = ws.json_err(&["fetch-spec", "--output", "specs"], 10);
    assert_eq!(err["kind"], "unsupported_server");
    assert!(
        err["hint"]
            .as_str()
            .unwrap()
            .contains("developer.atlassian.com/cloud/jira/platform/swagger-v3.v3.json"),
        "{err}"
    );
    // Nothing was created for a fetch that could not happen.
    assert!(!ws.path().join("specs").exists());
}

// ---------------------------------------------------------------------------
// Watching
// ---------------------------------------------------------------------------

/// `--all` is the screen with every box ticked, for a terminal that has none.
/// It sends one request per issue not already watched, and none for the rest —
/// watching is a set, so re-adding buys a request and changes nothing.
#[test]
fn watch_all_watches_every_issue_in_the_workspace_once() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);

    let journal = ws.path().join("calls.jsonl");
    let out = ws
        .cmd(&["watch", "--all", "--json"])
        .env("JIRED_FIXTURE_JOURNAL", &journal)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let report: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(report["schema"], "jired/watch/1");
    assert_eq!(report["failed"], 0);
    assert_eq!(report["unwatched"], 0);
    let watched = report["watched"].as_u64().unwrap();
    assert!(watched >= 3, "the workspace's issues: {report}");
    let issues = report["issues"].as_array().unwrap();
    assert_eq!(issues.len() as u64, watched);
    assert!(issues.iter().all(|i| i["status"] == "watching"), "{report}");

    // The account the credential belongs to is what goes in the body, and the
    // request is the dedicated watcher route rather than a field edit.
    let calls: Vec<Value> = std::fs::read_to_string(&journal)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let added: Vec<&Value> = calls
        .iter()
        .filter(|c| c["method"] == "POST" && c["path"].as_str().unwrap().ends_with("/watchers"))
        .collect();
    assert_eq!(added.len() as u64, watched);
    assert_eq!(added[0]["body"], "5b10a2ilya");

    // Run again: the server already says yes, so nothing is sent at all.
    let journal2 = ws.path().join("calls2.jsonl");
    let out = ws
        .cmd(&["watch", "--all", "--json"])
        .env("JIRED_FIXTURE_JOURNAL", &journal2)
        .output()
        .unwrap();
    let again: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(again["watched"], 0, "already watching: {again}");
    let calls2 = std::fs::read_to_string(&journal2).unwrap_or_default();
    assert!(
        !calls2.contains("\"POST\""),
        "nothing should be sent twice:\n{calls2}"
    );
}

/// The screen is the command, so without a terminal to draw it on the answer is
/// the exit code for "a value is missing" and the flag that needs no screen.
#[test]
fn watch_without_a_terminal_says_how_to_do_it_without_one() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);

    let err = ws.json_err(&["watch"], 7);
    assert_eq!(err["kind"], "missing_value");
    assert!(
        err["hint"].as_str().unwrap_or_default().contains("--all"),
        "{err}"
    );
}

/// An empty workspace is a real answer rather than an error, and it says what
/// would fill it.
#[test]
fn watch_on_an_empty_workspace_says_so() {
    let ws = Ws::new();
    ws.init();
    let report = ws.json(&["watch", "--all"]);
    assert_eq!(report["watched"], 0);
    assert!(report["issues"].as_array().unwrap().is_empty());
}

/// `--quiet` already takes the narration; what it leaves behind is the reason
/// `--silent` exists — the warnings and the log floor. A run that worked says
/// nothing at all, and a run that failed still says why.
#[test]
fn silent_says_nothing_unless_the_command_fails() {
    let ws = Ws::new();
    ws.init();

    let out = ws.cmd(&["pull", "--silent"]).output().unwrap();
    assert!(out.status.success());
    assert!(
        out.stdout.is_empty() && out.stderr.is_empty(),
        "a silent pull that worked has nothing to say:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    // The warning `--quiet` still prints, which is the gap `--silent` fills.
    let loud = ws.cmd(&["pull", "--quiet", "--insecure"]).output().unwrap();
    assert!(
        String::from_utf8_lossy(&loud.stderr).contains("TLS certificate verification is disabled"),
        "--quiet keeps warnings, or this test is pinning the wrong thing"
    );
    let quiet = ws
        .cmd(&["pull", "--silent", "--insecure"])
        .output()
        .unwrap();
    assert!(
        quiet.stderr.is_empty(),
        "--silent takes the warnings too: {}",
        String::from_utf8_lossy(&quiet.stderr)
    );

    // A failure is not noise. The message and the exit code both survive.
    let broken = ws
        .cmd(&["pull", "--silent"])
        .arg("--dir")
        .arg(ws.path().join("not-a-workspace"))
        .output()
        .unwrap();
    assert_eq!(broken.status.code(), Some(2));
    assert!(
        String::from_utf8_lossy(&broken.stderr).contains("no jired workspace"),
        "an exit code with no reason is not something anyone can act on"
    );
}

/// The result document is the machine's channel, not chatter: `--silent` leaves
/// it alone, warnings included.
#[test]
fn silent_keeps_the_json_document_and_the_warnings_in_it() {
    let ws = Ws::new();
    ws.init();

    let out = ws
        .cmd(&["pull", "--silent", "--json", "--insecure"])
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(
        out.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let report: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(report["schema"], "jired/pull/1");
    let warnings = report["warnings"].as_array().expect("the parked warning");
    assert!(
        warnings
            .iter()
            .any(|w| w.as_str().unwrap_or_default().contains("TLS")),
        "{report}"
    );
}

// ---------------------------------------------------------------------------
// release
// ---------------------------------------------------------------------------

/// The versions the "server" holds now, read off its own file, so a test can
/// see what a command really changed rather than what it reported.
fn remote_versions(ws: &Ws) -> Vec<Value> {
    let text = std::fs::read_to_string(ws.fixtures.join("versions.json")).unwrap();
    serde_json::from_str::<Value>(&text)
        .unwrap()
        .as_array()
        .unwrap()
        .clone()
}

/// A version's whole life without the web UI, on both deployments: the routes
/// are the platform API's, which Cloud serves under v3 and Data Center under
/// v2, and a path that only works on one of them is the failure this guards.
#[test]
fn a_version_can_be_created_listed_renamed_released_and_archived() {
    for deployment in [Deployment::Cloud, Deployment::DataCenter] {
        let ws = Ws::of(deployment);
        ws.init();
        let prefix = match deployment {
            Deployment::Cloud => "/rest/api/3",
            Deployment::DataCenter => "/rest/api/2",
        };
        let project_id = match deployment {
            Deployment::Cloud => 10000,
            Deployment::DataCenter => 10200,
        };

        let created = ws.json(&[
            "release",
            "create",
            "0.5.0",
            "--start",
            "2026-10-15",
            "--release-date",
            "2026-10-29",
            "--description",
            "Sprint 5",
        ]);
        assert_eq!(created["schema"], "jired/release-create/1");
        assert_eq!(created["request"]["method"], "POST");
        assert_eq!(created["request"]["path"], format!("{prefix}/version"));
        // `projectId`, which both deployments take; Cloud documents the key
        // spelling as deprecated.
        assert_eq!(created["request"]["body"]["projectId"], project_id);
        assert_eq!(created["request"]["body"]["startDate"], "2026-10-15");
        assert_eq!(created["request"]["body"]["releaseDate"], "2026-10-29");
        assert_eq!(created["version"]["released"], false);
        let id = created["version"]["id"].as_str().unwrap().to_string();

        // Listed straight away, unreleased, with its dates as days.
        let listed = ws.json(&["releases"]);
        assert_eq!(listed["schema"], "jired/releases/1");
        let ours = listed["versions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|v| v["id"] == id.as_str())
            .unwrap_or_else(|| panic!("{listed}"))
            .clone();
        assert_eq!(ours["name"], "0.5.0");
        assert_eq!(ours["start_date"], "2026-10-15");

        // By name or by id, the same version.
        let renamed = ws.json(&["release", "edit", "0.5.0", "--name", "0.5.1"]);
        assert_eq!(renamed["schema"], "jired/release-update/1");
        assert_eq!(renamed["request"]["method"], "PUT");
        assert_eq!(renamed["request"]["path"], format!("{prefix}/version/{id}"));
        // The endpoint merges, so only the change goes out.
        assert_eq!(
            renamed["request"]["body"],
            serde_json::json!({ "name": "0.5.1" })
        );

        let shipped = ws.json(&["release", "ship", &id, "--date", "2026-10-28"]);
        assert_eq!(shipped["schema"], "jired/release-ship/1");
        assert_eq!(shipped["released"], true);
        assert_eq!(
            shipped["request"]["body"],
            serde_json::json!({ "released": true, "releaseDate": "2026-10-28" })
        );

        let archived = ws.json(&["release", "archive", "0.5.1"]);
        assert_eq!(
            archived["request"]["body"],
            serde_json::json!({ "archived": true })
        );

        let remote = remote_versions(&ws);
        let ours = remote.iter().find(|v| v["id"] == id.as_str()).unwrap();
        assert_eq!(ours["name"], "0.5.1", "{deployment:?}");
        assert_eq!(ours["released"], true);
        assert_eq!(ours["archived"], true);
        assert_eq!(ours["releaseDate"], "2026-10-28");

        // A released, archived version is history: hidden unless asked for.
        let shown = ws.cmd(&["releases"]).output().unwrap();
        assert!(!String::from_utf8_lossy(&shown.stdout).contains("0.5.1"));
        let all = ws.cmd(&["releases", "--all"]).output().unwrap();
        assert!(String::from_utf8_lossy(&all.stdout).contains("0.5.1"));
    }
}

/// What can be said before anything is sent is said then, as exit 2, and a
/// dry run sends nothing at all.
#[test]
fn a_version_jira_would_refuse_is_refused_here_and_a_dry_run_sends_nothing() {
    let ws = Ws::new();
    ws.init();

    let err = ws.json_err(&["release", "create", "2.4.0"], 2);
    assert!(
        err["message"]
            .as_str()
            .unwrap()
            .contains("already has a version called `2.4.0`"),
        "{err}"
    );
    let err = ws.json_err(
        &[
            "release",
            "create",
            "3.0.0",
            "--start",
            "2026-10-29",
            "--release-date",
            "2026-10-15",
        ],
        2,
    );
    assert!(err["message"]
        .as_str()
        .unwrap()
        .contains("due out before it starts"));
    assert_eq!(
        ws.json_err(&["release", "ship", "2.3.0"], 2)["details"]["state"],
        "released"
    );
    assert_eq!(
        ws.json_err(&["release", "archive", "2.2.0"], 2)["details"]["state"],
        "archived"
    );
    let err = ws.json_err(&["release", "edit", "9.9.9", "--name", "x"], 2);
    assert!(
        err["hint"].as_str().unwrap().contains("2.4.0 (10403)"),
        "{err}"
    );
    // No flags is a mistake about the command, not about the version.
    ws.json_err(&["release", "edit", "2.4.0"], 7);

    let before = remote_versions(&ws);
    let dry = ws.json(&["release", "create", "3.0.0", "-n", "--issues", "PROJ-142"]);
    assert_eq!(dry["dry_run"], true);
    assert_eq!(dry["version"], Value::Null);
    assert_eq!(dry["request"]["body"]["name"], "3.0.0");
    ws.json(&["release", "ship", "2.4.0", "-n"]);
    ws.json(&["release", "archive", "2.4.0", "-n"]);
    assert_eq!(remote_versions(&ws), before);
}

/// The acceptance case from the request: a version made a moment ago can be
/// written into `fix_versions:` and pushed with nothing fetched in between.
#[test]
fn a_new_version_can_be_set_and_pushed_without_a_fetch() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);

    ws.json(&["release", "create", "0.5.0"]);
    ws.json(&[
        "set",
        "--field",
        "fix_versions",
        "--value",
        "0.5.0",
        "PROJ-142",
    ]);

    let dry = ws.json(&["push", "--dry-run"]);
    assert_eq!(
        dry["results"][0]["ops"][0]["request"]["body"]["fields"]["fixVersions"],
        serde_json::json!([{ "name": "0.5.0" }]),
        "{dry}"
    );
    let push = ws.json(&["push"]);
    assert_eq!(push["results"][0]["ok"], true, "{push}");
    assert!(ws.read(ISSUE_FILE).contains("- 0.5.0"));
}

/// `--issues` stages the version on each issue the way `sprint create --issues`
/// stages the sprint, and `--push` sends it.
#[test]
fn a_version_can_be_created_and_put_on_issues_in_one_go() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);

    let out = ws.json(&[
        "release",
        "create",
        "0.5.0",
        "--issues",
        "PROJ-142,PROJ-150",
        "--push",
    ]);
    assert_eq!(out["issues"].as_array().unwrap().len(), 2);
    assert_eq!(out["push"]["results"][0]["ok"], true, "{out}");
    // Added, not substituted: the version the issue was already in stays.
    let text = ws.read(ISSUE_FILE);
    assert!(
        text.contains("- 2.4.0") && text.contains("- 0.5.0"),
        "{text}"
    );
}

/// Jira answers an unknown name in `fixVersions` with a 400 after the push has
/// started. Checked at plan time instead, so `--dry-run` shows it and nothing
/// goes out — on an update and on a create alike.
#[test]
fn an_unknown_version_is_refused_at_plan_time() {
    for deployment in [Deployment::Cloud, Deployment::DataCenter] {
        let ws = Ws::of(deployment);
        ws.init();
        ws.json(&["pull"]);
        let file = deployment.issue_file();

        let text = ws.read(file);
        let edited = text.replacen("fix_versions:\n- ", "fix_versions:\n- 9.9.9\n- ", 1);
        assert_ne!(text, edited, "{text}");
        ws.write(file, &edited);

        let dry = ws.json_code(&["push", "--dry-run"], 2);
        let result = &dry["results"][0];
        assert_eq!(result["ok"], false);
        assert_eq!(result["error_kind"], "usage");
        assert!(
            result["error"]
                .as_str()
                .unwrap()
                .contains("`9.9.9` is not a version of"),
            "{dry}"
        );
        let hint = result["hint"].as_str().unwrap();
        assert!(hint.contains("jired release create \"9.9.9\""), "{hint}");

        let journal = ws.path().join("calls.jsonl");
        let out = ws
            .cmd(&["push"])
            .arg("--json")
            .env("JIRED_FIXTURE_JOURNAL", &journal)
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(2));
        assert!(!journal.exists(), "nothing may be sent for a refused issue");

        // A case difference is not a different version: the server matches
        // without regard to case, so neither does the check.
        let known = match deployment {
            Deployment::Cloud => "2.5.0",
            Deployment::DataCenter => "2026.09",
        };
        ws.write(
            file,
            &text.replacen(
                "fix_versions:\n- ",
                &format!("fix_versions:\n- {known}\n- "),
                1,
            ),
        );
        let dry = ws.json(&["push", "--dry-run"]);
        assert_eq!(dry["results"][0]["ok"], true, "{dry}");
    }

    // A new issue is checked before its create call, not after.
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);
    let report = ws.json(&["create", "--summary", "Ship it"]);
    let file = report["file"].as_str().unwrap().to_string();
    ws.write(
        &file,
        &ws.read(&file)
            .replace("fix_versions: []", "fix_versions:\n- 9.9.9"),
    );
    let dry = ws.json_code(&["push", "--dry-run"], 2);
    assert!(
        dry["results"][0]["error"]
            .as_str()
            .unwrap()
            .contains("9.9.9"),
        "{dry}"
    );
}

/// A workspace that has never listed the project's versions has no grounds to
/// call a name unknown, and must not refuse every version made in the web UI.
#[test]
fn a_workspace_that_never_listed_versions_checks_nothing() {
    let ws = Ws::new();
    std::fs::remove_file(ws.fixtures.join("versions.json")).unwrap();
    ws.init();
    ws.json(&["pull"]);
    let text = ws
        .read(ISSUE_FILE)
        .replace("- 2.4.0", "- made-in-the-web-ui");
    ws.write(ISSUE_FILE, &text);
    let dry = ws.json(&["push", "--dry-run"]);
    assert_eq!(dry["results"][0]["ok"], true, "{dry}");
}

/// Shipping names the issues that are not finished, and `--move-unfixed-to`
/// moves them on and pushes that before the version is released.
#[test]
fn shipping_a_version_names_what_is_unfinished_and_can_move_it_on() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);
    // Two issues in 2.4.0, one of which is finished.
    ws.json(&[
        "set",
        "--field",
        "fix_versions",
        "--value",
        "2.4.0",
        "PROJ-150",
        "--push",
    ]);
    ws.json(&["transition", "PROJ-150", "--to", "Done", "--push"]);
    ws.json(&["pull"]);

    let seen = ws.json(&["release", "ship", "2.4.0", "--dry-run"]);
    assert_eq!(seen["released"], false);
    let unfinished: Vec<&str> = seen["unfinished"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["key"].as_str().unwrap())
        .collect();
    assert_eq!(
        unfinished,
        ["PROJ-142"],
        "the Done one is finished and stays"
    );
    // Its planned date is already set, so releasing does not resend it.
    assert_eq!(
        seen["request"]["body"],
        serde_json::json!({ "released": true })
    );

    let err = ws.json_err(
        &["release", "ship", "2.4.0", "--move-unfixed-to", "2.3.0"],
        2,
    );
    assert!(
        err["message"].as_str().unwrap().contains("is released"),
        "{err}"
    );
    ws.json_err(
        &["release", "ship", "2.4.0", "--move-unfixed-to", "2.4.0"],
        2,
    );

    let out = ws.json(&["release", "ship", "2.4.0", "--move-unfixed-to", "2.5.0"]);
    assert_eq!(out["released"], true);
    assert_eq!(out["version"]["released"], true);
    assert_eq!(out["moved_to"]["name"], "2.5.0");
    assert_eq!(out["issues"][0]["key"], "PROJ-142");
    assert_eq!(out["push"]["results"][0]["ok"], true);

    let text = ws.read(ISSUE_FILE);
    assert!(
        text.contains("- 2.5.0") && !text.contains("- 2.4.0"),
        "{text}"
    );
    assert!(ws
        .read("PROJ-0150. Rotate IdP certificate.md")
        .contains("- 2.4.0"));
}

/// zsh does not word-split `$keys`, so `jired push $keys` hands over one
/// argument holding every key. That read as a file name — a key and a summary
/// — and pushed the first issue alone, reporting success.
#[test]
fn several_keys_in_one_argument_are_refused_rather_than_cut_to_the_first() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);
    ws.write(
        ISSUE_FILE,
        &ws.read(ISSUE_FILE)
            .replace("Fix login flow", "Fix the login flow"),
    );

    let err = ws.json_err(&["push", "PROJ-142 PROJ-150 PROJ-143"], 2);
    assert_eq!(
        err["details"]["keys"],
        serde_json::json!(["PROJ-142", "PROJ-150", "PROJ-143"])
    );
    ws.json_err(&["diff", "PROJ-142 PROJ-150"], 2);

    // A file name with a summary is still one issue.
    let ok = ws.json(&["push", "--dry-run", ISSUE_FILE]);
    assert_eq!(ok["results"][0]["key"], "PROJ-142");
}

/// A key the workspace does not have is a failure, not an empty push: "nothing
/// to push" read as the issue being clean.
#[test]
fn a_named_issue_the_workspace_does_not_have_fails_the_push() {
    let ws = Ws::new();
    ws.init();
    ws.json(&["pull"]);
    ws.write(
        ISSUE_FILE,
        &ws.read(ISSUE_FILE)
            .replace("Fix login flow", "Fix the login flow"),
    );

    let out = ws.json_code(&["push", "--dry-run", "PROJ-142", "PROJ-9999"], 2);
    let by_key = |key: &str| {
        out["results"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["key"] == key)
            .unwrap_or_else(|| panic!("{out}"))
            .clone()
    };
    assert_eq!(by_key("PROJ-142")["ok"], true);
    let missing = by_key("PROJ-9999");
    assert_eq!(missing["ok"], false);
    assert!(missing["error"]
        .as_str()
        .unwrap()
        .contains("not in this workspace"));

    // Named alone, it must not widen into "every issue".
    let out = ws.json_code(&["push", "--dry-run", "PROJ-9999"], 2);
    assert_eq!(out["results"].as_array().unwrap().len(), 1, "{out}");
}

// ---------------------------------------------------------------------------
// headings and line endings inside rich text
// ---------------------------------------------------------------------------

/// Staging a frontmatter field must leave the body alone. A description whose
/// own headings are `h2.` renders them as `## `, which the file reader took for
/// sections of the file: the description ended at the first one, the rest was
/// moved below `## Attachments` on the rewrite, and the push planned an
/// `update_description` that would have emptied it in Jira. A comment with a
/// heading of its own was the same, and an `h3.` in one stopped the file
/// parsing at all. And a code block with CRLF endings lost its `\r` when the
/// file was read back, so the untouched comment was planned as an edit.
#[test]
fn staging_a_field_leaves_headings_and_line_endings_in_rich_text_alone() {
    let ws = Ws::of(Deployment::DataCenter);
    ws.mutate_remote("DC-77", |issue| {
        issue["fields"]["description"] = Value::from(
            "h2. Задача\n\nRotate the bind account.\n\nh2. Критерии\n\n* the new secret works\n\nh1. Notes\n\nnone",
        );
        issue["fields"]["comment"]["comments"][0]["body"] = Value::from(
            "Done.\n\nh2. Log\n\n{code}\r\nbind: ok\r\nsearch: ok\r\n{code}\n\nh3. Next\n\nnothing",
        );
    });
    ws.init();
    ws.json(&["pull"]);
    let file = ws.issue_file();
    let before = ws.read(file);
    let body_of = |text: &str| text.split_once("\n---\n").unwrap().1.to_string();

    ws.json(&[
        "set",
        "--field",
        "fix_versions",
        "--value",
        "2026.09",
        "DC-77",
    ]);
    let after = ws.read(file);
    assert_eq!(
        body_of(&before),
        body_of(&after),
        "only the frontmatter may change"
    );

    let diff = ws.json(&["diff", "DC-77"]);
    let fields: Vec<&str> = diff["issues"][0]["fields"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["field"].as_str().unwrap())
        .collect();
    assert_eq!(fields, ["fix_versions"], "{diff}");

    let dry = ws.json(&["push", "--dry-run"]);
    let kinds: Vec<&str> = dry["results"][0]["ops"]
        .as_array()
        .unwrap()
        .iter()
        .map(|o| o["kind"].as_str().unwrap())
        .collect();
    assert_eq!(kinds, ["update_fields"], "{dry}");
}

/// A file an earlier jired already rewrote has its description's headings below
/// `## Attachments` and the description cut short. The reader is fixed, but the
/// file still says so: pushing it is refused at plan time, and `reset` is the
/// way back.
#[test]
fn a_description_an_earlier_rewrite_cut_short_is_not_pushed() {
    let ws = Ws::of(Deployment::DataCenter);
    ws.mutate_remote("DC-77", |issue| {
        issue["fields"]["description"] =
            Value::from("h2. Задача\n\nRotate the bind account.\n\nh2. Критерии\n\n* it works");
    });
    ws.init();
    ws.json(&["pull"]);
    let file = ws.issue_file();
    let clean = ws.read(file);

    // What the old reader wrote back: the description emptied, and its
    // sections moved to the end of the file.
    let (head, rest) = clean.split_once("## Description\n\n").unwrap();
    let (description, rest) = rest.split_once("## Checklist").unwrap();
    let damaged = format!("{head}## Description\n\n## Checklist{rest}\n{description}")
        .replace("fix_versions:\n- 2026.08", "fix_versions:\n- 2026.09");
    ws.write(file, &damaged);

    let dry = ws.json_code(&["push", "--dry-run"], 2);
    let result = &dry["results"][0];
    assert!(
        result["error"]
            .as_str()
            .unwrap()
            .contains("`## Задача` belongs inside"),
        "{dry}"
    );
    assert!(result["hint"]
        .as_str()
        .unwrap()
        .contains("jired reset DC-77"));

    ws.json(&["reset", "DC-77", "--yes"]);
    assert_eq!(ws.read(file), clean);
}

// ---------------------------------------------------------------------------
// filter
// ---------------------------------------------------------------------------

/// The filters the "server" holds now, off its own file.
fn remote_filters(ws: &Ws) -> Vec<Value> {
    let text = std::fs::read_to_string(ws.fixtures.join("filters.json")).unwrap();
    serde_json::from_str::<Value>(&text)
        .unwrap()
        .as_array()
        .unwrap()
        .clone()
}

/// The writes among the calls a command journaled; the fixture journals only
/// writes, so this is all of them.
fn writes(calls: &[Value]) -> Vec<String> {
    calls
        .iter()
        .map(|c| {
            format!(
                "{} {}",
                c["method"].as_str().unwrap(),
                c["path"].as_str().unwrap()
            )
        })
        .collect()
}

/// The request's acceptance case, on both deployments: a dry run prints the
/// calls and sends none; the real create answers with the id and the link; the
/// saved filter runs to exactly what `search` finds with its JQL; and it can be
/// edited, re-shared and deleted without the browser.
#[test]
fn a_filter_can_be_saved_run_edited_shared_and_deleted() {
    for deployment in [Deployment::Cloud, Deployment::DataCenter] {
        let ws = Ws::of(deployment);
        ws.init();
        let project = deployment.project();
        let (prefix, project_id) = match deployment {
            Deployment::Cloud => ("/rest/api/3", "10000"),
            Deployment::DataCenter => ("/rest/api/2", "10200"),
        };
        let name = "TSK 1.0.0 без регулярных";
        let jql = format!(
            "project = {project} AND fixVersion = \"1.0.0\" \
             AND (labels IS EMPTY OR labels != regular) AND summary !~ \"RELEASE\" \
             ORDER BY status, key"
        );
        let share = format!("project:{project}");
        let create = ["filter", "create", name, "--jql", &jql, "--share", &share];

        let before = remote_filters(&ws);
        let mut dry_args = create.to_vec();
        dry_args.push("-n");
        let (dry, calls) = journaled(&ws, &dry_args);
        assert_eq!(dry["schema"], "jired/filter-create/1");
        assert_eq!(dry["dry_run"], true);
        assert_eq!(dry["filter"], Value::Null);
        assert!(dry["matches"].as_u64().unwrap() > 0, "{dry}");
        assert!(calls.is_empty(), "a dry run sent {calls:?}");
        assert_eq!(remote_filters(&ws), before);
        let requests = dry["requests"].as_array().unwrap();
        assert_eq!(requests[0]["method"], "POST");
        assert_eq!(requests[0]["path"], format!("{prefix}/filter"));
        assert_eq!(requests[0]["body"]["name"], name);
        assert_eq!(requests[0]["body"]["jql"], jql.as_str());
        let last = requests.last().unwrap();
        assert_eq!(last["path"], format!("{prefix}/filter/{{id}}/permission"));
        assert_eq!(
            last["body"],
            serde_json::json!({ "type": "project", "projectId": project_id })
        );
        // Starred unless told otherwise — in the body on Data Center, with a
        // call of its own on Cloud.
        match deployment {
            Deployment::DataCenter => assert_eq!(requests[0]["body"]["favourite"], true),
            Deployment::Cloud => {
                assert!(requests[0]["body"].get("favourite").is_none());
                assert_eq!(
                    requests[1]["path"],
                    format!("{prefix}/filter/{{id}}/favourite")
                );
            }
        }

        let (made, calls) = journaled(&ws, &create);
        let id = made["filter"]["id"].as_str().unwrap().to_string();
        assert_eq!(
            made["filter"]["url"],
            format!("https://fixture.invalid/issues/?filter={id}")
        );
        assert_eq!(made["filter"]["favourite"], true);
        assert_eq!(
            made["filter"]["share_permissions"][0]["spec"],
            share.as_str()
        );
        assert_eq!(
            writes(&calls).last().unwrap(),
            &format!("POST {prefix}/filter/{id}/permission")
        );

        // A filter is its JQL: running it is running the query.
        let ran = ws.json(&["filter", "run", &id, "--limit", "100"]);
        let searched = ws.json(&["search", "--jql", &jql, "--limit", "100"]);
        assert_eq!(ran["schema"], "jired/search/1");
        assert_eq!(ran["issues"], searched["issues"], "{deployment:?}");
        assert!(!ran["issues"].as_array().unwrap().is_empty());
        // By name too: it is starred, so even Data Center can find it.
        let by_name = ws.json(&["filter", "show", name]);
        assert_eq!(by_name["schema"], "jired/filter/1");
        assert_eq!(by_name["filter"]["id"], id.as_str());

        // Only what was asked for is changed, and the name always goes with it.
        let edited = ws.json(&["filter", "edit", &id, "--description", "for the release"]);
        assert_eq!(edited["schema"], "jired/filter-update/1");
        assert_eq!(
            edited["requests"][0]["body"],
            serde_json::json!({ "name": name, "description": "for the release" })
        );
        assert_eq!(edited["filter"]["jql"], jql.as_str());
        assert_eq!(edited["matches"], Value::Null);

        let reshared = ws.json(&[
            "filter",
            "share",
            &id,
            "--remove",
            &share,
            "--add",
            "authenticated",
        ]);
        let specs: Vec<&str> = reshared["filter"]["share_permissions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p["spec"].as_str().unwrap())
            .collect();
        assert_eq!(specs, ["authenticated"], "{reshared}");
        assert_eq!(reshared["requests"][0]["method"], "DELETE");

        // Deleting asks, and a run that cannot ask is told to say --yes.
        ws.json_err(&["filter", "delete", &id], 7);
        assert!(remote_filters(&ws).iter().any(|f| f["id"] == id.as_str()));
        let dry = ws.json(&["filter", "delete", &id, "-n"]);
        assert_eq!(dry["deleted"], false);
        assert_eq!(dry["request"]["path"], format!("{prefix}/filter/{id}"));
        let gone = ws.json(&["filter", "delete", &id, "--yes"]);
        assert_eq!(gone["schema"], "jired/filter-delete/1");
        assert_eq!(gone["deleted"], true);
        assert!(!remote_filters(&ws).iter().any(|f| f["id"] == id.as_str()));
    }
}

/// A JQL the server cannot run is refused before anything is saved: Jira
/// itself would store it without a word.
#[test]
fn a_filter_whose_jql_jira_refuses_is_exit_2_and_nothing_is_written() {
    for deployment in [Deployment::Cloud, Deployment::DataCenter] {
        let ws = Ws::of(deployment);
        ws.init();
        let journal = ws.path().join("calls.jsonl");
        let before = remote_filters(&ws);
        let bad = "project = PROJ AND (labels IS EMPTY OR labels != regular";
        let out = ws
            .cmd(&["filter", "create", "Broken", "--jql", bad, "--json"])
            .env("JIRED_FIXTURE_JOURNAL", &journal)
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(2), "{deployment:?}");
        let err: Value = serde_json::from_slice(&out.stderr).unwrap();
        assert!(
            err["message"].as_str().unwrap().contains("Expecting ')'"),
            "{err}"
        );
        assert_eq!(err["details"]["jql"], bad);
        assert!(!journal.exists(), "something was sent");
        assert_eq!(remote_filters(&ws), before);

        // An edit checks the new query the same way.
        let err = ws.json_err(&["filter", "edit", "10500", "--jql", bad], 2);
        assert!(err["message"].as_str().unwrap().contains("refused the JQL"));
        // And a dry run, which checks too, is refused just the same.
        ws.json_err(&["filter", "create", "Broken", "--jql", bad, "-n"], 2);
    }
}

/// `FILTER` is matched the way `--sprint` is: a name two people share, or a
/// number that is one filter's id and another's name, is refused with the
/// ids; `id:` says which.
#[test]
fn a_filter_name_that_could_be_two_filters_is_refused_with_their_ids() {
    let ws = Ws::new();
    ws.init();

    let err = ws.json_err(&["filter", "show", "Open bugs"], 2);
    assert_eq!(err["details"]["ids"], serde_json::json!(["10501", "10502"]));
    assert!(err["hint"].as_str().unwrap().contains("id:10502"), "{err}");

    // 10501 is "Open bugs"'s id and the name of another filter.
    let err = ws.json_err(&["filter", "show", "10501"], 2);
    assert_eq!(err["details"]["ids"], serde_json::json!(["10501", "10503"]));
    let one = ws.json(&["filter", "show", "id:10501"]);
    assert_eq!(one["filter"]["name"], "Open bugs");
    assert_eq!(one["filter"]["owner"]["id"], "5b10a2ilya");

    // Someone else's filter can be read and not deleted.
    let theirs = ws.json(&["filter", "show", "id:10502"]);
    assert_eq!(theirs["filter"]["share_permissions"][0]["spec"], "global");
    let err = ws.json_err(&["filter", "delete", "id:10502", "--yes"], 3);
    assert!(err["message"].as_str().unwrap().contains("Ann B"), "{err}");
    assert_eq!(remote_filters(&ws).len(), 4);

    // A second filter of the same name is one Jira refuses; refused here, with
    // the one in the way named.
    let err = ws.json_err(
        &[
            "filter",
            "create",
            "Ready for review",
            "--jql",
            "project = PROJ",
        ],
        2,
    );
    assert_eq!(err["details"]["filter_id"], "10500");
}

/// Data Center has no route that lists or searches filters but its
/// favourites, so that is where `filters` and a name look — and where they
/// say so, rather than reporting that a filter does not exist.
#[test]
fn data_center_finds_filters_by_name_only_among_the_favourites() {
    let ws = Ws::of(Deployment::DataCenter);
    ws.init();

    let listed = ws.json(&["filters"]);
    assert_eq!(listed["schema"], "jired/filters/1");
    let ids: Vec<&str> = listed["filters"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["id"].as_str().unwrap())
        .collect();
    // 10501 is ours and not starred.
    assert!(!ids.contains(&"10501"), "{listed}");
    let mine = ws.json(&["filters", "--mine"]);
    assert_eq!(mine["filters"].as_array().unwrap().len(), 1);
    assert_eq!(mine["filters"][0]["id"], "10500");

    // So "Open bugs" is the one starred filter of that name, Ann's.
    let found = ws.json(&["filter", "show", "Open bugs"]);
    assert_eq!(found["filter"]["id"], "10502");
    // An unstarred filter is found by its id.
    let by_id = ws.json(&["filter", "show", "id:10501"]);
    assert_eq!(by_id["filter"]["favourite"], false);
    let err = ws.json_err(&["filter", "show", "Nobody's"], 2);
    assert!(
        err["hint"].as_str().unwrap().contains("favourites"),
        "{err}"
    );

    // Starring it is a field of the filter on Data Center.
    let starred = ws.json(&["filter", "edit", "id:10501", "--favourite"]);
    assert_eq!(
        starred["requests"],
        serde_json::json!([{
            "method": "PUT",
            "path": "/rest/api/2/filter/10501",
            "body": { "name": "Open bugs", "favourite": true },
        }])
    );
    let err = ws.json_err(&["filter", "show", "Open bugs"], 2);
    assert_eq!(err["details"]["ids"], serde_json::json!(["10501", "10502"]));
}

/// Cloud lists what the account owns as well as what it starred, and stars
/// with a call of its own.
#[test]
fn cloud_lists_owned_and_starred_filters_and_stars_with_its_own_call() {
    let ws = Ws::new();
    ws.init();

    let listed = ws.json(&["filters"]);
    assert_eq!(listed["listing"], "all");
    assert_eq!(listed["filters"].as_array().unwrap().len(), 4);
    let mine = ws.json(&["filters", "--mine"]);
    assert_eq!(mine["filters"].as_array().unwrap().len(), 2);
    let starred = ws.json(&["filters", "--favourite"]);
    assert_eq!(starred["filters"].as_array().unwrap().len(), 3);

    let (doc, calls) = journaled(&ws, &["filter", "edit", "id:10501", "--favourite"]);
    assert_eq!(writes(&calls), ["PUT /rest/api/3/filter/10501/favourite"]);
    assert_eq!(doc["filter"]["favourite"], true);

    // Nothing to change is said, and nothing is sent.
    let (doc, calls) = journaled(&ws, &["filter", "edit", "id:10501", "--favourite"]);
    assert!(calls.is_empty(), "{calls:?}");
    assert_eq!(doc["requests"], serde_json::json!([]));
    // No flags at all is a mistake about the command.
    ws.json_err(&["filter", "edit", "id:10501"], 7);
}
