//! `jired init` [M1.13] — a guided setup: authenticate, verify, choose a
//! project, and leave behind a workspace no later command has to ask about.
//!
//! The wizard only asks what it cannot work out. The URL tells it whether this
//! is Cloud or Data Center, the deployment tells it how to authenticate, the
//! server itself corrects both if the guess was wrong, and everything with a
//! sensible default is written into `.jired/config.toml` — documented — rather
//! than turned into a question. What is left is three answers: where Jira is,
//! who you are, and which project you want.

use crate::api::{AuthKind, Deployment, TlsOptions};
use crate::auth::CredStore;
use crate::cli::cmd::prompt::Ui;
use crate::cli::context::Context;
use crate::cli::GlobalArgs;
use crate::error::{ExitCode, JiredError, Result};
use crate::json::Out;
use crate::store::session::{NewAccount, SessionStore};
use clap::Args;
use secrecy::SecretString;
use std::path::PathBuf;

#[derive(Args, Debug, Clone, Default)]
pub struct InitArgs {
    /// Skip the initial pull
    #[arg(long)]
    pub no_pull: bool,

    /// Re-authenticate an already initialized workspace
    #[arg(long)]
    pub force: bool,

    /// Do not write the agent instructions and permission files
    #[arg(long)]
    pub no_agents: bool,

    #[command(flatten)]
    pub conn: crate::cli::ConnectionArgs,
}

#[derive(serde::Serialize)]
pub struct InitReport {
    pub account: AccountReport,
    pub workspace: WorkspaceReport,
    pub pulled: Option<crate::sync::pull::PullReport>,
}

#[derive(serde::Serialize)]
pub struct AccountReport {
    pub base_url: String,
    pub deployment: String,
    pub auth_kind: String,
    pub identity: Option<String>,
    pub display_name: Option<String>,
    pub account_id: Option<String>,
    pub secret_storage: String,
    /// Where the credential lives: keychain, local_db, or user_config.
    pub secret_location: String,
    pub tls: TlsReport,
}

#[derive(serde::Serialize)]
pub struct TlsReport {
    /// Extra CA bundle in use, whether supplied or pinned during this run.
    pub ca_path: Option<String>,
    pub insecure: bool,
    /// Set when this run pinned a certificate the user chose to trust.
    pub trusted_fingerprint: Option<String>,
}

#[derive(serde::Serialize)]
pub struct WorkspaceReport {
    pub root: String,
    pub config: String,
    pub project: Option<String>,
    pub gitignore_updated: bool,
    /// What `jired init-agents` wrote: instructions, then permission files.
    pub agent_files: Vec<String>,
}

