//! Assembles the per-run context: workspace, resolver, lock, stores.

use crate::cli::{ConnectionArgs, GlobalArgs};
use crate::config::{file::ConfigFile, Resolver};
use crate::error::{JiredError, Result};
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
        Self::open_with(g, &ConnectionArgs::default(), out)
    }

    /// Context for `init` and `doctor`, which may override the connection.
    pub fn open_with(g: &GlobalArgs, conn: &ConnectionArgs, out: Out) -> Result<Self> {
        let workspace = match &g.dir {
            // An explicit `--dir` must still point at a workspace. Without this
            // check the stores below would happily create `.jired/` inside a
            // typo'd path and only then complain about missing credentials.
            Some(dir) => {
                let ws = Workspace::at(dir);
                if !ws.is_initialized() {
                    return Err(JiredError::usage(format!(
                        "no jired workspace in {}",
                        dir.display()
                    ))
                    .with_detail("dir", dir.display().to_string())
                    .with_hint("run `jired init --dir <path>` to create one"));
                }
                ws
            }
            None => Workspace::discover(&std::env::current_dir()?)?,
        };
        Self::build(g, conn, out, workspace)
    }

    /// Context for `init`: the workspace is *this* directory, whether or not it
    /// exists yet. Deliberately not `discover`ing — running `init` inside a
    /// subdirectory of an existing workspace must set up here, not up there.
    pub fn open_uninitialized(g: &GlobalArgs, out: Out) -> Result<Self> {
        Self::open_uninitialized_with(g, &ConnectionArgs::default(), out)
    }

    pub fn open_uninitialized_with(
        g: &GlobalArgs,
        conn: &ConnectionArgs,
        out: Out,
    ) -> Result<Self> {
        let root = match &g.dir {
            Some(dir) => dir.clone(),
            None => std::env::current_dir()?,
        };
        Self::build(g, conn, out, Workspace::at(root))
    }

    /// Context for `config`, which has to work before `init` — setting
    /// `creds.storage --global` beforehand is a documented flow — but should
    /// still find the enclosing workspace when there is one.
    pub fn open_optional(g: &GlobalArgs, out: Out) -> Result<Self> {
        let root = match &g.dir {
            Some(dir) => dir.clone(),
            None => std::env::current_dir()?,
        };
        let workspace = Workspace::discover(&root).unwrap_or_else(|_| Workspace::at(root));
        Self::build(g, &ConnectionArgs::default(), out, workspace)
    }

    fn build(
        g: &GlobalArgs,
        conn: &ConnectionArgs,
        out: Out,
        workspace: Workspace,
    ) -> Result<Self> {
        let local_path = workspace
            .is_initialized()
            .then(|| workspace.config_path())
            .or_else(|| g.config.clone());

        // Two passes: whether we may prompt is itself a setting, so the chain has
        // to be resolvable before it is answered. The first resolver is built
        // non-interactive purely to read `ui.no_input`.
        let mut probe = Resolver::builder()
            .flag_bool("ui.no_input", g.no_input)
            .env_from_process()
            .local_file(local_path.clone())?
            .global_file(crate::config::global_config_path())?
            .interactive(false)
            .build();
        let no_input = g.no_input || probe.get_bool("ui.no_input")?.unwrap_or(false);
        let interactive = !no_input && is_interactive();

        let mut resolver = Resolver::builder()
            .flag("server.url", conn.url.clone())
            .flag(
                "server.deployment",
                conn.deployment.map(|d| d.as_str().to_string()),
            )
            .flag("auth.email", conn.email.clone())
            .flag("auth.kind", conn.auth.map(|a| a.as_str().to_string()))
            .flag("auth.token", conn.token.clone())
            .flag("workspace.project", g.project.clone())
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

        // The flag is warned about in `cli::run`, which fires even when the
        // command never gets this far. This covers the case that flag cannot:
        // `tls.insecure` set in a config file or the environment long ago.
        if !g.insecure && resolver.get_bool("tls.insecure")?.unwrap_or(false) {
            out.warn(crate::cli::INSECURE_TLS_WARNING);
        }

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
