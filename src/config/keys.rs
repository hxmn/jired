//! Single source of truth for every resolvable setting [M1.3].
//!
//! The CLI help, the `JIRED_*` env var list, the generated agent docs and the
//! config-file reader all derive from this table, so they cannot drift apart.

/// Where a resolved value came from — reported by `jired config list`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Flag,
    Env,
    LocalConfig,
    GlobalConfig,
    Prompt,
    Default,
}

impl Source {
    pub fn as_str(self) -> &'static str {
        match self {
            Source::Flag => "flag",
            Source::Env => "env",
            Source::LocalConfig => "local-config",
            Source::GlobalConfig => "global-config",
            Source::Prompt => "prompt",
            Source::Default => "default",
        }
    }
}

/// Whether a value may be persisted to a config file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sensitivity {
    /// Safe to write into `.jired/config.toml` or the global config.
    Plain,
    /// Never written to a config file; lives in the keychain or session.db.
    Secret,
}

/// What a setting accepts, so `config set` can reject a bad value where the
/// typo is still on screen rather than at the next command that reads it.
///
/// Deliberately not comparable: `Shaped` holds a function, and two function
/// pointers being equal says nothing useful about the two kinds being the same.
#[derive(Debug, Clone, Copy)]
pub enum Kind {
    Text,
    Bool,
    Bytes,
    /// One of a fixed vocabulary; the canonical spellings, for help and errors.
    Enum(&'static [&'static str]),
    /// A value with a shape rather than a vocabulary — `priority:asc`,
    /// `status:In Progress` — which no list of allowed strings can describe.
    /// `check` returns the canonical spelling, or `None` for "not one of these".
    Shaped {
        check: fn(&str) -> Option<String>,
        expected: &'static str,
    },
}

pub struct KeyDef {
    /// Dotted config-file path, e.g. `server.url`.
    pub key: &'static str,
    pub env: &'static str,
    /// Long flag name without leading dashes, if the value has one.
    pub flag: Option<&'static str>,
    pub sensitivity: Sensitivity,
    /// Whether a missing value may be asked for at an interactive prompt.
    ///
    /// Only the four values a person genuinely has to supply are askable;
    /// everything else has a sane default or a flag, and prompting for it would
    /// turn every command into a questionnaire.
    pub ask: bool,
    pub kind: Kind,
    pub doc: &'static str,
}

