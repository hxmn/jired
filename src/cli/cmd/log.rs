//! `jired log ISSUE-KEY` [M3.7] — who changed what, and when.
//!
//! Changelogs are fetched per issue and cached, rather than pulled for every
//! issue during `fetch`: they are large, rarely needed, and asking for them
//! wholesale is the difference between a fast sync and a slow one.

use crate::api::model::ChangelogEntry;
use crate::cli::context::Context;
use crate::cli::GlobalArgs;
use crate::error::{ExitCode, JiredError, Result};
use crate::json::Out;
use crate::sync::Engine;
use clap::Args;

#[derive(Args, Debug, Clone)]
pub struct LogArgs {
    /// Issue to show history for
    pub issue: String,
    /// Show at most this many entries (newest first)
    #[arg(long, default_value_t = 20)]
    pub limit: usize,
    /// Ignore the cached copy and ask the server
    #[arg(long)]
    pub refresh: bool,
}

pub fn run(g: &GlobalArgs, args: &LogArgs, out: Out) -> Result<ExitCode> {
    let mut ctx = Context::open(g, out)?;
    let engine = Engine::open(&ctx)?;
    let key = crate::project::naming::key_from_input(&args.issue);

    if engine.state.issue(&key)?.is_none() {
        return Err(JiredError::usage(format!("{key} is not in this workspace"))
            .with_detail("issue", key.clone())
            .with_hint("run `jired pull` first, or check the key"));
    }

    let cached = engine.state.changelog(&key)?;
    let entries: Vec<ChangelogEntry> = if args.refresh || cached.is_empty() {
        let client = engine.client(&mut ctx)?;
        let fetched = crate::api::block_on(client.changelog(&key))?;
        engine.state.save_changelog(&key, &fetched)?;
        fetched
    } else {
        cached
    };

    // Newest first: the last thing that happened is the thing being asked about.
    let mut entries = entries;
    entries.sort_by_key(|e| std::cmp::Reverse(e.created));
    entries.truncate(args.limit);

    for e in &entries {
        let when = e
            .created
            .as_ref()
            .map(crate::api::model::format_timestamp)
            .unwrap_or_default();
        out.say(format!(
            "{when}  {}",
            e.author.as_deref().unwrap_or("unknown")
        ));
        for item in &e.items {
            out.say(format!(
                "    {:<16} {} → {}",
                item.field,
                item.from.as_deref().unwrap_or("(none)"),
                item.to.as_deref().unwrap_or("(none)")
            ));
        }
    }
    if entries.is_empty() {
        out.say(format!("{key} has no recorded history"));
    }

    out.emit(
        "log",
        &serde_json::json!({ "key": key, "entries": entries }),
    );
    Ok(ExitCode::Success)
}
