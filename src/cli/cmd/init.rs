//! `jired init` [M1.13] — authenticate, verify, and set up the workspace.

use crate::api::{AuthKind, Deployment};
use crate::cli::context::Context;
use crate::cli::GlobalArgs;
use crate::error::{ExitCode, JiredError, Result};
use crate::json::Out;
use crate::store::session::{NewAccount, SessionStore};
use clap::Args;
use secrecy::SecretString;

#[derive(Args, Debug, Clone, Default)]
pub struct InitArgs {
    /// Skip the initial pull
    #[arg(long)]
    pub no_pull: bool,

    /// Re-authenticate an already initialized workspace
    #[arg(long)]
    pub force: bool,
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
}

#[derive(serde::Serialize)]
pub struct WorkspaceReport {
    pub root: String,
    pub project: Option<String>,
    pub gitignore_updated: bool,
    pub agent_docs: Vec<String>,
}

pub fn run(g: &GlobalArgs, args: &InitArgs, out: Out) -> Result<ExitCode> {
    let mut ctx = Context::open_uninitialized(g, out)?;

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

    // 1. Resolve connection details.
    let base_url = normalize_url(&ctx.resolver.require("server.url")?);
    let deployment = match ctx.resolver.get_value("server.deployment")? {
        Some(d) => Deployment::parse(&d)?,
        None => Deployment::guess_from_url(&base_url),
    };
    let email = ctx.resolver.get_value("auth.email")?;
    let auth_kind = match ctx.resolver.get_value("auth.kind")? {
        Some(k) => AuthKind::parse(&k)?,
        // Supplying an email only makes sense for API-token auth, so it is a
        // stronger signal than the URL-shape guess about the deployment.
        None if email.is_some() => AuthKind::ApiToken,
        None => deployment.default_auth_kind(),
    };
    let identity = match auth_kind {
        AuthKind::ApiToken | AuthKind::Basic => Some(ctx.resolver.require("auth.email")?),
        _ => email,
    };
    let token = SecretString::from(ctx.resolver.require("auth.token")?);

    // 2. Verify the credentials before persisting anything.
    let probe = crate::api::probe(
        &base_url,
        deployment,
        auth_kind,
        identity.as_deref(),
        &token,
        &crate::api::TlsOptions::from_resolver(&mut ctx.resolver)?,
    )?;

    if probe.deployment != deployment {
        out.warn(format!(
            "server reports a {} deployment; using that instead of {}",
            probe.deployment.as_str(),
            deployment.as_str()
        ));
    }

    // 3. Persist the account and its secret.
    let session = SessionStore::open(&ctx.workspace)?;
    let account = session.upsert_account(NewAccount {
        base_url: base_url.clone(),
        deployment: probe.deployment,
        auth_kind,
        identity: identity.clone(),
        display_name: probe.me.display_name.clone(),
        account_id: Some(probe.me.account_id.clone()),
        tls_ca_path: ctx.resolver.get_value("tls.ca_cert")?,
        tls_insecure: ctx.resolver.get_bool("tls.insecure")?.unwrap_or(false),
        token,
        allow_plaintext: g.insecure_store,
    })?;

    // 4. Choose a project.
    let project = match ctx.resolver.get_value("workspace.project")? {
        Some(p) => Some(p.to_uppercase()),
        None if ctx.resolver.interactive() => {
            let client = crate::api::build_client(&mut ctx, &account)?;
            let projects = crate::api::block_on(client.projects())?;
            crate::cli::cmd::init::pick_project(&projects, out)?
        }
        None => None,
    };

    // 5. Write workspace config, gitignore, agent docs.
    let mut cfg = ctx.local_config()?;
    cfg.set("server.url", &base_url)?;
    cfg.set("server.deployment", probe.deployment.as_str())?;
    cfg.set("auth.kind", auth_kind.as_str())?;
    if let Some(email) = &identity {
        cfg.set("auth.email", email)?;
    }
    if let Some(p) = &project {
        cfg.set("workspace.project", p)?;
    }
    cfg.save_to(&ctx.workspace.config_path())?;

    let gitignore_updated = ctx.workspace.ensure_gitignore()?;
    let docs = crate::cli::cmd::agent_docs::render();
    let mut agent_docs = Vec::new();
    for name in ["CLAUDE.md", "AGENTS.md"] {
        crate::cli::cmd::agent_docs::write_managed_block(&ctx.workspace.root().join(name), &docs)?;
        agent_docs.push(name.to_string());
    }

    // Creating the state database now means every later command can assume it.
    crate::store::state::StateStore::open(&ctx.workspace)?;

    out.say(format!(
        "initialized {} for {} <{}>",
        ctx.workspace.root().display(),
        probe.me.display_name.as_deref().unwrap_or("?"),
        identity.as_deref().unwrap_or(&probe.me.account_id)
    ));

    // 6. Optional initial pull.
    let pulled = if args.no_pull || project.is_none() {
        if project.is_none() && !args.no_pull {
            out.warn("no project selected; skipping initial pull");
        }
        None
    } else {
        let pull_args = crate::cli::cmd::pull::PullArgs::default();
        Some(crate::sync::pull::run(&mut ctx, &pull_args)?)
    };

    let report = InitReport {
        account: AccountReport {
            base_url,
            deployment: probe.deployment.as_str().to_string(),
            auth_kind: auth_kind.as_str().to_string(),
            identity,
            display_name: probe.me.display_name,
            account_id: Some(probe.me.account_id),
            secret_storage: account.secret_storage().to_string(),
        },
        workspace: WorkspaceReport {
            root: ctx.workspace.root().display().to_string(),
            project,
            gitignore_updated,
            agent_docs,
        },
        pulled,
    };
    out.emit("init", &report);
    Ok(ExitCode::Success)
}

/// Ask which project to track. The full TUI picker arrives in M4.7.
fn pick_project(projects: &[crate::api::Project], out: Out) -> Result<Option<String>> {
    use std::io::{BufRead, Write};
    if projects.is_empty() {
        out.warn("the account can see no projects");
        return Ok(None);
    }
    let mut err = std::io::stderr();
    for (i, p) in projects.iter().enumerate() {
        let _ = writeln!(err, "{:>3}. {:<12} {}", i + 1, p.key, p.name);
    }
    let _ = write!(err, "project number or key: ");
    let _ = err.flush();
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line)?;
    let line = line.trim();
    if line.is_empty() {
        return Ok(None);
    }
    if let Ok(n) = line.parse::<usize>() {
        return Ok(projects.get(n - 1).map(|p| p.key.clone()));
    }
    let wanted = line.to_uppercase();
    Ok(projects
        .iter()
        .find(|p| p.key.eq_ignore_ascii_case(&wanted))
        .map(|p| p.key.clone()))
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
}
