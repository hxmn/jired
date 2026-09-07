//! `jired list` — which issues in this workspace match, without leaving it.
//!
//! `search` is JQL on the server; this is its offline half. Asking the working
//! copy "which issues say X" had no answer, so the question was answered with
//! `grep -l '^summary:.*\[ALL\]' *.md` — a regex over jired's own file format,
//! which is exactly what the format is supposed to spare people, and which is
//! wrong the moment a filename and a summary disagree.
//!
//! It reads the files rather than the snapshot on purpose: the files are what a
//! following `jired label add` or `jired set` will edit, so what this lists and
//! what those change are the same thing even when the working copy is ahead of
//! the last fetch.

use crate::cli::context::Context;
use crate::cli::GlobalArgs;
use crate::doc::Frontmatter;
use crate::error::{ExitCode, Result};
use crate::json::Out;
use crate::style::{self, CYAN};
use crate::sync::Engine;
use clap::Args;

#[derive(Args, Debug, Clone, Default)]
pub struct ListArgs {
    /// Issues whose summary contains this text (case-insensitive)
    #[arg(long, value_name = "TEXT")]
    pub summary_contains: Option<String>,

    /// Issues carrying this label
    #[arg(long, value_name = "LABEL")]
    pub label: Option<String>,

    /// Issues in this status; repeat the flag to accept any of several
    #[arg(long, value_name = "STATUS")]
    pub status: Vec<String>,

    /// Issues in any status but this one; repeatable
    #[arg(long = "not-status", value_name = "STATUS")]
    pub not_status: Vec<String>,

    /// Issues whose status falls in one of Jira's three buckets, which is what
    /// "not finished yet" means across differently-named workflows
    #[arg(long, value_name = "CATEGORY", value_parser = ["new", "indeterminate", "done"])]
    pub category: Option<String>,

    /// Issues whose current sprint is this one
    #[arg(long, value_name = "SPRINT")]
    pub sprint: Option<String>,

    /// Issues assigned to this person: any part of the name or the id
    #[arg(long, value_name = "USER")]
    pub assignee: Option<String>,

    /// Issues of exactly this type, ignoring case — not a substring, so
    /// `sub-task` does not match `Analytics sub-task`
    #[arg(long, value_name = "TYPE")]
    pub issue_type: Option<String>,

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
}

/// A field `--columns` can ask for.
///
/// Deliberately the fields of a [`Match`] and nothing else: what the human
/// listing can show is what `--json` already carries, so the two never disagree
/// about what an issue is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Column {
    Key,
    Type,
    Status,
    Assignee,
    Summary,
    Sprint,
    Labels,
    File,
}

/// One matching issue, as both things a caller might want to address it by.
#[derive(Debug, Clone, serde::Serialize)]
struct Match {
    key: String,
    file: String,
    summary: String,
    status: String,
    #[serde(rename = "type")]
    issue_type: String,
    labels: Vec<String>,
    assignee: Option<String>,
    /// Every sprint it has been in, oldest first, exactly as `sprint:` has it.
    ///
    /// The whole list rather than the last of it, because the difference
    /// between the two is what splits carry-over from new work — and answering
    /// that from `--json` was the one question that still meant parsing the
    /// `.md` files. The listing's own column shows the last entry, which is
    /// where the issue is now, the way the `assignee` column shows a name out
    /// of the `@[Name](id)` this carries.
    sprint: Vec<String>,
}