pub fn run(g: &GlobalArgs, args: &InitArgs, out: Out) -> Result<ExitCode> {
    let mut ctx = Context::open_uninitialized_with(g, &args.conn, out)?;
    let ui = Ui::new(ctx.resolver.interactive(), !out.json && !out.quiet).assume_yes(g.yes);

    if ctx.workspace.is_initialized() && !args.force {
        let has_account = SessionStore::open(&ctx.workspace)
            .ok()
            .and_then(|s| s.account().ok().flatten())
            .is_some();
        if has_account {
            return Err(
                JiredError::usage("this directory is already a jired workspace")
                    .with_detail("root", ctx.workspace.root().display().to_string())
                    .with_hint("pass --force to re-authenticate"),
            );
        }
    }

    ctx.workspace.create()?;
    ctx.lock()?;

    ui.title("jired init");
    ui.detail(ctx.workspace.root().display().to_string());

    let wanted_store = match ctx.resolver.get_quiet("creds.storage")? {
        Some(v) => Some(CredStore::parse(&v)?),
        None => None,
    };

    // 1. Find a server that answers, with credentials it accepts.
    // `--force` means "authenticate again", so it never reuses a stored token.
    let conn = connect(&mut ctx, &ui, wanted_store, !args.force)?;

    // 2. Persist the account and its secret.
    let session = SessionStore::open(&ctx.workspace)?;
    let account = session.upsert_account(NewAccount {
        base_url: conn.base_url.clone(),
        deployment: conn.probe.deployment,
        auth_kind: conn.auth_kind,
        identity: conn.identity.clone(),
        display_name: conn.probe.me.display_name.clone(),
        account_id: Some(conn.probe.me.account_id.clone()),
        tls_ca_path: conn.tls.ca_path.as_ref().map(|p| p.display().to_string()),
        tls_insecure: conn.tls.insecure,
        token: conn.token.clone(),
        allow_plaintext: args.conn.insecure_store,
        store: wanted_store,
    })?;

    ui.good(format!(
        "signed in as {}",
        who(
            conn.probe.me.display_name.as_deref(),
            conn.identity.as_deref(),
            &conn.probe.me.account_id
        )
    ));
    if let Some(title) = &conn.probe.server_title {
        ui.detail(match &conn.probe.server_version {
            Some(v) => format!("{title} {v}"),
            None => title.clone(),
        });
    }
    let location = account.secret_location();
    match account.secret_storage() {
        "plaintext" => ui.caution(format!(
            "token stored UNENCRYPTED in {}",
            location.describe()
        )),
        "keyring" => ui.detail(format!("token stored in {}", location.describe())),
        _ => ui.detail(format!("token stored encrypted in {}", location.describe())),
    }

    // 3. Choose what this workspace tracks.
    let project = choose_project(&mut ctx, &ui, &account)?;

    // 4. Write a config file that answers every later question.
    let mut cfg = ctx.local_config()?;
    cfg.set_documented("server.url", &conn.base_url)?;
    cfg.set_documented("server.deployment", conn.probe.deployment.as_str())?;
    cfg.set_documented("auth.kind", conn.auth_kind.as_str())?;
    if let Some(email) = &conn.identity {
        cfg.set_documented("auth.email", email)?;
    }
    if let Some(p) = &project {
        cfg.set_documented("workspace.project", p)?;
    }
    if let Some(ca) = &conn.tls.ca_path {
        cfg.set_documented("tls.ca_cert", &ca.display().to_string())?;
    }
    // Record where the credential actually went, not where it was asked to go:
    // with the setting unset that is the only place the answer is written down,
    // and `doctor` compares against it later.
    cfg.set_documented("creds.storage", account.secret_location().as_str())?;
    // Everything else lands at its default, spelled out and commented, so the
    // file is the whole picture rather than a fragment plus hidden behaviour.
    for (key, value) in crate::config::DEFAULTS {
        if cfg.get(key).is_none() {
            cfg.set_documented(key, value)?;
        }
    }
    cfg.save_to(&ctx.workspace.config_path())?;

    // The resolver read its config file before this one existed, so tell it what
    // was just decided; otherwise the initial pull would go looking for a
    // project nobody had told it about.
    ctx.resolver.set_resolved("server.url", &conn.base_url);
    ctx.resolver
        .set_resolved("server.deployment", conn.probe.deployment.as_str());
    ctx.resolver
        .set_resolved("auth.kind", conn.auth_kind.as_str());
    if let Some(email) = &conn.identity {
        ctx.resolver.set_resolved("auth.email", email);
    }
    if let Some(p) = &project {
        ctx.resolver.set_resolved("workspace.project", p);
    }

    let gitignore_updated = ctx.workspace.ensure_gitignore()?;
    let mut agent_files = Vec::new();
    if !args.no_agents {
        use crate::cli::cmd::init_agents as agents;
        let docs = agents::render();
        for name in ["CLAUDE.md", "AGENTS.md"] {
            agents::write_managed_block(&ctx.workspace.root().join(name), &docs)?;
            agent_files.push(name.to_string());
        }
        // An agent that has to ask before every `jired status` is an agent
        // nobody lets near the workspace; granting the commands that cannot
        // change anything is part of setting one up.
        agents::write_permissions(ctx.workspace.root())?;
        agent_files.push(agents::CLAUDE_SETTINGS.to_string());
        agent_files.push(agents::CODEX_RULES.to_string());
    }

    // Creating the state database now means every later command can assume it.
    crate::store::state::StateStore::open(&ctx.workspace)?;

    // 5. Report what was set up, then optionally fill the workspace.
    out.say("");
    out.say(format!(
        "workspace ready: {}",
        ctx.workspace.root().display()
    ));
    out.say(format!(
        "  account    {}",
        who(
            conn.probe.me.display_name.as_deref(),
            conn.identity.as_deref(),
            &conn.probe.me.account_id
        )
    ));
    out.say(format!(
        "  server     {} ({})",
        conn.base_url,
        deployment_label(conn.probe.deployment)
    ));
    out.say(format!(
        "  project    {}",
        project.as_deref().unwrap_or("every visible project")
    ));
    out.say(format!(
        "  credential {} ({})",
        account.secret_location().describe(),
        account.secret_storage()
    ));
    out.say(format!(
        "  config     {}",
        ctx.workspace.config_path().display()
    ));
    if !agent_files.is_empty() {
        out.say(format!("  agents     {}", agent_files.join(", ")));
    }

    // Pulling needs a scope; without a project there is nothing to ask the
    // server for, and failing here would undo an otherwise finished setup.
    let pulled = if args.no_pull || project.is_none() {
        if project.is_none() && !args.no_pull {
            out.say("");
            out.say("no project selected — `jired fetch --jql <query>` chooses what to pull");
        }
        None
    } else {
        out.say("");
        let pull_args = crate::cli::cmd::pull::PullArgs::default();
        // Setup succeeded whatever the network did next. Propagating this error
        // would report a failed `init` for a workspace that is fully configured,
        // and would swallow the "next:" block that says how to retry.
        match crate::sync::pull::run(&mut ctx, &pull_args) {
            Ok(report) => {
                report.render(out);
                Some(report)
            }
            Err(e) => {
                out.warn(format!(
                    "the workspace is set up, but the first pull failed: {e}"
                ));
                if let Some(hint) = &e.hint {
                    out.warn(hint);
                }
                None
            }
        }
    };

    out.say("");
    out.say("next:");
    if pulled.is_none() {
        out.say("  jired pull     write issue files into this directory");
    }
    out.say("  jired status   what is dirty, behind, or conflicted");
    out.say("  jired diff     review changes before sending them");

    let report = InitReport {
        account: AccountReport {
            base_url: conn.base_url,
            deployment: conn.probe.deployment.as_str().to_string(),
            auth_kind: conn.auth_kind.as_str().to_string(),
            identity: conn.identity,
            display_name: conn.probe.me.display_name,
            account_id: Some(conn.probe.me.account_id),
            secret_storage: account.secret_storage().to_string(),
            secret_location: account.secret_location().as_str().to_string(),
            tls: TlsReport {
                ca_path: conn.tls.ca_path.map(|p| p.display().to_string()),
                insecure: conn.tls.insecure,
                trusted_fingerprint: conn.trusted_fingerprint,
            },
        },
        workspace: WorkspaceReport {
            root: ctx.workspace.root().display().to_string(),
            config: ctx.workspace.config_path().display().to_string(),
            project,
            gitignore_updated,
            agent_files,
        },
        pulled,
    };
    out.emit("init", &report);
    Ok(ExitCode::Success)
}

