//! "Which issue?", asked once for every command that has to ask it.
//!
//! `edit`, `comment` and `open` all reach the same dead end when nothing is
//! named — the user knows which issue they mean but not how it is spelled — and
//! they now all reach the same screen instead. Sharing it is not only about the
//! duplication: a picker that sorted differently depending on which command
//! opened it would be a worse tool than one list that behaves one way.
//!
//! The sort and the status filter live in the config file, so the list opens
//! the way it was left. The screen writes them back itself, which is why they
//! are settings rather than screen state: `jired config set ui.issues.sort
//! priority:asc` and `^s` in the picker are two spellings of one thing.

use crate::cli::context::Context;
use crate::error::{JiredError, Result};
use crate::project::Projector;
use crate::store::state::StateStore;
use crate::tui::catalog::{Catalog, KnownIssue};
use crate::tui::pick::{PickScreen, Scope, Sort, COLUMNS};

/// Which issues a command can sensibly be pointed at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Listing {
    /// Only the issues with a file in the working copy. What `edit` and
    /// `comment` need: they stage a change into that file, and offering an
    /// issue without one would hand back a key with nothing behind it.
    WithFiles,
    /// Every issue the workspace has a snapshot of. What `open` needs: it
    /// sends a URL to a browser and never touches the working copy.
    Known,
}

const SORT_KEY: &str = "ui.issues.sort";
const FILTER_KEY: &str = "ui.issues.filter";

/// Ask which issue, and hand back its key.
///
/// The caller has already decided the screen is worth opening; this is where it
/// finds out there is nothing to open it on.
pub fn choose(ctx: &mut Context, title: &str, listing: Listing) -> Result<String> {
    if !crate::tui::available() {
        return Err(
            JiredError::missing_value("no issue given, and no terminal to search on")
                .with_hint("name the issue: `jired edit PROJ-142`, or a file name"),
        );
    }

    let (sort, scope) = settings(ctx)?;
    let issues = issues(ctx, listing)?;
    let mut screen = PickScreen::new(title, &issues, sort, scope.clone(), chrono::Utc::now());
    if screen.is_empty() {
        return Err(match listing {
            Listing::WithFiles => JiredError::usage("this workspace has no issue files yet")
                .with_hint("run `jired pull`, or `jired create` to write one"),
            Listing::Known => JiredError::usage("this workspace knows no issues yet")
                .with_hint("run `jired pull` first, or name the issue: `jired open PROJ-142`"),
        });
    }

    let chosen = crate::tui::run(&mut screen)?;
    // Remembered whether or not an issue was chosen: re-ordering the list and
    // then deciding against everything in it is still a decision about the
    // order, and losing it would punish looking around.
    remember(ctx, sort, &scope, screen.sort(), &screen.scope());

    chosen.ok_or_else(|| JiredError::usage("no issue chosen").with_hint("nothing was changed"))
}

/// Note that this machine just went to an issue, for the `viewed` column.
///
/// Best-effort on purpose: a command that did its job must not fail because a
/// timestamp nobody asked for could not be written.
pub fn mark_viewed(state: &StateStore, key: &str) {
    if let Err(e) = state.mark_viewed(key) {
        tracing::debug!("could not stamp {key} as viewed: {e}");
    }
}

/// The sort and filter the picker should open with.
///
/// Both are validated here rather than inside the screen, so an unreadable
/// setting is a message the user can read instead of one painted over by the
/// list it was meant to arrange.
fn settings(ctx: &mut Context) -> Result<(Sort, Scope)> {
    let sort = match ctx.resolver.get_quiet(SORT_KEY)? {
        Some(spec) => Sort::parse(&spec).ok_or_else(|| {
            JiredError::usage(format!("`{spec}` is not a way to sort the issue list"))
                .with_detail("key", SORT_KEY)
                .with_hint(format!(
                    "write <column>:asc or <column>:desc, where the column is one of {}",
                    COLUMNS
                        .iter()
                        .map(|c| c.name())
                        .collect::<Vec<_>>()
                        .join(", ")
                ))
        })?,
        None => Sort::default(),
    };
    let scope = match ctx.resolver.get_quiet(FILTER_KEY)? {
        Some(spec) => Scope::parse(&spec).ok_or_else(|| {
            JiredError::usage(format!("`{spec}` is not a way to filter the issue list"))
                .with_detail("key", FILTER_KEY)
                .with_hint("write all, category:new|indeterminate|done, or status:<name>")
        })?,
        None => Scope::All,
    };
    Ok((sort, scope))
}

