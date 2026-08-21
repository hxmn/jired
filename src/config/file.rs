//! TOML config files, edited in place so user comments survive `config set`.

use crate::config::keys;
use crate::error::{JiredError, Result};
use std::path::{Path, PathBuf};
use toml_edit::{Array, Decor, DocumentMut, Item, Table, Value};

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
            // A list reads back in the same comma-separated spelling its
            // environment variable uses, so every layer agrees on one form.
            Item::Value(Value::Array(a)) => Some(
                a.iter()
                    .filter_map(scalar_string)
                    .collect::<Vec<_>>()
                    .join(","),
            ),
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

    /// Write a dotted key that holds several values, as a TOML array.
    ///
    /// A root-level key lands above the first table, which is where TOML needs
    /// it to stay a root-level key.
    pub fn set_list(&mut self, key: &str, values: &[String]) -> Result<()> {
        let parts: Vec<&str> = key.split('.').collect();
        let (last, tables) = parts.split_last().expect("non-empty key");
        let mut item: &mut Item = self.doc.as_item_mut();
        for part in tables {
            if item.get(part).is_none() {
                item[*part] = Item::Table(Table::new());
            }
            item = item.get_mut(part).expect("just inserted");
        }
        let mut array = Array::new();
        for value in values {
            array.push(value.as_str());
        }
        let mut value = Value::Array(array);
        // A blank line after it, so a root-level list does not read as part of
        // the table that follows it.
        *value.decor_mut() = Decor::new(" ", "\n");
        item[*last] = Item::Value(value);
        self.document(key);
        Ok(())
    }

    /// Write a key and, unless it already carries one, prefix it with the
    /// setting's own documentation as a comment.
    ///
    /// `init` writes the whole settings surface this way, so the file it leaves
    /// behind explains itself and nobody has to go looking for the docs to
    /// change one value.
    pub fn set_documented(&mut self, key: &str, value: &str) -> Result<()> {
        self.set(key, value)?;
        self.document(key);
        Ok(())
    }

    /// Prefix a key already in the document with its own documentation.
    fn document(&mut self, key: &str) {
        let Some(doc) = keys::lookup(key).map(|d| d.doc) else {
            return;
        };
        let parts: Vec<&str> = key.split('.').collect();
        let (last, tables) = parts.split_last().expect("non-empty key");
        let Some(item) = walk_mut(self.doc.as_item_mut(), tables) else {
            return;
        };
        if let Some(mut key) = item.as_table_mut().and_then(|t| t.key_mut(last)) {
            let decor = key.leaf_decor_mut();
            let existing = decor.prefix().and_then(|p| p.as_str()).unwrap_or("");
            if existing.trim().is_empty() {
                decor.set_prefix(comment_block(doc, existing.starts_with('\n')));
            }
        }
    }

    pub fn unset(&mut self, key: &str) -> bool {
        let parts: Vec<&str> = key.split('.').collect();
        let (last, tables) = parts.split_last().expect("non-empty key");
        let Some(item) = walk_mut(self.doc.as_item_mut(), tables) else {
            return false;
        };
        let removed = match item.as_table_like_mut() {
            Some(t) => t.remove(last).is_some(),
            None => false,
        };

        // `[attachments]` with nothing under it is a heading for content that is
        // gone, and reads like a setting that is still there.
        for depth in (0..tables.len()).rev() {
            let (name, parents) = tables[..=depth].split_last().expect("non-empty path");
            let Some(parent) = walk_mut(self.doc.as_item_mut(), parents) else {
                break;
            };
            let empty = parent
                .get(name)
                .and_then(Item::as_table_like)
                .is_some_and(|t| t.is_empty());
            if !empty {
                break;
            }
            if let Some(table) = parent.as_table_like_mut() {
                table.remove(name);
            }
        }
        removed
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

/// The text of an array element, for the scalars a setting can hold.
fn scalar_string(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.value().clone()),
        Value::Integer(i) => Some(i.value().to_string()),
        Value::Boolean(b) => Some(b.value().to_string()),
        Value::Float(f) => Some(f.value().to_string()),
        _ => None,
    }
}

