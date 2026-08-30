//! Jira REST abstraction [M1.6].
//!
//! One trait, two implementations. The differences that matter — REST v2 vs v3,
//! wiki markup vs ADF, usernames vs GDPR account ids, Agile endpoints — are
//! absorbed here so the sync engine never branches on deployment.

pub mod checklist;
pub mod client;
pub mod cloud;
pub mod custom;
pub mod datacenter;
pub mod fixture;
pub mod http;
pub mod model;
pub mod normalize;
pub mod rest;
pub mod scrub;
pub mod tls;

pub use client::{ApiSpec, FieldSet, JiraApi, SearchPage, SearchQuery, SpecModule, UserScope};
pub use http::TlsOptions;
pub use model::*;

use crate::error::{JiredError, Result};
use secrecy::SecretString;
use std::sync::OnceLock;

/// Which Jira product a workspace talks to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum Deployment {
    /// Atlassian-hosted (REST v3, ADF)
    Cloud,
    /// Self-hosted Data Center or Server (REST v2, wiki markup)
    #[value(name = "datacenter", alias = "dc", alias = "server")]
    DataCenter,
}

impl Deployment {
    pub fn as_str(self) -> &'static str {
        match self {
            Deployment::Cloud => "cloud",
            Deployment::DataCenter => "datacenter",
        }
    }

    pub fn parse(s: &str) -> Result<Self> {
        match s
            .trim()
            .to_ascii_lowercase()
            .replace(['-', '_'], "")
            .as_str()
        {
            "cloud" => Ok(Deployment::Cloud),
            "datacenter" | "dc" | "server" => Ok(Deployment::DataCenter),
            other => Err(JiredError::usage(format!(
                "unknown deployment `{other}` (expected cloud or datacenter)"
            ))),
        }
    }

    /// Atlassian-hosted instances always live under `atlassian.net`; anything
    /// else is assumed self-hosted until the server says otherwise.
    pub fn guess_from_url(url: &str) -> Self {
        if url.contains(".atlassian.net") || url.contains(".jira.com") {
            Deployment::Cloud
        } else {
            Deployment::DataCenter
        }
    }

    pub fn default_auth_kind(self) -> AuthKind {
        match self {
            Deployment::Cloud => AuthKind::ApiToken,
            Deployment::DataCenter => AuthKind::Pat,
        }
    }

    /// The rich-text format this deployment stores descriptions and comments in.
    pub fn markup(self) -> Markup {
        match self {
            Deployment::Cloud => Markup::Adf,
            Deployment::DataCenter => Markup::Wiki,
        }
    }
}

/// Rich-text representation used by a deployment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Markup {
    Adf,
    Wiki,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, clap::ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum AuthKind {
    /// Email plus an API token (Jira Cloud)
    #[value(name = "api-token", alias = "api_token", alias = "token")]
    ApiToken,
    /// Personal access token as a bearer header (Data Center)
    #[value(name = "pat", alias = "bearer")]
    Pat,
    /// Username and password
    Basic,
    /// Reuse an existing session cookie
    #[value(alias = "session")]
    Cookie,
    #[value(name = "oauth2", alias = "oauth")]
    OAuth2,
}

impl AuthKind {
    pub fn as_str(self) -> &'static str {
        match self {
            AuthKind::ApiToken => "api_token",
            AuthKind::Pat => "pat",
            AuthKind::Basic => "basic",
            AuthKind::Cookie => "cookie",
            AuthKind::OAuth2 => "oauth2",
        }
    }

    /// Whether this credential authenticates Jira's servlet pages, not just
    /// its REST API.
    ///
    /// Data Center's `/secure/*.jspa` web actions go through Seraph, which
    /// knows about basic auth and about an existing session cookie. A bearer
    /// token is not one of them: the personal-access-token authenticator is
    /// wired into the REST filters alone, so a PAT that reads and writes issues
    /// perfectly well is bounced to `SessionTimeoutMessage.jspa` — with HTTP
    /// 200 — the moment it asks for a web action. jired needs one operation
    /// there, the Move wizard behind a sub-task's parent, and this is what
    /// decides whether that operation can be attempted at all.
    pub fn opens_web_session(self) -> bool {
        match self {
            AuthKind::Basic | AuthKind::Cookie => true,
            AuthKind::Pat | AuthKind::OAuth2 | AuthKind::ApiToken => false,
        }
    }

    pub fn parse(s: &str) -> Result<Self> {
        match s
            .trim()
            .to_ascii_lowercase()
            .replace(['-', ' '], "_")
            .as_str()
        {
            "api_token" | "apitoken" | "token" => Ok(AuthKind::ApiToken),
            "pat" | "personal_access_token" | "bearer" => Ok(AuthKind::Pat),
            "basic" => Ok(AuthKind::Basic),
            "cookie" | "session" => Ok(AuthKind::Cookie),
            "oauth2" | "oauth" => Ok(AuthKind::OAuth2),
            other => Err(JiredError::usage(format!("unknown auth method `{other}`"))
                .with_hint("expected one of: api-token, pat, basic, cookie, oauth2")),
        }
    }
}

