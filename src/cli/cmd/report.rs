//! `jired report` — read the working copy back as figures.
//!
//! Offline like every other read: `report time` totals the `## Worklogs` the
//! last `pull` wrote, so it answers instantly and says how old its snapshot is
//! rather than reaching for the server behind the reader's back.

use crate::cli::context::Context;
use crate::cli::GlobalArgs;
use crate::error::{ExitCode, Result};
use crate::json::Out;
use crate::report::{Source, MAX_DAYS};
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
