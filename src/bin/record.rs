//! `jired-record` — capture a fixture set from a live Jira instance [M1.12].
//!
//! Fixtures let the whole CLI be exercised offline, but hand-written ones only
//! prove jired agrees with itself. This records what a real server actually
//! returns, scrubs it, and writes the layout `api::fixture::FixtureClient`
//! reads.
//!
//! ```sh
//! JIRED_URL=https://acme.atlassian.net \
//! JIRED_EMAIL=you@acme.com JIRED_TOKEN=$JIRA_TOKEN \
//!   jired-record --project PROJ --out tests/fixtures/cloud
//! ```
//!
//! The output is meant to be reviewed before committing: scrubbing is
//! conservative but it cannot know about a custom field holding a customer
//! name.

use jired::api::client::{FieldSet, SearchQuery};
use jired::api::scrub::Scrubber;
use jired::api::{block_on, AuthKind, Connection, Deployment, TlsOptions};
use jired::error::{ExitCode, JiredError, Result};
use secrecy::SecretString;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

struct Args {
    project: String,
    out: PathBuf,
    limit: usize,
    jql: Option<String>,
}

fn main() -> std::process::ExitCode {
    match run() {
        Ok(()) => std::process::ExitCode::from(ExitCode::Success.as_i32() as u8),
        Err(e) => {
            eprintln!("error: {e}");
            if let Some(hint) = &e.hint {
                eprintln!("hint: {hint}");
            }
            std::process::ExitCode::from(e.exit_code().as_i32() as u8)
        }
    }
}

fn run() -> Result<()> {
    let args = parse_args()?;
    let url = env_required("JIRED_URL")?;
    let token = SecretString::from(env_required("JIRED_TOKEN")?);
    let email = std::env::var("JIRED_EMAIL").ok().filter(|s| !s.is_empty());

    let deployment = match std::env::var("JIRED_DEPLOYMENT") {
        Ok(d) if !d.is_empty() => Deployment::parse(&d)?,
        _ => Deployment::guess_from_url(&url),
    };
    let auth_kind = match (&email, deployment) {
        (Some(_), _) => AuthKind::ApiToken,
        (None, _) => AuthKind::Pat,
    };

    let client = jired::api::make_client(Connection {
        base_url: url.clone(),
        deployment,
        auth_kind,
        identity: email.clone(),
        token: token.clone(),
        tls: TlsOptions::default(),
    })?;

    let mut scrubber = Scrubber::new();
    scrubber.redact_literal(host_of(&url), "fixture.invalid");
    if let Some(email) = &email {
        scrubber.redact_literal(email.clone(), "recorded@example.invalid");
    }
    // Belt and braces: a token should never reach a payload, but if a server
    // echoes one back in an error message it must not survive to disk.
    scrubber.redact_literal(secrecy::ExposeSecret::expose_secret(&token), "REDACTED");

    std::fs::create_dir_all(&args.out)?;
    eprintln!("recording from {url} into {}", args.out.display());

    block_on(async {
        let info = client.server_info().await?;
        write(
            &args.out,
            "serverInfo.json",
            &scrubber.scrub(&json!({
                "version": info.version,
                "deploymentType": match info.deployment.unwrap_or(deployment) {
                    Deployment::Cloud => "Cloud",
                    Deployment::DataCenter => "Server",
                },
                "serverTitle": info.title,
            })),
        )?;

        let me = client.myself().await?;
        write(
            &args.out,
            "myself.json",
            &scrubber.scrub(&json!({
                "accountId": me.account_id,
                "displayName": me.display_name,
                "emailAddress": me.email,
                "timeZone": me.time_zone,
            })),
        )?;

        let projects = client.projects().await?;
        write(
            &args.out,
            "projects.json",
            &scrubber.scrub(&serde_json::to_value(&projects)?),
        )?;

        let fields = client.fields().await?;
        write(
            &args.out,
            "fields.json",
            &scrubber.scrub(&serde_json::to_value(&fields)?),
        )?;

        let jql = args
            .jql
            .clone()
            .unwrap_or_else(|| format!("project = \"{}\" ORDER BY updated DESC", args.project));
        let page = client
            .search(&SearchQuery {
                jql: jql.clone(),
                start_at: 0,
                max_results: args.limit,
                fields: FieldSet::Full,
                expand_changelog: false,
            })
            .await?;
        eprintln!("  {} issue(s) of {} matched", page.issues.len(), page.total);

        // The raw payload is what the fixture player replays, so it is recorded
        // rather than jired's normalized view: that way the fixtures exercise
        // the normalization code instead of bypassing it.
        let issues: Vec<Value> = page.issues.iter().map(|i| scrubber.scrub(&i.raw)).collect();
        write(&args.out, "issues.json", &Value::Array(issues))?;

        if let Some(first) = page.issues.first() {
            let transitions = client.transitions(&first.key).await?;
            write(
                &args.out,
                "transitions.json",
                &scrubber.scrub(&serde_json::to_value(&transitions)?),
            )?;
        }

        Ok::<(), JiredError>(())
    })?;

    eprintln!(
        "done — review {} before committing; scrubbing cannot know about a \
         custom field holding a customer name",
        args.out.display()
    );
    Ok(())
}

fn write(dir: &Path, name: &str, value: &Value) -> Result<()> {
    let path = dir.join(name);
    std::fs::write(&path, format!("{}\n", serde_json::to_string_pretty(value)?))?;
    eprintln!("  wrote {}", path.display());
    Ok(())
}

fn host_of(url: &str) -> String {
    url.trim_start_matches("https://")
        .trim_start_matches("http://")
        .split('/')
        .next()
        .unwrap_or(url)
        .to_string()
}

fn env_required(name: &str) -> Result<String> {
    std::env::var(name)
        .ok()
        .filter(|v| !v.is_empty())
        .ok_or_else(|| {
            JiredError::missing_value(format!("{name} is not set"))
                .with_detail("env", name.to_string())
        })
}

fn parse_args() -> Result<Args> {
    let mut project = std::env::var("JIRED_PROJECT").unwrap_or_default();
    let mut out = PathBuf::from("tests/fixtures/recorded");
    let mut limit = 10usize;
    let mut jql = None;

    let mut argv = std::env::args().skip(1);
    while let Some(arg) = argv.next() {
        let mut value = || {
            argv.next()
                .ok_or_else(|| JiredError::usage(format!("{arg} needs a value")))
        };
        match arg.as_str() {
            "--project" => project = value()?,
            "--out" => out = PathBuf::from(value()?),
            "--limit" => {
                limit = value()?
                    .parse()
                    .map_err(|_| JiredError::usage("--limit must be a number"))?
            }
            "--jql" => jql = Some(value()?),
            "-h" | "--help" => {
                eprintln!(
                    "usage: jired-record [--project KEY] [--jql JQL] [--limit N] [--out DIR]\n\
                     reads JIRED_URL, JIRED_TOKEN, and JIRED_EMAIL (Cloud only)"
                );
                std::process::exit(0);
            }
            other => return Err(JiredError::usage(format!("unknown argument {other}"))),
        }
    }

    if project.is_empty() && jql.is_none() {
        return Err(JiredError::usage("a --project or --jql is required")
            .with_hint("jired-record --project PROJ --out tests/fixtures/cloud"));
    }
    Ok(Args {
        project,
        out,
        limit,
        jql,
    })
}