/// Follow a dotted path to the item it names.
fn walk_mut<'a>(mut item: &'a mut Item, path: &[&str]) -> Option<&'a mut Item> {
    for part in path {
        item = item.get_mut(part)?;
    }
    Some(item)
}

/// Wrap documentation into `#` comment lines short enough to read in an editor.
fn comment_block(doc: &str, leading_blank: bool) -> String {
    const WIDTH: usize = 74;
    let mut out = String::new();
    if leading_blank {
        out.push('\n');
    }
    let mut line = String::from("#");
    for word in doc.split_whitespace() {
        if line.len() + 1 + word.len() > WIDTH && line.len() > 1 {
            out.push_str(&line);
            out.push('\n');
            line = String::from("#");
        }
        line.push(' ');
        line.push_str(word);
    }
    out.push_str(&line);
    out.push('\n');
    out
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

    /// The config file `init` leaves behind has to be readable without the
    /// docs open in another window.
    #[test]
    fn documented_writes_explain_the_setting_they_set() {
        let mut cfg = ConfigFile::empty();
        cfg.set_documented("links.mode", "auto").unwrap();
        let toml = cfg.to_toml();
        assert!(toml.contains("mode = \"auto\""), "{toml}");
        assert!(toml.contains("# How grouping folders point"), "{toml}");
        assert!(
            toml.lines().all(|l| l.chars().count() <= 78),
            "comment lines must stay readable: {toml}"
        );

        // Re-writing the value keeps the comment, and never doubles it.
        cfg.set_documented("links.mode", "stub").unwrap();
        let toml = cfg.to_toml();
        assert_eq!(
            toml.matches("# How grouping folders point").count(),
            1,
            "{toml}"
        );
    }

    #[test]
    fn documented_writes_leave_a_users_own_comment_alone() {
        let mut cfg =
            ConfigFile::from_toml("[links]\n# mine, do not touch\nmode = \"stub\"\n").unwrap();
        cfg.set_documented("links.mode", "auto").unwrap();
        let toml = cfg.to_toml();
        assert!(toml.contains("# mine, do not touch"), "{toml}");
        assert!(!toml.contains("# How grouping folders"), "{toml}");
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

    /// A list is a TOML array in the file and a comma-separated string
    /// everywhere else, so an environment variable and a config file can hold
    /// the same setting without either side knowing which it came from.
    #[test]
    fn a_list_setting_writes_an_array_and_reads_back_comma_separated() {
        let mut cfg = ConfigFile::from_toml("[server]\nurl = \"https://a\"\n").unwrap();
        cfg.set_list(
            "ignored_custom_fields",
            &["customfield_10000".into(), "customfield_10001".into()],
        )
        .unwrap();

        let text = cfg.to_toml();
        assert!(
            text.contains("ignored_custom_fields = [\"customfield_10000\", \"customfield_10001\"]"),
            "{text}"
        );
        // A root key has to stay above the first table to remain a root key.
        assert!(
            text.find("ignored_custom_fields").unwrap() < text.find("[server]").unwrap(),
            "{text}"
        );
        assert_eq!(
            cfg.get("ignored_custom_fields").as_deref(),
            Some("customfield_10000,customfield_10001")
        );
        // And it survives being written and read back.
        let again = ConfigFile::from_toml(&text).unwrap();
        assert_eq!(
            again.get("ignored_custom_fields"),
            cfg.get("ignored_custom_fields")
        );
        assert_eq!(again.get("server.url").as_deref(), Some("https://a"));
    }

    /// A leftover `[server]` reads like a setting that is still configured.
    #[test]
    fn unset_takes_the_table_with_it_when_nothing_is_left() {
        let mut cfg =
            ConfigFile::from_toml("[server]\nurl = \"https://a\"\ndeployment = \"cloud\"\n")
                .unwrap();
        assert!(cfg.unset("server.url"));
        assert!(
            cfg.to_toml().contains("[server]"),
            "the table is still used"
        );

        assert!(cfg.unset("server.deployment"));
        assert!(!cfg.to_toml().contains("[server]"), "{}", cfg.to_toml());
    }
}
