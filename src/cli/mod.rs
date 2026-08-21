//! Command-line surface and dispatch.
//!
//! Every value a command needs is resolvable through the chain in
//! [`crate::config`]; the flags here are only its highest-priority rung.

pub mod cmd;
pub mod context;

use crate::error::{ExitCode, JiredError};
use crate::json::Out;
use clap::{Args, Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(
    name = "jired",
    version,
    about = "Work with Jira issues as Markdown files, offline-first",
    long_about = None,
    disable_help_subcommand = true
)]
pub struct Cli {
    #[command(flatten)]
    pub global: GlobalArgs,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Args, Debug, Clone, Default)]
pub struct GlobalArgs {
    /// Emit machine-readable JSON on stdout; human text goes to stderr
    #[arg(long, global = true)]
    pub json: bool,

    /// Never prompt; fail with exit code 7 if a required value is missing
    #[arg(long, global = true)]
    pub no_input: bool,

    /// Assume yes for confirmations
    #[arg(long, short = 'y', global = true)]
    pub yes: bool,

    /// Suppress informational output
    #[arg(long, short = 'q', global = true)]
    pub quiet: bool,

    /// Increase log verbosity (-v debug, -vv trace)
    #[arg(long, short = 'v', global = true, action = clap::ArgAction::Count)]
    pub verbose: u8,

    /// Workspace directory (defaults to the current directory)
    #[arg(long, global = true, value_name = "PATH")]
    pub dir: Option<PathBuf>,

    /// Additional config file to read before the global one
    #[arg(long, global = true, value_name = "PATH")]
    pub config: Option<PathBuf>,

    /// Jira base URL [env: JIRED_URL]
    #[arg(long, global = true, value_name = "URL")]
    pub url: Option<String>,

    /// API token or personal access token [env: JIRED_TOKEN]
    #[arg(long, global = true, value_name = "TOKEN", hide_env_values = true)]
    pub token: Option<String>,

    /// Account email for Jira Cloud [env: JIRED_EMAIL]
    #[arg(long, global = true, value_name = "EMAIL")]
    pub email: Option<String>,

    /// Authentication method [env: JIRED_AUTH]
    #[arg(long, global = true, value_name = "KIND")]
    pub auth: Option<String>,

    /// Deployment kind: cloud or datacenter [env: JIRED_DEPLOYMENT]
    #[arg(long, global = true, value_name = "KIND")]
    pub deployment: Option<String>,

    /// Project key [env: JIRED_PROJECT]
    #[arg(long, global = true, value_name = "KEY")]
    pub project: Option<String>,

    /// Named account profile [env: JIRED_PROFILE]
    #[arg(long, global = true, value_name = "NAME")]
    pub profile: Option<String>,

    /// Extra CA bundle (PEM) for self-signed instances [env: JIRED_CA_CERT]
    #[arg(long, global = true, value_name = "PATH")]
    pub ca_cert: Option<PathBuf>,

    /// Skip TLS certificate verification (dangerous)
    #[arg(long, global = true)]
    pub insecure: bool,

    /// Store credentials unencrypted when no keychain is available
    #[arg(long, global = true)]
    pub insecure_store: bool,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Authenticate, pick a project, and set up the workspace
    Init(cmd::init::InitArgs),
    /// Download remote state into the local snapshot without touching files
    Fetch(cmd::fetch::FetchArgs),
    /// Fetch and merge remote changes into the working copy
    Pull(cmd::pull::PullArgs),
    /// Send local changes to Jira
    Push(cmd::push::PushArgs),
    /// Show differences between working copy, base, and remote
    Diff(cmd::diff::DiffArgs),
    /// One-screen overview of the workspace
    Status(cmd::status::StatusArgs),
    /// Show an issue's cached changelog
    Log(cmd::log::LogArgs),
    /// Add a comment to an issue
    Comment(cmd::comment::CommentArgs),
    /// Create a new issue locally
    Create(cmd::create::CreateArgs),
    /// Change an issue's workflow status
    Transition(cmd::transition::TransitionArgs),
    /// Assign an issue
    Assign(cmd::assign::AssignArgs),
    /// Stage a file for upload to an issue
    Attach(cmd::attach::AttachArgs),
    /// Work with attachments
    #[command(subcommand)]
    Attachments(cmd::attach::AttachmentsCmd),
    /// Open an issue in the browser
    Open(cmd::open::OpenArgs),
    /// Run a JQL search
    Search(cmd::search::SearchArgs),
    /// Resolve recorded conflicts
    Resolve(cmd::resolve::ResolveArgs),
    /// Remove orphaned files and regenerate grouping folders
    Clean(cmd::clean::CleanArgs),
    /// Check the workspace over and repair what can be repaired
    Doctor(cmd::doctor::DoctorArgs),
    /// Read and write configuration
    #[command(subcommand)]
    Config(cmd::config::ConfigCmd),
    /// Show the authenticated account
    Whoami(cmd::whoami::WhoamiArgs),
    /// List projects
    Projects(cmd::list::ProjectsArgs),
    /// List boards
    Boards(cmd::list::BoardsArgs),
    /// List sprints
    Sprints(cmd::list::SprintsArgs),
    /// Generate a shell completion script
    Completion(cmd::completion::CompletionArgs),
    /// Regenerate CLAUDE.md and AGENTS.md
    AgentDocs(cmd::agent_docs::AgentDocsArgs),
    /// Print JSON Schemas for outputs and the frontmatter format
    Schema(cmd::schema::SchemaArgs),
}

