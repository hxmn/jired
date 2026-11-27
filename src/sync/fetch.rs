//! `jired fetch` [M1.8] — refresh the remote snapshot.
//!
//! Never writes a working-copy file. Each page is stored as it arrives, oldest
//! `updated` first, so a fetch interrupted by a network failure resumes from
//! the last page it stored rather than starting over.

use crate::api::client::{next_page, FieldSet, Page, SearchQuery};
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
    /// The fetch went on from where an earlier one stopped part way.
    pub resumed: bool,
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

    // The watermark says the whole project was read as of then, so only a
    // fetch that read the whole project may move it: `pull PROJ-1` moved it
    // to now, and an edit made to PROJ-7 in between was behind every
    // incremental pull after that, until `--full`. A sprint or a board is a
    // narrower scope just as a query is.
    let named = !args.wanted_issues()?.is_empty();
    let scope_is_whole_project =
        !named && args.jql.is_none() && args.sprint.is_none() && args.board.is_none();
    let watermark = engine.state.last_fetch_at()?;
    // Issues asked for by key are read whatever their `updated`: one the
    // watermark had already passed was otherwise never fetched at all.
    let incremental = !args.full && !named && args.updated_since.is_none() && watermark.is_some();
    // A fetch of the whole project that stopped part way left the newest
    // `updated` it stored; going on from there is the rest of that walk.
    let resume = (scope_is_whole_project && !args.full && args.updated_since.is_none())
        .then(|| engine.state.fetch_resume_at())
        .transpose()?
        .flatten();
    let from = match (incremental.then(|| watermark.expect("checked")), resume) {
        (Some(w), Some(r)) => Some(w.max(r)),
        (w, r) => w.or(r),
    };
    let since = from.map(skew_adjusted);

    let jql = build_jql(ctx, args, since.as_ref())?;
    tracing::debug!(%jql, "searching");

    // A first fetch of a few hundred issues is several round trips; without
    // this it looks like a hang.
    let progress = crate::sync::progress::Progress::spinner(ctx.out, "searching");
    let mut query = SearchQuery {
        jql: jql.clone(),
        start_at: 0,
        max_results: 100,
        fields: FieldSet::Full,
        expand_changelog: false,
        page_token: None,
    };
    let mut token: Option<String> = None;
    let mut present: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut fetched = 0usize;
    let mut changed_keys = Vec::new();
    let mut newest = resume;
    loop {
        let page = crate::api::block_on(client.search(&query))?;
        let count = page.issues.len();
        fetched += count;
        tracing::debug!("page: {fetched} of {} issues", page.total);
        progress.set_message(format!("fetched {fetched} of {} issue(s)", page.total));
        let links = web_links(ctx, engine, client, args, &page.issues)?;
        let resumable = scope_is_whole_project && args.updated_since.is_none();
        // One commit a page, its issues and how far the walk got together:
        // interrupted after this, the next fetch asks only for what is newer
        // than the newest stored.
        engine.state.write(|state| {
            for (issue, links) in page.issues.into_iter().zip(links) {
                newest = Some(newest.map_or(issue.updated, |n| n.max(issue.updated)));
                let key = issue.key.clone();
                // Once each: an issue edited during the walk moves to its end,
                // and is met again there.
                if store(engine, issue, links)? && !changed_keys.contains(&key) {
                    changed_keys.push(key.clone());
                }
                present.insert(key);
            }
            if resumable {
                state.set_fetch_resume_at(newest.as_ref())?;
            }
            Ok(())
        })?;
        match next_page(count, fetched, page.total, page.next_page_token, &token) {
            Page::Next(next) => {
                query.page_token = next.clone();
                token = next;
                query.start_at += count;
            }
            Page::Done => break,
            Page::Short => {
                return Err(JiredError::network(format!(
                    "the search stopped after {fetched} of the {} issues it matched",
                    page.total
                ))
                .with_detail("jql", jql.clone())
                .with_hint("run `jired fetch` again: it goes on from the last page it stored"));
            }
        }
    }
    progress.finish();
    changed_keys.sort();
    // A parent is not part of an incremental fetch when only its sub-tasks
    // changed, so the list it carries is brought up to date from theirs.
    engine.state.reconcile_subtasks()?;

    // Only a walk of the whole project from the start can tell "gone" from
    // "outside this query"; one that went on from an earlier stop saw only
    // what changed since.
    let deleted = if scope_is_whole_project && since.is_none() {
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

    // `--updated-since` read only what changed after a time of its own
    // choosing, which may be later than the watermark.
    if scope_is_whole_project && args.updated_since.is_none() {
        engine.state.set_last_fetch_at(&started)?;
        engine.state.set_fetch_resume_at(None)?;
    }

    Ok(FetchReport {
        jql: Some(jql),
        fetched,
        changed: changed_keys.len(),
        deleted,
        incremental: since.is_some(),
        resumed: resume.is_some(),
        since: since.as_ref().map(crate::api::model::format_timestamp),
        keys: changed_keys,
        fetched_at: crate::api::model::format_timestamp(&started),
    })
}

