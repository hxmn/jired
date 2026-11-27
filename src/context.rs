//! What a run works with: the workspace, its settings, where output goes, and
//! the client to its server.
//!
//! Outside `cli` so the sync engine can take one without depending on the
//! command line; how a `Context` is put together from flags is
//! `cli::context`'s business, in an `impl` block of its own there.

use crate::api::http::TlsOptions;
use crate::config::{file::ConfigFile, Resolver};
use crate::error::Result;
use crate::json::Out;
use crate::workspace::{Workspace, WorkspaceLock};
use std::path::PathBuf;

pub struct Context {
    pub workspace: Workspace,
    pub resolver: Resolver,
    pub out: Out,
    pub yes: bool,
    pub(crate) lock: Option<WorkspaceLock>,
}

impl Context {
    /// Take the workspace lock for the rest of this run.
    pub fn lock(&mut self) -> Result<()> {
        if self.lock.is_none() {
            self.lock = Some(self.workspace.lock()?);
        }
        Ok(())
    }

    /// The client for this workspace's account: the one way a command gets
    /// one, where each had opened the session store and built it by hand.
    pub fn client(&mut self) -> Result<Box<dyn crate::api::JiraApi>> {
        let session = crate::store::session::SessionStore::open(&self.workspace)?;
        let account = session.require_account()?;
        self.client_for(&account)
    }

    /// The client for `account`, with this workspace's TLS settings where the
    /// account recorded none.
    ///
    /// Here rather than in `api`, so the client knows nothing of where its
    /// connection was looked up: the command layer composes it and hands the
    /// engine the result.
    pub fn client_for(
        &mut self,
        account: &crate::store::session::Account,
    ) -> Result<Box<dyn crate::api::JiraApi>> {
        let tls = TlsOptions {
            ca_path: account
                .tls_ca_path
                .clone()
                .or(self.resolver.get_quiet("tls.ca_cert")?)
                .map(Into::into),
            insecure: account.tls_insecure
                || self.resolver.get_bool("tls.insecure")?.unwrap_or(false),
        };
        crate::api::open_client(crate::api::Connection {
            base_url: account.base_url.clone(),
            deployment: account.deployment,
            auth_kind: account.auth_kind,
            identity: account.identity.clone(),
            token: account.token()?,
            tls,
        })
    }

    pub fn local_config(&self) -> Result<ConfigFile> {
        ConfigFile::load(&self.workspace.config_path())
    }

    pub fn global_config_file(&self) -> Result<ConfigFile> {
        match crate::config::global_config_path() {
            Some(p) => ConfigFile::load(&p),
            None => Ok(ConfigFile::empty()),
        }
    }

    pub fn global_config_path(&self) -> Option<PathBuf> {
        crate::config::global_config_path()
    }
}
