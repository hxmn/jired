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

/// What `-V` prints: the release, the commit it was built from, and when.
pub const VERSION: &str = concat!(
    env!("CARGO_PKG_VERSION"),
    " (",
    env!("JIRED_COMMIT"),
    " ",
    env!("JIRED_BUILD_TIME"),
    ")"
);

/// What `--version` prints: the same, plus what a bug report always ends up
/// asking for next.
pub const LONG_VERSION: &str = concat!(
    env!("CARGO_PKG_VERSION"),
    "\ncommit:  ",
    env!("JIRED_COMMIT"),
    "\nbuilt:   ",
    env!("JIRED_BUILD_TIME"),
    "\nrustc:   ",
    env!("JIRED_RUSTC"),
    "\ntarget:  ",
    env!("JIRED_TARGET")
);

const EXAMPLES: &str = "\
Examples:
  jired init                      set up this directory against a Jira project
  jired pull                      write issue files and say what changed
  jired diff                      review local edits before sending them
  jired push --dry-run            show the exact API calls, send nothing
  jired push                      apply

Run `jired help <command>` for a command's own options.";

#[derive(Parser, Debug)]
#[command(
    name = "jired",
    version = VERSION,
    long_version = LONG_VERSION,
    about = "Work with Jira issues as Markdown files, offline-first",
    long_about = "Work with Jira issues as Markdown files, offline-first.\n\n\
                  Issues are fetched into the current directory as `.md` files and \
                  synced back with a git-like cycle: fetch, pull, diff, push. All \
                  reads and edits work without a network.",
    after_help = EXAMPLES,
)]
pub struct Cli {
    #[command(flatten)]
    pub global: GlobalArgs,

    #[command(subcommand)]
    pub command: Command,
}

/// Flags every command understands.
///
/// Grouped under their own help heading and displayed last, so a command's own
/// options are what the reader sees first.
#[derive(Args, Debug, Clone, Default)]
#[command(next_help_heading = "Global options", next_display_order = 900)]
pub struct GlobalArgs {
    /// Emit machine-readable JSON on stdout; human text goes to stderr
    #[arg(long, global = true)]
    pub json: bool,

    /// Never prompt; fail with exit code 7 if a required value is missing
    /// [env: JIRED_NO_INPUT] [config: ui.no_input]
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

    /// When to colour output
    #[arg(long, global = true, value_name = "WHEN", default_value = "auto")]
    pub color: crate::style::ColorChoice,

    /// Workspace directory (defaults to the current directory)
    #[arg(long, global = true, value_name = "PATH", value_hint = clap::ValueHint::DirPath)]
    pub dir: Option<PathBuf>,

    /// Additional config file to read before the global one
    #[arg(long, global = true, value_name = "PATH", value_hint = clap::ValueHint::FilePath)]
    pub config: Option<PathBuf>,

    /// Project key [env: JIRED_PROJECT] [config: workspace.project]
    #[arg(long, global = true, value_name = "KEY")]
    pub project: Option<String>,

    /// Extra CA bundle (PEM) for self-signed instances
    /// [env: JIRED_CA_CERT] [config: tls.ca_cert]
    #[arg(long, global = true, value_name = "PATH", value_hint = clap::ValueHint::FilePath)]
    pub ca_cert: Option<PathBuf>,

    /// Skip TLS certificate verification (dangerous)
    /// [env: JIRED_INSECURE] [config: tls.insecure]
    #[arg(long, global = true)]
    pub insecure: bool,
}

/// How to reach and authenticate against Jira.
///
/// Only `init` and `doctor` act on these: every other command uses the account
/// `init` stored. Keeping them off the global list is what stops
/// `jired pull --url other` from looking like it switches servers when it does
/// not.
#[derive(Args, Debug, Clone, Default)]
#[command(next_help_heading = "Connection")]
pub struct ConnectionArgs {
    /// Jira base URL [env: JIRED_URL] [config: server.url]
    #[arg(long, value_name = "URL")]
    pub url: Option<String>,

    /// API token or personal access token [env: JIRED_TOKEN]
    #[arg(long, value_name = "TOKEN", hide_env_values = true)]
    pub token: Option<String>,

    /// Account email for Jira Cloud [env: JIRED_EMAIL] [config: auth.email]
    #[arg(long, value_name = "EMAIL")]
    pub email: Option<String>,

    /// Authentication method [env: JIRED_AUTH] [config: auth.kind]
    #[arg(long, value_name = "KIND")]
    pub auth: Option<crate::api::AuthKind>,

    /// Deployment kind [env: JIRED_DEPLOYMENT] [config: server.deployment]
    #[arg(long, value_name = "KIND")]
    pub deployment: Option<crate::api::Deployment>,

