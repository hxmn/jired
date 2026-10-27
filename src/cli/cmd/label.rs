//! `jired label` — put a label on issues, or take one off.
//!
//! Labels are a list, so there are three verbs rather than one: `add` and
//! `remove` touch a single label and leave the rest alone, and `set` replaces
//! the lot. Without them, labelling six issues meant a script that rewrote
//! `labels:` in the YAML by hand — external code parsing jired's own file
//! format, which is the thing the format exists to spare people.
//!
//! Staged like `sprint add` and `assign`: the file is written, `status` and
//! `diff` show it, `push` sends it — or `--push` does, now.

use crate::cli::cmd::stage::{self, Wording};
use crate::cli::context::Context;
use crate::cli::GlobalArgs;
use crate::error::{ExitCode, JiredError, Result};
use crate::json::Out;
use clap::{Args, Subcommand};

#[derive(Subcommand, Debug)]
pub enum LabelCmd {
    /// Add a label, keeping the ones already there
    Add(LabelArgs),
    /// Take a label off, keeping the others
    Remove(LabelArgs),
    /// Replace every label with these
    Set(LabelArgs),
}

#[derive(Args, Debug, Clone, Default)]
pub struct LabelArgs {
    /// The label; `set` takes several, comma-separated
    #[arg(value_name = "LABEL", required = true)]
    pub label: String,

    /// Issues to label: keys, or paths to their files
    #[arg(value_name = "ISSUES", required = true, value_delimiter = ',')]
    pub issues: Vec<String>,

    /// Apply immediately instead of leaving the change staged
    #[arg(long)]
    pub push: bool,

    /// Show the API calls a push would make, send nothing, and leave the
    /// files as they were
    #[arg(long)]
    pub dry_run: bool,
}

/// What a verb does to the list, so the three share one implementation.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Verb {
    Add,
    Remove,
    Set,
}

pub fn run(g: &GlobalArgs, cmd: &LabelCmd, out: Out) -> Result<ExitCode> {
    let (verb, args) = match cmd {
        LabelCmd::Add(a) => (Verb::Add, a),
        LabelCmd::Remove(a) => (Verb::Remove, a),
        LabelCmd::Set(a) => (Verb::Set, a),
    };

    // Jira labels cannot contain spaces; a value with one is a list somebody
    // separated the wrong way, and sending it would create a label nobody meant.
    let wanted: Vec<String> = args
        .label
        .split(',')
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect();
    if wanted.is_empty() {
        return Err(
            JiredError::usage("no label given").with_hint("`jired label add regular PROJ-142`")
        );
    }
    if let Some(bad) = wanted.iter().find(|l| l.contains(char::is_whitespace)) {
        return Err(JiredError::usage(format!(
            "`{bad}` is not a label: Jira labels have no spaces"
        ))
        .with_hint("separate several labels with commas — `a,b`"));
    }
    if verb != Verb::Set && wanted.len() > 1 {
        return Err(
            JiredError::usage("`add` and `remove` take one label at a time".to_string())
                .with_hint("run it again for the next one, or use `jired label set a,b`"),
        );
    }

    let mut ctx = Context::open(g, out)?;
    ctx.lock()?;
    let labels = wanted.clone();
    let staged = stage::stage(&mut ctx, &args.issues, out, move |doc| {
        Ok(apply(&mut doc.frontmatter.labels, verb, &labels))
    })?;

    let what = match verb {
        Verb::Remove => format!("without `{}`", wanted.join("`, `")),
        _ => format!("`{}`", wanted.join("`, `")),
    };
    let already = match verb {
        Verb::Add => format!("already has `{}`", wanted.join("`, `")),
        Verb::Remove => format!("does not have `{}`", wanted.join("`, `")),
        Verb::Set => "already has exactly those labels".to_string(),
    };
    let outcome = stage::finish(
        &mut ctx,
        staged,
        &Wording {
            what: &what,
            verb: "labelled",
            already: &already,
        },
        args.push,
        args.dry_run,
        out,
        serde_json::json!({ "labels": wanted, "operation": match verb {
            Verb::Add => "add",
            Verb::Remove => "remove",
            Verb::Set => "set",
        }}),
    )?;
    out.emit("label", &outcome.doc);
    Ok(outcome.exit)
}

/// Apply the verb, reporting whether the list actually moved.
///
/// Order is the server's to decide and jira sorts them anyway, so `add` appends
/// and `set` keeps the order given rather than either of them sorting: a
/// re-ordering that changes nothing would otherwise read as a change.
fn apply(labels: &mut Vec<String>, verb: Verb, wanted: &[String]) -> bool {
    let before = labels.clone();
    match verb {
        Verb::Add => {
            for l in wanted {
                if !labels.iter().any(|x| x == l) {
                    labels.push(l.clone());
                }
            }
        }
        Verb::Remove => labels.retain(|x| !wanted.iter().any(|w| w == x)),
        Verb::Set => *labels = wanted.to_vec(),
    }
    *labels != before
}

#[cfg(test)]
mod tests {
    use super::*;

    fn list(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn add_keeps_what_is_there_and_says_when_nothing_moved() {
        let mut labels = list(&["auth"]);
        assert!(apply(&mut labels, Verb::Add, &list(&["regular"])));
        assert_eq!(labels, ["auth", "regular"]);
        // Already there: not a change, so nothing is staged for it.
        assert!(!apply(&mut labels, Verb::Add, &list(&["regular"])));
        assert_eq!(labels, ["auth", "regular"]);
    }

    #[test]
    fn remove_takes_one_off_and_leaves_the_rest() {
        let mut labels = list(&["auth", "regular"]);
        assert!(apply(&mut labels, Verb::Remove, &list(&["auth"])));
        assert_eq!(labels, ["regular"]);
        assert!(!apply(&mut labels, Verb::Remove, &list(&["auth"])));
    }

    #[test]
    fn set_replaces_the_lot_in_the_order_it_was_given() {
        let mut labels = list(&["auth", "regular"]);
        assert!(apply(&mut labels, Verb::Set, &list(&["b", "a"])));
        assert_eq!(labels, ["b", "a"]);
        assert!(!apply(&mut labels, Verb::Set, &list(&["b", "a"])));
        // Clearing is `set` with nothing, which the CLI refuses; the list
        // itself has no opinion.
        assert!(apply(&mut labels, Verb::Set, &[]));
        assert!(labels.is_empty());
    }
}