/// Every setting resolvable through the precedence chain.
pub const KEYS: &[KeyDef] = &[
    KeyDef {
        key: "server.url",
        env: "JIRED_URL",
        flag: Some("url"),
        sensitivity: Sensitivity::Plain,
        ask: true,
        kind: Kind::Text,
        doc: "Base URL of the Jira instance (https://x.atlassian.net or https://jira.corp/jira)",
    },
    KeyDef {
        key: "server.deployment",
        env: "JIRED_DEPLOYMENT",
        flag: Some("deployment"),
        sensitivity: Sensitivity::Plain,
        ask: false,
        kind: Kind::Enum(&["cloud", "datacenter"]),
        doc: "Deployment kind: cloud or datacenter (probed automatically when unset)",
    },
    KeyDef {
        key: "auth.email",
        env: "JIRED_EMAIL",
        flag: Some("email"),
        sensitivity: Sensitivity::Plain,
        ask: true,
        kind: Kind::Text,
        doc: "Account email (Jira Cloud API-token auth)",
    },
    KeyDef {
        key: "auth.kind",
        env: "JIRED_AUTH",
        flag: Some("auth"),
        sensitivity: Sensitivity::Plain,
        ask: false,
        kind: Kind::Enum(&["api-token", "pat", "basic", "cookie", "oauth2"]),
        doc: "Authentication method: api-token, pat, or basic",
    },
    KeyDef {
        key: "auth.token",
        env: "JIRED_TOKEN",
        flag: Some("token"),
        sensitivity: Sensitivity::Secret,
        ask: true,
        kind: Kind::Text,
        doc: "API token or personal access token (never written to config files)",
    },
    KeyDef {
        key: "creds.storage",
        env: "JIRED_CREDS_STORAGE",
        flag: None,
        sensitivity: Sensitivity::Plain,
        ask: false,
        kind: Kind::Enum(&["keychain", "local_db", "user_config"]),
        doc: "Where the credential is kept. keychain: the OS keychain. local_db: \
              encrypted in .jired/session.db, so it belongs to this workspace \
              alone. user_config: encrypted in credentials.toml in your user \
              config directory, shared by every workspace on this machine. The \
              file-backed stores need JIRED_SECRET_KEY (or --insecure-store). \
              Unset means keychain when this machine has one, otherwise local_db; \
              `jired init` records what was actually used, and `jired doctor` \
              moves the credential when this value changes",
    },
    KeyDef {
        key: "workspace.project",
        env: "JIRED_PROJECT",
        flag: Some("project"),
        sensitivity: Sensitivity::Plain,
        ask: true,
        kind: Kind::Text,
        doc: "Default project key for fetch/pull scope",
    },
    // `workspace.profile` used to live here. Nothing read it — the session store
    // hardcodes the `default` profile — so advertising it as a setting, a flag
    // and an env var promised a feature that did not exist. It comes back with
    // multi-profile support, not before.
    KeyDef {
        key: "tls.ca_cert",
        env: "JIRED_CA_CERT",
        flag: Some("ca-cert"),
        sensitivity: Sensitivity::Plain,
        ask: false,
        kind: Kind::Text,
        doc: "Path to an additional CA bundle (PEM) for self-signed Data Center instances",
    },
    KeyDef {
        key: "tls.insecure",
        env: "JIRED_INSECURE",
        flag: Some("insecure"),
        sensitivity: Sensitivity::Plain,
        ask: false,
        kind: Kind::Bool,
        doc: "Disable TLS certificate verification (prints a warning on every run)",
    },
    KeyDef {
        key: "ui.no_input",
        env: "JIRED_NO_INPUT",
        flag: Some("no-input"),
        sensitivity: Sensitivity::Plain,
        ask: false,
        kind: Kind::Bool,
        doc: "Never prompt; fail with exit code 7 when a required value is missing",
    },
    KeyDef {
        key: "attachments.auto_download_max",
        env: "JIRED_ATTACH_AUTO_MAX",
        flag: None,
        sensitivity: Sensitivity::Plain,
        ask: false,
        kind: Kind::Bytes,
        doc: "Attachments up to this size (bytes) are downloaded eagerly on pull",
    },
    KeyDef {
        key: "attachments.max_upload",
        env: "JIRED_ATTACH_MAX_UPLOAD",
        flag: None,
        sensitivity: Sensitivity::Plain,
        ask: false,
        kind: Kind::Bytes,
        doc: "Refuse to upload attachments larger than this size (bytes)",
    },
    KeyDef {
        key: "children.done_transition",
        env: "JIRED_CHILDREN_DONE_TRANSITION",
        flag: None,
        sensitivity: Sensitivity::Plain,
        ask: false,
        kind: Kind::Text,
        doc: "Transition applied when a subtask checkbox under `## Children` is \
              ticked (disabled when unset)",
    },
    KeyDef {
        key: "ignored_custom_fields",
        env: "JIRED_IGNORED_CUSTOM_FIELDS",
        flag: None,
        sensitivity: Sensitivity::Plain,
        ask: false,
        // A list of field ids or names: anything is a legal element, so there is
        // nothing for `canonicalize` to check beyond leaving it alone.
        kind: Kind::Text,
        doc: "Custom fields to keep out of issue files entirely, by id \
              (customfield_10000) or by the name the instance gives them. A \
              list in a config file, comma-separated in the environment; edit \
              it with `jired config add`/`remove`. The values are still \
              fetched and still in the database, so un-ignoring a field brings \
              it back; `jired doctor` removes an ignored field from files that \
              already carry it",
    },
    KeyDef {
        key: "links.mode",
        env: "JIRED_LINKS_MODE",
        flag: None,
        sensitivity: Sensitivity::Plain,
        ask: false,
        kind: Kind::Enum(&["auto", "symlink", "hardlink", "stub"]),
        doc: "How grouping folders point at issue files. auto: a symlink, or a \
              hard link where symlinks are not permitted (Windows without \
              Developer Mode), or a small Markdown stub file if neither works. \
              symlink, hardlink, stub: always use that one and fail if it is \
              unavailable",
    },
    KeyDef {
        key: "sprints.include_closed",
        env: "JIRED_SPRINTS_INCLUDE_CLOSED",
        flag: None,
        sensitivity: Sensitivity::Plain,
        ask: false,
        kind: Kind::Bool,
        doc: "Project closed sprints into sprints/ (default false)",
    },
    // Written by the issue picker itself as well as by hand: a sort order
    // re-chosen on every run is not a preference, it is a chore.
    KeyDef {
        key: "ui.issues.sort",
        env: "JIRED_ISSUES_SORT",
        flag: None,
        sensitivity: Sensitivity::Plain,
        ask: false,
        kind: Kind::Shaped {
            check: issue_sort,
            expected: "expected <column>:asc or <column>:desc, where the column is one of \
                       key, summary, status, priority, assignee, reporter, updated, \
                       created, viewed",
        },
        doc: "How the issue picker orders its rows: <column>:asc or <column>:desc, \
              where the column is one of key, summary, status, priority, assignee, \
              reporter, updated, created, viewed (default updated:desc). ^s and ^r \
              in the picker change it and write it back here",
    },
    KeyDef {
        key: "ui.issues.filter",
        env: "JIRED_ISSUES_FILTER",
        flag: None,
        sensitivity: Sensitivity::Plain,
        ask: false,
        kind: Kind::Shaped {
            check: issue_filter,
            expected: "expected all, category:new, category:indeterminate, \
                       category:done, or status:<name>",
        },
        doc: "Which issues the picker lists: all, category:<new|indeterminate|done>, \
              or status:<name> (default all). ^f in the picker changes it and writes \
              it back here",
    },
];

