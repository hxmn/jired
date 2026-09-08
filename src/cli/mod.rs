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

    /// Say nothing at all unless the command fails
    ///
    /// Everything `--quiet` silences, plus the warnings and the retry log that
    /// still reach stderr without it — for a cron job or a CI step that should
    /// speak only when something went wrong. The error and the exit code
    /// survive: an exit code with no reason is not something anyone can act on.
    /// `--json` keeps its result document, warnings and all.
    #[arg(long, global = true, conflicts_with = "verbose")]
    pub silent: bool,

    /// Keep only these paths of the --json document: `dirty,results.ok`
    ///
    /// Dotted names walk objects and map over arrays, so `results.ok` keeps the
    /// `ok` of every result. `schema` is always kept. A projected document no
    /// longer matches its published schema — that is what was asked for.
    #[arg(long, global = true, value_name = "PATHS", value_delimiter = ',')]
    pub fields: Vec<String>,

    /// Drop null and empty values from the --json document
    #[arg(long, global = true)]
    pub json_compact: bool,

    /// Increase log verbosity (-v debug, -vv trace)
    #[arg(long, short = 'v', global = true, action = clap::ArgAction::Count)]
    pub verbose: u8,

    /// When to colour output
    #[arg(long, global = true, value_name = "WHEN", default_value = "auto")]
    pub color: crate::style::ColorChoice,

    /// Plain output, no colour — the same as `--color never`
    ///
    /// The spelling everything else uses, so nobody has to discover that this
    /// tool calls it `--color never`.
    #[arg(long, global = true, conflicts_with = "color")]
    pub no_color: bool,

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

