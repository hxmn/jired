//! Credentials kept in the user configuration directory [M1.5, D11].
//!
//! `$XDG_CONFIG_HOME/jired/credentials.toml` (`~/Library/Application Support` and
//! `%APPDATA%` on the other platforms), keyed by the same `host:identity` string
//! the keychain entry uses. That key is what makes this backend worth having:
//! ten workspaces pointing at one Jira share one credential, so re-authenticating
//! once is enough for all of them.
//!
//! The file holds whatever `auth::store` sealed — an encrypted blob when
//! `JIRED_SECRET_KEY` is set, the raw token only when the user explicitly asked
//! for that with `--insecure-store`. It is never `config.toml`: that file is
//! meant to be readable, diffable and occasionally shared, and a secret in it
//! would eventually be pasted into a bug report.

use crate::error::{JiredError, Result};
use std::path::{Path, PathBuf};
use toml_edit::{value, DocumentMut, Item, Table};

pub const FILE: &str = "credentials.toml";

const HEADER: &str = "\
# jired credentials. Written by `jired init`; never commit or share this file.
# Each table is one account, keyed by host and identity. `secret` is hex-encoded
# and, when `kdf` is present, encrypted with the passphrase in JIRED_SECRET_KEY.
";

/// What one account's entry holds.
pub struct Entry {
    pub blob: Vec<u8>,
    /// Encryption parameters; absent means the blob is the raw token.
    pub kdf: Option<String>,
}

pub fn path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("jired").join(FILE))
}

fn require_path() -> Result<PathBuf> {
    path().ok_or_else(|| {
        JiredError::auth("this system has no user configuration directory")
            .with_hint("set creds.storage to local_db to keep the credential in the workspace")
    })
}

fn read(path: &Path) -> Result<DocumentMut> {
    match std::fs::read_to_string(path) {
        Ok(text) => text.parse::<DocumentMut>().map_err(|e| {
            JiredError::auth(format!("{} is not valid TOML: {e}", path.display()))
                .with_hint("fix or delete the file, then run `jired init --force`")
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(DocumentMut::new()),
        Err(e) => Err(JiredError::auth(format!(
            "cannot read {}: {e}",
            path.display()
        ))),
    }
}

/// Write one account's secret, leaving every other entry untouched.
pub fn save(name: &str, blob: &[u8], kdf: Option<&str>) -> Result<PathBuf> {
    let path = require_path()?;
    let mut doc = read(&path)?;

    let mut table = Table::new();
    table.insert("secret", value(hex::encode(blob)));
    if let Some(kdf) = kdf {
        table.insert("kdf", value(kdf));
    }
    doc[name] = Item::Table(table);

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
        // The directory holds credentials even if other tools put files here.
        let _ = crate::workspace::restrict_dir_mode(parent);
    }
    std::fs::write(&path, render(&doc))
        .map_err(|e| JiredError::auth(format!("cannot write {}: {e}", path.display())))?;
    crate::workspace::restrict_file_mode(&path)?;
    Ok(path)
}

pub fn load(name: &str) -> Result<Option<Entry>> {
    let path = require_path()?;
    if !path.is_file() {
        return Ok(None);
    }
    // A credential file readable by other users is a finding, not a detail.
    if let Ok(true) = crate::workspace::restrict_file_mode(&path) {
        tracing::warn!("fixed permissions on {} (now 0600)", path.display());
    }
    let doc = read(&path)?;
    let Some(entry) = doc.get(name).and_then(|i| i.as_table_like()) else {
        return Ok(None);
    };
    let Some(secret) = entry.get("secret").and_then(|v| v.as_str()) else {
        return Ok(None);
    };
    let blob = hex::decode(secret).map_err(|_| {
        JiredError::auth(format!(
            "the credential for {name} in {} is corrupt",
            path.display()
        ))
        .with_hint("run `jired init --force` to store it again")
    })?;
    Ok(Some(Entry {
        blob,
        kdf: entry
            .get("kdf")
            .and_then(|v| v.as_str())
            .map(str::to_string),
    }))
}

/// Best-effort removal; a missing file or entry is not an error.
pub fn remove(name: &str) {
    let Some(path) = path() else { return };
    if !path.is_file() {
        return;
    }
    let Ok(mut doc) = read(&path) else { return };
    if doc.as_table_mut().remove(name).is_none() {
        return;
    }
    let _ = std::fs::write(&path, render(&doc));
}

/// Render the file with exactly one copy of the header.
///
/// Comments at the top of a TOML file come back as decoration attached to the
/// first entry, so removing that entry would otherwise take the header with it,
/// and re-adding it blindly would duplicate it.
fn render(doc: &DocumentMut) -> String {
    let text = doc.to_string();
    let body: Vec<&str> = text
        .lines()
        .skip_while(|l| l.trim_start().starts_with('#') || l.trim().is_empty())
        .collect();
    let mut out = String::from(HEADER);
    if !body.is_empty() {
        out.push('\n');
        out.push_str(&body.join("\n"));
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `dirs::config_dir()` follows `XDG_CONFIG_HOME` on Linux, which is how the
    /// tests stay out of the developer's real credential file.
    #[test]
    #[cfg(target_os = "linux")]
    fn entries_round_trip_without_disturbing_each_other() {
        let dir = tempfile::tempdir().unwrap();
        temp_env_config_home(dir.path(), || {
            save("a.example:me", b"\x01\x02\x03", Some("{\"salt\":\"ab\"}")).unwrap();
            save("b.example:you", b"\x04", None).unwrap();

            let a = load("a.example:me").unwrap().unwrap();
            assert_eq!(a.blob, vec![1, 2, 3]);
            assert_eq!(a.kdf.as_deref(), Some("{\"salt\":\"ab\"}"));

            let b = load("b.example:you").unwrap().unwrap();
            assert_eq!(b.blob, vec![4]);
            assert!(b.kdf.is_none(), "a plaintext entry has no kdf");

            assert!(load("missing").unwrap().is_none());

            remove("a.example:me");
            assert!(load("a.example:me").unwrap().is_none());
            assert!(
                load("b.example:you").unwrap().is_some(),
                "removing one entry must not disturb another"
            );

            let text = std::fs::read_to_string(path().unwrap()).unwrap();
            assert!(text.starts_with("# jired credentials"), "{text}");
        });
    }

    /// The file holds secrets, so it must not be readable by anyone else.
    #[test]
    #[cfg(target_os = "linux")]
    fn the_file_is_created_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        temp_env_config_home(dir.path(), || {
            let written = save("host:me", b"tok", None).unwrap();
            let mode = std::fs::metadata(&written).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "{written:?}");
        });
    }

    /// Environment mutation is process-wide; keep it in one place and restore it.
    #[cfg(target_os = "linux")]
    fn temp_env_config_home(dir: &Path, f: impl FnOnce()) {
        use std::sync::Mutex;
        static LOCK: Mutex<()> = Mutex::new(());
        let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let previous = std::env::var_os("XDG_CONFIG_HOME");
        // Safety-equivalent concern to any env test: serialized by LOCK above.
        std::env::set_var("XDG_CONFIG_HOME", dir);
        f();
        match previous {
            Some(v) => std::env::set_var("XDG_CONFIG_HOME", v),
            None => std::env::remove_var("XDG_CONFIG_HOME"),
        }
    }
}
