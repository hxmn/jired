//! `jired logwork` — book time against an issue without hand-editing the file.
//!
//! Like `comment`, this stages the entry in the working copy rather than
//! reaching for the server: it then shows up in `status` and `diff` and goes
//! out through the normal `push` path, or immediately with `--push`.
//!
//! Both values can come from the command line, and with both in hand nothing is
//! asked — a scripted `logwork` behaves the same with or without a terminal.
//! Without them there are three rungs, in the order the terminal can support
//! them: the full-screen form, line prompts, and exit 7 naming what is missing.

use crate::cli::cmd::edit::{edit_issue, push_one, report_push};
use crate::cli::cmd::prompt::Ui;
use crate::cli::context::Context;
use crate::cli::GlobalArgs;
use crate::doc::{WorklogEntry, WorklogId};
use crate::error::{ExitCode, JiredError, Result};
use crate::json::Out;
use crate::sync::Engine;
use crate::tui::catalog::{Catalog, KnownIssue};
use crate::tui::logwork::{Booking, LogworkScreen, Seed};
use clap::Args;

#[derive(Args, Debug, Clone, Default)]
pub struct LogworkArgs {
    /// Issue to book the time against: a key, or the path to its file
    pub issue: Option<String>,

    /// How long, in weeks, days, hours and minutes: `2h 30m`, `1d`, `45m`
    pub time: Option<String>,

    /// What the time went on
    #[arg(short = 'm', long = "message")]
    pub message: Option<String>,

    /// When the work started (defaults to now)
    #[arg(long, value_name = "TIMESTAMP")]
    pub started: Option<String>,

    /// Fill in the form even when the arguments would have been enough
    #[arg(long, short = 'i')]
    pub interactive: bool,

    /// Send the worklog immediately instead of leaving it staged
    #[arg(long)]
    pub push: bool,

    /// With --push, show the API call without sending it
    #[arg(long, requires = "push")]
    pub dry_run: bool,
}

pub fn run(g: &GlobalArgs, args: &LogworkArgs, out: Out) -> Result<ExitCode> {
    let mut ctx = Context::open(g, out)?;
    ctx.lock()?;

    let seed = seed(args)?;
    let Some(booking) = book(&mut ctx, args, seed)? else {
        return Err(JiredError::generic("cancelled — no time was booked"));
    };

    // The last gate, so the flags-only path cannot get past what the form and
    // the prompts both check: a duration Jira would refuse fails a push after
    // the field edits beside it have already gone out.
    let time = booking.time.trim().to_string();
    if crate::doc::parse_duration(&time).is_none() {
        return Err(
            JiredError::usage(format!("`{time}` is not a length of time"))
                .with_detail("time", time)
                .with_hint("write it in weeks, days, hours or minutes: `2h 30m`, `1d`, `45m`"),
        );
    }

    let started = booking.started.clone();
    let key = crate::project::naming::key_from_input(&booking.issue);

    let file = edit_issue(&mut ctx, &key, |doc| {
        doc.worklogs.push(WorklogEntry {
            id: WorklogId::New,
            time_spent: time.clone(),
            // Left blank on purpose: the author is whoever pushes it, and
            // writing a name here would only invite editing it.
            author: String::new(),
            started: started.clone(),
            body: booking.note.clone(),
        });
        Ok(())
    })?;

    let pushed = if args.push {
        let report = push_one(&mut ctx, &key, args.dry_run)?;
        report_push(&report, out);
        Some(report)
    } else {
        None
    };

    match &pushed {
        Some(report) if report.dry_run => {}
        Some(report) if report.results.iter().all(|r| r.ok) => {
            out.say(format!("logged {time} against {key}"))
        }
        Some(_) => out.say(format!(
            "{time} is staged in {file}, but the push failed — \
             fix the cause and run `jired push`"
        )),
        None => out.say(format!(
            "staged {time} against {key} in {file} — run `jired push` to send it"
        )),
    }

    out.emit(
        "logwork",
        &serde_json::json!({
            "key": key,
            "file": file,
            "time_spent": time,
            "started": started,
            "pushed": pushed.is_some(),
            "push": pushed,
        }),
    );
    Ok(match pushed {
        Some(report) => report.exit_code(),
        None => ExitCode::Success,
    })
}

