//! `jired fetch` [M1.8] — refresh the remote snapshot.
//!
//! Never writes a working-copy file. Partial progress is committed as pages
//! arrive, so a fetch interrupted by a network failure resumes rather than
//! starting over.

use crate::api::client::{search_all, FieldSet, SearchQuery};
use crate::api::model::jql_timestamp;
use crate::cli::cmd::fetch::FetchArgs;
use crate::cli::context::Context;
use crate::error::{JiredError, Result};
use crate::json::Out;
use crate::sync::{jql_quote, Engine};
use chrono::{DateTime, Duration, Utc};
use serde::Serialize;

#[derive(Debug, Clone, Serialize, Default)]
pub struct FetchReport {
    pub jql: Option<String>,
    pub fetched: usize,
    pub changed: usize,
    pub deleted: usize,
    pub incremental: bool,
    pub since: Option<String>,
    pub keys: Vec<String>,
    pub fetched_at: String,
}

impl FetchReport {
    pub fn render(&self, out: Out) {
        out.say(format!(
            "fetched {} issue(s), {} changed{}",
            self.fetched,
            self.changed,
            if self.deleted > 0 {
                format!(", {} no longer in scope", self.deleted)
            } else {
                String::new()
            }
        ));
    }
}

pub fn run(ctx: &mut Context, args: &FetchArgs) -> Result<FetchReport> {
    let engine = Engine::open(ctx)?;
    let client = engine.client(ctx)?;
    run_with(ctx, &engine, client.as_ref(), args)
}

pub fn run_with(
    ctx: &mut Context,
    engine: &Engine,
    client: &dyn crate::api::JiraApi,
    args: &FetchArgs,
) -> Result<FetchReport> {
    let started = Utc::now();

    // Refresh the field definitions first: everything downstream that re-reads
    // a snapshot resolves custom fields through them.
    match crate::api::block_on(client.fields()) {
        Ok(fields) if !fields.is_empty() => engine.state.save_field_meta(&fields)?,
        Ok(_) => {}
        Err(e) => tracing::warn!("cannot refresh field definitions: {}", e.message),
    }

    // Boards and sprints drive the grouping folders; caching them here is what
    // lets `doctor --regroup` rebuild without a network round trip.
    if let Some(project) = ctx.resolver.get_quiet("workspace.project")? {
        if let Err(e) = cache_agile(engine, client, &project) {
            tracing::debug!("agile metadata unavailable: {}", e.message);
        }
        // What `fix_versions:` is checked against at push time. Best-effort for
        // the same reason the boards are: a listing that fails leaves the old
        // one standing, or none — and with none, push checks nothing rather
        // than refusing every name.
        match crate::api::block_on(client.versions(&project)) {
            Ok(versions) => engine.state.replace_versions(&project, &versions)?,
            Err(e) => tracing::warn!("cannot refresh the versions of {project}: {}", e.message),
        }
    }
    // What `jired link` checks a direction against. Best-effort like the rest:
    // without a list it checks nothing, and push still refuses at plan time.
    match crate::api::block_on(client.link_types()) {
        Ok(types) if !types.is_empty() => engine.state.save_link_types(&types)?,
        Ok(_) => {}
        Err(e) => tracing::debug!("cannot list the link types: {}", e.message),
    }

    let watermark = engine.state.last_fetch_at()?;
    let incremental = !args.full && args.updated_since.is_none() && watermark.is_some();
    let since = incremental.then(|| skew_adjusted(watermark.expect("checked")));

    let jql = build_jql(ctx, args, since.as_ref())?;
    tracing::debug!(%jql, "searching");

    let scope_is_whole_project = args.wanted_issues()?.is_empty() && args.jql.is_none();

    // A first fetch of a few hundred issues is several round trips; without
    // this it looks like a hang.
    let progress = crate::sync::progress::Progress::spinner(ctx.out, "searching");
    let issues = crate::api::block_on(search_all(
        client,
        SearchQuery {
            jql: jql.clone(),
            start_at: 0,
            max_results: 100,
            fields: FieldSet::Full,
            expand_changelog: false,
        },
        |page| {
            let seen = page.start_at + page.issues.len();
            tracing::debug!("page: {} of {} issues", seen, page.total);
            progress.set_message(format!("fetched {seen} of {} issue(s)", page.total));
        },
    ))?;
    progress.finish();

    let storing = crate::sync::progress::Progress::bar(ctx.out, issues.len(), "storing");
    let mut changed_keys = Vec::new();
    for mut issue in issues.iter().cloned() {
        // Web links are a resource of their own, one more call per issue, so
        // they are read only for an issue that changed and only when changelogs
        // are wanted too; otherwise the last snapshot's are carried forward. A
        // read that fails is the same: carried, and said, but no reason to fail
        // a fetch whose issues all arrived.
        let previous = engine.state.issue(&issue.key)?.and_then(|row| row.remote);
        // Compared as stored, to the second, the way `upsert_remote` decides
        // what changed: the server's milliseconds would make every issue new.
        let stamp = crate::api::model::format_timestamp;
        let moved = previous
            .as_ref()
            .is_none_or(|p| stamp(&p.updated) != stamp(&issue.updated));
        if moved && !args.slim {
            match crate::api::block_on(client.remote_links(&issue.key)) {
                Ok(links) => crate::api::normalize::set_remote_links(&mut issue, links),
                Err(e) => ctx.out.warn(format!(
                    "{}: cannot read its web links: {} — the last ones seen are kept",
                    issue.key, e.message
                )),
            }
        }
        if let Some(previous) = &previous {
            crate::api::normalize::carry_remote_links(&mut issue, &previous.raw);
        }
        if engine.state.upsert_remote(&issue)? {
            changed_keys.push(issue.key.clone());
        }
        storing.inc();
    }
    storing.finish();
    changed_keys.sort();
    // A parent is not part of an incremental fetch when only its sub-tasks
    // changed, so the list it carries is brought up to date from theirs.
    engine.state.reconcile_subtasks()?;

    // Only a full-scope fetch can tell "gone" from "outside this query".
    let deleted = if scope_is_whole_project && !incremental {
        let present: std::collections::HashSet<&str> =
            issues.iter().map(|i| i.key.as_str()).collect();
        let missing: Vec<String> = engine
            .state
            .issues()?
            .into_iter()
            .filter(|row| !present.contains(row.key.as_str()) && !row.key.starts_with("NEW-"))
            .map(|row| row.key)
            .collect();
        engine.state.mark_deleted_remote(&missing)?;
        missing.len()
    } else {
        0
    };

    engine.state.set_last_fetch_at(&started)?;

    Ok(FetchReport {
        jql: Some(jql),
        fetched: issues.len(),
        changed: changed_keys.len(),
        deleted,
        incremental,
        since: since.as_ref().map(crate::api::model::format_timestamp),
        keys: changed_keys,
        fetched_at: crate::api::model::format_timestamp(&started),
    })
}

