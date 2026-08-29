//! `jired fetch-spec` — the server's own description of the API it serves.
//!
//! Atlassian publishes no OpenAPI document for Data Center, but every Jira
//! serves a WADL beside each REST module it mounts. Written to disk under a name
//! that says which server and which version answered, because a description is
//! only worth keeping if you can tell later what it describes.

use crate::api::{ApiSpec, Deployment, SpecModule};
use crate::cli::context::Context;
use crate::cli::GlobalArgs;
use crate::error::{io_error, ExitCode, JiredError, Result};
use crate::json::Out;
use crate::style::{self, BOLD, CYAN, DIM, YELLOW};
use crate::sync::render::human_size;
use clap::Args;
use std::path::PathBuf;

#[derive(Args, Debug, Clone)]
pub struct FetchSpecArgs {
    /// Directory to write the specification files into
    #[arg(
        long,
        short = 'o',
        value_name = "PATH",
        default_value = ".",
        value_hint = clap::ValueHint::DirPath,
    )]
    pub output: PathBuf,
}

#[derive(serde::Serialize)]
pub struct FetchSpecReport {
    pub base_url: String,
    pub deployment: String,
    pub version: Option<String>,
    pub title: Option<String>,
    pub output_dir: String,
    pub specs: Vec<WrittenSpec>,
    pub skipped: Vec<SkippedSpec>,
}

#[derive(serde::Serialize)]
pub struct WrittenSpec {
    /// The REST module described, as `spec_modules` names it.
    pub module: String,
    /// The endpoint it was read from.
    pub path: String,
    /// Where it was written, absolute where the path could be resolved.
    pub file: String,
    pub media_type: String,
    pub bytes: u64,
}

#[derive(serde::Serialize)]
pub struct SkippedSpec {
    pub module: String,
    pub path: String,
    pub reason: String,
}

pub fn run(g: &GlobalArgs, args: &FetchSpecArgs, out: Out) -> Result<ExitCode> {
    let mut ctx = Context::open(g, out)?;
    let session = crate::store::session::SessionStore::open(&ctx.workspace)?;
    let account = session.require_account()?;
    let client = crate::api::build_client(&mut ctx, &account)?;

    let modules = client.spec_modules();
    if modules.is_empty() {
        return Err(serves_no_spec(client.deployment()));
    }

    let info = crate::api::block_on(client.server_info())?;
    let version = info.version.clone();
    out.say(format!(
        "{} {} {}",
        style::out(BOLD, info.title.as_deref().unwrap_or("Jira")),
        style::out(BOLD, version.as_deref().unwrap_or("(no version)")),
        style::out(DIM, format!("at {}", account.base_url)),
    ));

    std::fs::create_dir_all(&args.output)
        .map_err(|e| io_error("create the output directory", &args.output, e))?;
    // Reported rather than the flag as typed: a relative `-o out` is ambiguous
    // in a log read later, and this is a command whose whole product is files.
    let output_dir = args
        .output
        .canonicalize()
        .unwrap_or_else(|_| args.output.clone());

    let mut specs = Vec::new();
    let mut skipped = Vec::new();
    // The first refusal, kept in case every module refuses: a run that wrote
    // nothing should fail with what the server actually said, not with a
    // summary that hides it.
    let mut first_error: Option<JiredError> = None;

    for module in &modules {
        match crate::api::block_on(client.api_spec(module)) {
            Ok(Some(spec)) if is_spec(&spec.media_type, &spec.bytes) => {
                let name = spec_file_name(&account.base_url, account.deployment, &version, module);
                let file = output_dir.join(&name);
                std::fs::write(&file, &spec.bytes)
                    .map_err(|e| io_error("write the specification", &file, e))?;
                out.say(format!(
                    "  {} {}",
                    style::out(BOLD, &name),
                    style::out(DIM, human_size(spec.bytes.len() as u64))
                ));
                specs.push(WrittenSpec {
                    module: module.name.clone(),
                    path: module.path.clone(),
                    file: file.display().to_string(),
                    media_type: spec.media_type,
                    bytes: spec.bytes.len() as u64,
                });
            }
            // A 200 that is not a description means something answered in the
            // server's place — an SSO proxy serving its login page for every
            // path. Writing it would leave an HTML page named `.wadl`.
            Ok(Some(spec)) => {
                let reason = not_a_spec_reason(&spec);
                out.warn(format!("{}: {reason}", module.path));
                skipped.push(SkippedSpec {
                    module: module.name.clone(),
                    path: module.path.clone(),
                    reason,
                });
            }
            Ok(None) => skipped.push(SkippedSpec {
                module: module.name.clone(),
                path: module.path.clone(),
                reason: "this server does not serve one".to_string(),
            }),
            // One module refusing is not the run failing: a token without
            // permission for the Agile plugin still gets the platform API.
            Err(e) => {
                out.warn(format!("{}: {}", module.path, e.message));
                skipped.push(SkippedSpec {
                    module: module.name.clone(),
                    path: module.path.clone(),
                    reason: e.message.clone(),
                });
                first_error.get_or_insert(e);
            }
        }
    }

    if specs.is_empty() {
        return Err(first_error.unwrap_or_else(|| nothing_served(&modules)));
    }

    for skip in &skipped {
        out.say(style::out(
            YELLOW,
            format!("  {} — {}", skip.module, skip.reason),
        ));
    }
    out.say(format!(
        "{} {} into {}",
        specs.len(),
        style::out(DIM, if specs.len() == 1 { "file" } else { "files" }),
        style::out(CYAN, output_dir.display().to_string())
    ));

    let report = FetchSpecReport {
        base_url: account.base_url.clone(),
        deployment: account.deployment.as_str().to_string(),
        version,
        title: info.title,
        output_dir: output_dir.display().to_string(),
        specs,
        skipped,
    };
    out.emit("fetch-spec", &report);
    Ok(ExitCode::Success)
}

