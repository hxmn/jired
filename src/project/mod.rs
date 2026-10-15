//! Filesystem projector [M1.11].
//!
//! Owns every write to the working copy: issue files, renames, and the hidden
//! attachment folders that track them. Nothing else in jired touches those
//! paths, so the rules about identity and renaming live in one place.

pub mod links;
pub mod naming;

use crate::doc::IssueDoc;
use crate::error::{JiredError, Result};
use crate::store::state::content_hash;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// An issue file discovered in the working copy.
#[derive(Debug, Clone)]
pub struct FoundFile {
    pub key: String,
    pub path: PathBuf,
    pub relative: String,
}

#[derive(Debug, Clone, Default)]
pub struct WriteOutcome {
    pub relative: String,
    pub renamed_from: Option<String>,
    pub content_hash: String,
    /// False when the file already had exactly this content.
    pub changed: bool,
}

pub struct Projector {
    root: PathBuf,
}

impl Projector {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Every issue file in the workspace, keyed by issue.
    ///
    /// Only the top level is scanned: grouping folders hold links, not
    /// originals, and treating a link as a second copy would double-count edits.
    pub fn scan(&self) -> Result<BTreeMap<String, FoundFile>> {
        let mut found = BTreeMap::new();
        let entries = match std::fs::read_dir(&self.root) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(found),
            Err(e) => return Err(e.into()),
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if !name.ends_with(".md") || name.starts_with('.') {
                continue;
            }
            let Some(key) = naming::key_from_file_name(name) else {
                continue;
            };
            found.insert(
                key.clone(),
                FoundFile {
                    key,
                    path: path.clone(),
                    relative: name.to_string(),
                },
            );
        }
        Ok(found)
    }

    /// Find one issue's file from a key, a file name, or a path.
    ///
    /// The spelling is normalized first; the case-insensitive sweep after it is
    /// for a file somebody renamed by hand into a different case than the key
    /// it carries.
    pub fn locate(&self, input: &str) -> Result<Option<FoundFile>> {
        let key = naming::key_from_input(input)?;
        let files = self.scan()?;
        if let Some(found) = files.get(&key) {
            return Ok(Some(found.clone()));
        }
        Ok(files
            .into_values()
            .find(|f| f.key.eq_ignore_ascii_case(&key)))
    }

    pub fn path_of(&self, relative: &str) -> PathBuf {
        self.root.join(relative)
    }

    pub fn read(&self, relative: &str) -> Result<IssueDoc> {
        let path = self.path_of(relative);
        let text = std::fs::read_to_string(&path).map_err(crate::error::io_ctx("read", &path))?;
        IssueDoc::parse(&text).map_err(|e| {
            // Name the file: a parse error is almost always one bad edit, and
            // the underlying message does not say which file it was in. The
            // kind is preserved so a hand-edit stays a usage error.
            JiredError::new(e.kind, format!("{}: {}", relative, e.message))
                .with_details(e.details.clone())
                .with_detail("file", relative)
                .with_hint(
                    e.hint
                        .clone()
                        .unwrap_or_else(|| "run `jired pull --force` to restore the file".into()),
                )
        })
    }

    pub fn read_raw(&self, relative: &str) -> Result<String> {
        let path = self.path_of(relative);
        std::fs::read_to_string(&path).map_err(crate::error::io_ctx("read", &path))
    }

    /// Write an issue file, renaming from `previous` when the summary changed.
    ///
    /// The rename moves the attachment folder too, so the two never drift apart.
    pub fn write(&self, doc: &IssueDoc, previous: Option<&str>) -> Result<WriteOutcome> {
        let key = &doc.frontmatter.key;
        let target = naming::file_name(key, &doc.frontmatter.summary);
        let text = doc.render()?;
        let hash = content_hash(&text);

        let mut renamed_from = None;
        if let Some(prev) = previous {
            if prev != target {
                self.rename(prev, &target)?;
                renamed_from = Some(prev.to_string());
            }
        }

        let path = self.path_of(&target);
        let unchanged = std::fs::read_to_string(&path)
            .map(|existing| existing == text)
            .unwrap_or(false);
        if !unchanged {
            std::fs::create_dir_all(&self.root)
                .map_err(crate::error::io_ctx("create", &self.root))?;
            std::fs::write(&path, &text).map_err(crate::error::io_ctx("write", &path))?;
        }

        Ok(WriteOutcome {
            relative: target,
            renamed_from,
            content_hash: hash,
            changed: !unchanged,
        })
    }

    /// Move an issue file and its attachment folder to a new name.
    pub fn rename(&self, from: &str, to: &str) -> Result<()> {
        let from_path = self.path_of(from);
        if from_path.exists() {
            std::fs::rename(&from_path, self.path_of(to))
                .map_err(|e| JiredError::generic(format!("cannot rename {from} to {to}: {e}")))?;
        }
        let from_dir = self.root.join(dot_dir_of(from));
        if from_dir.is_dir() {
            let _ = std::fs::rename(&from_dir, self.root.join(dot_dir_of(to)));
        }
        Ok(())
    }

    /// Remove an issue file and its attachment folder.
    pub fn remove(&self, relative: &str) -> Result<()> {
        let path = self.path_of(relative);
        if path.exists() {
            std::fs::remove_file(&path)?;
        }
        let dir = self.root.join(dot_dir_of(relative));
        if dir.is_dir() {
            std::fs::remove_dir_all(&dir)?;
        }
        Ok(())
    }

    pub fn attachment_dir(&self, relative: &str) -> PathBuf {
        self.root.join(dot_dir_of(relative))
    }

    /// Hash of the file as it currently sits on disk.
    pub fn hash_of(&self, relative: &str) -> Option<String> {
        std::fs::read_to_string(self.path_of(relative))
            .ok()
            .map(|t| content_hash(&t))
    }
}