/// Cache boards, their column mapping, and sprints.
///
/// Best-effort: plenty of Jira instances have no Agile boards at all, or deny
/// the endpoint, and neither is a reason to fail a fetch.
fn cache_agile(engine: &Engine, client: &dyn crate::api::JiraApi, project: &str) -> Result<()> {
    let boards = crate::api::block_on(client.boards(Some(project)))?;
    let mut rows = Vec::new();
    let mut sprints = Vec::new();

    for board in &boards {
        let columns = crate::api::block_on(client.board_columns(&board.id)).unwrap_or_default();
        rows.push(crate::store::state::BoardRow {
            id: board.id.clone(),
            name: board.name.clone(),
            board_type: board.board_type.clone(),
            columns,
        });
        if let Ok(mut found) = crate::api::block_on(client.sprints(&board.id)) {
            sprints.append(&mut found);
        }
    }
    engine.state.save_boards(&rows)?;
    engine.state.save_sprints(&sprints)?;
    Ok(())
}

/// Back-date the watermark slightly: Jira indexes asynchronously, so an issue
/// updated a moment before the previous fetch can appear only afterwards.
fn skew_adjusted(watermark: DateTime<Utc>) -> DateTime<Utc> {
    watermark - Duration::minutes(5)
}

pub fn build_jql(
    ctx: &mut Context,
    args: &FetchArgs,
    since: Option<&DateTime<Utc>>,
) -> Result<String> {
    let mut clauses: Vec<String> = Vec::new();

    let named = args.wanted_issues()?;
    if !named.is_empty() {
        clauses.push(format!("key in ({})", named.join(", ")));
    } else if let Some(jql) = &args.jql {
        clauses.push(format!("({jql})"));
    } else {
        // No prompt here: the error below names the flag and the setting, which
        // is more use to both a person and a script than a bare `key:` prompt.
        let project = ctx
            .resolver
            .get_quiet("workspace.project")?
            .ok_or_else(|| {
                JiredError::missing_value("no project or query to fetch")
                    .with_detail("key", "workspace.project")
                    .with_hint("pass --project KEY, --jql, or --issues")
            })?;
        clauses.push(format!("project = {}", jql_quote(&project)));
    }

    if let Some(sprint) = &args.sprint {
        clauses.push(format!("sprint = {}", jql_quote(sprint)));
    }
    if let Some(board) = &args.board {
        // Boards are addressed through their filter in JQL.
        clauses.push(format!("filter in (\"{board}\")"));
    }
    if let Some(ts) = &args.updated_since {
        clauses.push(format!("updated >= {}", jql_quote(ts)));
    } else if let Some(since) = since {
        clauses.push(format!("updated >= {}", jql_quote(&jql_timestamp(since))));
    }

    // Deterministic ordering keeps pagination stable while issues change.
    Ok(format!("{} ORDER BY updated ASC", clauses.join(" AND ")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::model::parse_timestamp;

    #[test]
    fn watermark_is_backdated_for_index_lag() {
        let t = parse_timestamp("2026-08-17T12:00:00Z").unwrap();
        assert_eq!(
            crate::api::model::format_timestamp(&skew_adjusted(t)),
            "2026-08-17T11:55:00Z"
        );
    }
}