pub fn run(g: &GlobalArgs, args: &ListArgs, out: Out) -> Result<ExitCode> {
    let mut ctx = Context::open(g, out)?;
    let engine = Engine::open(&mut ctx)?;

    // Only built when asked for: `--category` is the one filter the files
    // cannot answer alone, because the bucket behind a status name is the
    // server's classification, not anything the frontmatter carries.
    let categories = match args.category {
        Some(_) => status_categories(&engine.state.issues()?),
        None => std::collections::BTreeMap::new(),
    };

    let mut matched: Vec<Match> = Vec::new();
    // Sorted by key, because `scan` returns a map keyed by it — a listing that
    // reshuffles between runs cannot be diffed against the last one.
    for (key, file) in engine.projector.scan()? {
        // A file that will not parse is not a match and not a reason to fail:
        // `doctor` and `diff` are where a broken file gets reported.
        let Ok(doc) = engine.projector.read(&file.relative) else {
            continue;
        };
        let category = categories
            .get(&doc.frontmatter.status.trim().to_lowercase())
            .map(String::as_str);
        if !matches(&doc.frontmatter, args, category) {
            continue;
        }
        let fm = doc.frontmatter;
        matched.push(Match {
            key,
            file: file.relative,
            summary: fm.summary,
            status: fm.status,
            issue_type: fm.issue_type,
            labels: fm.labels,
            assignee: fm.assignee,
            sprint: fm.sprint,
        });
    }

    if args.keys {
        // Nothing but the keys, so the output is an argument list: no heading,
        // no count, no colour — `jired list --keys | xargs jired label add x`.
        for m in &matched {
            out.say(&m.key);
        }
    } else if !args.columns.is_empty() {
        let tabs = tab_separated();
        for line in rows(&matched, &args.columns, tabs) {
            out.say(line);
        }
        // The note that nothing matched is for a person, and a person is at a
        // terminal — into a pipe it would be a row `cut` cannot tell from data.
        if matched.is_empty() && !tabs {
            out.say("no issue in this workspace matches");
        }
    } else {
        for m in &matched {
            out.say(format!(
                "{} {} {}",
                style::out(CYAN, format!("{:<12}", m.key)),
                style::status(&format!("{:<14}", m.status), ""),
                m.summary
            ));
        }
        if matched.is_empty() {
            out.say("no issue in this workspace matches");
            out.hint("`jired list` with no filter lists everything it can see");
        }
    }

    out.emit(
        "list",
        &serde_json::json!({ "issues": matched, "count": matched.len() }),
    );
    Ok(ExitCode::Success)
}

/// Whether the listing should be tab-separated rather than lined up.
///
/// Decided by the stream and not by a flag: columns that line up are for
/// somebody reading them, and a delimiter `cut -f2` can name is for whatever is
/// on the other end of the pipe. Neither caller should have to say which they
/// are, and the padding that helps the first breaks the second.
fn tab_separated() -> bool {
    use is_terminal::IsTerminal;
    !std::io::stdout().is_terminal()
}

/// The value one column takes for one issue.
///
/// Empty where the issue has nothing — an unassigned issue prints a blank cell
/// rather than a placeholder, because a placeholder is a value a script would
/// have to know to throw away.
fn cell(m: &Match, column: Column) -> String {
    match column {
        Column::Key => m.key.clone(),
        Column::Type => m.issue_type.clone(),
        Column::Status => m.status.clone(),
        // The name, out of the file's `@[Name](id)`: this is the column a
        // person reads, and `--json` still carries the id for addressing them.
        Column::Assignee => m
            .assignee
            .as_deref()
            .map(crate::doc::frontmatter::display_name)
            .unwrap_or_default(),
        Column::Summary => m.summary.clone(),
        // Where it is now, which is the last of the ones it has been in: a cell
        // wide enough for a whole history is a cell nobody can read across.
        Column::Sprint => m.sprint.last().cloned().unwrap_or_default(),
        Column::Labels => m.labels.join(","),
        Column::File => m.file.clone(),
    }
}