/// The name a description is filed under: which server, which version, which
/// module.
///
/// Two servers' descriptions differ, and so do two versions of one server's, so
/// a bare `application.wadl` in a directory is a file nobody can date. The host
/// keeps its dots — `jira.corp.example` is how anyone would recognise it — and
/// so does the version, while everything else is flattened to a dash.
fn spec_file_name(
    base_url: &str,
    deployment: Deployment,
    version: &Option<String>,
    module: &SpecModule,
) -> String {
    let host = slug(host_of(base_url));
    let version = slug(version.as_deref().unwrap_or("unknown"));
    format!(
        "{host}-{}-{version}-{}.{}",
        deployment.as_str(),
        module.name,
        module.extension
    )
}

/// The host part of a base URL, without scheme, port, path or credentials.
fn host_of(base_url: &str) -> &str {
    let rest = base_url
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(base_url);
    let authority = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    // A `user@host` authority is legal and the user half is not identity worth
    // putting in a filename.
    let host = authority.rsplit('@').next().unwrap_or(authority);
    host.rsplit_once(':')
        .map(|(host, _port)| host)
        .filter(|host| !host.is_empty())
        .unwrap_or(host)
}

/// Filename-safe form: lowercase, dots kept, every other run of unsafe
/// characters collapsed to a single dash.
fn slug(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        if ch.is_ascii_alphanumeric() || ch == '.' {
            out.push(ch.to_ascii_lowercase());
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    out.trim_matches(['-', '.']).to_string()
}

/// Whether what came back describes an API rather than being a page about one.
///
/// An instance behind an SSO proxy answers 200 with its login page for every
/// path, including this one. Jersey sends XML and says so, so a body that
/// announces or looks like HTML is the proxy talking.
fn is_spec(media_type: &str, bytes: &[u8]) -> bool {
    let media_type = media_type.to_ascii_lowercase();
    if media_type.contains("html") {
        return false;
    }
    let head = String::from_utf8_lossy(&bytes[..bytes.len().min(256)])
        .trim_start()
        .to_ascii_lowercase();
    !head.starts_with("<!doctype html") && !head.starts_with("<html") && !head.is_empty()
}

fn not_a_spec_reason(spec: &ApiSpec) -> String {
    let what = if spec.media_type.is_empty() {
        "an unlabelled document".to_string()
    } else {
        format!("`{}`", spec.media_type)
    };
    format!("the server answered with {what}, not a specification")
}

/// Cloud has nothing to fetch — and the reason is worth more than the refusal.
fn serves_no_spec(deployment: Deployment) -> JiredError {
    let what = match deployment {
        Deployment::Cloud => "Jira Cloud",
        Deployment::DataCenter => "this server",
    };
    let err =
        JiredError::unsupported_server(format!("{what} serves no description of its own API"))
            .with_detail("deployment", deployment.as_str());
    match deployment {
        Deployment::Cloud => err.with_hint(format!(
            "Atlassian publishes the Cloud API as OpenAPI instead — fetch it \
             directly from {}",
            crate::api::cloud::PUBLISHED_OPENAPI
        )),
        Deployment::DataCenter => err,
    }
}

fn nothing_served(modules: &[SpecModule]) -> JiredError {
    let paths: Vec<&str> = modules.iter().map(|m| m.path.as_str()).collect();
    JiredError::unsupported_server("this server described none of its REST modules")
        .with_detail("paths", paths.join(", "))
        .with_hint(
            "the WADL endpoints may be disabled on this instance, or a proxy in \
             front of it may be answering for them",
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn module(name: &str) -> SpecModule {
        SpecModule {
            name: name.to_string(),
            path: format!("/rest/{name}/application.wadl"),
            accept: "application/xml".to_string(),
            extension: "wadl".to_string(),
        }
    }

    #[test]
    fn a_file_name_says_which_server_and_which_version() {
        assert_eq!(
            spec_file_name(
                "https://jira.corp.example/jira",
                Deployment::DataCenter,
                &Some("9.12.4".to_string()),
                &module("api-2"),
            ),
            "jira.corp.example-datacenter-9.12.4-api-2.wadl"
        );
    }

    /// A server that will not say its version still has to produce a file, and
    /// one that says something odd must not produce a path.
    #[test]
    fn odd_versions_and_hosts_still_make_one_safe_name() {
        assert_eq!(
            spec_file_name(
                "https://jira.corp.example",
                Deployment::DataCenter,
                &None,
                &module("api-2"),
            ),
            "jira.corp.example-datacenter-unknown-api-2.wadl"
        );
        let name = spec_file_name(
            "https://jira.corp.example",
            Deployment::DataCenter,
            &Some("10.3.0-SNAPSHOT/../..".to_string()),
            &module("agile-1.0"),
        );
        assert_eq!(
            name,
            "jira.corp.example-datacenter-10.3.0-snapshot-agile-1.0.wadl"
        );
        assert!(!name.contains('/'), "a version cannot escape the directory");
    }

    #[test]
    fn a_host_is_taken_without_scheme_port_or_path() {
        assert_eq!(
            host_of("https://jira.corp.example/jira"),
            "jira.corp.example"
        );
        assert_eq!(
            host_of("https://jira.corp.example:8443/"),
            "jira.corp.example"
        );
        assert_eq!(
            host_of("http://user@jira.corp.example"),
            "jira.corp.example"
        );
        assert_eq!(host_of("jira.corp.example"), "jira.corp.example");
    }

    /// The failure this guards against: a proxy's login page saved as a `.wadl`
    /// and read months later as though the server had sent it.
    #[test]
    fn a_login_page_is_not_a_specification() {
        assert!(is_spec(
            "application/vnd.sun.wadl+xml",
            b"<?xml version=\"1.0\"?><application/>"
        ));
        assert!(is_spec("", b"<?xml version=\"1.0\"?><application/>"));
        assert!(!is_spec(
            "text/html;charset=utf-8",
            b"<!doctype html><html>"
        ));
        assert!(!is_spec("", b"<!DOCTYPE html>\n<html lang=\"en\">"));
        assert!(!is_spec("application/xml", b""));
    }
}
