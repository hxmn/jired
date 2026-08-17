//! Grouping folders [M4.2, M4.3, M4.4].
//!
//! `by-parent/`, `sprints/` and `boards/` are views, never the source of truth:
//! every one of them is rebuilt from `state.db`, so `clean --regroup` can throw
//! the lot away and get it back. Nothing reads an issue *through* a grouping
//! folder — the workspace scan only looks at the top level — which is what
//! keeps a link from ever being mistaken for a second copy of an issue.

use crate::error::Result;
use crate::project::links::{self, LinkKind, LinkMode};
use crate::project::naming;
use crate::sync::Engine;
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Debug, Clone, Serialize, Default)]
pub struct GroupReport {
    pub parents: usize,
    pub sprints: usize,
    pub boards: usize,
    pub links: usize,
    pub kind: Option<LinkKind>,
    /// Groups that could not be built, and why.
    pub warnings: Vec<String>,
}

pub struct GroupOptions {
    pub mode: LinkMode,
    pub include_closed_sprints: bool,
}

impl GroupOptions {
    pub fn from_context(ctx: &mut crate::cli::context::Context) -> Result<Self> {
        Ok(Self {
            mode: match ctx.resolver.get_value("links.mode")? {
                Some(m) => LinkMode::parse(&m)?,
                None => LinkMode::Auto,
            },
            include_closed_sprints: ctx
                .resolver
                .get_bool("sprints.include_closed")?
                .unwrap_or(false),
        })
    }
}

/// Rebuild every grouping folder from the state database.
pub fn regroup(engine: &Engine, root: &Path, options: &GroupOptions) -> Result<GroupReport> {
    let mut report = GroupReport::default();
    let files = engine.projector.scan()?;
    let markup = engine.markup();

    // Clearing first is what makes this idempotent: a group someone renamed or
    // an issue that left a sprint would otherwise linger forever.
    for name in [
        naming::GROUP_PARENT,
        naming::GROUP_SPRINTS,
        naming::GROUP_BOARDS,
    ] {
        let dir = root.join(name);
        if dir.exists() {
            std::fs::remove_dir_all(&dir)?;
        }
    }

    let mut by_parent: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut by_sprint: BTreeMap<String, Vec<String>> = BTreeMap::new();

    for row in engine.state.issues()? {
        if row.deleted_remote || !files.contains_key(&row.key) {
            continue;
        }
        // Prefer base: it describes the file that is actually on disk. An
        // issue fetched but never pulled has only a remote snapshot, and
        // grouping it would point at a file that does not exist.
        let Some(snapshot) = row.base.as_ref() else {
            continue;
        };
        let issue = crate::api::normalize::issue_from_json(
            &snapshot.raw,
            markup,
            &engine.state.field_map()?,
        )?;

        // An epic link and a parent are the same relationship for grouping.
        if let Some(parent) = issue.fields.parent.or(issue.fields.epic) {
            by_parent.entry(parent).or_default().push(row.key.clone());
        }
        if let Some(sprint) = issue.fields.sprint {
            by_sprint.entry(sprint).or_default().push(row.key.clone());
        }
    }

    let closed: std::collections::HashSet<String> = if options.include_closed_sprints {
        Default::default()
    } else {
        engine
            .state
            .sprints()?
            .into_iter()
            .filter(|s| s.state == "closed")
            .map(|s| s.name)
            .collect()
    };

    for (parent, children) in &by_parent {
        // Name the folder after the parent's own file when we have it, so the
        // folder reads like the issue rather than like a key.
        let label = files
            .get(parent)
            .map(|f| f.relative.trim_end_matches(".md").to_string())
            .unwrap_or_else(|| parent.clone());
        let dir = root
            .join(naming::GROUP_PARENT)
            .join(naming::group_dir(&label, None));
        link_all(&files, children, &dir, options.mode, &mut report)?;
        report.parents += 1;
    }

    for (sprint, members) in &by_sprint {
        if closed.contains(sprint) {
            continue;
        }
        let dir = root
            .join(naming::GROUP_SPRINTS)
            .join(naming::group_dir(sprint, None));
        link_all(&files, members, &dir, options.mode, &mut report)?;
        report.sprints += 1;
    }

    report.boards = regroup_boards(engine, root, &files, options, &mut report)?;
    Ok(report)
}