/// One line per issue, in the columns that were asked for.
fn rows(matched: &[Match], columns: &[Column], tabs: bool) -> Vec<String> {
    let cells: Vec<Vec<String>> = matched
        .iter()
        .map(|m| {
            columns
                .iter()
                // A tab or a newline inside a summary would forge a column
                // boundary in the output, which is worse than losing it.
                .map(|&c| cell(m, c).replace(['\t', '\n', '\r'], " "))
                .collect()
        })
        .collect();
    if tabs {
        return cells.iter().map(|row| row.join("\t")).collect();
    }
    // Measured in characters rather than bytes: a Cyrillic summary is half as
    // many characters as it is bytes, and a byte width would over-pad it.
    let widths: Vec<usize> = (0..columns.len())
        .map(|i| {
            cells
                .iter()
                .map(|row| row[i].chars().count())
                .max()
                .unwrap_or(0)
        })
        .collect();
    cells
        .iter()
        .map(|row| {
            let mut line = String::new();
            for (i, value) in row.iter().enumerate() {
                let last = i + 1 == row.len();
                // Padded before painting, like the default listing: an escape
                // counts towards a format width and would knock the following
                // columns out of line. The last column is not padded at all.
                let text = match last {
                    true => value.clone(),
                    false => format!("{value:<width$}", width = widths[i]),
                };
                line.push_str(&match columns[i] {
                    Column::Key => style::out(CYAN, text),
                    _ => text,
                });
                if !last {
                    line.push(' ');
                }
            }
            // An empty trailing cell leaves the separator behind it, and a line
            // ending in spaces is invisible until something copies it back out.
            line.trim_end().to_string()
        })
        .collect()
}

/// The status names this workspace has seen, each with the category the server
/// files it under: `new`, `indeterminate` or `done`.
///
/// Read from the snapshots rather than asked for, like everything else here:
/// every status a workflow actually uses is on some issue jired has pulled. A
/// status on no snapshot — typed into a file by hand, say — has no category,
/// and `--category` does not claim it.
pub(crate) fn status_categories(
    rows: &[crate::store::state::IssueRow],
) -> std::collections::BTreeMap<String, String> {
    let mut map = std::collections::BTreeMap::new();
    for row in rows {
        let Some(status) = row
            .remote
            .as_ref()
            .map(|r| &r.raw)
            .or(row.base.as_ref().map(|b| &b.raw))
            .and_then(|raw| raw.pointer("/fields/status"))
        else {
            continue;
        };
        if let (Some(name), Some(key)) = (
            status.pointer("/name").and_then(|v| v.as_str()),
            status
                .pointer("/statusCategory/key")
                .and_then(|v| v.as_str()),
        ) {
            map.insert(name.trim().to_lowercase(), key.to_string());
        }
    }
    map
}

