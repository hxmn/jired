//! `jired users` — the people this workspace knows, and how to name them.
//!
//! A file names a person as `@[Display Name](id)`, and the id is the half
//! nobody can type: an account id on Cloud, a username on Data Center. Finding
//! one meant `grep -rhoE '\[Name\]\([^)]*\)' *.md` over the working copy —
//! which also answered with every spelling a renamed person had ever had, one
//! line each, as though they were different people.
//!
//! Offline, like `fields`: everyone the snapshots and the files mention is
//! already on disk, so the answer costs no request and works on a plane.

use crate::api::model::UserRef;
use crate::cli::context::Context;
use crate::cli::GlobalArgs;
use crate::error::{ExitCode, Result};
use crate::json::Out;
use crate::style::{self, CYAN, DIM};
use crate::sync::Engine;
use chrono::{DateTime, Utc};
use clap::Args;
use std::collections::BTreeMap;

#[derive(Args, Debug, Clone, Default)]
pub struct UsersArgs {
    /// Only people whose name — any name they have had — or id contains this
    #[arg(value_name = "TEXT")]
    pub filter: Option<String>,
}

/// One person, however many names they have gone by.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct KnownPerson {
    /// The name they were last seen under.
    pub display_name: String,
    pub account_id: String,
    /// What goes into a file to name them: `@[Name](id)`.
    pub mention: String,
    /// Earlier names under the same id, most recent first.
    pub also_known_as: Vec<String>,
    /// How many times the workspace mentions them, which is what ranks them.
    pub seen: usize,
}

/// Every sighting of one id, by the name it was under.
#[derive(Default)]
struct Sightings {
    names: BTreeMap<String, Seen>,
}

/// One name: when it was last seen, and how often.
#[derive(Default, Clone, Copy)]
struct Seen {
    last: Option<DateTime<Utc>>,
    times: usize,
}

impl Sightings {
    fn add(&mut self, name: &str, at: Option<DateTime<Utc>>) {
        let name = name.trim();
        if name.is_empty() {
            return;
        }
        let seen = self.names.entry(name.to_string()).or_default();
        seen.last = seen.last.max(at);
        seen.times += 1;
    }
}

/// Fold sightings into one entry per id, named by the most recent name.
///
/// "Most recent" is by when the thing that carries the name happened — the
/// issue's last update, the comment, the worklog — because a rename shows up
/// first on whatever was touched after it. A mention in a file carries no date
/// and so never outranks one that does; between two undated names the one seen
/// more often wins.
fn people(seen: BTreeMap<String, Sightings>) -> Vec<KnownPerson> {
    let mut out: Vec<KnownPerson> = seen
        .into_iter()
        .filter(|(_, s)| !s.names.is_empty())
        .map(|(id, s)| {
            let mut names: Vec<(String, Seen)> = s.names.into_iter().collect();
            // Every sighting counts, whatever name it came under.
            let seen = names.iter().map(|(_, s)| s.times).sum();
            // A payload with no display name, or a mention written as
            // `@[nilsson.m](nilsson.m)`, carries the id where the name goes.
            // That is not another name the person had, only the absence of
            // one, so it is dropped wherever a real name was seen.
            if names.iter().any(|(n, _)| !n.eq_ignore_ascii_case(&id)) {
                names.retain(|(n, _)| !n.eq_ignore_ascii_case(&id));
            }
            names.sort_by(|a, b| {
                b.1.last
                    .cmp(&a.1.last)
                    .then_with(|| b.1.times.cmp(&a.1.times))
                    .then_with(|| a.0.cmp(&b.0))
            });
            let display_name = names[0].0.clone();
            KnownPerson {
                mention: format!("@[{display_name}]({id})"),
                also_known_as: names.into_iter().skip(1).map(|(n, _)| n).collect(),
                display_name,
                account_id: id,
                seen,
            }
        })
        .collect();
    out.sort_by(|a, b| {
        b.seen.cmp(&a.seen).then_with(|| {
            a.display_name
                .to_lowercase()
                .cmp(&b.display_name.to_lowercase())
        })
    });
    out
}

/// Every `@[Name](id)` in a text, as `(name, id)`.
fn mentions(text: &str) -> Vec<(String, String)> {
    let mut found = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find("@[") {
        rest = &rest[start + 2..];
        let Some(close) = rest.find("](") else { break };
        let name = &rest[..close];
        // A name cannot span lines or hold another bracket: anything that does
        // is prose that happens to contain `@[`, not a mention.
        if name.contains(['\n', '[', ']']) {
            continue;
        }
        let after = &rest[close + 2..];
        let Some(end) = after.find(')') else { break };
        let id = &after[..end];
        if !id.is_empty() && !id.contains(char::is_whitespace) {
            found.push((name.to_string(), id.to_string()));
        }
        rest = &after[end..];
    }
    found
}