/// A server that answered, and the credentials it accepted.
struct Connected {
    base_url: String,
    auth_kind: AuthKind,
    identity: Option<String>,
    token: SecretString,
    tls: TlsOptions,
    trusted_fingerprint: Option<String>,
    probe: crate::api::Probe,
}

/// Gather connection details and keep trying until the server accepts them.
///
/// Non-interactively this is a straight line that fails on the first problem;
/// interactively every failure comes back as a question, because the answer to
/// "wrong token" is another token, not a stack trace and a lost setup.
fn connect(
    ctx: &mut Context,
    ui: &Ui,
    store: Option<CredStore>,
    reuse_stored: bool,
) -> Result<Connected> {
    let mut tls = TlsOptions::from_resolver(&mut ctx.resolver)?;
    // A certificate trusted during an earlier init stays trusted; that decision
    // was made once and does not need making again.
    if tls.ca_path.is_none() && ctx.workspace.trusted_ca_path().is_file() {
        tls.ca_path = Some(ctx.workspace.trusted_ca_path());
        ui.detail(format!(
            "using the certificate trusted earlier ({})",
            ctx.workspace.trusted_ca_path().display()
        ));
    }
    if tls.insecure {
        ui.caution("TLS certificate verification is disabled for this workspace");
    }

    let fixed_deployment = match ctx.resolver.get_quiet("server.deployment")? {
        Some(v) => Some(Deployment::parse(&v)?),
        None => None,
    };
    let fixed_auth = match ctx.resolver.get_quiet("auth.kind")? {
        Some(v) => Some(AuthKind::parse(&v)?),
        None => None,
    };

    let mut base_url = match ctx.resolver.get_quiet("server.url")? {
        Some(v) => normalize_url(&v),
        None if ui.interactive() => ask_url(ui)?,
        None => normalize_url(&ctx.resolver.require("server.url")?),
    };
    let mut identity = ctx.resolver.get_quiet("auth.email")?;
    let mut deployment = fixed_deployment.unwrap_or_else(|| Deployment::guess_from_url(&base_url));
    let mut auth_kind = fixed_auth.unwrap_or_else(|| default_auth(deployment, identity.is_some()));
    let mut token = ctx
        .resolver
        .get_quiet("auth.token")?
        .map(SecretString::from);
    let mut reuse_stored = reuse_stored;
    let mut announced: Option<(Deployment, AuthKind)> = None;
    let mut trusted_fingerprint = None;
    let mut certificate_offered = false;

    loop {
        if announced != Some((deployment, auth_kind)) {
            ui.blank();
            ui.good(format!(
                "{} — {}",
                deployment_label(deployment),
                auth_label(auth_kind)
            ));
            announced = Some((deployment, auth_kind));
        }

        if needs_email(auth_kind) && identity.is_none() {
            identity = Some(if ui.interactive() {
                ask_email(ui)?
            } else {
                ctx.resolver.require("auth.email")?
            });
        }
        if token.is_none() && reuse_stored {
            // A keychain entry or the shared credential file may already hold
            // this account's token from another workspace; asking again for
            // something the machine can already produce is busywork.
            let entry = crate::auth::keyring_user(&base_url, identity.as_deref());
            if let Some(found) = crate::auth::existing(&entry, store) {
                ui.good(format!("using the credential already stored for {entry}"));
                token = Some(found);
            }
        }
        if token.is_none() {
            token = Some(SecretString::from(ask_token(
                ctx, ui, &base_url, deployment, auth_kind,
            )?));
        }
        let candidate = token.clone().expect("a token was just resolved");

        let attempt = ui.working(&format!("verifying with {base_url}"), || {
            crate::api::probe(
                &base_url,
                deployment,
                auth_kind,
                identity.as_deref(),
                &candidate,
                &tls,
            )
        });

        let err = match attempt {
            Ok(probe) => {
                if probe.deployment != deployment {
                    ui.detail(format!(
                        "the server reports {}; using that",
                        deployment_label(probe.deployment)
                    ));
                }
                if let Some(path) = &tls.ca_path {
                    ctx.resolver
                        .set_resolved("tls.ca_cert", path.display().to_string());
                }
                return Ok(Connected {
                    base_url,
                    auth_kind,
                    identity,
                    token: candidate,
                    tls,
                    trusted_fingerprint,
                    probe,
                });
            }
            Err(e) => e,
        };

        if !ui.interactive() {
            return Err(err);
        }

        // An unknown certificate is the server's situation, not a mistake the
        // user made, so it gets its own question before the generic menu.
        if !certificate_offered
            && !tls.insecure
            && tls.ca_path.is_none()
            && crate::api::tls::looks_like_certificate_problem(&err)
        {
            certificate_offered = true;
            if let Some((path, fingerprint)) = offer_certificate(ctx, ui, &base_url)? {
                tls.ca_path = Some(path);
                trusted_fingerprint = Some(fingerprint);
                continue;
            }
        }

        ui.blank();
        ui.failure(err.message.clone());
        if let Some(hint) = err.hint.clone() {
            ui.detail(hint);
        }

        // Whatever the user picks, the next attempt asks rather than reusing:
        // a stored credential that just failed is not worth trying twice.
        reuse_stored = false;
        match recovery(ui, &err, deployment)? {
            Recovery::Credentials => token = None,
            Recovery::Switch => {
                deployment = other_deployment(deployment);
                auth_kind = fixed_auth.unwrap_or_else(|| deployment.default_auth_kind());
                token = None;
            }
            Recovery::Url => {
                base_url = ask_url(ui)?;
                deployment =
                    fixed_deployment.unwrap_or_else(|| Deployment::guess_from_url(&base_url));
                auth_kind =
                    fixed_auth.unwrap_or_else(|| default_auth(deployment, identity.is_some()));
                token = None;
                certificate_offered = false;
            }
            Recovery::Quit => return Err(err),
        }
    }
}

