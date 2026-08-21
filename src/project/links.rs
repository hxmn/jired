//! Cross-platform links for grouping folders [M4.1].
//!
//! Grouping folders hold pointers to issue files, never copies. Symlinks are
//! the obvious mechanism but Windows only grants them with Developer Mode or
//! elevation, so the projector falls back rather than failing: a hard link
//! where the filesystem allows one, and otherwise a small Markdown stub that a
//! human can still follow and `doctor --regroup` can still rebuild.

use crate::error::{JiredError, Result};
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum LinkMode {
    /// Try symlink, then hard link, then stub.
    Auto,
    Symlink,
    Hardlink,
    Stub,
}

impl LinkMode {
    pub fn parse(value: &str) -> Result<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "auto" => Ok(LinkMode::Auto),
            "symlink" | "symbolic" => Ok(LinkMode::Symlink),
            "hardlink" | "hard" => Ok(LinkMode::Hardlink),
            "stub" | "file" => Ok(LinkMode::Stub),
            other => Err(JiredError::usage(format!(
                "unknown links.mode `{other}` (expected auto, symlink, hardlink, or stub)"
            ))),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            LinkMode::Auto => "auto",
            LinkMode::Symlink => "symlink",
            LinkMode::Hardlink => "hardlink",
            LinkMode::Stub => "stub",
        }
    }
}

/// How a particular link ended up being represented.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum LinkKind {
    Symlink,
    Hardlink,
    Stub,
}

/// Create `link` pointing at `target`, replacing whatever was there.
///
/// `target` and `link` are both absolute; the symlink written is relative so
/// the workspace survives being moved or mounted elsewhere.
pub fn create(target: &Path, link: &Path, mode: LinkMode) -> Result<LinkKind> {
    if let Some(parent) = link.parent() {
        std::fs::create_dir_all(parent)?;
    }
    remove(link)?;

    let relative = relative_from(link.parent().unwrap_or(Path::new(".")), target);

    match mode {
        LinkMode::Symlink => symlink(&relative, link).map(|()| LinkKind::Symlink),
        LinkMode::Hardlink => hardlink(target, link).map(|()| LinkKind::Hardlink),
        LinkMode::Stub => write_stub(target, link, &relative).map(|()| LinkKind::Stub),
        LinkMode::Auto => {
            if symlink(&relative, link).is_ok() {
                return Ok(LinkKind::Symlink);
            }
            if hardlink(target, link).is_ok() {
                return Ok(LinkKind::Hardlink);
            }
            write_stub(target, link, &relative).map(|()| LinkKind::Stub)
        }
    }
}

/// Remove a link of any of the three kinds.
///
/// `symlink_metadata` rather than `exists`, because a symlink whose target is
/// gone reports as non-existent and would otherwise be left behind forever.
pub fn remove(link: &Path) -> Result<()> {
    match std::fs::symlink_metadata(link) {
        Ok(meta) if meta.is_dir() => Ok(std::fs::remove_dir_all(link)?),
        Ok(_) => Ok(std::fs::remove_file(link)?),
        Err(_) => Ok(()),
    }
}

/// True when the path is one of jired's stub files rather than real content.
pub fn is_stub_file(path: &Path) -> bool {
    let Ok(text) = std::fs::read_to_string(path) else {
        return false;
    };
    text.starts_with("---\nlink:")
}

/// The issue file a stub points at, relative to the stub's own directory.
pub fn stub_target(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    text.lines()
        .find_map(|l| l.strip_prefix("link: "))
        .map(|t| t.trim().trim_matches('\'').trim_matches('"').to_string())
}

#[cfg(unix)]
fn symlink(target: &Path, link: &Path) -> Result<()> {
    std::os::unix::fs::symlink(target, link)
        .map_err(|e| JiredError::generic(format!("cannot link {}: {e}", link.display())))
}

#[cfg(windows)]
fn symlink(target: &Path, link: &Path) -> Result<()> {
    // Windows needs to know whether the target is a file or a directory, and
    // refuses without Developer Mode or elevation — which is exactly why the
    // caller has a fallback.
    std::os::windows::fs::symlink_file(target, link)
        .map_err(|e| JiredError::generic(format!("cannot link {}: {e}", link.display())))
}

fn hardlink(target: &Path, link: &Path) -> Result<()> {
    std::fs::hard_link(target, link)
        .map_err(|e| JiredError::generic(format!("cannot hard-link {}: {e}", link.display())))
}

/// A stub is a real Markdown file, so it stays readable in any editor even
/// where neither kind of link is available.
fn write_stub(target: &Path, link: &Path, relative: &Path) -> Result<()> {
    let name = target
        .file_stem()
        .and_then(|n| n.to_str())
        .unwrap_or_default();
    let rel = relative.to_string_lossy().replace('\\', "/");
    std::fs::write(
        link,
        format!("---\nlink: '{rel}'\n---\n\n# {name}\n\n[Open the issue]({rel})\n"),
    )?;
    Ok(())
}