/// The web links of each issue on a page that moved since its last
/// snapshot, read a few at a time; `None` for one not read, whose last links
/// are carried forward.
///
/// Web links are a resource of their own, one more call per issue, so they
/// are read only for an issue that changed and only when changelogs are
/// wanted too. A read that fails is the same as one not made: carried, and
/// said, but no reason to fail a fetch whose issues all arrived.
fn web_links(
    ctx: &mut Context,
    engine: &Engine,
    client: &dyn crate::api::JiraApi,
    args: &FetchArgs,
    issues: &[crate::api::model::RemoteIssue],
) -> Result<Vec<Option<Vec<serde_json::Value>>>> {
    use futures::StreamExt;
    if args.slim {
        return Ok(vec![None; issues.len()]);
    }
    let mut moved = Vec::with_capacity(issues.len());
    for issue in issues {
        let previous = engine.state.issue(&issue.key)?.and_then(|row| row.remote);
        // Decided the way `upsert_remote` decides what changed: the stored
        // second, and the payload's own milliseconds when the seconds tie.
        let stamp = crate::api::model::format_timestamp;
        let exact = crate::store::state::exact_updated;
        moved.push(previous.as_ref().is_none_or(|p| {
            stamp(&p.updated) != stamp(&issue.updated)
                || matches!((exact(&p.raw), exact(&issue.raw)), (Some(a), Some(b)) if a != b)
        }));
    }
    let reads = issues.iter().zip(&moved).map(|(issue, moved)| async move {
        match moved {
            true => Some((issue.key.clone(), client.remote_links(&issue.key).await)),
            false => None,
        }
    });
    let mut read: Vec<_> = crate::api::block_on(
        futures::stream::iter(reads.enumerate().map(|(n, f)| async move { (n, f.await) }))
            .buffer_unordered(crate::api::CONCURRENCY)
            .collect::<Vec<_>>(),
    );
    read.sort_by_key(|(n, _)| *n);
    Ok(read
        .into_iter()
        .map(|(_, result)| match result {
            Some((_, Ok(links))) => Some(links),
            Some((key, Err(e))) => {
                ctx.out.warn(format!(
                    "{key}: cannot read its web links: {} — the last ones seen are kept",
                    e.message
                ));
                None
            }
            None => None,
        })
        .collect())
}

/// Store one fetched issue's snapshot, with the web links read for it or the
/// last ones carried; whether it changed.
fn store(
    engine: &Engine,
    mut issue: crate::api::model::RemoteIssue,
    links: Option<Vec<serde_json::Value>>,
) -> Result<bool> {
    let previous = engine.state.issue(&issue.key)?.and_then(|row| row.remote);
    if let Some(links) = links {
        crate::api::normalize::set_remote_links(&mut issue, links);
    }
    if let Some(previous) = &previous {
        crate::api::normalize::carry_remote_links(&mut issue, &previous.raw);
    }
    engine.state.upsert_remote(&issue)
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