    /// Store credentials unencrypted when no keychain is available
    #[arg(long)]
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
    #[command(visible_alias = "st")]
    Status(cmd::status::StatusArgs),
    /// Show what changed, newest first — the whole workspace, or one issue
    Log(cmd::log::LogArgs),
    /// Read the working copy back as figures
    #[command(subcommand)]
    Report(cmd::report::ReportCmd),
    /// Add a comment to an issue
    Comment(cmd::comment::CommentArgs),
    /// Book time against an issue
    Logwork(cmd::logwork::LogworkArgs),
    /// Create a new issue locally
    Create(cmd::create::CreateArgs),
    /// Edit an issue in a form
    Edit(cmd::edit::EditArgs),
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
    /// Drop local changes and go back to the last sync
    Reset(cmd::reset::ResetArgs),
    /// Resolve recorded conflicts
    Resolve(cmd::resolve::ResolveArgs),
    /// Remove files whose issues left the workspace scope
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

/// Shown on every run whose traffic is unverified, whether the setting came
/// from `--insecure`, `JIRED_INSECURE` or `tls.insecure` in a config file.
pub const INSECURE_TLS_WARNING: &str =
    "TLS certificate verification is disabled (--insecure / tls.insecure); \
     traffic can be intercepted";

pub fn run(cli: Cli) -> ExitCode {
    let out = Out::new(cli.global.json, cli.global.quiet);
    crate::style::init(cli.global.color);
    crate::json::set_verbose(cli.global.verbose > 0);
    init_tracing(cli.global.verbose);

    // Warned here as well as in `Context` so it still appears when the command
    // fails before a context exists. `Context` covers the other half: the same
    // setting coming from the config file or the environment, months later.
    if cli.global.insecure {
        out.warn(INSECURE_TLS_WARNING);
    }

    let result = dispatch(&cli, out);

    // A reader that closed the pipe (`jired status | head`) is not an error;
    // the work finished, nobody is listening any more.
    if crate::json::stdout_is_closed() {
        return ExitCode::Success;
    }

    match result {
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
        Command::Report(a) => cmd::report::run(g, a, out),
        Command::Comment(a) => cmd::comment::run(g, a, out),
        Command::Logwork(a) => cmd::logwork::run(g, a, out),
        Command::Create(a) => cmd::create::run(g, a, out),
        Command::Edit(a) => cmd::edit::run(g, a, out),
        Command::Transition(a) => cmd::transition::run(g, a, out),
        Command::Assign(a) => cmd::assign::run(g, a, out),
        Command::Attach(a) => cmd::attach::run(g, a, out),
        Command::Attachments(a) => cmd::attach::run_attachments(g, a, out),
        Command::Open(a) => cmd::open::run(g, a, out),
        Command::Search(a) => cmd::search::run(g, a, out),
        Command::Reset(a) => cmd::reset::run(g, a, out),
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
///
/// A usage error, not a generic one: the user asked for something jired does not
/// do, which is the same class of mistake as a bad flag, and an agent has to be
/// able to tell "unimplemented" from "crashed". `instead` is what to do today;
/// the milestone id is internal roadmap vocabulary and stays out of the message.
pub(crate) fn not_yet(what: &str, milestone: &str, instead: &str) -> JiredError {
    JiredError::usage(format!("`{what}` is not implemented yet"))
        .with_detail("milestone", milestone)
        .with_hint(instead)
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
        let mut init = Cli::command();
        let init = init
            .find_subcommand_mut("init")
            .expect("init is a subcommand");
        let help = init.render_long_help().to_string();
        assert!(help.contains("--token"));
        assert!(
            !help.to_lowercase().contains("default: "),
            "no defaults echoed"
        );
    }

    /// Connection flags belong to the commands that act on them. Elsewhere they
    /// were accepted and silently ignored, which reads like switching servers.
    #[test]
    fn connection_flags_are_not_global() {
        let root = Cli::command().render_long_help().to_string();
        for flag in ["--token", "--email", "--auth ", "--deployment"] {
            assert!(
                !root.contains(flag),
                "{flag} should not be on every command's help"
            );
        }
        assert!(Cli::try_parse_from(["jired", "pull", "--token", "x"]).is_err());
        assert!(Cli::try_parse_from(["jired", "init", "--token", "x"]).is_ok());
    }

    /// `jired pull PROJ-1` used to fail while `jired push PROJ-1` worked.
    #[test]
    fn issues_are_positional_everywhere() {
        for cmd in ["pull", "fetch", "push", "diff", "resolve"] {
            assert!(
                Cli::try_parse_from(["jired", cmd, "PROJ-1"]).is_ok(),
                "{cmd} should accept a positional issue"
            );
        }
    }

    /// The root help is where someone lands first; it should show the loop.
    #[test]
    fn root_help_carries_examples() {
        let help = Cli::command().render_long_help().to_string();
        assert!(help.contains("jired init"));
        assert!(help.contains("Examples:"));
    }
}