/// Whether one issue passes every filter that was given.
///
/// Every filter is a conjunction and every text comparison is
/// case-insensitive: these are values read off a screen and typed back, not
/// identifiers. `--summary-contains` is a substring because that is what
/// somebody looking for `[ALL]` means; the rest match whole values, because a
/// status half-named is a different status. A repeated `--status` is the one
/// disjunction: an issue is in one status, so several can only mean "any of".
fn matches(fm: &Frontmatter, args: &ListArgs, category: Option<&str>) -> bool {
    let contains = |haystack: &str, needle: &str| {
        haystack
            .to_lowercase()
            .contains(&needle.trim().to_lowercase())
    };
    let same = |value: &str, wanted: &str| value.trim().eq_ignore_ascii_case(wanted.trim());

    if let Some(text) = &args.summary_contains {
        if !contains(&fm.summary, text) {
            return false;
        }
    }
    if let Some(label) = &args.label {
        if !fm.labels.iter().any(|l| same(l, label)) {
            return false;
        }
    }
    if !args.status.is_empty() && !args.status.iter().any(|s| same(&fm.status, s)) {
        return false;
    }
    if args.not_status.iter().any(|s| same(&fm.status, s)) {
        return false;
    }
    if let Some(wanted) = &args.category {
        // A status jired has never seen on a snapshot has no known category,
        // and claiming it for any bucket would be a guess.
        if category != Some(wanted.as_str()) {
            return false;
        }
    }
    if let Some(issue_type) = &args.issue_type {
        if !same(&fm.issue_type, issue_type) {
            return false;
        }
    }
    if let Some(sprint) = &args.sprint {
        // The sprint it is *in* is the last entry; the earlier ones are where
        // it has been, and matching those would answer a different question.
        match fm.sprint.last() {
            Some(current) => {
                if !same(current, sprint) {
                    return false;
                }
            }
            None => return false,
        }
    }
    if let Some(user) = &args.assignee {
        // The file spells an assignee `@[Ilya A](5b10a2ilya)`, so a substring
        // matches either half — the name a person reads or the id a script has.
        match &fm.assignee {
            Some(mention) => {
                if !contains(mention, user) {
                    return false;
                }
            }
            None => return false,
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn issue() -> Frontmatter {
        Frontmatter {
            key: "TSK-1".into(),
            summary: "[ALL] Реализовать иерархию".into(),
            status: "In Progress".into(),
            issue_type: "Task".into(),
            labels: vec!["regular".into(), "auth".into()],
            assignee: Some("@[Maria Nilsson](nilsson.m)".into()),
            sprint: vec!["TSK Спринт 5".into(), "TSK Спринт 6".into()],
            ..Frontmatter::default()
        }
    }

    fn args() -> ListArgs {
        ListArgs::default()
    }

    #[test]
    fn no_filter_matches_everything() {
        assert!(matches(&issue(), &args(), None));
    }

    #[test]
    fn a_summary_is_searched_by_substring_and_ignoring_case() {
        let mut a = args();
        a.summary_contains = Some("[all]".into());
        assert!(matches(&issue(), &a, None));
        a.summary_contains = Some("иерархию".into());
        assert!(matches(&issue(), &a, None));
        a.summary_contains = Some("nothing".into());
        assert!(!matches(&issue(), &a, None));
    }

    /// A status half-named is a different status, so these match whole values.
    #[test]
    fn the_other_filters_match_a_whole_value() {
        let mut a = args();
        a.status = vec!["in progress".into()];
        assert!(matches(&issue(), &a, None));
        a.status = vec!["In".into()];
        assert!(!matches(&issue(), &a, None));

        let mut a = args();
        a.label = Some("auth".into());
        assert!(matches(&issue(), &a, None));
        a.label = Some("au".into());
        assert!(!matches(&issue(), &a, None));
    }

    /// An issue is in one status, so several `--status` can only mean "any of".
    #[test]
    fn a_repeated_status_filter_accepts_any_of_them() {
        let mut a = args();
        a.status = vec!["To Do".into(), "in progress".into()];
        assert!(matches(&issue(), &a, None));
        a.status = vec!["To Do".into(), "Done".into()];
        assert!(!matches(&issue(), &a, None));
    }

    #[test]
    fn not_status_excludes_and_stacks() {
        let mut a = args();
        a.not_status = vec!["Done".into()];
        assert!(matches(&issue(), &a, None));
        a.not_status = vec!["Done".into(), "in progress".into()];
        assert!(!matches(&issue(), &a, None));
    }

    /// The bucket is the server's classification; a status jired has never
    /// seen on a snapshot has none, and `--category` must not claim it.
    #[test]
    fn category_matches_the_servers_bucket_and_not_a_guess() {
        let mut a = args();
        a.category = Some("indeterminate".into());
        assert!(matches(&issue(), &a, Some("indeterminate")));
        assert!(!matches(&issue(), &a, Some("done")));
        assert!(!matches(&issue(), &a, None));
    }

    /// An assignee is `@[Name](id)` in the file, so either half finds them.
    #[test]
    fn an_assignee_is_found_by_name_or_by_id() {
        let mut a = args();
        a.assignee = Some("nilsson.m".into());
        assert!(matches(&issue(), &a, None));
        a.assignee = Some("Maria".into());
        assert!(matches(&issue(), &a, None));
        a.assignee = Some("someone else".into());
        assert!(!matches(&issue(), &a, None));

        let mut unassigned = issue();
        unassigned.assignee = None;
        assert!(!matches(&unassigned, &a, None));
    }

    /// The sprint an issue is in is the last of the ones it has been in.
    #[test]
    fn the_sprint_filter_asks_where_the_issue_is_now() {
        let mut a = args();
        a.sprint = Some("TSK Спринт 6".into());
        assert!(matches(&issue(), &a, None));
        // It has been in 5; it is not in 5.
        a.sprint = Some("TSK Спринт 5".into());
        assert!(!matches(&issue(), &a, None));
    }

    fn found() -> Vec<Match> {
        let fm = issue();
        vec![
            Match {
                key: "TSK-1".into(),
                file: "TSK-1.md".into(),
                summary: fm.summary.clone(),
                status: fm.status.clone(),
                issue_type: fm.issue_type.clone(),
                labels: fm.labels.clone(),
                assignee: fm.assignee.clone(),
                sprint: fm.sprint.clone(),
            },
            Match {
                key: "TSK-102".into(),
                file: "TSK-102.md".into(),
                summary: "Fix\tthe login".into(),
                status: "Done".into(),
                issue_type: "Sub-task".into(),
                labels: Vec::new(),
                assignee: None,
                sprint: Vec::new(),
            },
        ]
    }

    /// Into a pipe the columns are a record `cut -f2` can read, so nothing is
    /// padded and nothing but a tab separates two values.
    #[test]
    fn columns_are_tab_separated_off_a_terminal() {
        let columns = [Column::Key, Column::Type, Column::Assignee];
        let lines = rows(&found(), &columns, true);
        assert_eq!(lines[0], "TSK-1\tTask\tMaria Nilsson");
        // An issue with nothing there keeps its cell: a row of a fixed width is
        // what makes the field after it addressable by number.
        assert_eq!(lines[1], "TSK-102\tSub-task\t");
    }

    /// On a terminal the same values line up, which is the whole reason a
    /// person asked for a column rather than for `--json`.
    ///
    /// No `key` column here on purpose: it is the one that is painted, and the
    /// colour mode is process-global, so asserting on it would race whichever
    /// other test set it last.
    #[test]
    fn columns_line_up_on_a_terminal() {
        let columns = [Column::Type, Column::Status, Column::Summary];
        let lines = rows(&found(), &columns, false);
        assert_eq!(lines[0], "Task     In Progress [ALL] Реализовать иерархию");
        assert_eq!(lines[1], "Sub-task Done        Fix the login");
    }

    /// A tab inside a summary would forge a column boundary, which is worse
    /// than losing it; a trailing empty cell must not leave whitespace behind.
    #[test]
    fn a_value_cannot_forge_a_column_boundary() {
        let columns = [Column::Type, Column::Summary, Column::Assignee];
        assert_eq!(
            rows(&found(), &columns, true)[1],
            "Sub-task\tFix the login\t"
        );
        let aligned = rows(&found(), &columns, false);
        assert!(aligned[1].ends_with("Fix the login"), "{:?}", aligned[1]);
    }

    /// The nine type names on the instance this came from differ by a prefix,
    /// so a substring match would answer a different question: `sub-task` is
    /// not `Analytics sub-task`, and the docs promise as much.
    #[test]
    fn a_type_matches_the_whole_name_ignoring_case() {
        let mut a = args();
        a.issue_type = Some("TASK".into());
        assert!(matches(&issue(), &a, None));

        let mut sub = issue();
        sub.issue_type = "Analytics sub-task".into();
        a.issue_type = Some("sub-task".into());
        assert!(!matches(&sub, &a, None));
        a.issue_type = Some("analytics sub-task".into());
        assert!(matches(&sub, &a, None));
    }

    #[test]
    fn filters_are_a_conjunction() {
        let mut a = args();
        a.label = Some("regular".into());
        a.status = vec!["Done".into()];
        assert!(!matches(&issue(), &a, None), "both have to hold");
    }
}