enum Recovery {
    Credentials,
    Switch,
    Url,
    Quit,
}

/// Offer the ways out of a failed sign-in, in the order that most likely fixes
/// this particular failure.
fn recovery(ui: &Ui, err: &JiredError, deployment: Deployment) -> Result<Recovery> {
    let switch = other_deployment(deployment);
    let options = vec![
        "Enter the token again".to_string(),
        format!(
            "Sign in as {} instead ({})",
            deployment_label(switch),
            auth_label(switch.default_auth_kind())
        ),
        "Use a different Jira URL".to_string(),
        "Stop here".to_string(),
    ];
    // A rejected credential is a token problem; anything else is more often the
    // address being wrong.
    let default = if err.kind == crate::error::ErrorKind::Auth {
        0
    } else {
        2
    };
    Ok(match ui.choose("What next?", &options, default)? {
        0 => Recovery::Credentials,
        1 => Recovery::Switch,
        2 => Recovery::Url,
        _ => Recovery::Quit,
    })
}

fn ask_url(ui: &Ui) -> Result<String> {
    loop {
        let answer = ui.ask(
            "Where is your Jira?",
            Some(
                "https://your-team.atlassian.net for Jira Cloud\n\
                 https://jira.company.com or https://jira.company.com/jira for self-hosted",
            ),
            None,
        )?;
        match validate_url(&answer) {
            Ok(url) => return Ok(url),
            Err(why) => ui.detail(why),
        }
    }
}

