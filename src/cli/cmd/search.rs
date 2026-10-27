//! `jired search` [M3.7].
//!
//! JQL on the server — the online half of `jired list`. Every row carries the
//! fields a follow-up question needs (type, parent, sprint, versions, labels),
//! because a listing that names only key, summary and status sent every check
//! after it back to the server one issue at a time.

use crate::api::RemoteIssue;
use crate::cli::cmd::list_issues::{layout, tab_separated};
use crate::cli::context::Context;
use crate::cli::GlobalArgs;
use crate::error::{ExitCode, JiredError, Result};
use crate::json::Out;
use crate::style::{self, CYAN};
use clap::Args;

/// The server's own ceiling for one page of results.
const PAGE: usize = 100;

/// The most rows `--limit` pages through. Past this a search is an export, and
/// the answer to "how many" is `--count`, which costs one request at any size.
pub const MAX_LIMIT: usize = 1000;

#[derive(Args, Debug, Clone)]
pub struct SearchArgs {
    /// JQL query to run, e.g. "project = PROJ AND status = Open"
    #[arg(value_name = "JQL")]
    pub query: Option<String>,

    /// JQL query to run (same as the positional form)
    #[arg(long, value_name = "JQL", conflicts_with = "query")]
    pub jql: Option<String>,

    /// Maximum number of results; above 100 the pages are fetched in turn
    #[arg(long, default_value_t = 50)]
    pub limit: usize,

    /// Print only how many issues match, in one request
    #[arg(long, conflicts_with_all = ["columns", "keys"])]
    pub count: bool,

    /// Print these fields, in this order, instead of the default line:
    /// aligned on a terminal, tab-separated anywhere else
    #[arg(
        long,
        value_name = "COLUMNS",
        value_delimiter = ',',
        conflicts_with = "keys"
    )]
    pub columns: Vec<Column>,

    /// Print only the keys, one per line, for piping into another command
    #[arg(long)]
    pub keys: bool,

    /// Also pull the matching issues into the working copy
    #[arg(long, hide = true)]
    pub pull: bool,
}

/// A field `--columns` can ask for: the fields of a row of the JSON document,
/// so the listing and `--json` never disagree about what an issue is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Column {
    Key,
    Type,
    Status,
    Assignee,
    Summary,
    Parent,
    Epic,
    Sprint,
    /// Spelled as the JSON field is; the kebab-case clap would derive is kept
    /// as an alias, since that is what `--help` offered first.
    #[value(name = "fix_versions", alias = "fix-versions")]
    FixVersions,
    Labels,
    Priority,
    Components,
    Created,
    Updated,
}

/// One matching issue.
#[derive(Debug, Clone, serde::Serialize)]
struct Row {
    key: String,
    summary: String,
    status: String,
    status_category: String,
    #[serde(rename = "type")]
    issue_type: String,
    /// The display name, as before; the id is beside it for addressing them.
    assignee: Option<String>,
    assignee_id: Option<String>,
    parent: Option<String>,
    epic: Option<String>,
    /// Every sprint it has been in, oldest first — the same list `list --json`
    /// carries and the same one JQL's `sprint = X` matches against.
    sprint: Vec<String>,
    fix_versions: Vec<String>,
    labels: Vec<String>,
    priority: Option<String>,
    components: Vec<String>,
    created: Option<String>,
    updated: String,
}

impl Row {
    fn of(i: &RemoteIssue) -> Self {
        let f = &i.fields;
        Self {
            key: i.key.clone(),
            summary: f.summary.clone(),
            status: f.status.clone(),
            status_category: i.status_category().to_string(),
            issue_type: f.issue_type.clone(),
            assignee: f.assignee.as_ref().map(|u| u.display_name.clone()),
            assignee_id: f.assignee.as_ref().map(|u| u.id.clone()),
            parent: f.parent.clone(),
            epic: f.epic.clone(),
            sprint: f.sprint.clone(),
            fix_versions: f.fix_versions.clone(),
            labels: f.labels.clone(),
            priority: f.priority.clone(),
            components: f.components.clone(),
            created: f.created.as_ref().map(crate::api::model::format_timestamp),
            updated: crate::api::model::format_timestamp(&i.updated),
        }
    }