pub fn run(g: &GlobalArgs, args: &UsersArgs, out: Out) -> Result<ExitCode> {
    let mut ctx = Context::open(g, out)?;
    let engine = Engine::open(&mut ctx)?;
    let markup = engine.markup();
    let field_map = engine.field_map()?;

    let mut seen: BTreeMap<String, Sightings> = BTreeMap::new();
    let mut add = |user: Option<&UserRef>, at: Option<DateTime<Utc>>| {
        if let Some(u) = user.filter(|u| !u.id.is_empty()) {
            seen.entry(u.id.clone())
                .or_default()
                .add(&u.display_name, at);
        }
    };
    for row in engine.state.issues()? {
        // The newer snapshot: a rename the last fetch saw is the one to report.
        let Some(raw) = row
            .remote
            .as_ref()
            .map(|r| &r.raw)
            .or(row.base.as_ref().map(|b| &b.raw))
        else {
            continue;
        };
        let Ok(issue) = crate::api::normalize::issue_from_json(raw, markup, &field_map) else {
            continue;
        };
        add(issue.fields.assignee.as_ref(), Some(issue.updated));
        add(issue.fields.reporter.as_ref(), Some(issue.updated));
        for c in &issue.comments {
            add(c.author.as_ref(), c.updated.or(c.created));
        }
        for w in &issue.worklogs {
            add(w.author.as_ref(), w.updated.or(w.created).or(w.started));
        }
    }
    // The files too: a custom user field, a mention in a description, a value
    // staged by hand — all spelled the same way, none in a field above.
    for file in engine.projector.scan()?.values() {
        let Ok(text) = engine.projector.read_raw(&file.relative) else {
            continue;
        };
        for (name, id) in mentions(&text) {
            seen.entry(id).or_default().add(&name, None);
        }
    }

    let mut found = people(seen);
    if let Some(text) = &args.filter {
        let text = text.to_lowercase();
        found.retain(|p| {
            p.account_id.to_lowercase().contains(&text)
                || std::iter::once(&p.display_name)
                    .chain(&p.also_known_as)
                    .any(|n| n.to_lowercase().contains(&text))
        });
    }

    if found.is_empty() {
        out.say(match &args.filter {
            Some(text) => format!("nobody in this workspace matches `{text}`"),
            None => "this workspace knows nobody yet".to_string(),
        });
        out.hint("run `jired pull` to bring issues, and the people on them, down");
    }
    let name_width = crate::cli::cmd::list::column(found.iter().map(|p| p.display_name.len()), 28);
    let id_width = crate::cli::cmd::list::column(found.iter().map(|p| p.account_id.len()), 28);
    for p in &found {
        let aka = match p.also_known_as.is_empty() {
            true => String::new(),
            false => style::out(DIM, format!("  (was {})", p.also_known_as.join(", "))),
        };
        out.say(format!(
            "{} {} {}{aka}",
            style::out(CYAN, format!("{:<name_width$}", p.display_name)),
            style::out(DIM, format!("{:<id_width$}", p.account_id)),
            p.mention
        ));
    }

    out.emit(
        "users",
        &serde_json::json!({ "users": found, "count": found.len() }),
    );
    Ok(ExitCode::Success)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(text: &str) -> Option<DateTime<Utc>> {
        crate::api::model::parse_timestamp(text)
    }

    /// The field report: Maria Nilsson became Maria Bergman, and a grep over
    /// the files found two people sharing one id. One id is one person, named
    /// by what they were called last.
    #[test]
    fn a_renamed_person_is_one_entry_under_the_newer_name() {
        let mut seen: BTreeMap<String, Sightings> = BTreeMap::new();
        let s = seen.entry("nilsson.m".into()).or_default();
        s.add("Maria Nilsson", at("2026-03-01T10:00:00Z"));
        s.add("Maria Nilsson", at("2026-04-01T10:00:00Z"));
        s.add("Maria Bergman", at("2026-10-01T10:00:00Z"));
        s.add("Maria Nilsson", None);
        // Where the server sent no display name, the id stands in for one:
        // not a name she was ever known by.
        s.add("nilsson.m", at("2026-10-02T10:00:00Z"));
        seen.entry("jd".into()).or_default().add("Jane D", None);

        let found = people(seen);
        assert_eq!(found.len(), 2);
        let maria = &found[0];
        assert_eq!(maria.display_name, "Maria Bergman");
        assert_eq!(maria.account_id, "nilsson.m");
        assert_eq!(maria.mention, "@[Maria Bergman](nilsson.m)");
        assert_eq!(maria.also_known_as, ["Maria Nilsson"]);
        assert_eq!(maria.seen, 5);
        assert_eq!(found[1].display_name, "Jane D");
    }

    #[test]
    fn mentions_are_read_out_of_text_and_prose_is_not() {
        let text = "assignee: '@[Ilya A](5b10a2)'\n\nAsk @[Jane D](jd) or email [x](y). \
                    An @[unclosed one\n and @[Two Words](id with space).";
        assert_eq!(
            mentions(text),
            [
                ("Ilya A".to_string(), "5b10a2".to_string()),
                ("Jane D".to_string(), "jd".to_string()),
            ]
        );
    }
}
