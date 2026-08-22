//! `jired report` — read the working copy back as figures.
//!
//! Offline like every other read: `report time` totals the `## Worklogs` the
//! last `pull` wrote, so it answers instantly and says how old its snapshot is
//! rather than reaching for the server behind the reader's back.

use crate::cli::context::Context;
use crate::cli::GlobalArgs;
use crate::error::{ExitCode, Result};
use crate::json::Out;
use crate::report::{TimeReport, Window};
use crate::sync::Engine;
use crate::tui::report::ReportScreen;
use clap::{Args, Subcommand};

#[derive(Subcommand, Debug)]
pub enum ReportCmd {
    /// How much time everyone booked, by person
    Time(TimeArgs),
}

#[derive(Args, Debug, Clone)]
pub struct TimeArgs {
    /// How many days to count, today included
    #[arg(
        long,
        value_name = "N",
        default_value_t = 7,
        value_parser = clap::value_parser!(u32).range(1..=3650),
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

    let window = Window::last(args.days, chrono::Utc::now());
    let me = engine.account.display_name.clone().unwrap_or_default();
    let mut report = TimeReport::build(&window, &me, &issues);
    report.project = ctx
        .resolver
        .get_quiet("workspace.project")?
        .filter(|p| !p.is_empty());
    if let Some(at) = engine.state.last_fetch_at()? {
        report.last_fetch_at = Some(crate::api::model::format_timestamp(&at));
        report.last_fetch_age_seconds = Some((chrono::Utc::now() - at).num_seconds().max(0));
    }

    // The screen is a way of reading the report, not of producing it: the
    // document is emitted either way, and a pipe or `--no-input` gets the table.
    if !out.json && !out.quiet && ctx.resolver.interactive() && crate::tui::available() {
        let mut screen = ReportScreen::new(&report, args.show_issues);
        crate::tui::run(&mut screen)?;
    } else {
        report.render(out, args.show_issues);
    }

    out.emit("report-time", &report);
    Ok(ExitCode::Success)
}
