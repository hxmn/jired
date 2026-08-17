//! Workspace layout, the run lock, and `.gitignore` upkeep [M1.4].
//!
//! All machine state lives in `.jired/` at the workspace root (decision D1):
//! one gitignore entry, one 0700 boundary, and nothing scattered among the
//! user's issue files.

use crate::error::{JiredError, Result};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

pub const STATE_DIR: &str = ".jired";
pub const CONFIG_FILE: &str = "config.toml";
pub const SESSION_DB: &str = "session.db";
pub const STATE_DB: &str = "state.db";
pub const LOCK_FILE: &str = "lock";
pub const LOGS_DIR: &str = "logs";
pub const TEMPLATES_DIR: &str = "templates";

const GITIGNORE_BEGIN: &str = "# BEGIN jired";
const GITIGNORE_END: &str = "# END jired";

#[derive(Debug, Clone)]
pub struct Workspace {
    root: PathBuf,
}

impl Workspace {
    /// A workspace rooted at `root`, whether or not it is initialized yet.
    pub fn at(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Find the workspace containing `start` by walking up to the first
    /// directory holding `.jired/`, mirroring how git discovers a repository.
    pub fn discover(start: &Path) -> Result<Self> {
        let start = if start.is_absolute() {
            start.to_path_buf()
        } else {
            std::env::current_dir()?.join(start)
        };
        let mut dir = start.as_path();
        loop {
            if dir.join(STATE_DIR).is_dir() {
                return Ok(Self::at(dir));
            }
            match dir.parent() {
                Some(p) => dir = p,
                None => {
                    return Err(JiredError::usage(format!(
                        "no jired workspace found in {} or any parent directory",
                        start.display()
                    ))
                    .with_hint("run `jired init` to create one"))
                }
            }
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
    pub fn state_dir(&self) -> PathBuf {
        self.root.join(STATE_DIR)
    }
    pub fn config_path(&self) -> PathBuf {
        self.state_dir().join(CONFIG_FILE)
    }
    pub fn session_db_path(&self) -> PathBuf {
        self.state_dir().join(SESSION_DB)
    }
    pub fn state_db_path(&self) -> PathBuf {
        self.state_dir().join(STATE_DB)
    }
    pub fn lock_path(&self) -> PathBuf {
        self.state_dir().join(LOCK_FILE)
    }
    pub fn logs_dir(&self) -> PathBuf {
        self.state_dir().join(LOGS_DIR)
    }
    pub fn templates_dir(&self) -> PathBuf {
        self.state_dir().join(TEMPLATES_DIR)
    }

    pub fn is_initialized(&self) -> bool {
        self.state_dir().is_dir()
    }

    /// Create `.jired/` and its subdirectories, restricting the directory mode
    /// on unix because it holds credentials.
    pub fn create(&self) -> Result<()> {
        let dir = self.state_dir();
        fs::create_dir_all(&dir)?;
        fs::create_dir_all(self.logs_dir())?;
        fs::create_dir_all(self.templates_dir())?;
        restrict_dir_mode(&dir)?;
        Ok(())
    }

    /// Take the exclusive run lock; only one jired may mutate a workspace.
    pub fn lock(&self) -> Result<WorkspaceLock> {
        WorkspaceLock::acquire(&self.lock_path())
    }

    /// Ensure `.gitignore` ignores `.jired/`, editing only our sentinel block.
    ///
    /// Returns true when the file was changed, so `init` can report it.
    pub fn ensure_gitignore(&self) -> Result<bool> {
        let path = self.root.join(".gitignore");
        let existing = fs::read_to_string(&path).unwrap_or_default();
        let block = format!(
            "{GITIGNORE_BEGIN}\n\
             # jired machine state: credentials, sync database, logs.\n\
             /{STATE_DIR}/\n\
             {GITIGNORE_END}\n"
        );

        let updated = match (existing.find(GITIGNORE_BEGIN), existing.find(GITIGNORE_END)) {
            (Some(start), Some(end)) if end > start => {
                let end = end + GITIGNORE_END.len();
                let end = existing[end..]
                    .find('\n')
                    .map(|n| end + n + 1)
                    .unwrap_or(existing.len());
                let mut s = String::with_capacity(existing.len());
                s.push_str(&existing[..start]);
                s.push_str(&block);
                s.push_str(&existing[end..]);
                s
            }
            _ => {
                let mut s = existing.clone();
                if !s.is_empty() && !s.ends_with('\n') {
                    s.push('\n');
                }
                if !s.is_empty() {
                    s.push('\n');
                }
                s.push_str(&block);
                s
            }
        };

        if updated == existing {
            return Ok(false);
        }
        fs::write(&path, updated)?;
        Ok(true)
    }
}

/// Advisory exclusive lock over a workspace, released on drop.
#[derive(Debug)]
pub struct WorkspaceLock {
    file: File,
    path: PathBuf,
}

impl WorkspaceLock {
    fn acquire(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(path)?;

        if let Err(reason) = file.try_lock() {
            // The holder records its pid; a dead pid means a crashed run, and
            // the OS has already released the advisory lock for us, so a stale
            // lock *file* never blocks anyone.
            let mut err =
                JiredError::locked("another jired process is using this workspace".to_string())
                    .with_hint("wait for it to finish, or remove .jired/lock if it crashed")
                    .with_detail("reason", reason.to_string());
            if let Some((pid, since)) = read_holder(path) {
                err = err.with_detail("pid", pid).with_detail("since", since);
            }
            return Err(err);
        }

        let mut f = &file;
        f.set_len(0)?;
        let _ = write!(
            f,
            "{}\n{}\n",
            std::process::id(),
            chrono::Utc::now().to_rfc3339()
        );
        let _ = f.flush();

        Ok(Self {
            file,
            path: path.to_path_buf(),
        })
    }
}

impl Drop for WorkspaceLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
        let _ = fs::remove_file(&self.path);
    }
}

fn read_holder(path: &Path) -> Option<(u32, String)> {
    let mut buf = String::new();
    File::open(path).ok()?.read_to_string(&mut buf).ok()?;
    let mut lines = buf.lines();
    let pid = lines.next()?.trim().parse().ok()?;
    let since = lines.next().unwrap_or("").trim().to_string();
    Some((pid, since))
}

#[cfg(unix)]
pub fn restrict_dir_mode(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = fs::metadata(path)?.permissions();
    perms.set_mode(0o700);
    fs::set_permissions(path, perms)?;
    Ok(())
}

#[cfg(not(unix))]
pub fn restrict_dir_mode(_path: &Path) -> Result<()> {
    Ok(())
}

/// Force 0600 on a file holding secrets, reporting whether it had to be fixed.
#[cfg(unix)]
pub fn restrict_file_mode(path: &Path) -> Result<bool> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = fs::metadata(path)?.permissions();
    let mode = perms.mode() & 0o777;
    if mode == 0o600 {
        return Ok(false);
    }
    perms.set_mode(0o600);
    fs::set_permissions(path, perms)?;
    Ok(true)
}

