//! `jired logwork` — book time against an issue without hand-editing the file.
//!
//! Like `comment`, this stages the entry in the working copy rather than
//! reaching for the server: it then shows up in `status` and `diff` and goes
//! out through the normal `push` path, or immediately with `--push`.

use crate::cli::cmd::edit::{edit_issue, push_one, report_push};
use crate::cli::context::Context;
use crate::cli::GlobalArgs;
use crate::doc::{WorklogEntry, WorklogId};
use crate::error::{ExitCode, JiredError, Result};
use crate::json::Out;
use clap::Args;

#[derive(Args, Debug, Clone)]
pub struct LogworkArgs {
    /// Issue to book the time against: a key, or the path to its file
    pub issue: String,

    /// How long, in weeks, days, hours and minutes: `2h 30m`, `1d`, `45m`
    pub time: String,

    /// What the time went on
    #[arg(short = 'm', long = "message")]
    pub message: Option<String>,

    /// When the work started (defaults to now)
    #[arg(long, value_name = "TIMESTAMP")]
    pub started: Option<String>,

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

    // Refuse a duration Jira would refuse, here, where it costs nothing: the
    // alternative is a push that fails after the field edits beside it went out.
    let time = args.time.trim().to_string();
    if crate::doc::parse_duration(&time).is_none() {
        return Err(
            JiredError::usage(format!("`{time}` is not a length of time"))
                .with_detail("time", time)
                .with_hint("write it in weeks, days, hours or minutes: `2h 30m`, `1d`, `45m`"),
        );
    }

    // Timestamping when the work started, rather than leaving it to whenever the
    // push happens, is the whole reason a worklog carries a start time.
    let started = match &args.started {
        Some(text) => crate::api::model::parse_timestamp(text)
            .map(|t| crate::api::model::format_timestamp(&t))
            .ok_or_else(|| {
                JiredError::usage(format!("`{text}` is not a timestamp"))
                    .with_hint("use an ISO-8601 time such as 2026-08-21T09:00:00Z")
            })?,
        None => crate::api::model::format_timestamp(&chrono::Utc::now()),
    };

    let body = args.message.clone().unwrap_or_default();
    let key = crate::project::naming::key_from_input(&args.issue);

    let file = edit_issue(&mut ctx, &key, |doc| {
        doc.worklogs.push(WorklogEntry {
            id: WorklogId::New,
            time_spent: time.clone(),
            // Left blank on purpose: the author is whoever pushes it, and
            // writing a name here would only invite editing it.
            author: String::new(),
            started: started.clone(),
            body: body.clone(),
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
            "time_spent": args.time.trim(),
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
