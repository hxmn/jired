//! `jired search --jql` [M3.7].

use crate::cli::context::Context;
use crate::cli::GlobalArgs;
use crate::error::{ExitCode, Result};
use crate::json::Out;
use clap::Args;

#[derive(Args, Debug, Clone)]
pub struct SearchArgs {
    /// JQL query to run
    #[arg(long)]
    pub jql: String,
    /// Maximum number of results
    #[arg(long, default_value_t = 50)]
    pub limit: usize,
    /// Also pull the matching issues into the working copy
    #[arg(long)]
    pub pull: bool,
}

pub fn run(g: &GlobalArgs, args: &SearchArgs, out: Out) -> Result<ExitCode> {
    if args.pull {
        return Err(crate::cli::not_yet("jired search --pull", "M3.7"));
    }
    let mut ctx = Context::open(g, out)?;
    let session = crate::store::session::SessionStore::open(&ctx.workspace)?;
    let account = session.require_account()?;
    let client = crate::api::build_client(&mut ctx, &account)?;

    let page = crate::api::block_on(client.search(&crate::api::SearchQuery {
        jql: args.jql.clone(),
        start_at: 0,
        max_results: args.limit.min(100),
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
                "assignee": i.assignee_name(),
            })
        })
        .collect();
    for r in &rows {
        out.say(format!(
            "{:<12} {:<14} {}",
            r["key"].as_str().unwrap_or(""),
            r["status"].as_str().unwrap_or("-"),
            r["summary"].as_str().unwrap_or("")
        ));
    }
    out.emit(
        "search",
        &serde_json::json!({"jql": args.jql, "total": page.total, "issues": rows}),
    );
    Ok(ExitCode::Success)
}