/// Path from `base` to `target`, walking up as far as needed.
pub fn relative_from(base: &Path, target: &Path) -> std::path::PathBuf {
    let base: Vec<_> = base.components().collect();
    let target: Vec<_> = target.components().collect();
    let shared = base
        .iter()
        .zip(target.iter())
        .take_while(|(a, b)| a == b)
        .count();

    let mut out = std::path::PathBuf::new();
    for _ in shared..base.len() {
        out.push("..");
    }
    for component in &target[shared..] {
        out.push(component.as_os_str());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workspace() -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let issue = dir.path().join("PROJ-1. Task.md");
        std::fs::write(&issue, "---\nkey: PROJ-1\n---\n\n# Task\n").unwrap();
        (dir, issue)
    }

    #[test]
    fn mode_parsing() {
        assert_eq!(LinkMode::parse("auto").unwrap(), LinkMode::Auto);
        assert_eq!(LinkMode::parse("SYMLINK").unwrap(), LinkMode::Symlink);
        assert_eq!(LinkMode::parse("hard").unwrap(), LinkMode::Hardlink);
        assert_eq!(LinkMode::parse("stub").unwrap(), LinkMode::Stub);
        assert_eq!(LinkMode::parse("nope").unwrap_err().exit_code().as_i32(), 2);
    }

    #[test]
    fn relative_paths_walk_up_out_of_the_group_folder() {
        assert_eq!(
            relative_from(
                Path::new("/ws/sprints/Sprint 42"),
                Path::new("/ws/PROJ-1. x.md")
            ),
            Path::new("../../PROJ-1. x.md")
        );
        assert_eq!(
            relative_from(Path::new("/ws"), Path::new("/ws/PROJ-1. x.md")),
            Path::new("PROJ-1. x.md")
        );
    }

    #[test]
    fn every_mode_produces_something_that_resolves_to_the_issue() {
        let (dir, issue) = workspace();
        for (mode, expected) in [
            (LinkMode::Symlink, LinkKind::Symlink),
            (LinkMode::Hardlink, LinkKind::Hardlink),
            (LinkMode::Stub, LinkKind::Stub),
        ] {
            let link = dir.path().join("sprints/Sprint 42/PROJ-1. Task.md");
            let kind = create(&issue, &link, mode).unwrap();
            assert_eq!(kind, expected);
            assert!(
                std::fs::symlink_metadata(&link).is_ok(),
                "{mode:?} produced nothing"
            );

            match kind {
                // A symlink or hard link reads as the issue itself.
                LinkKind::Symlink | LinkKind::Hardlink => {
                    assert!(std::fs::read_to_string(&link)
                        .unwrap()
                        .contains("key: PROJ-1"));
                }
                // A stub names the issue and points at it.
                LinkKind::Stub => {
                    let text = std::fs::read_to_string(&link).unwrap();
                    assert!(text.contains("link: '../../PROJ-1. Task.md'"), "{text}");
                    assert!(is_stub_file(&link));
                    assert_eq!(stub_target(&link).as_deref(), Some("../../PROJ-1. Task.md"));
                }
            }
        }
    }

    #[test]
    fn auto_falls_back_rather_than_failing() {
        let (dir, issue) = workspace();
        let link = dir.path().join("by-parent/PROJ-9/PROJ-1. Task.md");
        // On unix this lands on symlink; the point is that it always lands
        // somewhere and the result is usable.
        let kind = create(&issue, &link, LinkMode::Auto).unwrap();
        assert!(matches!(
            kind,
            LinkKind::Symlink | LinkKind::Hardlink | LinkKind::Stub
        ));
        assert!(std::fs::symlink_metadata(&link).is_ok());
    }

    #[test]
    fn creating_over_an_existing_link_replaces_it() {
        let (dir, issue) = workspace();
        let link = dir.path().join("sprints/S/PROJ-1. Task.md");
        create(&issue, &link, LinkMode::Stub).unwrap();
        create(&issue, &link, LinkMode::Symlink).unwrap();
        assert!(std::fs::symlink_metadata(&link).unwrap().is_symlink());
    }

    #[test]
    fn a_dangling_symlink_is_still_removable() {
        let (dir, issue) = workspace();
        let link = dir.path().join("sprints/S/PROJ-1. Task.md");
        create(&issue, &link, LinkMode::Symlink).unwrap();
        std::fs::remove_file(&issue).unwrap();

        // `exists()` is false for a broken symlink, so a naive check would
        // leave it behind for good.
        assert!(!link.exists());
        remove(&link).unwrap();
        assert!(std::fs::symlink_metadata(&link).is_err());
    }

    #[test]
    fn a_real_issue_file_is_not_mistaken_for_a_stub() {
        let (_dir, issue) = workspace();
        assert!(!is_stub_file(&issue));
        assert!(stub_target(&issue).is_none());
    }
}
