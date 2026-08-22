//! `jired whoami` [M1.13] — who the stored credentials authenticate as.

use crate::cli::context::Context;
use crate::cli::GlobalArgs;
use crate::error::{ExitCode, Result};
use crate::json::Out;
use crate::style::{self, BOLD, CYAN, DIM};
use clap::Args;

#[derive(Args, Debug, Clone, Default)]
pub struct WhoamiArgs {
    /// Ask the server instead of reporting the cached identity
    #[arg(long)]
    pub refresh: bool,
}

#[derive(serde::Serialize)]
pub struct WhoamiReport {
    pub base_url: String,
    pub deployment: String,
    pub auth_kind: String,
    pub identity: Option<String>,
    pub display_name: Option<String>,
    pub account_id: Option<String>,
    pub secret_storage: String,
    /// Where the credential is kept: keychain, local_db, or user_config.
    pub secret_location: String,
}

pub fn run(g: &GlobalArgs, args: &WhoamiArgs, out: Out) -> Result<ExitCode> {
    let mut ctx = Context::open(g, out)?;
    let session = crate::store::session::SessionStore::open(&ctx.workspace)?;
    let account = session.require_account()?;

    let (display_name, account_id) = if args.refresh {
        let client = crate::api::build_client(&mut ctx, &account)?;
        let me = crate::api::block_on(client.myself())?;
        session.record_verification(account.id, &me)?;
        (me.display_name, Some(me.account_id))
    } else {
        (account.display_name.clone(), account.account_id.clone())
    };

    let report = WhoamiReport {
        base_url: account.base_url.clone(),
        deployment: account.deployment.as_str().to_string(),
        auth_kind: account.auth_kind.as_str().to_string(),
        identity: account.identity.clone(),
        display_name,
        account_id,
        secret_storage: account.secret_storage().to_string(),
        secret_location: account.secret_location().as_str().to_string(),
    };

    out.say(format!(
        "{} {} on {} {}",
        style::out(BOLD, report.display_name.as_deref().unwrap_or("?")),
        style::out(
            DIM,
            format!("<{}>", report.identity.as_deref().unwrap_or("?"))
        ),
        style::out(CYAN, &report.base_url),
        style::out(DIM, format!("({})", report.deployment))
    ));
    out.say(style::out(
        DIM,
        format!(
            "credential in {} ({})",
            account.secret_location().describe(),
            report.secret_storage
        ),
    ));
    out.emit("whoami", &report);
    Ok(ExitCode::Success)
}
