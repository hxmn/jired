//! Secret-leak guards [M1.12, M5.5, M6.1].
//!
//! Two things must never carry a credential: the fixtures committed to this
//! repository, and anything jired prints. Both are checked mechanically here
//! rather than by review, because a review only has to miss once.
//!
//! The vendored API descriptions under `specs/` are held to the same rule.
//! They were recorded from a real instance, which is exactly why: a WADL names
//! the server it came from in its `base` URL and again in the link it offers
//! to its own detailed form.

use assert_cmd::Command;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Substrings that would indicate a real credential or host in a fixture.
///
/// Deliberately blunt: a fixture has no legitimate reason to contain any of
/// these, so a false positive means the fixture should be renamed, not the
/// rule relaxed.
const FORBIDDEN_IN_FIXTURES: &[&str] = &[
    ".atlassian.net",
    "Authorization",
    "Bearer ",
    "Basic ",
    "api_token=",
    "ATATT",      // Atlassian API token prefix
    "avatarUrls", // signed URLs, and the recorder drops them
];

/// Hosts that name an XML vocabulary rather than a server.
///
/// A WADL declares its own namespaces by URI, and Jersey stamps the generator
/// it was built with. Nothing ever requests these — they are identifiers — so
/// they are the one exception to the reserved-host rule, listed by name so a
/// real host cannot slip in behind a wildcard.
const XML_NAMESPACES: &[&str] = &["wadl.dev.java.net", "www.w3.org", "jersey.java.net"];

/// Everything committed that was recorded from a server: the fixtures the tests
/// replay, and the API descriptions the Data Center routes are checked against.
fn recorded_files() -> Vec<PathBuf> {
    let mut out = walk(&repo().join("tests/fixtures"));
    out.extend(walk(&repo().join("specs")));
    out
}

#[test]
fn committed_fixtures_carry_no_credentials_or_real_hosts() {
    let mut checked = 0;

    for entry in recorded_files() {
        let text = std::fs::read_to_string(&entry).unwrap();
        checked += 1;
        for needle in FORBIDDEN_IN_FIXTURES {
            assert!(
                !text.contains(needle),
                "{} contains {needle:?}",
                entry.display()
            );
        }
    }
    assert!(
        checked > 0,
        "no fixtures or specs were checked; did the layout move?"
    );
}

#[test]
fn fixture_hosts_are_reserved_names() {
    // `.invalid` and `.example` are reserved by RFC 2606, so a fixture URL can
    // never accidentally resolve to something real.
    for entry in recorded_files() {
        let text = std::fs::read_to_string(&entry).unwrap();
        for line in text.lines() {
            for token in line.split(['"', ' ', ',', '\'', '<', '>']) {
                if let Some(rest) = token
                    .strip_prefix("https://")
                    .or(token.strip_prefix("http://"))
                {
                    let host = rest.split('/').next().unwrap_or("");
                    assert!(
                        host.ends_with(".invalid")
                            || host.contains(".example")
                            || XML_NAMESPACES.contains(&host),
                        "{}: {host} is not a reserved test host",
                        entry.display()
                    );
                }
            }
        }
    }
}

/// The scan has to reach the specs, not just be pointed at them: they are the
/// files most likely to carry a real host, and the largest, so a silent miss
/// here would be the one that matters.
#[test]
fn the_vendored_specs_are_scanned() {
    let specs: Vec<PathBuf> = recorded_files()
        .into_iter()
        .filter(|p| p.extension().is_some_and(|e| e == "wadl"))
        .collect();
    assert!(
        specs.len() >= 5,
        "expected the vendored descriptions and the fixture copies, found {specs:?}"
    );
    assert!(
        specs
            .iter()
            .any(|p| p.starts_with(repo().join("specs"))),
        "the specs directory was not walked: {specs:?}"
    );
}