/// Write back whatever the screen changed, and nothing it did not.
///
/// Into this workspace's config even when the old value came from the global
/// one: the list was re-arranged here, and quietly re-arranging every other
/// workspace to match would be a larger thing than was asked for.
fn remember(ctx: &mut Context, was_sort: Sort, was_scope: &Scope, sort: Sort, scope: &Scope) {
    if was_sort == sort && was_scope == scope {
        return;
    }
    let write = || -> Result<()> {
        let mut file = ctx.local_config()?;
        if was_sort != sort {
            file.set(SORT_KEY, &sort.to_string())?;
        }
        if was_scope != scope {
            file.set(FILTER_KEY, &scope.to_string())?;
        }
        file.save_to(&ctx.workspace.config_path())
    };
    // A preference that could not be saved is worth a line, not a failure: the
    // issue was chosen and the command it was chosen for still has work to do.
    if let Err(e) = write() {
        ctx.out
            .warn(format!("could not remember the list settings: {e}"));
    }
}

/// The issues to offer, in no particular order — the screen sorts them.
fn issues(ctx: &mut Context, listing: Listing) -> Result<Vec<KnownIssue>> {
    let state = StateStore::open(&ctx.workspace)?;
    let project = ctx.resolver.get_quiet("workspace.project")?;
    let catalog = Catalog::from_rows(&state.issues()?, project.as_deref());
    if listing == Listing::Known {
        return Ok(catalog.issues);
    }

    let projector = Projector::new(ctx.workspace.root());
    let files = projector.scan()?;
    let mut issues: Vec<KnownIssue> = catalog
        .issues
        .into_iter()
        .filter(|i| files.contains_key(&i.key))
        .collect();
    // Locally created issues have no snapshot to build a row out of, so the
    // file is the only thing that knows about them. Left out, the one issue
    // someone just wrote would be the one the picker could not find.
    for (key, file) in &files {
        if issues.iter().any(|i| &i.key == key) {
            continue;
        }
        let doc = projector.read(&file.relative)?;
        let fm = &doc.frontmatter;
        issues.push(KnownIssue {
            key: key.clone(),
            summary: fm.summary.clone(),
            issue_type: fm.issue_type.clone(),
            status: fm.status.clone(),
            // The file carries no status category: it is a Jira concept, and
            // an issue that has never been to the server has no answer for it.
            status_category: String::new(),
            priority: fm.priority.clone().unwrap_or_default(),
            assignee: fm.assignee.as_deref().map(display_name).unwrap_or_default(),
            reporter: fm.reporter.as_deref().map(display_name).unwrap_or_default(),
            created: fm
                .created
                .as_deref()
                .and_then(crate::api::model::parse_timestamp),
            updated: fm
                .updated
                .as_deref()
                .and_then(crate::api::model::parse_timestamp),
            viewed: state.issue(key)?.and_then(|row| row.viewed_at),
        });
    }
    Ok(issues)
}

/// The readable half of the `@[Ilya A](5b10a2)` spelling a file carries.
fn display_name(mention: &str) -> String {
    let name = mention
        .strip_prefix("@[")
        .and_then(|rest| rest.split_once("](").map(|(name, _)| name));
    name.unwrap_or(mention).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_mention_reads_as_the_name_inside_it() {
        assert_eq!(display_name("@[Ilya A](5b10a2)"), "Ilya A");
        // Everything else is passed through: a file edited by hand may hold a
        // bare name, and showing it is better than showing nothing.
        assert_eq!(display_name("Ilya A"), "Ilya A");
        assert_eq!(display_name(""), "");
    }
}