/// `boards/<Board>/<Column>/` from the board's column-to-status mapping.
fn regroup_boards(
    engine: &Engine,
    root: &Path,
    files: &BTreeMap<String, crate::project::FoundFile>,
    options: &GroupOptions,
    report: &mut GroupReport,
) -> Result<usize> {
    let boards = engine.state.boards()?;
    if boards.is_empty() {
        return Ok(0);
    }
    let markup = engine.markup();
    let field_map = engine.state.field_map()?;

    let mut status_of: BTreeMap<String, String> = BTreeMap::new();
    for row in engine.state.issues()? {
        if row.deleted_remote || !files.contains_key(&row.key) {
            continue;
        }
        let Some(snapshot) = &row.base else { continue };
        let issue = crate::api::normalize::issue_from_json(&snapshot.raw, markup, &field_map)?;
        if let Some(id) = issue.fields.status_id {
            status_of.insert(row.key.clone(), id);
        }
    }

    let mut count = 0;
    for board in &boards {
        if board.columns.is_empty() {
            report.warnings.push(format!(
                "board {} has no column configuration, so it was skipped",
                board.name
            ));
            continue;
        }
        for column in &board.columns {
            let members: Vec<String> = status_of
                .iter()
                .filter(|(_, status)| column.status_ids.contains(status))
                .map(|(key, _)| key.clone())
                .collect();
            if members.is_empty() {
                continue;
            }
            let dir = root
                .join(naming::GROUP_BOARDS)
                .join(naming::group_dir(&board.name, None))
                .join(naming::group_dir(&column.name, None));
            link_all(files, &members, &dir, options.mode, report)?;
        }
        count += 1;
    }
    Ok(count)
}

fn link_all(
    files: &BTreeMap<String, crate::project::FoundFile>,
    keys: &[String],
    dir: &Path,
    mode: LinkMode,
    report: &mut GroupReport,
) -> Result<()> {
    for key in keys {
        let Some(file) = files.get(key) else {
            continue;
        };
        let link = dir.join(&file.relative);
        match links::create(&file.path, &link, mode) {
            Ok(kind) => {
                report.kind.get_or_insert(kind);
                report.links += 1;
            }
            Err(e) => report.warnings.push(format!("{key}: {}", e.message)),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn group_roots_are_the_three_documented_names() {
        assert_eq!(naming::GROUP_PARENT, "by-parent");
        assert_eq!(naming::GROUP_SPRINTS, "sprints");
        assert_eq!(naming::GROUP_BOARDS, "boards");
    }
}

/// An issue whose board link was dragged into a different column.
#[derive(Debug, Clone, Serialize)]
pub struct BoardMove {
    pub key: String,
    pub board: String,
    pub column: String,
    pub from_status: String,
    /// The status the column stands for. Taken from the column's name, which is
    /// what a transition target is matched against.
    pub to_status: String,
}

/// Find issues whose board link no longer sits in the column their status
/// implies (decision D4).
///
/// Moving a card is the natural way to change a status on a board, so the move
/// is staged as a transition and applied by `push` — the same single mutation
/// point as every other change, rather than a second path to the server.
///
/// Directories are read as they are on disk rather than only where a column is
/// expected: a link dropped into a folder that is not a column would otherwise
/// be ignored without a word, and the user would think they had moved it.
pub fn detect_moves(engine: &Engine, root: &Path) -> Result<(Vec<BoardMove>, Vec<String>)> {
    let boards_root = root.join(naming::GROUP_BOARDS);
    let boards = engine.state.boards()?;
    if !boards_root.is_dir() || boards.is_empty() {
        return Ok((Vec::new(), Vec::new()));
    }
    let markup = engine.markup();
    let field_map = engine.state.field_map()?;

    // Where each issue's status says it should be.
    let mut expected: BTreeMap<String, (String, String)> = BTreeMap::new();
    for row in engine.state.issues()? {
        let Some(snapshot) = &row.base else { continue };
        let issue = crate::api::normalize::issue_from_json(&snapshot.raw, markup, &field_map)?;
        if let Some(id) = issue.fields.status_id {
            expected.insert(row.key.clone(), (id, issue.fields.status.clone()));
        }
    }

    let mut moves = Vec::new();
    let mut warnings = Vec::new();

    for board in &boards {
        let board_dir = boards_root.join(naming::group_dir(&board.name, None));
        let Ok(columns_on_disk) = std::fs::read_dir(&board_dir) else {
            continue;
        };
        for column_entry in columns_on_disk.flatten() {
            if !column_entry.path().is_dir() {
                continue;
            }
            let column_name = column_entry.file_name().to_string_lossy().into_owned();
            let column = board
                .columns
                .iter()
                .find(|c| naming::group_dir(&c.name, None) == column_name);

            let Ok(links) = std::fs::read_dir(column_entry.path()) else {
                continue;
            };
            for entry in links.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                let Some(key) = naming::key_from_file_name(&name) else {
                    continue;
                };
                let Some((status_id, status_name)) = expected.get(&key) else {
                    continue;
                };

                let Some(column) = column else {
                    warnings.push(format!(
                        "{key} sits in `{}/{column_name}`, which is not a column on that board, \
                         so nothing was staged",
                        board.name
                    ));
                    continue;
                };
                // Still where it belongs.
                if column.status_ids.contains(status_id) {
                    continue;
                }
                moves.push(BoardMove {
                    key,
                    board: board.name.clone(),
                    column: column.name.clone(),
                    from_status: status_name.clone(),
                    to_status: column.name.clone(),
                });
            }
        }
    }
    moves.sort_by(|a, b| a.key.cmp(&b.key));
    moves.dedup_by(|a, b| a.key == b.key);
    warnings.sort();
    Ok((moves, warnings))
}