/// Everything needed to talk to a server, without touching the database.
pub struct Connection {
    pub base_url: String,
    pub deployment: Deployment,
    pub auth_kind: AuthKind,
    pub identity: Option<String>,
    pub token: SecretString,
    pub tls: TlsOptions,
}

pub struct Probe {
    pub deployment: Deployment,
    pub me: Myself,
    pub server_title: Option<String>,
    pub server_version: Option<String>,
}

/// Minimum Data Center version jired supports (decision D20): PAT auth and the
/// v2 endpoints jired relies on are only dependable from 9.0 onward.
pub const MIN_DC_MAJOR: u32 = 9;

/// Verify credentials and detect the deployment before anything is persisted.
pub fn probe(
    base_url: &str,
    assumed: Deployment,
    auth_kind: AuthKind,
    identity: Option<&str>,
    token: &SecretString,
    tls: &TlsOptions,
) -> Result<Probe> {
    let conn = Connection {
        base_url: base_url.to_string(),
        deployment: assumed,
        auth_kind,
        identity: identity.map(str::to_string),
        token: token.clone(),
        tls: tls.clone(),
    };
    let client = open_client(conn)?;
    block_on(async move {
        let info = client.server_info().await?;
        if let Some(deployment) = info.deployment {
            check_supported(deployment, info.version.as_deref())?;
        }
        let me = client.myself().await?;
        Ok(Probe {
            deployment: info.deployment.unwrap_or(assumed),
            me,
            server_title: info.title,
            server_version: info.version,
        })
    })
}

fn check_supported(deployment: Deployment, version: Option<&str>) -> Result<()> {
    if deployment != Deployment::DataCenter {
        return Ok(());
    }
    let Some(version) = version else {
        return Ok(());
    };
    let major: u32 = version
        .split('.')
        .next()
        .and_then(|m| m.parse().ok())
        .unwrap_or(u32::MAX);
    if major < MIN_DC_MAJOR {
        return Err(JiredError::unsupported_server(format!(
            "Jira Data Center {version} is not supported (jired needs {MIN_DC_MAJOR}.0 or newer)"
        ))
        .with_detail("version", version.to_string())
        .with_detail("minimum", format!("{MIN_DC_MAJOR}.0"))
        .with_hint(
            "ask whoever runs this Jira to upgrade; older releases lack the \
             PAT auth and v2 endpoints jired depends on",
        ));
    }
    Ok(())
}

/// Environment variable that redirects every request to a recorded fixture
/// directory. This is how the end-to-end tests exercise the CLI offline.
pub const FIXTURES_ENV: &str = "JIRED_FIXTURES";

fn fixture_dir() -> Option<std::path::PathBuf> {
    std::env::var_os(FIXTURES_ENV)
        .filter(|v| !v.is_empty())
        .map(std::path::PathBuf::from)
}

/// Build a client for a connection, honouring fixture mode.
pub fn open_client(conn: Connection) -> Result<Box<dyn JiraApi>> {
    match fixture_dir() {
        Some(dir) => Ok(Box::new(fixture::FixtureClient::open(
            &dir,
            conn.deployment,
        )?)),
        None => make_client(conn),
    }
}

/// Build the client for an already-authenticated workspace account.
pub fn build_client(
    ctx: &mut crate::cli::context::Context,
    account: &crate::store::session::Account,
) -> Result<Box<dyn JiraApi>> {
    if let Some(dir) = fixture_dir() {
        return Ok(Box::new(fixture::FixtureClient::open(
            &dir,
            account.deployment,
        )?));
    }
    let tls = TlsOptions {
        ca_path: account
            .tls_ca_path
            .clone()
            .or(ctx.resolver.get_quiet("tls.ca_cert")?)
            .map(Into::into),
        insecure: account.tls_insecure || ctx.resolver.get_bool("tls.insecure")?.unwrap_or(false),
    };
    make_client(Connection {
        base_url: account.base_url.clone(),
        deployment: account.deployment,
        auth_kind: account.auth_kind,
        identity: account.identity.clone(),
        token: account.token()?,
        tls,
    })
}

pub fn make_client(conn: Connection) -> Result<Box<dyn JiraApi>> {
    let deployment = conn.deployment;
    let http = http::HttpClient::new(conn)?;
    Ok(match deployment {
        Deployment::Cloud => Box::new(cloud::client(http)),
        Deployment::DataCenter => Box::new(datacenter::client(http)),
    })
}

/// The REST modules a deployment describes about itself.
///
/// Shared by the live client and the fixture one so a recording answers for
/// exactly the modules a real server of that kind would be asked about.
pub fn spec_modules(deployment: Deployment) -> Vec<SpecModule> {
    match deployment {
        Deployment::Cloud => cloud::spec_modules(),
        Deployment::DataCenter => datacenter::spec_modules(),
    }
}