fn ask_email(ui: &Ui) -> Result<String> {
    loop {
        let answer = ui.ask(
            "Your Atlassian account email",
            Some("the address you sign in to Jira with"),
            None,
        )?;
        if answer.contains('@') && !answer.contains(char::is_whitespace) {
            return Ok(answer);
        }
        ui.detail("that does not look like an email address");
    }
}

fn ask_token(
    ctx: &mut Context,
    ui: &Ui,
    base_url: &str,
    deployment: Deployment,
    auth_kind: AuthKind,
) -> Result<String> {
    if !ui.interactive() {
        return ctx.resolver.require("auth.token");
    }
    let label = match auth_kind {
        AuthKind::ApiToken => "API token",
        AuthKind::Pat => "Personal access token",
        AuthKind::Basic => "Password",
        _ => "Credential",
    };
    if let Some(page) = token_page(base_url, deployment, auth_kind) {
        ui.blank();
        ui.info(format!("Create one at {page}"));
        if browser_available() && ui.confirm("Open that page in your browser?", true)? {
            if let Err(e) = open::that_detached(&page) {
                ui.detail(format!("could not open a browser: {e}"));
            }
        }
    }
    ui.ask_secret(label, Some("paste it here — the input stays hidden"))
}

/// Where this deployment lets a user mint the credential jired needs.
fn token_page(base_url: &str, deployment: Deployment, auth_kind: AuthKind) -> Option<String> {
    match (deployment, auth_kind) {
        (Deployment::Cloud, AuthKind::ApiToken) => {
            Some("https://id.atlassian.com/manage-profile/security/api-tokens".to_string())
        }
        (Deployment::DataCenter, AuthKind::Pat) => Some(format!(
            "{base_url}/secure/ViewProfile.jspa (the Personal Access Tokens tab)"
        )),
        _ => None,
    }
}

