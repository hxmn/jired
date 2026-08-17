//! TOML config files, edited in place so user comments survive `config set`.

use crate::config::keys;
use crate::error::{JiredError, Result};
use std::path::{Path, PathBuf};
use toml_edit::{DocumentMut, Item, Table, Value};

#[derive(Debug, Clone)]
pub struct ConfigFile {
    doc: DocumentMut,
    path: Option<PathBuf>,
}

impl ConfigFile {
    pub fn empty() -> Self {
        Self {
            doc: DocumentMut::new(),
            path: None,
        }
    }

    pub fn from_toml(text: &str) -> Result<Self> {
        let doc: DocumentMut = text.parse().map_err(|e: toml_edit::TomlError| {
            JiredError::usage(format!("invalid config TOML: {e}"))
        })?;
        Ok(Self { doc, path: None })
    }

    /// Load a config file; a missing file is an empty config, not an error.
    pub fn load(path: &Path) -> Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(text) => {
                let mut cfg = Self::from_toml(&text)
                    .map_err(|e| JiredError::usage(format!("{}: {}", path.display(), e.message)))?;
                cfg.path = Some(path.to_path_buf());
                Ok(cfg)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let mut cfg = Self::empty();
                cfg.path = Some(path.to_path_buf());
                Ok(cfg)
            }
            Err(e) => Err(JiredError::generic(format!(
                "cannot read {}: {e}",
                path.display()
            ))),
        }
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// Read a dotted key such as `server.url`, stringifying scalars.
    pub fn get(&self, key: &str) -> Option<String> {
        let mut item: &Item = self.doc.as_item();
        for part in key.split('.') {
            item = item.get(part)?;
        }
        match item {
            Item::Value(Value::String(s)) => Some(s.value().clone()),
            Item::Value(Value::Integer(i)) => Some(i.value().to_string()),
            Item::Value(Value::Boolean(b)) => Some(b.value().to_string()),
            Item::Value(Value::Float(f)) => Some(f.value().to_string()),
            _ => None,
        }
    }

    /// Write a dotted key, creating intermediate tables.
    ///
    /// Refuses secrets outright: a token in a plaintext config file would defeat
    /// the whole keychain design.
    pub fn set(&mut self, key: &str, value: &str) -> Result<()> {
        if keys::is_secret(key) {
            return Err(JiredError::usage(format!(
                "`{key}` is a secret and is never stored in config files"
            ))
            .with_detail("key", key)
            .with_hint("run `jired init` to store credentials in the keychain"));
        }
        let parts: Vec<&str> = key.split('.').collect();
        let (last, tables) = parts.split_last().expect("non-empty key");
        let mut item: &mut Item = self.doc.as_item_mut();
        for part in tables {
            if item.get(part).is_none() {
                item[*part] = Item::Table(Table::new());
            }
            item = item.get_mut(part).expect("just inserted");
        }
        item[*last] = Item::Value(parse_scalar(value));
        Ok(())
    }

    pub fn unset(&mut self, key: &str) -> bool {
        let parts: Vec<&str> = key.split('.').collect();
        let (last, tables) = parts.split_last().expect("non-empty key");
        let mut item: &mut Item = self.doc.as_item_mut();
        for part in tables {
            match item.get_mut(part) {
                Some(next) => item = next,
                None => return false,
            }
        }
        match item.as_table_like_mut() {
            Some(t) => t.remove(last).is_some(),
            None => false,
        }
    }

    pub fn to_toml(&self) -> String {
        self.doc.to_string()
    }

    pub fn save(&self) -> Result<()> {
        let path = self
            .path
            .as_ref()
            .ok_or_else(|| JiredError::generic("config file has no path"))?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, self.to_toml())?;
        Ok(())
    }

    pub fn save_to(&mut self, path: &Path) -> Result<()> {
        self.path = Some(path.to_path_buf());
        self.save()
    }
}

/// Preserve types that round-trip: booleans and integers stay scalars.
fn parse_scalar(value: &str) -> Value {
    match value {
        "true" => return Value::from(true),
        "false" => return Value::from(false),
        _ => {}
    }
    if let Ok(i) = value.parse::<i64>() {
        return Value::from(i);
    }
    Value::from(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_dotted_keys() {
        let mut cfg = ConfigFile::empty();
        cfg.set("server.url", "https://x.atlassian.net").unwrap();
        cfg.set("workspace.project", "PROJ").unwrap();
        assert_eq!(
            cfg.get("server.url").as_deref(),
            Some("https://x.atlassian.net")
        );
        let reparsed = ConfigFile::from_toml(&cfg.to_toml()).unwrap();
        assert_eq!(reparsed.get("workspace.project").as_deref(), Some("PROJ"));
    }

    #[test]
    fn preserves_comments_on_edit() {
        let src = "# my workspace\n[server]\nurl = \"https://a\"  # inline\n";
        let mut cfg = ConfigFile::from_toml(src).unwrap();
        cfg.set("workspace.project", "PROJ").unwrap();
        let out = cfg.to_toml();
        assert!(out.contains("# my workspace"));
        assert!(out.contains("# inline"));
    }

    #[test]
    fn refuses_to_store_secrets() {
        let mut cfg = ConfigFile::empty();
        let err = cfg.set("auth.token", "abc123").unwrap_err();
        assert_eq!(err.exit_code().as_i32(), 2);
        assert!(!cfg.to_toml().contains("abc123"));
    }

    #[test]
    fn typed_scalars_survive() {
        let mut cfg = ConfigFile::empty();
        cfg.set("tls.insecure", "true").unwrap();
        cfg.set("attachments.max_upload", "1024").unwrap();
        let toml = cfg.to_toml();
        assert!(toml.contains("insecure = true"), "{toml}");
        assert!(toml.contains("max_upload = 1024"), "{toml}");
    }

    #[test]
    fn missing_file_loads_as_empty() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = ConfigFile::load(&dir.path().join("nope.toml")).unwrap();
        assert!(cfg.get("server.url").is_none());
    }

    #[test]
    fn unset_removes_key() {
        let mut cfg = ConfigFile::from_toml("[server]\nurl = \"https://a\"\n").unwrap();
        assert!(cfg.unset("server.url"));
        assert!(cfg.get("server.url").is_none());
        assert!(!cfg.unset("server.url"));
    }
}
