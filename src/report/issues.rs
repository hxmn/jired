//! `jired report issues` — how many issues, broken down by one or two fields.
//!
//! The question behind a sprint chart — issues, stories and bugs per sprint —
//! was answered with a JQL count per cell: 75 searches in a shell loop and `jq`
//! to read each total. Every figure in that table is already on the issues
//! themselves, so one pass over them answers the lot.
//!
//! An issue with several values for a field — the sprints it has been through,
//! its labels, its fix versions — counts once under each of them, which is how
//! JQL's `sprint = X` matches too: an issue carried over from sprint 41 into 42
//! is one of 41's issues and one of 42's. A row's total is still the issues in
//! it, each counted once, so a row total can be less than the sum of its cells.

use crate::doc::Frontmatter;
use crate::json::Out;
use serde::Serialize;
use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};

/// What an issue with nothing in a field is counted under. Spelled out rather
/// than left blank: a blank row in a chart is a row nobody can name.
pub const NONE: &str = "(none)";

/// A field `--by` can break the count down by.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Dim {
    /// Every sprint the issue has been in
    Sprint,
    Type,
    Status,
    /// Jira's three buckets: new, indeterminate, done
    Category,
    Assignee,
    Label,
    #[value(name = "fix_version", alias = "fix-version")]
    FixVersion,
    Priority,
    Parent,
    Epic,
    Component,
}

impl Dim {
    pub fn name(self) -> &'static str {
        match self {
            Dim::Sprint => "sprint",
            Dim::Type => "type",
            Dim::Status => "status",
            Dim::Category => "category",
            Dim::Assignee => "assignee",
            Dim::Label => "label",
            Dim::FixVersion => "fix_version",
            Dim::Priority => "priority",
            Dim::Parent => "parent",
            Dim::Epic => "epic",
            Dim::Component => "component",
        }
    }
}

/// What one issue says about every field a report can count by, from whichever
/// source the report reads.
#[derive(Debug, Clone, Default)]
pub struct Facts {
    pub key: String,
    pub sprint: Vec<String>,
    pub issue_type: String,
    pub status: String,
    pub category: Option<String>,
    pub assignee: Option<String>,
    pub labels: Vec<String>,
    pub fix_versions: Vec<String>,
    pub priority: Option<String>,
    pub parent: Option<String>,
    pub epic: Option<String>,
    pub components: Vec<String>,
}

impl Facts {
    /// From a working-copy file. `category` comes from the snapshots, since the
    /// bucket behind a status name is the server's and not in the file.
    pub fn from_frontmatter(fm: &Frontmatter, category: Option<String>) -> Self {
        Self {
            key: fm.key.clone(),
            sprint: fm.sprint.clone(),
            issue_type: fm.issue_type.clone(),
            status: fm.status.clone(),
            category,
            // The name a chart shows, out of the file's `@[Name](id)`.
            assignee: fm
                .assignee
                .as_deref()
                .map(crate::doc::frontmatter::display_name),
            labels: fm.labels.clone(),
            fix_versions: fm.fix_versions.clone(),
            priority: fm.priority.clone(),
            parent: fm.parent.clone(),
            epic: fm.epic.clone(),
            components: fm.components.clone(),
        }
    }

    pub fn from_remote(issue: &crate::api::RemoteIssue) -> Self {
        let f = &issue.fields;
        Self {
            key: issue.key.clone(),
            sprint: f.sprint.clone(),
            issue_type: f.issue_type.clone(),
            status: f.status.clone(),
            category: Some(issue.status_category().to_string()).filter(|c| !c.is_empty()),
            assignee: f.assignee.as_ref().map(|u| u.display_name.clone()),
            labels: f.labels.clone(),
            fix_versions: f.fix_versions.clone(),
            priority: f.priority.clone(),
            parent: f.parent.clone(),
            epic: f.epic.clone(),
            components: f.components.clone(),
        }
    }