    /// The value one column takes. Empty where the issue has nothing, never a
    /// placeholder a script would have to know to throw away.
    fn cell(&self, column: Column) -> String {
        let opt = |v: &Option<String>| v.clone().unwrap_or_default();
        match column {
            Column::Key => self.key.clone(),
            Column::Type => self.issue_type.clone(),
            Column::Status => self.status.clone(),
            Column::Assignee => opt(&self.assignee),
            Column::Summary => self.summary.clone(),
            Column::Parent => opt(&self.parent),
            Column::Epic => opt(&self.epic),
            // Where it is now, the last of the ones it has been in, as `list`
            // shows it; `--json` has the whole history.
            Column::Sprint => self.sprint.last().cloned().unwrap_or_default(),
            Column::FixVersions => self.fix_versions.join(","),
            Column::Labels => self.labels.join(","),
            Column::Priority => opt(&self.priority),
            Column::Components => self.components.join(","),
            Column::Created => opt(&self.created),
            Column::Updated => self.updated.clone(),
        }
    }
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

    if args.count {
        // No issues, only the total the server reports beside them: one
        // request whatever the size of the answer. Counting by fetching was 75
        // searches and a jq loop for one chart.
        let page = crate::api::block_on(client.search(&crate::api::SearchQuery {
            jql: jql.clone(),
            start_at: 0,
            max_results: 0,
            fields: crate::api::FieldSet::Summary,
            expand_changelog: false,
        }))?;
        out.say(page.total.to_string());
        out.emit(
            "search-count",
            &serde_json::json!({"jql": jql, "total": page.total}),
        );
        return Ok(ExitCode::Success);
    }

    // Silently returning fewer rows than asked for looks like a query that found
    // less than it did, so say when the ceiling bit.
    let limit = args.limit.clamp(1, MAX_LIMIT);
    if args.limit > MAX_LIMIT {
        out.warn(format!(
            "--limit stops at {MAX_LIMIT}; showing at most {MAX_LIMIT} — `--count` says how many \
             there are"
        ));
    }

    let (issues, total) = fetch(client.as_ref(), &jql, limit)?;
    let rows: Vec<Row> = issues.iter().map(Row::of).collect();

    if args.keys {
        for r in &rows {
            out.say(&r.key);
        }
    } else if !args.columns.is_empty() {
        let tabs = tab_separated();
        let cells: Vec<Vec<String>> = rows
            .iter()
            .map(|r| args.columns.iter().map(|&c| r.cell(c)).collect())
            .collect();
        let keyed: Vec<bool> = args.columns.iter().map(|c| *c == Column::Key).collect();
        for line in layout(cells, &keyed, tabs) {
            out.say(line);
        }
        if rows.is_empty() && !tabs {
            out.say("no issues match");
        }
    } else {
        render_default(&rows, total, out);
    }

    out.emit(
        "search",
        &serde_json::json!({"jql": jql, "total": total, "issues": rows}),
    );
    Ok(ExitCode::Success)
}

/// Up to `limit` matches, a page at a time, and the total the server reports.
fn fetch(
    client: &dyn crate::api::JiraApi,
    jql: &str,
    limit: usize,
) -> Result<(Vec<RemoteIssue>, usize)> {
    let mut issues: Vec<RemoteIssue> = Vec::new();
    let mut total = 0;
    while issues.len() < limit {
        let page = crate::api::block_on(client.search(&crate::api::SearchQuery {
            jql: jql.to_string(),
            start_at: issues.len(),
            max_results: (limit - issues.len()).min(PAGE),
            fields: crate::api::FieldSet::Listing,
            expand_changelog: false,
        }))?;
        total = page.total;
        let count = page.issues.len();
        issues.extend(page.issues);
        if count == 0 || issues.len() >= total {
            break;
        }
    }
    issues.truncate(limit);
    Ok((issues, total))
}

fn render_default(rows: &[Row], total: usize, out: Out) {
    if rows.is_empty() {
        out.say("no issues match");
        return;
    }
    let key_width = rows.iter().map(|r| r.key.len()).max().unwrap_or(10).max(6);
    let status_width = rows
        .iter()
        .map(|r| r.status.len())
        .max()
        .unwrap_or(10)
        .clamp(6, 20);
    for r in rows {
        // Padded before painting: escape codes count towards a format width
        // and would knock every following column out of line.
        out.say(format!(
            "{} {} {}",
            style::out(CYAN, format!("{:<key_width$}", r.key)),
            style::status(&format!("{:<status_width$}", r.status), &r.status_category),
            r.summary
        ));
    }
    if total > rows.len() {
        out.say(format!("\nshowing {} of {}", rows.len(), total));
        out.hint("raise --limit, narrow the query, or `--count` for the number alone");
    }
}