pub fn run(cli: Cli) -> ExitCode {
    let out = Out::new(cli.global.json, cli.global.quiet);
    init_tracing(cli.global.verbose);

    if cli.global.insecure {
        eprintln!(
            "warning: TLS certificate verification is disabled (--insecure); \
             traffic can be intercepted"
        );
    }

    match dispatch(&cli, out) {
        Ok(code) => code,
        Err(err) => {
            out.report_error(&err);
            err.exit_code()
        }
    }
}

fn dispatch(cli: &Cli, out: Out) -> crate::Result<ExitCode> {
    let g = &cli.global;
    match &cli.command {
        // Commands that must run before a workspace exists.
        Command::Completion(a) => cmd::completion::run(a, out),
        Command::Schema(a) => cmd::schema::run(a, out),
        Command::Init(a) => cmd::init::run(g, a, out),

        Command::Fetch(a) => cmd::fetch::run(g, a, out),
        Command::Pull(a) => cmd::pull::run(g, a, out),
        Command::Push(a) => cmd::push::run(g, a, out),
        Command::Diff(a) => cmd::diff::run(g, a, out),
        Command::Status(a) => cmd::status::run(g, a, out),
        Command::Log(a) => cmd::log::run(g, a, out),
        Command::Comment(a) => cmd::comment::run(g, a, out),
        Command::Create(a) => cmd::create::run(g, a, out),
        Command::Transition(a) => cmd::transition::run(g, a, out),
        Command::Assign(a) => cmd::assign::run(g, a, out),
        Command::Attach(a) => cmd::attach::run(g, a, out),
        Command::Attachments(a) => cmd::attach::run_attachments(g, a, out),
        Command::Open(a) => cmd::open::run(g, a, out),
        Command::Search(a) => cmd::search::run(g, a, out),
        Command::Resolve(a) => cmd::resolve::run(g, a, out),
        Command::Clean(a) => cmd::clean::run(g, a, out),
        Command::Doctor(a) => cmd::doctor::run(g, a, out),
        Command::Config(a) => cmd::config::run(g, a, out),
        Command::Whoami(a) => cmd::whoami::run(g, a, out),
        Command::Projects(a) => cmd::list::run_projects(g, a, out),
        Command::Boards(a) => cmd::list::run_boards(g, a, out),
        Command::Sprints(a) => cmd::list::run_sprints(g, a, out),
        Command::AgentDocs(a) => cmd::agent_docs::run(g, a, out),
    }
}

fn init_tracing(verbose: u8) {
    use tracing_subscriber::EnvFilter;
    let default = match verbose {
        0 => "warn",
        1 => "info,jired=debug",
        _ => "debug,jired=trace",
    };
    let filter = EnvFilter::try_from_env("JIRED_LOG").unwrap_or_else(|_| EnvFilter::new(default));
    // Logs always go to stderr so `--json` stdout stays parseable.
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_target(false)
        .try_init();
}

/// Marker for command paths that a later milestone delivers.
pub(crate) fn not_yet(what: &str, milestone: &str) -> JiredError {
    JiredError::generic(format!("`{what}` is not implemented yet"))
        .with_detail("milestone", milestone)
        .with_hint(format!("planned for milestone {milestone}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn global_flags_are_accepted_after_the_subcommand() {
        let cli = Cli::try_parse_from(["jired", "status", "--json", "--no-input"]).unwrap();
        assert!(cli.global.json);
        assert!(cli.global.no_input);
    }

    #[test]
    fn token_flag_never_leaks_into_help() {
        let help = Cli::command().render_long_help().to_string();
        assert!(help.contains("--token"));
        assert!(
            !help.to_lowercase().contains("default: "),
            "no defaults echoed"
        );
    }
}