    /// Every value the issue is counted under for `dim`, [`NONE`] when empty.
    fn values(&self, dim: Dim) -> Vec<String> {
        let one = |v: &str| vec![v.to_string()];
        let opt = |v: &Option<String>| v.iter().cloned().collect::<Vec<_>>();
        let mut values: Vec<String> = match dim {
            Dim::Sprint => self.sprint.clone(),
            Dim::Type => one(&self.issue_type),
            Dim::Status => one(&self.status),
            Dim::Category => opt(&self.category),
            Dim::Assignee => opt(&self.assignee),
            Dim::Label => self.labels.clone(),
            Dim::FixVersion => self.fix_versions.clone(),
            Dim::Priority => opt(&self.priority),
            Dim::Parent => opt(&self.parent),
            Dim::Epic => opt(&self.epic),
            Dim::Component => self.components.clone(),
        };
        values.retain(|v| !v.trim().is_empty());
        values.sort();
        values.dedup();
        if values.is_empty() {
            values.push(NONE.to_string());
        }
        values
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct IssuesReport {
    pub by: Vec<&'static str>,
    /// `workspace` for the working copy, `server` for `--jql`.
    pub source: &'static str,
    pub jql: Option<String>,
    /// Issues counted, each once.
    pub total: usize,
    /// The second field's values, in column order; empty for one field.
    pub columns: Vec<String>,
    pub rows: Vec<Row>,
    /// Per column, the issues in it, each once; empty for one field.
    pub totals: BTreeMap<String, usize>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Row {
    pub value: String,
    /// Per column, for two fields; empty for one.
    pub counts: BTreeMap<String, usize>,
    /// Issues in this row, each once.
    pub total: usize,
}

impl IssuesReport {
    pub fn build(by: &[Dim], issues: &[Facts]) -> Self {
        let first = by[0];
        let second = by.get(1).copied();

        let mut rows: BTreeMap<String, (BTreeMap<String, usize>, usize)> = BTreeMap::new();
        let mut totals: BTreeMap<String, usize> = BTreeMap::new();
        let mut columns: BTreeSet<String> = BTreeSet::new();
        for issue in issues {
            let across = second.map(|d| issue.values(d)).unwrap_or_default();
            for value in issue.values(first) {
                let (counts, total) = rows.entry(value).or_default();
                *total += 1;
                for column in &across {
                    *counts.entry(column.clone()).or_default() += 1;
                }
            }
            for column in across {
                *totals.entry(column.clone()).or_default() += 1;
                columns.insert(column);
            }
        }

        let mut rows: Vec<Row> = rows
            .into_iter()
            .map(|(value, (counts, total))| Row {
                value,
                counts,
                total,
            })
            .collect();
        rows.sort_by(|a, b| natural(&a.value, &b.value));
        let mut columns: Vec<String> = columns.into_iter().collect();
        columns.sort_by(|a, b| natural(a, b));

        Self {
            by: by.iter().map(|d| d.name()).collect(),
            source: "workspace",
            jql: None,
            total: issues.len(),
            columns,
            rows,
            totals,
        }
    }

    /// The header row and the body, cell by cell, total row last — the one
    /// table both renderings draw.
    fn table(&self) -> (Vec<String>, Vec<Vec<String>>) {
        let mut head = vec![self.by.first().copied().unwrap_or_default().to_string()];
        head.extend(self.columns.iter().cloned());
        head.push(match self.columns.is_empty() {
            true => "issues".to_string(),
            false => "total".to_string(),
        });
        let mut body: Vec<Vec<String>> = self
            .rows
            .iter()
            .map(|r| {
                let mut line = vec![r.value.clone()];
                line.extend(
                    self.columns
                        .iter()
                        .map(|c| r.counts.get(c).copied().unwrap_or(0).to_string()),
                );
                line.push(r.total.to_string());
                line
            })
            .collect();
        let mut last = vec!["total".to_string()];
        last.extend(
            self.columns
                .iter()
                .map(|c| self.totals.get(c).copied().unwrap_or(0).to_string()),
        );
        last.push(self.total.to_string());
        body.push(last);
        (head, body)
    }

    /// A Markdown pipe table, ready to paste into a page.
    pub fn markdown(&self) -> String {
        let (head, body) = self.table();
        let cell = |s: &str| s.replace('|', "\\|");
        let mut out = format!(
            "| {} |\n",
            head.iter().map(|h| cell(h)).collect::<Vec<_>>().join(" | ")
        );
        // Names left, counts right: a column of figures is read by its units.
        let align: Vec<&str> = (0..head.len())
            .map(|i| if i == 0 { "---" } else { "---:" })
            .collect();
        out.push_str(&format!("| {} |\n", align.join(" | ")));
        for line in body {
            out.push_str(&format!(
                "| {} |\n",
                line.iter().map(|c| cell(c)).collect::<Vec<_>>().join(" | ")
            ));
        }
        out
    }

    pub fn render(&self, out: Out, markdown: bool) {
        if markdown {
            out.say(self.markdown().trim_end());
            return;
        }
        use crate::style::{self, BOLD, DIM};
        let (head, body) = self.table();
        let widths: Vec<usize> = (0..head.len())
            .map(|i| {
                std::iter::once(&head)
                    .chain(body.iter())
                    .map(|line| line[i].chars().count())
                    .max()
                    .unwrap_or(0)
            })
            .collect();
        let line = |cells: &[String]| {
            cells
                .iter()
                .enumerate()
                .map(|(i, c)| match i {
                    0 => format!("{c:<w$}", w = widths[0]),
                    _ => format!("{c:>w$}", w = widths[i]),
                })
                .collect::<Vec<_>>()
                .join("  ")
        };
        out.say(style::out(DIM, line(&head)));
        let last = body.len() - 1;
        for (i, cells) in body.iter().enumerate() {
            match i == last {
                true => out.say(style::out(BOLD, line(cells))),
                false => out.say(line(cells)),
            }
        }
    }
}

/// Order names the way a person counts: `Sprint 9` before `Sprint 10`, case
/// ignored, and [`NONE`] after everything that has a name.
pub fn natural(a: &str, b: &str) -> Ordering {
    match (a == NONE, b == NONE) {
        (true, false) => return Ordering::Greater,
        (false, true) => return Ordering::Less,
        _ => {}
    }
    let (mut x, mut y) = (chunks(a).into_iter(), chunks(b).into_iter());
    loop {
        match (x.next(), y.next()) {
            (None, None) => return a.cmp(b),
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(p), Some(q)) => {
                let order = match (p.parse::<u128>(), q.parse::<u128>()) {
                    (Ok(m), Ok(n)) => m.cmp(&n),
                    _ => p.to_lowercase().cmp(&q.to_lowercase()),
                };
                if order != Ordering::Equal {
                    return order;
                }
            }
        }
    }
}

/// Runs of digits and runs of everything else.
fn chunks(s: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for c in s.chars() {
        match out.last_mut() {
            Some(last)
                if last.chars().last().is_some_and(|l| l.is_ascii_digit())
                    == c.is_ascii_digit() =>
            {
                last.push(c)
            }
            _ => out.push(c.to_string()),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn issue(key: &str, sprints: &[&str], issue_type: &str) -> Facts {
        Facts {
            key: key.into(),
            sprint: sprints.iter().map(|s| s.to_string()).collect(),
            issue_type: issue_type.into(),
            ..Facts::default()
        }
    }

    #[test]
    fn names_sort_the_way_people_count() {
        let mut names = vec!["Sprint 10", "sprint 9", NONE, "Sprint 2", "Backlog"];
        names.sort_by(|a, b| natural(a, b));
        assert_eq!(
            names,
            ["Backlog", "Sprint 2", "sprint 9", "Sprint 10", NONE]
        );
    }

    /// The chart the field report needed: per sprint, per type, with totals —
    /// and a carried-over issue counted in both sprints, as JQL counts it.
    #[test]
    fn a_matrix_counts_every_sprint_an_issue_was_in() {
        let issues = vec![
            issue("T-1", &["Sprint 9", "Sprint 10"], "Story"),
            issue("T-2", &["Sprint 10"], "Bug"),
            issue("T-3", &["Sprint 10"], "Story"),
            issue("T-4", &[], "Task"),
        ];
        let report = IssuesReport::build(&[Dim::Sprint, Dim::Type], &issues);
        assert_eq!(report.by, ["sprint", "type"]);
        assert_eq!(report.columns, ["Bug", "Story", "Task"]);
        let values: Vec<&str> = report.rows.iter().map(|r| r.value.as_str()).collect();
        assert_eq!(values, ["Sprint 9", "Sprint 10", NONE]);
        let ten = &report.rows[1];
        assert_eq!(ten.total, 3);
        assert_eq!(ten.counts["Story"], 2);
        assert_eq!(ten.counts["Bug"], 1);
        assert_eq!(report.rows[2].counts["Task"], 1);
        // Every issue once, in the corner and in each column.
        assert_eq!(report.total, 4);
        assert_eq!(report.totals["Story"], 2);

        let md = report.markdown();
        assert!(
            md.starts_with("| sprint | Bug | Story | Task | total |\n"),
            "{md}"
        );
        assert!(md.contains("| Sprint 10 | 1 | 2 | 0 | 3 |"), "{md}");
        assert!(md.trim_end().ends_with("| total | 1 | 2 | 1 | 4 |"), "{md}");
    }

    #[test]
    fn one_field_is_a_list_of_counts() {
        let issues = vec![issue("T-1", &[], "Story"), issue("T-2", &[], "Story")];
        let report = IssuesReport::build(&[Dim::Type], &issues);
        assert!(report.columns.is_empty());
        assert_eq!(report.rows.len(), 1);
        assert_eq!(report.rows[0].total, 2);
        assert!(report.markdown().contains("| type | issues |"));
    }

    #[test]
    fn a_pipe_in_a_value_does_not_split_the_markdown_cell() {
        let issues = vec![issue("T-1", &["A | B"], "Story")];
        let md = IssuesReport::build(&[Dim::Sprint], &issues).markdown();
        assert!(md.contains("| A \\| B | 1 |"), "{md}");
    }
}
