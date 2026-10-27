//! `jired report` — read the working copy back as figures.
//!
//! Offline like every other read: `report time` totals the `## Worklogs` the
//! last `pull` wrote, so it answers instantly and says how old its snapshot is
//! rather than reaching for the server behind the reader's back.

use crate::cli::cmd::list_issues::status_categories;
use crate::cli::context::Context;
use crate::cli::GlobalArgs;
use crate::error::{ExitCode, JiredError, Result};
use crate::json::Out;
use crate::report::issues::{Dim, Facts, IssuesReport};
use crate::report::{Source, MAX_DAYS};
use crate::sync::Engine;
use crate::tui::report::ReportScreen;
use clap::{Args, Subcommand};

#[derive(Subcommand, Debug)]
pub enum ReportCmd {
    /// How many issues, broken down by one field or a matrix of two
    Issues(IssuesArgs),
    /// How much time everyone booked, by person and by day
    Time(TimeArgs),
}

#[derive(Args, Debug, Clone)]
pub struct IssuesArgs {
    /// The field to count by, or two for a matrix: `--by sprint,type`
    ///
    /// An issue with several values — the sprints it has been in, its labels,
    /// its fix versions — counts under each, the way JQL's `sprint = X` matches.
    #[arg(
        long,
        value_name = "FIELD[,FIELD]",
        value_delimiter = ',',
        required = true,
        num_args = 1..
    )]
    pub by: Vec<Dim>,

    /// Count the issues this JQL finds on the server instead of the working copy
    #[arg(long, value_name = "JQL")]
    pub jql: Option<String>,

    /// How to print it: an aligned table, or Markdown to paste into a page
    #[arg(long, value_name = "FORMAT", default_value = "text")]
    pub format: Format,
}

/// The two renderings of a report table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Format {
    Text,
    #[value(alias = "markdown")]
    Md,
}

#[derive(Args, Debug, Clone)]
pub struct TimeArgs {
    /// How many days to count, today included — one column each
    #[arg(
        long,
        value_name = "N",
        default_value_t = 7,
        value_parser = clap::value_parser!(u32).range(1..=MAX_DAYS as i64),
    )]
    pub days: u32,

    /// Break each person down into the issues they booked against
    #[arg(long)]
    pub show_issues: bool,
}

pub fn run(g: &GlobalArgs, cmd: &ReportCmd, out: Out) -> Result<ExitCode> {
    match cmd {
        ReportCmd::Issues(args) => issues(g, args, out),
        ReportCmd::Time(args) => time(g, args, out),
    }
}

fn issues(g: &GlobalArgs, args: &IssuesArgs, out: Out) -> Result<ExitCode> {
    if args.by.len() > 2 {
        return Err(JiredError::usage(format!(
            "`--by` takes one field or two, not {}",
            args.by.len()
        ))
        .with_hint("`--by sprint,type`: the first is the rows, the second the columns"));
    }
    let mut ctx = Context::open(g, out)?;
    let facts: Vec<Facts> = match &args.jql {
        // The server, for the issues the workspace does not hold: every sprint
        // the project has had, not only the ones somebody pulled.
        Some(jql) => {
            let session = crate::store::session::SessionStore::open(&ctx.workspace)?;
            let account = session.require_account()?;
            let client = crate::api::build_client(&mut ctx, &account)?;
            let progress = crate::sync::progress::Progress::spinner(out, "searching");
            let found = crate::api::block_on(crate::api::client::search_all(
                client.as_ref(),
                crate::api::SearchQuery {
                    jql: jql.clone(),
                    start_at: 0,
                    max_results: 100,
                    fields: crate::api::FieldSet::Listing,
                    expand_changelog: false,
                },
                |page| {
                    let seen = page.start_at + page.issues.len();
                    progress.set_message(format!("fetched {seen} of {} issue(s)", page.total));
                },
            ))?;
            progress.finish();
            found.iter().map(Facts::from_remote).collect()
        }
        // The files, like `list`: offline, and the same issues the commands
        // that edit them would act on.
        None => {
            let engine = Engine::open(&mut ctx)?;
            let categories = status_categories(&engine.state.issues()?);
            let mut facts = Vec::new();
            for file in engine.projector.scan()?.values() {
                // A file that will not parse is `doctor`'s to report, not a
                // reason for a count to fail.
                let Ok(doc) = engine.projector.read(&file.relative) else {
                    continue;
                };
                let category = categories
                    .get(&doc.frontmatter.status.trim().to_lowercase())
                    .cloned();
                facts.push(Facts::from_frontmatter(&doc.frontmatter, category));
            }
            facts
        }
    };

    let mut report = IssuesReport::build(&args.by, &facts);
    if let Some(jql) = &args.jql {
        report.source = "server";
        report.jql = Some(jql.clone());
    }
    if report.total == 0 {
        out.say(match &args.jql {
            Some(_) => "no issues match",
            None => "no issues in this workspace",
        });
    } else {
        report.render(out, args.format == Format::Md);
    }
    out.emit("report-issues", &report);
    Ok(ExitCode::Success)
}

fn time(g: &GlobalArgs, args: &TimeArgs, out: Out) -> Result<ExitCode> {
    let mut ctx = Context::open(g, out)?;
    let engine = Engine::open(&mut ctx)?;

    // Every file, not just the ones the snapshot knows: an issue created
    // locally can be booked against before it has ever been pushed.
    let mut issues = Vec::new();
    for file in engine.projector.scan()?.values() {
        issues.push(engine.projector.read(&file.relative)?);
    }

    let last_fetch = engine.state.last_fetch_at()?;
    let now = chrono::Utc::now();
    let source = Source {
        issues: &issues,
        me: engine.account.display_name.clone().unwrap_or_default(),
        now,
        project: ctx
            .resolver
            .get_quiet("workspace.project")?
            .filter(|p| !p.is_empty()),
        last_fetch,
        last_fetch_age_seconds: last_fetch.map(|at| (now - at).num_seconds().max(0)),
    };
    let report = source.report(args.days, 0);

    // The screen is a way of reading the report, not of producing it: the
    // document is emitted either way, and a pipe or `--no-input` gets the table.
    // Only the screen can move the window, so what is emitted is always the
    // window the command line asked for.
    if !out.json && !out.quiet && ctx.resolver.interactive() && crate::tui::available() {
        let mut screen = ReportScreen::new(&source, args.days, args.show_issues);
        crate::tui::run(&mut screen)?;
    } else {
        report.render(out, args.show_issues);
    }

    out.emit("report-time", &report);
    Ok(ExitCode::Success)
}