/// Whether launching a browser stands a chance of being useful.
///
/// Over ssh into a headless box it would either fail or, worse, open something
/// on a machine nobody is looking at.
fn browser_available() -> bool {
    if cfg!(target_os = "linux") {
        return std::env::var_os("DISPLAY").is_some()
            || std::env::var_os("WAYLAND_DISPLAY").is_some();
    }
    std::env::var_os("SSH_CONNECTION").is_none()
}

/// Show the certificate the server presented and, if the user recognizes it,
/// pin it for this workspace.
///
/// Returns the pinned bundle and its fingerprint, or `None` when the user said
/// no or the certificate could not be read.
fn offer_certificate(ctx: &Context, ui: &Ui, base_url: &str) -> Result<Option<(PathBuf, String)>> {
    let Some((host, port)) = host_and_port(base_url) else {
        return Ok(None);
    };

    ui.blank();
    ui.caution(format!(
        "{host} presented a TLS certificate this machine does not trust"
    ));
    let cert = match ui.working("fetching the certificate", || {
        crate::api::tls::inspect(&host, port)
    }) {
        Ok(cert) => cert,
        Err(e) => {
            ui.detail(format!("could not read the certificate: {}", e.message));
            return Ok(None);
        }
    };

    ui.blank();
    ui.field("issued to", &cert.subject);
    ui.field("issued by", &cert.issuer);
    ui.field(
        "valid",
        format!("{} to {}", cert.not_before, cert.not_after),
    );
    ui.field("covers", cert.names.join(", "));
    ui.field("sha-256", &cert.fingerprint);
    ui.blank();
    ui.detail(cert.summary());

    // Pinning fixes an unknown issuer. It cannot fix a certificate that is
    // expired or issued for someone else, so do not pretend otherwise.
    if cert.expired || !cert.covers_host {
        ui.caution(if cert.expired {
            "trusting it would not help: the certificate is outside its validity period"
        } else {
            "trusting it would not help: the certificate is not valid for this hostname"
        });
        ui.detail(
            "ask whoever runs this Jira for a current certificate, or re-run with --insecure",
        );
        return Ok(None);
    }

    ui.detail("check the fingerprint against what your Jira administrator publishes");
    // Deliberately not honouring `--yes`: pinning a certificate is a trust
    // decision about a fingerprint a person has to have looked at. "Assume yes
    // for confirmations" must not silently mean "trust any certificate".
    if !ui.confirm_deliberate("Trust this certificate for this workspace?", false)? {
        return Ok(None);
    }

    let path = ctx.workspace.trusted_ca_path();
    std::fs::write(&path, cert.pem())
        .map_err(|e| JiredError::generic(format!("cannot write {}: {e}", path.display())))?;
    ui.good(format!("certificate trusted; stored in {}", path.display()));
    Ok(Some((path, cert.fingerprint)))
}

/// Ask which project this workspace tracks.
fn choose_project(
    ctx: &mut Context,
    ui: &Ui,
    account: &crate::store::session::Account,
) -> Result<Option<String>> {
    if let Some(p) = ctx.resolver.get_quiet("workspace.project")? {
        return Ok(Some(p.trim().to_uppercase()));
    }
    if !ui.interactive() {
        return Ok(None);
    }

    let client = crate::api::build_client(ctx, account)?;
    let projects = match ui.working("loading projects", || {
        crate::api::block_on(client.projects())
    }) {
        Ok(projects) => projects,
        Err(e) => {
            ui.caution(format!("cannot list projects: {}", e.message));
            return Ok(None);
        }
    };

    if projects.is_empty() {
        ui.caution("this account cannot see any projects yet");
        return Ok(None);
    }
    if projects.len() == 1 {
        let only = &projects[0];
        ui.good(format!("tracking {} — {}", only.key, only.name));
        return Ok(Some(only.key.clone()));
    }
    pick_project(ui, &projects)
}

/// How many projects to list before asking for a filter instead.
const PROJECT_PAGE: usize = 12;

