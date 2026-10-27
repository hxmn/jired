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
use crate::report::{Columns, Lookup, Source, TableStyle, Window, MAX_DAYS};
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
    #[arg(long, value_enum, default_value_t = Format::Text)]
    pub format: Format,
}

/// The two renderings of a report table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Format {
    /// Lined up for a terminal
    Text,
    /// A Markdown pipe table with a total row
    #[value(alias = "markdown")]
    Md,
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

    /// A person as `Ilya A` (short) or `Ilya Ashbourne` (full)
    /// [config: report.names]
    #[arg(long, value_name = "FORM")]
    pub names: Option<Names>,

    /// Durations as `40h 30m` (hm) or `40.5` (decimal)
    /// [config: report.hours]
    #[arg(long, value_name = "FORM")]
    pub hours: Option<Hours>,

    /// Rows most time first (hours), or by key and people by name (key)
    /// [config: report.sort]
    #[arg(long, value_name = "ORDER")]
    pub sort: Option<Sort>,

    /// With --format md, make each key a link to the issue [config: report.links]
    #[arg(long)]
    pub links: bool,

    /// What the totals row and column are called [config: report.total_label]
    #[arg(long, value_name = "TEXT")]
    pub total_label: Option<String>,
}

/// How a person is named in a table.
#[derive(clap::ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum Names {
    Full,
    Short,
}

/// How a duration is written in a table.
#[derive(clap::ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hours {
    Hm,
    Decimal,
}

/// What order a table's rows are in.
#[derive(clap::ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sort {
    Hours,
    Key,
}

/// What `--rollup` folds into what.
#[derive(clap::ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rollup {
    /// A sub-task's time goes to its parent, and its parent's to the next
    Parent,
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
    let style = table_style(&mut ctx, &engine, args)?;

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
                    out.say(report.table(args.columns, &style).markdown());
                }
            }
            Format::Text if args.columns == Columns::People => {
                report.render_table(out, args.columns, &style)
            }
            Format::Text => report.render(out, args.show_issues),
        },
    }

    out.emit("report-time", &report);
    Ok(ExitCode::Success)
}

/// The table's house style: each flag, else its `report.*` setting, else the
/// terminal's defaults. A flag beats the setting so a one-off table can differ
/// from the page the setting was made for.
fn table_style(ctx: &mut Context, engine: &Engine, args: &TimeArgs) -> Result<TableStyle> {
    let setting = |ctx: &mut Context, key: &str| -> Result<Option<String>> {
        Ok(ctx.resolver.get_value(key)?.map(|v| v.to_lowercase()))
    };
    let short_names = match args.names {
        Some(n) => n == Names::Short,
        None => setting(ctx, "report.names")?.as_deref() == Some("short"),
    };
    let decimal_hours = match args.hours {
        Some(h) => h == Hours::Decimal,
        None => setting(ctx, "report.hours")?.as_deref() == Some("decimal"),
    };
    let by_key = match args.sort {
        Some(s) => s == Sort::Key,
        None => setting(ctx, "report.sort")?.as_deref() == Some("key"),
    };
    // Links are Markdown; on a terminal a key is already what to type.
    let links = (args.format == Format::Md
        && (args.links || ctx.resolver.get_bool("report.links")?.unwrap_or(false)))
    .then(|| engine.base_url().to_string());
    let total = match &args.total_label {
        Some(t) => t.clone(),
        None => ctx
            .resolver
            .get_value("report.total_label")?
            .unwrap_or_else(|| "Total".into()),
    };
    Ok(TableStyle {
        short_names,
        decimal_hours,
        by_key,
        links,
        total,
    })
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
