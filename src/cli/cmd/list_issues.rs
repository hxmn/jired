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

    /// Issues in this status
    #[arg(long, value_name = "STATUS")]
    pub status: Option<String>,

    /// Issues whose current sprint is this one
    #[arg(long, value_name = "SPRINT")]
    pub sprint: Option<String>,

    /// Issues assigned to this person: any part of the name or the id
    #[arg(long, value_name = "USER")]
    pub assignee: Option<String>,

    /// Issues of this type
    #[arg(long, value_name = "TYPE")]
    pub issue_type: Option<String>,

    /// Print only the keys, one per line, for piping into another command
    #[arg(long)]
    pub keys: bool,
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
    /// The sprint it is in now, which is the last of the ones it has been in.
    sprint: Option<String>,
}

pub fn run(g: &GlobalArgs, args: &ListArgs, out: Out) -> Result<ExitCode> {
    let mut ctx = Context::open(g, out)?;
    let engine = Engine::open(&mut ctx)?;

    let mut matched: Vec<Match> = Vec::new();
    // Sorted by key, because `scan` returns a map keyed by it — a listing that
    // reshuffles between runs cannot be diffed against the last one.
    for (key, file) in engine.projector.scan()? {
        // A file that will not parse is not a match and not a reason to fail:
        // `doctor` and `diff` are where a broken file gets reported.
        let Ok(doc) = engine.projector.read(&file.relative) else {
            continue;
        };
        if !matches(&doc.frontmatter, args) {
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
            sprint: fm.sprint.last().cloned(),
        });
    }

    if args.keys {
        // Nothing but the keys, so the output is an argument list: no heading,
        // no count, no colour — `jired list --keys | xargs jired label add x`.
        for m in &matched {
            out.say(&m.key);
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

/// Whether one issue passes every filter that was given.
///
/// Every filter is a conjunction and every text comparison is
/// case-insensitive: these are values read off a screen and typed back, not
/// identifiers. `--summary-contains` is a substring because that is what
/// somebody looking for `[ALL]` means; the rest match whole values, because a
/// status half-named is a different status.
fn matches(fm: &Frontmatter, args: &ListArgs) -> bool {
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
    if let Some(status) = &args.status {
        if !same(&fm.status, status) {
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
        assert!(matches(&issue(), &args()));
    }

    #[test]
    fn a_summary_is_searched_by_substring_and_ignoring_case() {
        let mut a = args();
        a.summary_contains = Some("[all]".into());
        assert!(matches(&issue(), &a));
        a.summary_contains = Some("иерархию".into());
        assert!(matches(&issue(), &a));
        a.summary_contains = Some("nothing".into());
        assert!(!matches(&issue(), &a));
    }

    /// A status half-named is a different status, so these match whole values.
    #[test]
    fn the_other_filters_match_a_whole_value() {
        let mut a = args();
        a.status = Some("in progress".into());
        assert!(matches(&issue(), &a));
        a.status = Some("In".into());
        assert!(!matches(&issue(), &a));

        let mut a = args();
        a.label = Some("auth".into());
        assert!(matches(&issue(), &a));
        a.label = Some("au".into());
        assert!(!matches(&issue(), &a));
    }

    /// An assignee is `@[Name](id)` in the file, so either half finds them.
    #[test]
    fn an_assignee_is_found_by_name_or_by_id() {
        let mut a = args();
        a.assignee = Some("nilsson.m".into());
        assert!(matches(&issue(), &a));
        a.assignee = Some("Maria".into());
        assert!(matches(&issue(), &a));
        a.assignee = Some("someone else".into());
        assert!(!matches(&issue(), &a));

        let mut unassigned = issue();
        unassigned.assignee = None;
        assert!(!matches(&unassigned, &a));
    }

    /// The sprint an issue is in is the last of the ones it has been in.
    #[test]
    fn the_sprint_filter_asks_where_the_issue_is_now() {
        let mut a = args();
        a.sprint = Some("TSK Спринт 6".into());
        assert!(matches(&issue(), &a));
        // It has been in 5; it is not in 5.
        a.sprint = Some("TSK Спринт 5".into());
        assert!(!matches(&issue(), &a));
    }

    #[test]
    fn filters_are_a_conjunction() {
        let mut a = args();
        a.label = Some("regular".into());
        a.status = Some("Done".into());
        assert!(!matches(&issue(), &a), "both have to hold");
    }
}