/// `PROJ-0001. Summary.md` → `.PROJ-0001. Summary`
fn dot_dir_of(file_name: &str) -> String {
    format!(".{}", file_name.strip_suffix(".md").unwrap_or(file_name))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doc::{Frontmatter, SyncMeta};

    fn doc(key: &str, summary: &str) -> IssueDoc {
        IssueDoc {
            frontmatter: Frontmatter {
                key: key.into(),
                id: "1".into(),
                summary: summary.into(),
                jired: SyncMeta::default(),
                ..Default::default()
            },
            heading: summary.into(),
            preamble: String::new(),
            description: "body".into(),
            checklist: vec![],
            children: vec![],
            comments: vec![],
            worklogs: Vec::new(),
            attachments: vec![],
            extra_sections: vec![],
        }
    }

    #[test]
    fn writes_and_reads_back() {
        let dir = tempfile::tempdir().unwrap();
        let p = Projector::new(dir.path());
        let out = p.write(&doc("PROJ-1", "First task"), None).unwrap();
        assert_eq!(out.relative, "PROJ-0001. First task.md");
        assert!(out.changed);

        let back = p.read(&out.relative).unwrap();
        assert_eq!(back.frontmatter.key, "PROJ-1");
        assert_eq!(back.description, "body");
    }

    #[test]
    fn rewriting_identical_content_is_a_no_op() {
        let dir = tempfile::tempdir().unwrap();
        let p = Projector::new(dir.path());
        let d = doc("PROJ-1", "First task");
        let first = p.write(&d, None).unwrap();
        let modified = std::fs::metadata(p.path_of(&first.relative))
            .unwrap()
            .modified()
            .unwrap();

        let second = p.write(&d, Some(&first.relative)).unwrap();
        assert!(!second.changed, "an unchanged issue must not be rewritten");
        assert_eq!(
            modified,
            std::fs::metadata(p.path_of(&second.relative))
                .unwrap()
                .modified()
                .unwrap()
        );
    }

    #[test]
    fn a_summary_change_renames_the_file_and_its_attachment_folder() {
        let dir = tempfile::tempdir().unwrap();
        let p = Projector::new(dir.path());
        let first = p.write(&doc("PROJ-1", "Old name"), None).unwrap();

        let att = p.attachment_dir(&first.relative);
        std::fs::create_dir_all(&att).unwrap();
        std::fs::write(att.join("trace.har"), b"x").unwrap();

        let second = p
            .write(&doc("PROJ-1", "New name"), Some(&first.relative))
            .unwrap();
        assert_eq!(second.relative, "PROJ-0001. New name.md");
        assert_eq!(
            second.renamed_from.as_deref(),
            Some("PROJ-0001. Old name.md")
        );
        assert!(!p.path_of(&first.relative).exists());
        assert!(
            p.attachment_dir(&second.relative)
                .join("trace.har")
                .exists(),
            "attachments must follow the rename"
        );
    }

    #[test]
    fn scan_finds_issues_by_key_prefix_and_ignores_everything_else() {
        let dir = tempfile::tempdir().unwrap();
        let p = Projector::new(dir.path());
        p.write(&doc("PROJ-1", "One"), None).unwrap();
        p.write(&doc("PROJ-2", "Two"), None).unwrap();
        std::fs::write(dir.path().join("README.md"), "notes").unwrap();
        std::fs::write(dir.path().join("CLAUDE.md"), "docs").unwrap();
        std::fs::create_dir_all(dir.path().join("sprints/Sprint 42")).unwrap();

        let found = p.scan().unwrap();
        assert_eq!(found.keys().collect::<Vec<_>>(), vec!["PROJ-1", "PROJ-2"]);
    }

    #[test]
    fn a_hand_renamed_file_is_still_found_by_its_key() {
        let dir = tempfile::tempdir().unwrap();
        let p = Projector::new(dir.path());
        let out = p.write(&doc("PROJ-1", "One"), None).unwrap();
        std::fs::rename(
            p.path_of(&out.relative),
            dir.path().join("PROJ-1. renamed by hand.md"),
        )
        .unwrap();

        let found = p.scan().unwrap();
        assert_eq!(found["PROJ-1"].relative, "PROJ-1. renamed by hand.md");
    }

    #[test]
    fn remove_takes_the_attachment_folder_with_it() {
        let dir = tempfile::tempdir().unwrap();
        let p = Projector::new(dir.path());
        let out = p.write(&doc("PROJ-1", "One"), None).unwrap();
        std::fs::create_dir_all(p.attachment_dir(&out.relative)).unwrap();

        p.remove(&out.relative).unwrap();
        assert!(!p.path_of(&out.relative).exists());
        assert!(!p.attachment_dir(&out.relative).exists());
    }

    #[test]
    fn scanning_a_missing_directory_is_empty_not_an_error() {
        let p = Projector::new("/nonexistent/jired/workspace");
        assert!(p.scan().unwrap().is_empty());
    }
}