impl GlobalArgs {
    /// What the two colour flags add up to.
    ///
    /// `--no-color` is the shorthand; `--color` is the one that can also say
    /// `always`. They conflict at the parser, so only one is ever set.
    pub fn color_choice(&self) -> crate::style::ColorChoice {
        if self.no_color {
            crate::style::ColorChoice::Never
        } else {
            self.color
        }
    }
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

/// Every command, in the order `--help` lists them: alphabetical.
///
/// Not the order somebody works in — fetch, pull, edit, push — because a help
/// listing is not a tutorial: it is where you go having half-remembered a name,
/// and the only order that answers "is there one called `watch`" is the one a
/// dictionary uses. The `run` match below follows the same order so the two
/// cannot drift.
#[derive(Subcommand, Debug)]
pub enum Command {
    /// Assign an issue
    Assign(cmd::assign::AssignArgs),
    /// Stage a file for upload to an issue
    Attach(cmd::attach::AttachArgs),
    /// Work with attachments
    #[command(subcommand)]
    Attachments(cmd::attach::AttachmentsCmd),
    /// List boards
    Boards(cmd::list::BoardsArgs),
    /// Remove files whose issues left the workspace scope
    Clean(cmd::clean::CleanArgs),
    /// Add a comment to an issue
    Comment(cmd::comment::CommentArgs),
    /// Generate a shell completion script
    Completion(cmd::completion::CompletionArgs),
    /// Read and write configuration
    #[command(subcommand)]
    Config(cmd::config::ConfigCmd),
    /// Create a new issue locally
    Create(cmd::create::CreateArgs),
    /// Show differences between working copy, base, and remote
    Diff(cmd::diff::DiffArgs),
    /// Check the workspace over and repair what can be repaired
    Doctor(cmd::doctor::DoctorArgs),
    /// Edit an issue in a form
    Edit(cmd::edit::EditArgs),
    /// Download remote state into the local snapshot without touching files
    Fetch(cmd::fetch::FetchArgs),
    /// Write the server's own description of its REST API to a directory
    FetchSpec(cmd::fetch_spec::FetchSpecArgs),
    /// What this instance's fields are called
    Fields(cmd::fields::FieldsArgs),
    /// Authenticate, pick a project, and set up the workspace
    Init(cmd::init::InitArgs),
    /// Write the agent instructions and the read-only command permissions
    InitAgents(cmd::init_agents::InitAgentsArgs),
    /// Put a label on issues, or take one off
    #[command(subcommand)]
    Label(cmd::label::LabelCmd),
    /// Which issues in this workspace match, without asking the server
    List(cmd::list_issues::ListArgs),
    /// Show what changed, newest first — the whole workspace, or one issue
    Log(cmd::log::LogArgs),
    /// Book time against an issue
    Logwork(cmd::logwork::LogworkArgs),
    /// Open an issue in the browser
    Open(cmd::open::OpenArgs),
    /// Move sub-tasks under a different parent
    Parent(cmd::parent::ParentArgs),
    /// List projects
    Projects(cmd::list::ProjectsArgs),
    /// Fetch and merge remote changes into the working copy
    Pull(cmd::pull::PullArgs),
    /// Send local changes to Jira
    Push(cmd::push::PushArgs),
    /// Read the working copy back as figures
    #[command(subcommand)]
    Report(cmd::report::ReportCmd),
    /// Drop local changes and go back to the last sync
    Reset(cmd::reset::ResetArgs),
    /// Resolve recorded conflicts
    Resolve(cmd::resolve::ResolveArgs),
    /// Print JSON Schemas for outputs and the frontmatter format
    Schema(cmd::schema::SchemaArgs),
    /// Run a JQL search
    Search(cmd::search::SearchArgs),
    /// Write any editable field on any number of issues
    Set(cmd::set::SetArgs),
    /// Everything the workspace knows about one issue
    Show(cmd::show::ShowArgs),
    /// Work with sprints
    #[command(subcommand)]
    Sprint(cmd::sprint::SprintCmd),
    /// List sprints
    Sprints(cmd::list::SprintsArgs),
    /// One-screen overview of the workspace
    #[command(visible_alias = "st")]
    Status(cmd::status::StatusArgs),
    /// Change an issue's workflow status
    Transition(cmd::transition::TransitionArgs),
    /// Which binary this is, and what changed in it
    Version(cmd::version::VersionArgs),
    /// Watch issues, or stop watching them, on a screen of tick boxes
    Watch(cmd::watch::WatchArgs),
    /// Show the authenticated account
    Whoami(cmd::whoami::WhoamiArgs),
}

/// Shown on every run whose traffic is unverified, whether the setting came
/// from `--insecure`, `JIRED_INSECURE` or `tls.insecure` in a config file.
pub const INSECURE_TLS_WARNING: &str =
    "TLS certificate verification is disabled (--insecure / tls.insecure); \
     traffic can be intercepted";

pub fn run(cli: Cli) -> ExitCode {
    let out = match cli.global.silent {
        true => Out::silent(cli.global.json),
        false => Out::new(cli.global.json, cli.global.quiet),
    };
    crate::style::init(cli.global.color_choice());
    crate::json::set_shape(crate::json::Shape {
        fields: cli.global.fields.clone(),
        compact: cli.global.json_compact,
    });
    crate::json::set_verbose(cli.global.verbose > 0);
    init_tracing(cli.global.verbose, cli.global.silent);

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
        // The same order as the enum above: alphabetical, so a reader
        // looking for one command does not read the other thirty-nine.
        Command::Assign(a) => cmd::assign::run(g, a, out),
        Command::Attach(a) => cmd::attach::run(g, a, out),
        Command::Attachments(a) => cmd::attach::run_attachments(g, a, out),
        Command::Boards(a) => cmd::list::run_boards(g, a, out),
        Command::Clean(a) => cmd::clean::run(g, a, out),
        Command::Comment(a) => cmd::comment::run(g, a, out),
        Command::Completion(a) => cmd::completion::run(a, out),
        Command::Config(a) => cmd::config::run(g, a, out),
        Command::Create(a) => cmd::create::run(g, a, out),
        Command::Diff(a) => cmd::diff::run(g, a, out),
        Command::Doctor(a) => cmd::doctor::run(g, a, out),
        Command::Edit(a) => cmd::edit::run(g, a, out),
        Command::Fetch(a) => cmd::fetch::run(g, a, out),
        Command::FetchSpec(a) => cmd::fetch_spec::run(g, a, out),
        Command::Fields(a) => cmd::fields::run(g, a, out),
        Command::Init(a) => cmd::init::run(g, a, out),
        Command::InitAgents(a) => cmd::init_agents::run(g, a, out),
        Command::Label(a) => cmd::label::run(g, a, out),
        Command::List(a) => cmd::list_issues::run(g, a, out),
        Command::Log(a) => cmd::log::run(g, a, out),
        Command::Logwork(a) => cmd::logwork::run(g, a, out),
        Command::Open(a) => cmd::open::run(g, a, out),
        Command::Parent(a) => cmd::parent::run(g, a, out),
        Command::Projects(a) => cmd::list::run_projects(g, a, out),
        Command::Pull(a) => cmd::pull::run(g, a, out),
        Command::Push(a) => cmd::push::run(g, a, out),
        Command::Report(a) => cmd::report::run(g, a, out),
        Command::Reset(a) => cmd::reset::run(g, a, out),
        Command::Resolve(a) => cmd::resolve::run(g, a, out),
        Command::Schema(a) => cmd::schema::run(a, out),
        Command::Search(a) => cmd::search::run(g, a, out),
        Command::Set(a) => cmd::set::run(g, a, out),
        Command::Show(a) => cmd::show::run(g, a, out),
        Command::Sprint(a) => cmd::sprint::run(g, a, out),
        Command::Sprints(a) => cmd::list::run_sprints(g, a, out),
        Command::Status(a) => cmd::status::run(g, a, out),
        Command::Transition(a) => cmd::transition::run(g, a, out),
        Command::Version(a) => cmd::version::run(g, a, out),
        Command::Watch(a) => cmd::watch::run(g, a, out),
        Command::Whoami(a) => cmd::whoami::run(g, a, out),
    }
}