/// The token is passed to a full workflow; it must appear in no output.
#[test]
fn no_command_output_or_config_contains_the_token() {
    const TOKEN: &str = "leak-canary-token-value";
    let dir = TempDir::new().unwrap();
    let fixtures = dir.path().join(".fixtures");
    std::fs::create_dir_all(&fixtures).unwrap();
    for entry in std::fs::read_dir(repo().join("tests/fixtures/cloud"))
        .unwrap()
        .flatten()
    {
        std::fs::copy(entry.path(), fixtures.join(entry.file_name())).unwrap();
    }

    let run = |args: &[&str]| -> String {
        let out = Command::cargo_bin("jired")
            .unwrap()
            .current_dir(dir.path())
            .env("JIRED_FIXTURES", &fixtures)
            .env("JIRED_NO_KEYRING", "1")
            .env("JIRED_SECRET_KEY", "passphrase")
            .env("JIRED_URL", "https://fixture.invalid")
            .env("JIRED_EMAIL", "ilya@example.invalid")
            .env("JIRED_TOKEN", TOKEN)
            .env("JIRED_PROJECT", "PROJ")
            // Maximum verbosity: if a log line would ever print the credential,
            // this is where it shows up.
            .env("JIRED_LOG", "trace")
            .args(args)
            .arg("--no-input")
            .output()
            .unwrap();
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )
    };

    let mut output = String::new();
    output.push_str(&run(&["init", "--no-pull", "--json"]));
    output.push_str(&run(&["pull", "--json"]));
    output.push_str(&run(&["status", "--json"]));
    output.push_str(&run(&["whoami", "--json"]));
    output.push_str(&run(&["config", "list", "--all", "--json"]));
    output.push_str(&run(&["diff", "--json"]));
    output.push_str(&run(&["push", "--dry-run", "--json"]));

    assert!(
        !output.contains(TOKEN),
        "a credential reached command output:\n{output}"
    );

    // And nothing readable on disk holds it either, apart from the encrypted
    // blob in session.db.
    let config = std::fs::read_to_string(dir.path().join(".jired/config.toml")).unwrap();
    assert!(!config.contains(TOKEN), "credential in config.toml");

    for name in ["CLAUDE.md", "AGENTS.md", ".gitignore"] {
        let text = std::fs::read_to_string(dir.path().join(name)).unwrap();
        assert!(!text.contains(TOKEN), "credential in {name}");
    }

    let session = std::fs::read(dir.path().join(".jired/session.db")).unwrap();
    assert!(
        !contains(&session, TOKEN.as_bytes()),
        "credential stored verbatim in session.db"
    );
}

#[test]
fn generated_agent_files_only_reference_the_token_by_variable() {
    let docs = std::fs::read_to_string(repo().join("README.md")).unwrap();
    // A doc example that pastes a literal token teaches the habit of doing it.
    for line in docs.lines().filter(|l| l.contains("--token")) {
        assert!(line.contains('$'), "README shows a literal token: {line}");
    }
}

fn walk(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(root) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            out.extend(walk(&path));
        } else if path
            .extension()
            .is_some_and(|e| e == "json" || e == "wadl")
        {
            out.push(path);
        }
    }
    out
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

/// Disabling certificate verification has to be conspicuous every time, not
/// once at setup: a workspace configured with `--insecure` months ago is still
/// insecure today.
#[test]
fn insecure_tls_warns_on_every_run() {
    let dir = TempDir::new().unwrap();
    let out = Command::cargo_bin("jired")
        .unwrap()
        .current_dir(dir.path())
        .args(["status", "--insecure", "--no-input"])
        .output()
        .unwrap();

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("TLS certificate verification is disabled"),
        "expected a warning, got: {stderr}"
    );
    assert!(stderr.contains("--insecure"), "{stderr}");
}

/// The credential file must not be readable by anyone else, and jired must fix
/// it rather than trusting whatever mode it finds.
#[cfg(unix)]
#[test]
fn the_session_database_is_private() {
    use std::os::unix::fs::PermissionsExt;

    let dir = TempDir::new().unwrap();
    let fixtures = dir.path().join(".fixtures");
    std::fs::create_dir_all(&fixtures).unwrap();
    for entry in std::fs::read_dir(repo().join("tests/fixtures/cloud"))
        .unwrap()
        .flatten()
    {
        std::fs::copy(entry.path(), fixtures.join(entry.file_name())).unwrap();
    }

    let run = |args: &[&str]| {
        Command::cargo_bin("jired")
            .unwrap()
            .current_dir(dir.path())
            .env("JIRED_FIXTURES", &fixtures)
            .env("JIRED_NO_KEYRING", "1")
            .env("JIRED_SECRET_KEY", "passphrase")
            .env("JIRED_URL", "https://fixture.invalid")
            .env("JIRED_EMAIL", "ilya@example.invalid")
            .env("JIRED_TOKEN", "token")
            .env("JIRED_PROJECT", "PROJ")
            .args(args)
            .args(["--json", "--no-input"])
            .output()
            .unwrap()
    };
    assert!(run(&["init", "--no-pull"]).status.success());

    let session = dir.path().join(".jired/session.db");
    let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(
        mode(&session),
        0o600,
        "session.db must not be group readable"
    );
    assert_eq!(mode(&dir.path().join(".jired")), 0o700);

    // A careless chmod is corrected on the next open, not tolerated.
    std::fs::set_permissions(&session, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(run(&["whoami"]).status.success());
    assert_eq!(mode(&session), 0o600, "a loosened mode must be repaired");
}