/// Everything the command line already decided.
///
/// `--started` is normalized here rather than inside the form, so a timestamp
/// nobody can read is refused before a screen is drawn over the complaint.
fn seed(args: &LogworkArgs) -> Result<Seed> {
    let started = args
        .started
        .as_ref()
        .map(|text| {
            crate::api::model::parse_timestamp(text)
                .map(|t| crate::api::model::format_timestamp(&t))
                .ok_or_else(|| {
                    JiredError::usage(format!("`{text}` is not a timestamp"))
                        .with_hint("use an ISO-8601 time such as 2026-08-21T09:00:00Z")
                })
        })
        .transpose()?;
    Ok(Seed {
        issue: args.issue.clone(),
        time: args.time.clone(),
        started,
        note: args.message.clone(),
    })
}

/// Resolve the seed into a complete entry, asking for what is missing.
///
/// `None` means the user backed out.
fn book(ctx: &mut Context, args: &LogworkArgs, seed: Seed) -> Result<Option<Booking>> {
    let interactive = ctx.resolver.interactive();
    if args.interactive && !interactive {
        return Err(
            JiredError::missing_value("--interactive needs a terminal to ask on")
                .with_hint("pass the issue and the time as arguments instead"),
        );
    }

    let complete = seed.issue.is_some() && seed.time.is_some();
    if interactive && (args.interactive || !complete) {
        let engine = Engine::open(ctx)?;
        let project = ctx
            .resolver
            .get_quiet("workspace.project")?
            .unwrap_or_default();
        let issues = bookable(&engine, &project)?;

        if crate::tui::available() {
            let now = chrono::Utc::now();
            let mut screen = LogworkScreen::new(&project, &issues, now, &seed);
            return crate::tui::run(&mut screen);
        }
        // No alternate screen here — over a dumb terminal the line prompts are
        // the whole interface rather than a consolation prize.
        let ui = Ui::new(true, !ctx.out.json && !ctx.out.quiet);
        return ask(&ui, seed).map(Some);
    }

    let (Some(issue), Some(time)) = (seed.issue, seed.time) else {
        return Err(
            JiredError::missing_value("an issue and a length of time are required").with_hint(
                "pass them as arguments — `jired logwork PROJ-142 \"2h 30m\"` — \
                    or run `jired logwork` in a terminal",
            ),
        );
    };
    Ok(Some(Booking {
        issue,
        time,
        started: seed
            .started
            .unwrap_or_else(|| crate::api::model::format_timestamp(&chrono::Utc::now())),
        note: seed.note.unwrap_or_default(),
    }))
}

/// The issues time can be booked against: the ones with a file to stage it in.
///
/// Most-recently-touched first, which is the order someone looking for "the
/// issue I was just in" wants. A file the snapshot knows nothing about — a
/// locally created issue — comes after, rather than being invisible.
fn bookable(engine: &Engine, project: &str) -> Result<Vec<KnownIssue>> {
    let files = engine.projector.scan()?;
    let catalog = Catalog::from_rows(
        &engine.state.issues()?,
        Some(project).filter(|p| !p.is_empty()),
    );
    let mut issues: Vec<KnownIssue> = catalog
        .issues
        .into_iter()
        .filter(|i| files.contains_key(&i.key))
        .collect();
    for (key, file) in &files {
        if issues.iter().any(|i| &i.key == key) {
            continue;
        }
        let doc = engine.projector.read(&file.relative)?;
        issues.push(KnownIssue {
            key: key.clone(),
            summary: doc.frontmatter.summary.clone(),
            issue_type: doc.frontmatter.issue_type.clone(),
            status: doc.frontmatter.status.clone(),
        });
    }
    Ok(issues)
}

/// The line-based form, for terminals the full-screen one cannot run on.
///
/// It asks for the two things an entry cannot do without and takes the rest
/// from the seed; anything more would be a worse version of the screen.
fn ask(ui: &Ui, seed: Seed) -> Result<Booking> {
    ui.title("log work");
    let issue = match seed.issue {
        Some(issue) => issue,
        None => ui.ask("Issue", Some("the key of the issue the time went to"), None)?,
    };

    let time = match seed.time {
        Some(time) => time,
        None => loop {
            let answer = ui.ask(
                "Time",
                Some("weeks, days, hours and minutes — 2h 30m, 1d, 45m"),
                None,
            )?;
            if crate::doc::parse_duration(&answer).is_some() {
                break answer;
            }
            // Re-asking beats failing: the value is one word, and whoever typed
            // it is still sitting there.
            ui.question_line(format!("`{answer}` is not a length of time"));
        },
    };

    Ok(Booking {
        issue,
        time,
        started: seed
            .started
            .unwrap_or_else(|| crate::api::model::format_timestamp(&chrono::Utc::now())),
        note: seed.note.unwrap_or_default(),
    })
}
