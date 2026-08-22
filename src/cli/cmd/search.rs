//! `jired search` [M3.7].

use crate::cli::context::Context;
use crate::cli::GlobalArgs;
use crate::error::{ExitCode, JiredError, Result};
use crate::json::Out;
use crate::style::{self, CYAN};
use clap::Args;

/// The server's own ceiling for one page of results.
const MAX_PAGE: usize = 100;

#[derive(Args, Debug, Clone)]
pub struct SearchArgs {
    /// JQL query to run, e.g. "project = PROJ AND status = Open"
    #[arg(value_name = "JQL")]
    pub query: Option<String>,

    /// JQL query to run (same as the positional form)
    #[arg(long, value_name = "JQL", conflicts_with = "query")]
    pub jql: Option<String>,

    /// Maximum number of results (the server caps a page at 100)
    #[arg(long, default_value_t = 50)]
    pub limit: usize,

    /// Also pull the matching issues into the working copy
    #[arg(long, hide = true)]
    pub pull: bool,
}

pub fn run(g: &GlobalArgs, args: &SearchArgs, out: Out) -> Result<ExitCode> {
    if args.pull {
        return Err(crate::cli::not_yet(
            "jired search --pull",
            "M3.7",
            "run the search, then `jired pull` with the same --jql",
        ));
    }
    let jql = args
        .query
        .clone()
        .or_else(|| args.jql.clone())
        .ok_or_else(|| {
            JiredError::usage("no query given")
                .with_hint("pass the JQL as an argument, e.g. jired search \"project = PROJ\"")
        })?;

    let mut ctx = Context::open(g, out)?;
    let session = crate::store::session::SessionStore::open(&ctx.workspace)?;
    let account = session.require_account()?;
    let client = crate::api::build_client(&mut ctx, &account)?;

    // Silently returning fewer rows than asked for looks like a query that found
    // less than it did, so say when the ceiling bit.
    let limit = args.limit.min(MAX_PAGE);
    if args.limit > MAX_PAGE {
        out.warn(format!(
            "the server returns at most {MAX_PAGE} results per query; showing {MAX_PAGE}"
        ));
    }

    let page = crate::api::block_on(client.search(&crate::api::SearchQuery {
        jql: jql.clone(),
        start_at: 0,
        max_results: limit,
        fields: crate::api::FieldSet::Summary,
        expand_changelog: false,
    }))?;

    let rows: Vec<serde_json::Value> = page
        .issues
        .iter()
        .map(|i| {
            serde_json::json!({
                "key": i.key,
                "summary": i.summary(),
                "status": i.status(),
                "status_category": i.status_category(),
                "assignee": i.assignee_name(),
            })
        })
        .collect();

    if rows.is_empty() {
        out.say("no issues match");
    } else {
        let key_width = rows
            .iter()
            .filter_map(|r| r["key"].as_str().map(str::len))
            .max()
            .unwrap_or(10)
            .max(6);
        let status_width = rows
            .iter()
            .filter_map(|r| r["status"].as_str().map(str::len))
            .max()
            .unwrap_or(10)
            .clamp(6, 20);
        for r in &rows {
            // Padded before painting: escape codes count towards a format
            // width and would knock every following column out of line.
            out.say(format!(
                "{} {} {}",
                style::out(
                    CYAN,
                    format!("{:<key_width$}", r["key"].as_str().unwrap_or(""))
                ),
                style::status(
                    &format!("{:<status_width$}", r["status"].as_str().unwrap_or("-")),
                    r["status_category"].as_str().unwrap_or_default()
                ),
                r["summary"].as_str().unwrap_or("")
            ));
        }
        if page.total as usize > rows.len() {
            out.say(format!("\nshowing {} of {}", rows.len(), page.total));
            out.hint("raise --limit, or narrow the query");
        }
    }

    out.emit(
        "search",
        &serde_json::json!({"jql": jql, "total": page.total, "issues": rows}),
    );
    Ok(ExitCode::Success)
}
