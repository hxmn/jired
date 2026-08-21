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
        doc: "Base URL of the Jira instance (https://x.atlassian.net or https://jira.corp/jira)",
    },
    KeyDef {
        key: "server.deployment",
        env: "JIRED_DEPLOYMENT",
        flag: Some("deployment"),
        sensitivity: Sensitivity::Plain,
        ask: false,
        doc: "Deployment kind: cloud or datacenter (probed automatically when unset)",
    },
    KeyDef {
        key: "auth.email",
        env: "JIRED_EMAIL",
        flag: Some("email"),
        sensitivity: Sensitivity::Plain,
        ask: true,
        doc: "Account email (Jira Cloud API-token auth)",
    },
    KeyDef {
        key: "auth.kind",
        env: "JIRED_AUTH",
        flag: Some("auth"),
        sensitivity: Sensitivity::Plain,
        ask: false,
        doc: "Authentication method: api-token, pat, or basic",
    },
    KeyDef {
        key: "auth.token",
        env: "JIRED_TOKEN",
        flag: Some("token"),
        sensitivity: Sensitivity::Secret,
        ask: true,
        doc: "API token or personal access token (never written to config files)",
    },
    KeyDef {
        key: "creds.storage",
        env: "JIRED_CREDS_STORAGE",
        flag: None,
        sensitivity: Sensitivity::Plain,
        ask: false,
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
        doc: "Default project key for fetch/pull scope",
    },
    KeyDef {
        key: "workspace.profile",
        env: "JIRED_PROFILE",
        flag: Some("profile"),
        sensitivity: Sensitivity::Plain,
        ask: false,
        doc: "Named account profile to use",
    },
    KeyDef {
        key: "tls.ca_cert",
        env: "JIRED_CA_CERT",
        flag: Some("ca-cert"),
        sensitivity: Sensitivity::Plain,
        ask: false,
        doc: "Path to an additional CA bundle (PEM) for self-signed Data Center instances",
    },
    KeyDef {
        key: "tls.insecure",
        env: "JIRED_INSECURE",
        flag: Some("insecure"),
        sensitivity: Sensitivity::Plain,
        ask: false,
        doc: "Disable TLS certificate verification (prints a warning on every run)",
    },
    KeyDef {
        key: "ui.no_input",
        env: "JIRED_NO_INPUT",
        flag: Some("no-input"),
        sensitivity: Sensitivity::Plain,
        ask: false,
        doc: "Never prompt; fail with exit code 7 when a required value is missing",
    },
    KeyDef {
        key: "attachments.auto_download_max",
        env: "JIRED_ATTACH_AUTO_MAX",
        flag: None,
        sensitivity: Sensitivity::Plain,
        ask: false,
        doc: "Attachments up to this size (bytes) are downloaded eagerly on pull",
    },
    KeyDef {
        key: "attachments.max_upload",
        env: "JIRED_ATTACH_MAX_UPLOAD",
        flag: None,
        sensitivity: Sensitivity::Plain,
        ask: false,
        doc: "Refuse to upload attachments larger than this size (bytes)",
    },
    KeyDef {
        key: "children.done_transition",
        env: "JIRED_CHILDREN_DONE_TRANSITION",
        flag: None,
        sensitivity: Sensitivity::Plain,
        ask: false,
        doc: "Transition applied when a subtask checkbox under `## Children` is \
              ticked (disabled when unset)",
    },
    KeyDef {
        key: "ignored_custom_fields",
        env: "JIRED_IGNORED_CUSTOM_FIELDS",
        flag: None,
        sensitivity: Sensitivity::Plain,
        ask: false,
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
        doc: "Project closed sprints into sprints/ (default false)",
    },
];

/// Settings that hold several values rather than one.
///
/// They are the only keys `jired config add` and `remove` accept, and the only
/// ones written to a config file as a TOML array.
pub const LIST_KEYS: &[&str] = &["ignored_custom_fields"];

pub fn lookup(key: &str) -> Option<&'static KeyDef> {
    KEYS.iter().find(|k| k.key == key)
}

pub fn is_secret(key: &str) -> bool {
    lookup(key).is_some_and(|k| k.sensitivity == Sensitivity::Secret)
}

pub fn is_list(key: &str) -> bool {
    LIST_KEYS.contains(&key)
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