/// Settings that hold several values rather than one.
///
/// They are the only keys `jired config add` and `remove` accept, and the only
/// ones written to a config file as a TOML array.
pub const LIST_KEYS: &[&str] = &["ignored_custom_fields"];

/// Environment variables that are not settings but still change what jired does.
///
/// They have no config key and no flag, so `KEYS` cannot describe them — but
/// leaving them out of every table meant `JIRED_SECRET_KEY`, which headless runs
/// require, appeared in no help output at all.
pub const ENV_ONLY: &[(&str, &str)] = &[
    (
        "JIRED_SECRET_KEY",
        "Passphrase encrypting the credential in the file-backed stores. \
         Required for local_db or user_config storage on a machine with no keychain",
    ),
    (
        "JIRED_NO_KEYRING",
        "Set to 1 to ignore the OS keychain even when one is available",
    ),
    (
        "JIRED_LOG",
        "tracing filter for diagnostics, e.g. `jired=debug` (overrides -v)",
    ),
    (
        "JIRED_FIXTURES",
        "Directory of recorded API responses to serve instead of a live server (testing)",
    ),
];

pub fn lookup(key: &str) -> Option<&'static KeyDef> {
    KEYS.iter().find(|k| k.key == key)
}

pub fn is_secret(key: &str) -> bool {
    lookup(key).is_some_and(|k| k.sensitivity == Sensitivity::Secret)
}

pub fn is_list(key: &str) -> bool {
    LIST_KEYS.contains(&key)
}