fn pick_project(ui: &Ui, projects: &[crate::api::Project]) -> Result<Option<String>> {
    let mut filter = String::new();
    loop {
        let matches: Vec<&crate::api::Project> = projects
            .iter()
            .filter(|p| matches_filter(p, &filter))
            .collect();
        if matches.is_empty() {
            ui.question_line(format!(
                "nothing matches `{filter}`; showing everything again"
            ));
            filter.clear();
            continue;
        }

        ui.blank();
        for (i, p) in matches.iter().take(PROJECT_PAGE).enumerate() {
            ui.question_line(format!("{:>3}. {:<10} {}", i + 1, p.key, p.name));
        }
        if matches.len() > PROJECT_PAGE {
            ui.question_line(format!(
                "     … and {} more — type part of a name to narrow the list",
                matches.len() - PROJECT_PAGE
            ));
        }

        // Enter must not mean "every project": that leaves a workspace whose
        // plain `jired pull` fails with "no project or query to fetch". The
        // no-project route stays available, but has to be asked for by name.
        let answer = ui.ask(
            "Which project should this workspace track?",
            Some(
                "a number, a project key, or part of a name to narrow the list\n\
                 `all` tracks every project you can see (`jired fetch --jql` picks the scope)",
            ),
            Some(&matches[0].key),
        )?;
        if answer.eq_ignore_ascii_case("all") {
            ui.good("tracking every project this account can see");
            return Ok(None);
        }

        if let Ok(n) = answer.parse::<usize>() {
            match matches.iter().take(PROJECT_PAGE).nth(n.wrapping_sub(1)) {
                Some(p) => return Ok(Some(p.key.clone())),
                None => {
                    ui.question_line("that number is not on the list");
                    continue;
                }
            }
        }
        if let Some(p) = projects
            .iter()
            .find(|p| p.key.eq_ignore_ascii_case(answer.trim()))
        {
            return Ok(Some(p.key.clone()));
        }
        // Anything else is a search: it is what someone types when the list is
        // long, and guessing wrong costs them nothing.
        filter = answer.to_lowercase();
    }
}

fn matches_filter(p: &crate::api::Project, filter: &str) -> bool {
    filter.is_empty()
        || p.key.to_lowercase().contains(filter)
        || p.name.to_lowercase().contains(filter)
}

fn who(display_name: Option<&str>, identity: Option<&str>, account_id: &str) -> String {
    match (display_name, identity) {
        (Some(name), Some(email)) => format!("{name} <{email}>"),
        (Some(name), None) => name.to_string(),
        (None, Some(email)) => email.to_string(),
        (None, None) => account_id.to_string(),
    }
}

fn deployment_label(d: Deployment) -> &'static str {
    match d {
        Deployment::Cloud => "Jira Cloud",
        Deployment::DataCenter => "Jira Data Center",
    }
}

fn auth_label(kind: AuthKind) -> &'static str {
    match kind {
        AuthKind::ApiToken => "signing in with your email and an API token",
        AuthKind::Pat => "signing in with a personal access token",
        AuthKind::Basic => "signing in with your username and password",
        AuthKind::Cookie => "signing in with a session cookie",
        AuthKind::OAuth2 => "signing in with OAuth",
    }
}

fn other_deployment(d: Deployment) -> Deployment {
    match d {
        Deployment::Cloud => Deployment::DataCenter,
        Deployment::DataCenter => Deployment::Cloud,
    }
}

fn needs_email(kind: AuthKind) -> bool {
    matches!(kind, AuthKind::ApiToken | AuthKind::Basic)
}

/// Supplying an email only makes sense for API-token auth, so it is a stronger
/// signal than the URL-shape guess about the deployment.
fn default_auth(deployment: Deployment, has_email: bool) -> AuthKind {
    if has_email {
        AuthKind::ApiToken
    } else {
        deployment.default_auth_kind()
    }
}

fn host_and_port(url: &str) -> Option<(String, u16)> {
    // Only https has a certificate to look at.
    let rest = url.strip_prefix("https://")?;
    let authority = rest.split(['/', '?', '#']).next()?;
    let authority = authority.rsplit('@').next()?;
    match authority.rsplit_once(':') {
        Some((host, port)) if !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()) => {
            Some((host.to_string(), port.parse().ok()?))
        }
        _ => Some((authority.to_string(), 443)),
    }
}

