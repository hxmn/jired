//! Assembles the per-run context: workspace, resolver, lock, stores.

use crate::cli::GlobalArgs;
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
    lock: Option<WorkspaceLock>,
}

impl Context {
    /// Context for a command that requires an initialized workspace.
    pub fn open(g: &GlobalArgs, out: Out) -> Result<Self> {
        let workspace = match &g.dir {
            Some(dir) => Workspace::at(dir),
            None => Workspace::discover(&std::env::current_dir()?)?,
        };
        Self::build(g, out, workspace)
    }

    /// Context for `init`, where the workspace may not exist yet.
    pub fn open_uninitialized(g: &GlobalArgs, out: Out) -> Result<Self> {
        let root = match &g.dir {
            Some(dir) => dir.clone(),
            None => std::env::current_dir()?,
        };
        Self::build(g, out, Workspace::at(root))
    }

    fn build(g: &GlobalArgs, out: Out, workspace: Workspace) -> Result<Self> {
        let interactive = !g.no_input && is_interactive();
        let local_path = workspace
            .is_initialized()
            .then(|| workspace.config_path())
            .or_else(|| g.config.clone());

        let resolver = Resolver::builder()
            .flag("server.url", g.url.clone())
            .flag("server.deployment", g.deployment.clone())
            .flag("auth.email", g.email.clone())
            .flag("auth.kind", g.auth.clone())
            .flag("auth.token", g.token.clone())
            .flag("workspace.project", g.project.clone())
            .flag("workspace.profile", g.profile.clone())
            .flag(
                "tls.ca_cert",
                g.ca_cert.as_ref().map(|p| p.display().to_string()),
            )
            .flag_bool("tls.insecure", g.insecure)
            .flag_bool("ui.no_input", g.no_input)
            .env_from_process()
            .local_file(local_path)?
            .global_file(crate::config::global_config_path())?
            .interactive(interactive)
            .prompter(Box::new(crate::cli::cmd::prompt::TerminalPrompter))
            .build();

        Ok(Self {
            workspace,
            resolver,
            out,
            yes: g.yes,
            lock: None,
        })
    }

    /// Take the workspace lock for the rest of this run.
    pub fn lock(&mut self) -> Result<()> {
        if self.lock.is_none() {
            self.lock = Some(self.workspace.lock()?);
        }
        Ok(())
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

/// Interaction requires a terminal on both ends: a piped stdin means a script.
pub fn is_interactive() -> bool {
    use is_terminal::IsTerminal;
    std::io::stdin().is_terminal() && std::io::stderr().is_terminal()
}