/// Keys whose name is closest to `key`, for a "did you mean" hint.
///
/// A single edit-distance pass over a 15-row table: cheap, and it turns a dead
/// end into a correction.
pub fn nearest(key: &str) -> Vec<&'static str> {
    let mut scored: Vec<(usize, &'static str)> = KEYS
        .iter()
        .map(|k| (distance(&key.to_lowercase(), k.key), k.key))
        .filter(|(d, _)| *d <= 4)
        .collect();
    scored.sort();
    scored.into_iter().take(3).map(|(_, k)| k).collect()
}

/// Levenshtein distance, two rows at a time.
fn distance(a: &str, b: &str) -> usize {
    let b_chars: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b_chars.len()).collect();
    let mut current = vec![0usize; b_chars.len() + 1];
    for (i, ca) in a.chars().enumerate() {
        current[0] = i + 1;
        for (j, cb) in b_chars.iter().enumerate() {
            let cost = usize::from(ca != *cb);
            current[j + 1] = (prev[j] + cost).min(prev[j + 1] + 1).min(current[j] + 1);
        }
        std::mem::swap(&mut prev, &mut current);
    }
    prev[b_chars.len()]
}

/// Check a value against what its setting accepts, returning the canonical
/// spelling so a config file never accumulates `local-db`, `localdb` and
/// `local_db` for the same thing.
pub fn canonicalize(key: &str, value: &str) -> crate::error::Result<String> {
    use crate::error::JiredError;
    let Some(def) = lookup(key) else {
        return Ok(value.to_string());
    };
    let fail = |expected: String| {
        JiredError::usage(format!("`{key}` does not accept `{value}`"))
            .with_detail("key", key.to_string())
            .with_detail("value", value.to_string())
            .with_hint(expected)
    };
    match def.kind {
        Kind::Text => Ok(value.to_string()),
        Kind::Bool => match value.trim().to_ascii_lowercase().as_str() {
            "true" | "yes" | "on" | "1" => Ok("true".into()),
            "false" | "no" | "off" | "0" => Ok("false".into()),
            _ => Err(fail("expected true or false".into())),
        },
        Kind::Bytes => value
            .trim()
            .parse::<u64>()
            .map(|n| n.to_string())
            .map_err(|_| fail("expected a whole number of bytes, e.g. 10485760".into())),
        Kind::Enum(allowed) => {
            let normalized = value.trim().to_ascii_lowercase().replace(['-', ' '], "_");
            allowed
                .iter()
                .find(|a| a.replace('-', "_") == normalized)
                .map(|a| (*a).to_string())
                .ok_or_else(|| fail(format!("expected one of: {}", allowed.join(", "))))
        }
        Kind::Shaped { check, expected } => check(value).ok_or_else(|| fail(expected.to_string())),
    }
}

/// `<column>:asc|desc` for the issue picker, canonicalized.
fn issue_sort(value: &str) -> Option<String> {
    crate::tui::pick::Sort::parse(value).map(|s| s.to_string())
}

/// `all`, `category:<key>` or `status:<name>` for the issue picker.
fn issue_filter(value: &str) -> Option<String> {
    crate::tui::pick::Scope::parse(value).map(|s| s.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn keys_and_env_vars_are_unique() {
        let mut keys = HashSet::new();
        let mut envs = HashSet::new();
        for k in KEYS {
            assert!(keys.insert(k.key), "duplicate config key {}", k.key);
            assert!(envs.insert(k.env), "duplicate env var {}", k.env);
            assert!(k.env.starts_with("JIRED_"), "{} lacks JIRED_ prefix", k.env);
        }
    }

    #[test]
    fn every_list_key_is_a_real_key() {
        for key in LIST_KEYS {
            assert!(lookup(key).is_some(), "{key} is not in KEYS");
            assert!(is_list(key));
        }
        assert!(!is_list("server.url"));
    }

    #[test]
    fn only_the_token_is_secret() {
        let secrets: Vec<_> = KEYS
            .iter()
            .filter(|k| k.sensitivity == Sensitivity::Secret)
            .map(|k| k.key)
            .collect();
        assert_eq!(secrets, vec!["auth.token"]);
    }
}