#[cfg(not(unix))]
pub fn restrict_file_mode(_path: &Path) -> Result<bool> {
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_makes_layout_and_restricts_mode() {
        let dir = tempfile::tempdir().unwrap();
        let ws = Workspace::at(dir.path());
        ws.create().unwrap();
        assert!(ws.state_dir().is_dir());
        assert!(ws.logs_dir().is_dir());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(ws.state_dir()).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o700);
        }
    }

    #[test]
    fn discover_walks_up_to_the_root() {
        let dir = tempfile::tempdir().unwrap();
        let ws = Workspace::at(dir.path());
        ws.create().unwrap();
        let nested = dir.path().join("sprints").join("Sprint 42");
        fs::create_dir_all(&nested).unwrap();
        let found = Workspace::discover(&nested).unwrap();
        assert_eq!(
            found.root().canonicalize().unwrap(),
            dir.path().canonicalize().unwrap()
        );
    }

    #[test]
    fn discover_without_workspace_is_a_usage_error() {
        let dir = tempfile::tempdir().unwrap();
        let err = Workspace::discover(dir.path()).unwrap_err();
        assert_eq!(err.exit_code().as_i32(), 2);
    }

    #[test]
    fn second_lock_holder_is_rejected_with_exit_8() {
        let dir = tempfile::tempdir().unwrap();
        let ws = Workspace::at(dir.path());
        ws.create().unwrap();
        let _held = ws.lock().unwrap();
        let err = ws.lock().unwrap_err();
        assert_eq!(err.exit_code().as_i32(), 8);
        assert_eq!(err.details["pid"], std::process::id());
    }

    #[test]
    fn lock_is_released_on_drop() {
        let dir = tempfile::tempdir().unwrap();
        let ws = Workspace::at(dir.path());
        ws.create().unwrap();
        drop(ws.lock().unwrap());
        let _second = ws.lock().expect("lock should be free after drop");
    }

    #[test]
    fn stale_lock_file_from_a_dead_process_is_reusable() {
        // A crashed run leaves the file behind but not the advisory lock.
        let dir = tempfile::tempdir().unwrap();
        let ws = Workspace::at(dir.path());
        ws.create().unwrap();
        fs::write(ws.lock_path(), "999999\n2020-01-01T00:00:00Z\n").unwrap();
        let _lock = ws.lock().expect("stale lock file must not block");
    }

    #[test]
    fn gitignore_block_is_idempotent_and_preserves_user_lines() {
        let dir = tempfile::tempdir().unwrap();
        let ws = Workspace::at(dir.path());
        ws.create().unwrap();
        fs::write(dir.path().join(".gitignore"), "target/\n").unwrap();

        assert!(ws.ensure_gitignore().unwrap());
        let first = fs::read_to_string(dir.path().join(".gitignore")).unwrap();
        assert!(first.starts_with("target/\n"));
        assert!(first.contains("/.jired/"));

        assert!(
            !ws.ensure_gitignore().unwrap(),
            "second run must be a no-op"
        );
        assert_eq!(
            first,
            fs::read_to_string(dir.path().join(".gitignore")).unwrap()
        );
        assert_eq!(first.matches(GITIGNORE_BEGIN).count(), 1);
    }

    #[test]
    fn gitignore_block_is_rewritten_in_place_when_stale() {
        let dir = tempfile::tempdir().unwrap();
        let ws = Workspace::at(dir.path());
        ws.create().unwrap();
        fs::write(
            dir.path().join(".gitignore"),
            format!("a/\n{GITIGNORE_BEGIN}\nold-entry\n{GITIGNORE_END}\nb/\n"),
        )
        .unwrap();
        assert!(ws.ensure_gitignore().unwrap());
        let text = fs::read_to_string(dir.path().join(".gitignore")).unwrap();
        assert!(text.contains("a/\n"));
        assert!(text.contains("b/\n"));
        assert!(!text.contains("old-entry"));
        assert_eq!(text.matches(GITIGNORE_BEGIN).count(), 1);
    }
}
