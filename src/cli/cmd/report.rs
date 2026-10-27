//! `jired report` — read the working copy back as figures.
//!
//! Offline like every other read: `report time` totals the `## Worklogs` the
//! last `pull` wrote, so it answers instantly and says how old its snapshot is
//! rather than reaching for the server behind the reader's back.

use crate::cli::context::Context;
use crate::cli::GlobalArgs;
use crate::error::{ExitCode, JiredError, Result};
use crate::json::Out;
use crate::report::{Columns, Lookup, Source, Window, MAX_DAYS};
use crate::sync::Engine;
use crate::tui::report::ReportScreen;
use clap::{Args, Subcommand};

#[derive(Subcommand, Debug)]
pub enum ReportCmd {
    /// How much time everyone booked, by person and by day
    Time(TimeArgs),
}

#[derive(Args, Debug, Clone)]
pub struct TimeArgs {
    /// How many days to count, one column each [default: 7]
    ///
    /// Back from today, or from `--until`; forward from `--since` when only
    /// that is given.
    #[arg(
        long,
        value_name = "N",
        value_parser = clap::value_parser!(u32).range(1..=MAX_DAYS as i64),
    )]
    pub days: Option<u32>,

    /// First day to count, inclusive: `2026-10-19`
    #[arg(long, value_name = "DATE")]
    pub since: Option<chrono::NaiveDate>,

    /// Last day to count, inclusive [default: today]
    #[arg(long, value_name = "DATE", conflicts_with_all = ["days"], requires = "since")]
    pub until: Option<chrono::NaiveDate>,

    /// Fold each sub-task's time into the issue at the top of its `parent:`
    #[arg(long, value_name = "WHAT")]
    pub rollup: Option<Rollup>,

    /// Which way up the table is: a column per day, or a column per person
    #[arg(long, value_enum, default_value_t = Columns::Days)]
    pub columns: Columns,

    /// `text` for a terminal, `md` for a Markdown table to paste into a page
    #[arg(long, value_enum, default_value_t = Format::Text)]
    pub format: Format,

    /// Break each person down into the issues they booked against
    #[arg(long)]
    pub show_issues: bool,
}

/// What `--rollup` folds into what.
#[derive(clap::ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rollup {
    /// A sub-task's time goes to its parent, and its parent's to the next
    Parent,
}

#[derive(clap::ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// Lined up for a terminal
    Text,
    /// A Markdown pipe table with a total row
    Md,
}

pub fn run(g: &GlobalArgs, cmd: &ReportCmd, out: Out) -> Result<ExitCode> {
    match cmd {
        ReportCmd::Time(args) => time(g, args, out),
    }
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

    // The snapshots are what know whose account each worklog is, and what a
    // parent with no file of its own is called. Base before remote, so the
    // newer copy has the last word on a name.
    let mut lookup = Lookup {
        me_id: engine.account.account_id.clone(),
        ..Default::default()
    };
    for row in engine.state.issues()? {
        if let Some(base) = &row.base {
            lookup.learn(&base.raw);
        }
        if let Some(remote) = &row.remote {
            lookup.learn(&remote.raw);
        }
    }

    let last_fetch = engine.state.last_fetch_at()?;
    let now = chrono::Utc::now();
    let window = window(args, now)?;
    let source = Source {
        issues: &issues,
        me: engine.account.display_name.clone().unwrap_or_default(),
        lookup,
        rollup: args.rollup.is_some(),
        now,
        project: ctx
            .resolver
            .get_quiet("workspace.project")?
            .filter(|p| !p.is_empty()),
        last_fetch,
        last_fetch_age_seconds: last_fetch.map(|at| (now - at).num_seconds().max(0)),
    };
    let report = source.report_in(&window);

    // The screen is a way of reading the report, not of producing it: the
    // document is emitted either way, and a pipe or `--no-input` gets the table.
    // Only the screen can move the window, so what is emitted is always the
    // window the command line asked for. It draws the one shape, a column per
    // day in text, and a window that ends in the future is not one it can walk.
    let back = window.back(now);
    let screen_can = args.format == Format::Text && args.columns == Columns::Days;
    match back {
        Some(back)
            if screen_can
                && !out.json
                && !out.quiet
                && ctx.resolver.interactive()
                && crate::tui::available() =>
        {
            let mut screen = ReportScreen::new(&source, window.days, args.show_issues);
            screen.go_back(back);
            crate::tui::run(&mut screen)?;
        }
        _ => match args.format {
            Format::Md => {
                if !report.people.is_empty() {
                    out.say(report.table(args.columns).markdown());
                }
            }
            Format::Text if args.columns == Columns::People => {
                report.render_table(out, args.columns)
            }
            Format::Text => report.render(out, args.show_issues),
        },
    }

    out.emit("report-time", &report);
    Ok(ExitCode::Success)
}

/// The window the flags name: `--since`/`--until` as dates, both counted;
/// `--since` with `--days` counting forward; `--days` alone counting back from
/// today, which is what it always meant.
fn window(args: &TimeArgs, now: chrono::DateTime<chrono::Utc>) -> Result<Window> {
    let Some(since) = args.since else {
        return Ok(Window::last(args.days.unwrap_or(7), now));
    };
    let until = match (args.until, args.days) {
        (Some(until), _) => until,
        (None, Some(days)) => since + chrono::Duration::days(i64::from(days) - 1),
        (None, None) => now.date_naive(),
    };
    Window::between(since, until, now).ok_or_else(|| {
        JiredError::usage(format!(
            "{since} to {until} is not a window of 1 to {MAX_DAYS} days"
        ))
        .with_hint("--since is the first day counted and --until the last, both included")
    })
}