fn init_tracing(verbose: u8, silent: bool) {
    use tracing_subscriber::EnvFilter;
    let default = log_filter(verbose, silent);
    // `JIRED_LOG` still wins: somebody who set it asked for those logs by name,
    // and a flag about chatter should not overrule an instruction about logging.
    let filter = EnvFilter::try_from_env("JIRED_LOG").unwrap_or_else(|_| EnvFilter::new(default));
    // Logs always go to stderr so `--json` stdout stays parseable.
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_target(false)
        .try_init();
}

/// The log level each verbosity means.
///
/// `--silent` lowers the floor below `warn`, which is where the retries live: a
/// pull that reaches a slow server writes a line per attempt, and those are the
/// loudest thing left once `--quiet` has taken the narration. `error` rather
/// than `off`, because a log line at that level is about the failure the exit
/// code is already reporting.
fn log_filter(verbose: u8, silent: bool) -> &'static str {
    match (silent, verbose) {
        (true, _) => "error",
        (false, 0) => "warn",
        (false, 1) => "info,jired=debug",
        (false, _) => "debug,jired=trace",
    }
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

    /// `--silent` lowers the log floor below the retry warnings; `-v` raises
    /// it, and clap refuses the pair, so the two never have to be reconciled.
    #[test]
    fn silence_takes_the_log_floor_below_the_retries() {
        assert_eq!(log_filter(0, false), "warn");
        assert_eq!(log_filter(0, true), "error");
        assert_eq!(log_filter(1, false), "info,jired=debug");
        assert_eq!(log_filter(2, false), "debug,jired=trace");
    }

    #[test]
    fn silent_and_verbose_cannot_be_asked_for_together() {
        assert!(Cli::try_parse_from(["jired", "pull", "--silent"]).is_ok());
        assert!(Cli::try_parse_from(["jired", "pull", "-v"]).is_ok());
        assert!(Cli::try_parse_from(["jired", "pull", "--silent", "-v"]).is_err());
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