/// Whether an issue payload paged its comments, or its worklogs [M6.5].
///
/// An issue payload carries only the first page of each — 50 comments and 20
/// worklogs on a stock instance — and says how many there are. A file showing
/// 50 of 120 comments reads as a discussion that stopped; one showing 20 of 45
/// worklogs is simply wrong about the time booked.
pub fn comments_are_paged(issue: &model::RemoteIssue) -> bool {
    issue.comment_total > issue.comments.len()
}

pub fn worklogs_are_paged(issue: &model::RemoteIssue) -> bool {
    issue.worklog_total > issue.worklogs.len()
}

/// Put a completed comment listing where the rest of jired will find it.
///
/// Into [`model::RemoteIssue::raw`], and the parsed list re-derived from it —
/// not onto the struct alone. That is the whole point: `raw` is the payload
/// `fetch` stores and `pull` re-parses to render a file, so a completion only
/// the struct knew about was dropped before it could reach one. This is why
/// the worklog completion, which has worked this way since it was written,
/// had never actually put a missing worklog in a file.
pub fn set_comments(
    issue: &mut model::RemoteIssue,
    values: Vec<serde_json::Value>,
    markup: Markup,
) {
    issue.raw["fields"]["comment"] = serde_json::json!({
        "comments": values,
        "total": values.len(),
    });
    let (parsed, total) = normalize::comments_from(issue.raw.pointer("/fields/comment"), markup);
    issue.comment_total = total.max(issue.comment_total);
    issue.comments = parsed;
}

/// The same for worklogs. See [`set_comments`].
pub fn set_worklogs(
    issue: &mut model::RemoteIssue,
    values: Vec<serde_json::Value>,
    markup: Markup,
) {
    issue.raw["fields"]["worklog"] = serde_json::json!({
        "worklogs": values,
        "total": values.len(),
    });
    let (parsed, total) = normalize::worklogs_from(issue.raw.pointer("/fields/worklog"), markup);
    issue.worklog_total = total.max(issue.worklog_total);
    issue.worklogs = parsed;
}

/// Build the assignee/reporter payload for a deployment: Cloud wants an
/// `accountId`, Data Center a `name`.
pub fn user_field_value(deployment: Deployment, user_id: Option<&str>) -> serde_json::Value {
    match deployment {
        Deployment::Cloud => cloud::user_field_value(user_id),
        Deployment::DataCenter => datacenter::user_field_value(user_id),
    }
}

/// Run one async operation from a synchronous command handler.
///
/// Commands are one-shot, so a single shared runtime keeps thread creation off
/// the hot path without leaking a runtime per call.
pub fn block_on<F: std::future::Future>(fut: F) -> F::Output {
    static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    let rt = RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(4)
            .enable_all()
            .build()
            .expect("cannot start the async runtime")
    });
    rt.block_on(fut)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deployment_parsing_accepts_the_common_spellings() {
        for s in ["cloud", "Cloud", "CLOUD"] {
            assert_eq!(Deployment::parse(s).unwrap(), Deployment::Cloud);
        }
        for s in ["datacenter", "data-center", "dc", "server"] {
            assert_eq!(Deployment::parse(s).unwrap(), Deployment::DataCenter);
        }
        assert_eq!(
            Deployment::parse("nope").unwrap_err().exit_code().as_i32(),
            2
        );
    }

    #[test]
    fn deployment_guess_from_url() {
        assert_eq!(
            Deployment::guess_from_url("https://acme.atlassian.net"),
            Deployment::Cloud
        );
        assert_eq!(
            Deployment::guess_from_url("https://jira.corp.example/jira"),
            Deployment::DataCenter
        );
    }

    #[test]
    fn markup_follows_deployment() {
        assert_eq!(Deployment::Cloud.markup(), Markup::Adf);
        assert_eq!(Deployment::DataCenter.markup(), Markup::Wiki);
    }

    #[test]
    fn old_data_center_is_refused_with_exit_10() {
        let err = check_supported(Deployment::DataCenter, Some("8.20.1")).unwrap_err();
        assert_eq!(err.exit_code().as_i32(), 10);
        check_supported(Deployment::DataCenter, Some("9.4.0")).unwrap();
        check_supported(Deployment::DataCenter, Some("10.3.2")).unwrap();
        // An unparseable version must not lock the user out.
        check_supported(Deployment::DataCenter, Some("weird")).unwrap();
        check_supported(Deployment::DataCenter, None).unwrap();
    }

    #[test]
    fn auth_kind_parsing() {
        assert_eq!(AuthKind::parse("api-token").unwrap(), AuthKind::ApiToken);
        assert_eq!(AuthKind::parse("PAT").unwrap(), AuthKind::Pat);
        assert_eq!(AuthKind::parse("basic").unwrap(), AuthKind::Basic);
        assert!(AuthKind::parse("magic").is_err());
    }
}