/// Trim a trailing slash so stored URLs concatenate predictably.
fn normalize_url(url: &str) -> String {
    let url = url.trim().trim_end_matches('/');
    if url.starts_with("http://") || url.starts_with("https://") {
        url.to_string()
    } else {
        format!("https://{url}")
    }
}

/// Normalize what the user typed, or explain what is wrong with it.
fn validate_url(raw: &str) -> std::result::Result<String, String> {
    let url = normalize_url(raw);
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .unwrap_or("");
    let host = rest.split('/').next().unwrap_or("");
    if host.is_empty() {
        return Err("that has no hostname in it".into());
    }
    if host.contains(char::is_whitespace) || rest.contains(' ') {
        return Err("a URL cannot contain spaces".into());
    }
    if !host.contains('.') && host != "localhost" && !host.starts_with("localhost:") {
        return Err(format!("`{host}` does not look like a hostname"));
    }
    Ok(url)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_normalization() {
        assert_eq!(
            normalize_url("https://x.atlassian.net/"),
            "https://x.atlassian.net"
        );
        assert_eq!(normalize_url("x.atlassian.net"), "https://x.atlassian.net");
        assert_eq!(
            normalize_url("http://jira.corp/jira/"),
            "http://jira.corp/jira"
        );
    }

    #[test]
    fn typed_urls_are_checked_before_a_round_trip_is_attempted() {
        assert_eq!(
            validate_url(" acme.atlassian.net "),
            Ok("https://acme.atlassian.net".to_string())
        );
        assert_eq!(
            validate_url("http://localhost:8080/jira"),
            Ok("http://localhost:8080/jira".to_string())
        );
        assert!(validate_url("").is_err());
        assert!(validate_url("jira corp").is_err());
        assert!(validate_url("jira").is_err());
    }

    #[test]
    fn the_url_tells_us_which_product_and_therefore_which_credential() {
        let cloud = Deployment::guess_from_url("https://acme.atlassian.net");
        assert_eq!(cloud, Deployment::Cloud);
        assert_eq!(default_auth(cloud, false), AuthKind::ApiToken);
        assert!(needs_email(default_auth(cloud, false)));

        let dc = Deployment::guess_from_url("https://jira.corp.example/jira");
        assert_eq!(dc, Deployment::DataCenter);
        assert_eq!(default_auth(dc, false), AuthKind::Pat);
        assert!(!needs_email(default_auth(dc, false)));
        // An email in the environment means API-token auth wherever it points.
        assert_eq!(default_auth(dc, true), AuthKind::ApiToken);
    }

    #[test]
    fn certificate_inspection_targets_the_right_host_and_port() {
        assert_eq!(
            host_and_port("https://jira.corp.example/jira"),
            Some(("jira.corp.example".into(), 443))
        );
        assert_eq!(
            host_and_port("https://jira.corp.example:8443"),
            Some(("jira.corp.example".into(), 8443))
        );
        // Plain http has no certificate to show.
        assert_eq!(host_and_port("http://jira.corp.example"), None);
    }

    #[test]
    fn every_deployment_and_credential_has_a_readable_label() {
        for d in [Deployment::Cloud, Deployment::DataCenter] {
            assert!(!deployment_label(d).is_empty());
            assert!(!auth_label(d.default_auth_kind()).is_empty());
            assert_ne!(other_deployment(d), d);
        }
    }

    #[test]
    fn identity_falls_back_to_whatever_is_known() {
        assert_eq!(who(Some("Ilya A"), Some("a@b.c"), "5b1"), "Ilya A <a@b.c>");
        assert_eq!(who(None, Some("a@b.c"), "5b1"), "a@b.c");
        assert_eq!(who(None, None, "5b1"), "5b1");
    }

    #[test]
    fn project_filtering_matches_key_or_name_case_insensitively() {
        let p = crate::api::Project {
            id: "1".into(),
            key: "PROJ".into(),
            name: "Platform".into(),
            project_type: None,
        };
        assert!(matches_filter(&p, ""));
        assert!(matches_filter(&p, "proj"));
        assert!(matches_filter(&p, "platf"));
        assert!(!matches_filter(&p, "ops"));
    }
}
