//! Configuration resolution [M1.3].
//!
//! Precedence, highest first: CLI flag → `JIRED_*` env → `.jired/config.toml` →
//! `$XDG_CONFIG_HOME/jired/config.toml` → interactive prompt. `--no-input` (or a
//! non-TTY stdin) removes the last rung, turning a missing required value into
//! exit code 7 that names the value.

pub mod file;
pub mod keys;

use crate::error::{JiredError, Result};
use file::ConfigFile;
use keys::Source;
use std::collections::HashMap;
use std::path::PathBuf;

/// Asks the user for a value; only consulted when interaction is allowed.
pub trait Prompter {
    fn prompt(&mut self, key: &str, doc: &str, secret: bool) -> Result<Option<String>>;
}

/// Prompter used when interaction is disabled; never called.
pub struct NoPrompter;

impl Prompter for NoPrompter {
    fn prompt(&mut self, _key: &str, _doc: &str, _secret: bool) -> Result<Option<String>> {
        Ok(None)
    }
}

pub struct Resolver {
    flags: HashMap<String, String>,
    env: HashMap<String, String>,
    local: ConfigFile,
    global: ConfigFile,
    defaults: HashMap<String, String>,
    interactive: bool,
    prompter: Box<dyn Prompter>,
    /// Values obtained from prompts, cached so one run never asks twice.
    prompted: HashMap<String, String>,
}

impl Resolver {
    pub fn builder() -> ResolverBuilder {
        ResolverBuilder::default()
    }

    /// Resolve a key, returning the value and where it came from.
    pub fn get(&mut self, key: &str) -> Result<Option<(String, Source)>> {
        if let Some(v) = self.flags.get(key) {
            return Ok(Some((v.clone(), Source::Flag)));
        }
        if let Some(def) = keys::lookup(key) {
            if let Some(v) = self.env.get(def.env) {
                if !v.is_empty() {
                    return Ok(Some((v.clone(), Source::Env)));
                }
            }
        }
        if let Some(v) = self.local.get(key) {
            return Ok(Some((v, Source::LocalConfig)));
        }
        if let Some(v) = self.global.get(key) {
            return Ok(Some((v, Source::GlobalConfig)));
        }
        if let Some(v) = self.prompted.get(key) {
            return Ok(Some((v.clone(), Source::Prompt)));
        }
        if self.interactive {
            if let Some(def) = keys::lookup(key) {
                let secret = keys::is_secret(key);
                if let Some(v) = self.prompter.prompt(key, def.doc, secret)? {
                    self.prompted.insert(key.to_string(), v.clone());
                    return Ok(Some((v, Source::Prompt)));
                }
            }
        }
        if let Some(v) = self.defaults.get(key) {
            return Ok(Some((v.clone(), Source::Default)));
        }
        Ok(None)
    }

    pub fn get_value(&mut self, key: &str) -> Result<Option<String>> {
        Ok(self.get(key)?.map(|(v, _)| v))
    }

    /// Resolve a key that the command cannot proceed without.
    ///
    /// The error names both the config key and its env var so a non-interactive
    /// caller can fix the invocation without reading the docs.
    pub fn require(&mut self, key: &str) -> Result<String> {
        match self.get(key)? {
            Some((v, _)) => Ok(v),
            None => {
                let def = keys::lookup(key);
                let env = def.map(|d| d.env).unwrap_or("");
                let flag = def.and_then(|d| d.flag);
                let mut err = JiredError::missing_value(format!(
                    "missing required value `{key}`{}",
                    if self.interactive {
                        ""
                    } else {
                        " and interactive input is disabled"
                    }
                ))
                .with_detail("key", key)
                .with_detail("env", env);
                if let Some(flag) = flag {
                    err = err
                        .with_detail("flag", format!("--{flag}"))
                        .with_hint(format!("pass --{flag}, or set {env}"));
                } else {
                    err = err.with_hint(format!("set {env}, or `jired config set {key} <value>`"));
                }
                Err(err)
            }
        }
    }

    pub fn get_bool(&mut self, key: &str) -> Result<Option<bool>> {
        Ok(self
            .get_value(key)?
            .map(|v| matches!(v.to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on")))
    }

    pub fn get_u64(&mut self, key: &str) -> Result<Option<u64>> {
        match self.get_value(key)? {
            Some(v) => v.parse::<u64>().map(Some).map_err(|_| {
                JiredError::usage(format!("value of `{key}` must be a number, got `{v}`"))
                    .with_detail("key", key)
            }),
            None => Ok(None),
        }
    }

    pub fn interactive(&self) -> bool {
        self.interactive
    }

    /// Effective settings and their origins, for `jired config list`.
    pub fn describe(&mut self) -> Vec<(&'static str, Option<String>, Source)> {
        let mut out = Vec::new();
        // Iterate the static table so output ordering is deterministic.
        for def in keys::KEYS {
            if def.sensitivity == keys::Sensitivity::Secret {
                // Secrets are reported as present/absent, never echoed.
                let present = self.get(def.key).ok().flatten();
                out.push((
                    def.key,
                    present.as_ref().map(|_| "<set>".to_string()),
                    present.map(|(_, s)| s).unwrap_or(Source::Default),
                ));
                continue;
            }
            match self.get(def.key).ok().flatten() {
                Some((v, src)) => out.push((def.key, Some(v), src)),
                None => out.push((def.key, None, Source::Default)),
            }
        }
        out
    }
}

#[derive(Default)]
pub struct ResolverBuilder {
    flags: HashMap<String, String>,
    env: Option<HashMap<String, String>>,
    local: Option<ConfigFile>,
    global: Option<ConfigFile>,
    defaults: HashMap<String, String>,
    interactive: bool,
    prompter: Option<Box<dyn Prompter>>,
}

impl ResolverBuilder {
    /// Record a flag value; `None` means the flag was absent.
    pub fn flag(mut self, key: &str, value: Option<impl Into<String>>) -> Self {
        if let Some(v) = value {
            self.flags.insert(key.to_string(), v.into());
        }
        self
    }

    pub fn flag_bool(mut self, key: &str, value: bool) -> Self {
        if value {
            self.flags.insert(key.to_string(), "true".into());
        }
        self
    }

    pub fn env_from_process(mut self) -> Self {
        let mut map = HashMap::new();
        for def in keys::KEYS {
            if let Ok(v) = std::env::var(def.env) {
                map.insert(def.env.to_string(), v);
            }
        }
        self.env = Some(map);
        self
    }

    pub fn env_map(mut self, map: HashMap<String, String>) -> Self {
        self.env = Some(map);
        self
    }

    pub fn local_file(mut self, path: Option<PathBuf>) -> Result<Self> {
        self.local = Some(match path {
            Some(p) => ConfigFile::load(&p)?,
            None => ConfigFile::empty(),
        });
        Ok(self)
    }

    pub fn global_file(mut self, path: Option<PathBuf>) -> Result<Self> {
        self.global = Some(match path {
            Some(p) => ConfigFile::load(&p)?,
            None => ConfigFile::empty(),
        });
        Ok(self)
    }

    pub fn local(mut self, cfg: ConfigFile) -> Self {
        self.local = Some(cfg);
        self
    }

    pub fn global(mut self, cfg: ConfigFile) -> Self {
        self.global = Some(cfg);
        self
    }

    pub fn default_value(mut self, key: &str, value: impl Into<String>) -> Self {
        self.defaults.insert(key.to_string(), value.into());
        self
    }

    pub fn interactive(mut self, yes: bool) -> Self {
        self.interactive = yes;
        self
    }

    pub fn prompter(mut self, p: Box<dyn Prompter>) -> Self {
        self.prompter = Some(p);
        self
    }

    pub fn build(self) -> Resolver {
        Resolver {
            flags: self.flags,
            env: self.env.unwrap_or_default(),
            local: self.local.unwrap_or_else(ConfigFile::empty),
            global: self.global.unwrap_or_else(ConfigFile::empty),
            defaults: builtin_defaults(self.defaults),
            interactive: self.interactive,
            prompter: self.prompter.unwrap_or(Box::new(NoPrompter)),
            prompted: HashMap::new(),
        }
    }
}

fn builtin_defaults(mut user: HashMap<String, String>) -> HashMap<String, String> {
    let builtin = [
        ("attachments.auto_download_max", "262144"),
        ("attachments.max_upload", "52428800"),
        ("links.mode", "auto"),
        ("sprints.include_closed", "false"),
        ("tls.insecure", "false"),
    ];
    for (k, v) in builtin {
        user.entry(k.to_string()).or_insert_with(|| v.to_string());
    }
    user
}

/// Path of the global config file, honouring `XDG_CONFIG_HOME`.
pub fn global_config_path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("jired").join("config.toml"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ErrorKind;

    fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    struct FixedPrompter(&'static str);
    impl Prompter for FixedPrompter {
        fn prompt(&mut self, _k: &str, _d: &str, _s: bool) -> Result<Option<String>> {
            Ok(Some(self.0.to_string()))
        }
    }

    /// Each rung of the chain must win over every rung below it.
    #[test]
    fn precedence_matrix() {
        let local = ConfigFile::from_toml("[server]\nurl = \"https://local\"\n").unwrap();
        let global = ConfigFile::from_toml("[server]\nurl = \"https://global\"\n").unwrap();

        let all = |flag: bool, e: bool, l: bool, g: bool, interactive: bool| {
            let mut b = Resolver::builder()
                .env_map(if e {
                    env(&[("JIRED_URL", "https://env")])
                } else {
                    env(&[])
                })
                .local(if l {
                    local.clone()
                } else {
                    ConfigFile::empty()
                })
                .global(if g {
                    global.clone()
                } else {
                    ConfigFile::empty()
                })
                .interactive(interactive)
                .prompter(Box::new(FixedPrompter("https://prompt")));
            if flag {
                b = b.flag("server.url", Some("https://flag"));
            }
            b.build().get("server.url").unwrap()
        };

        assert_eq!(all(true, true, true, true, true).unwrap().1, Source::Flag);
        assert_eq!(all(false, true, true, true, true).unwrap().1, Source::Env);
        assert_eq!(
            all(false, false, true, true, true).unwrap().1,
            Source::LocalConfig
        );
        assert_eq!(
            all(false, false, false, true, true).unwrap().1,
            Source::GlobalConfig
        );
        assert_eq!(
            all(false, false, false, false, true).unwrap().1,
            Source::Prompt
        );
        assert!(all(false, false, false, false, false).is_none());

        let val = all(true, true, true, true, true).unwrap().0;
        assert_eq!(val, "https://flag");
    }

    #[test]
    fn empty_env_var_does_not_shadow_config() {
        let mut r = Resolver::builder()
            .env_map(env(&[("JIRED_URL", "")]))
            .local(ConfigFile::from_toml("[server]\nurl = \"https://local\"\n").unwrap())
            .build();
        assert_eq!(r.get_value("server.url").unwrap().unwrap(), "https://local");
    }

    #[test]
    fn missing_required_value_is_exit_7_and_names_the_value() {
        let mut r = Resolver::builder().interactive(false).build();
        let err = r.require("server.url").unwrap_err();
        assert_eq!(err.kind, ErrorKind::MissingValue);
        assert_eq!(err.exit_code().as_i32(), 7);
        assert_eq!(err.details["key"], "server.url");
        assert_eq!(err.details["env"], "JIRED_URL");
        assert_eq!(err.details["flag"], "--url");
    }

    #[test]
    fn prompt_is_cached_for_the_whole_run() {
        struct CountingPrompter(std::rc::Rc<std::cell::Cell<u32>>);
        impl Prompter for CountingPrompter {
            fn prompt(&mut self, _k: &str, _d: &str, _s: bool) -> Result<Option<String>> {
                self.0.set(self.0.get() + 1);
                Ok(Some("x".into()))
            }
        }
        let count = std::rc::Rc::new(std::cell::Cell::new(0));
        let mut r = Resolver::builder()
            .interactive(true)
            .prompter(Box::new(CountingPrompter(count.clone())))
            .build();
        r.require("server.url").unwrap();
        r.require("server.url").unwrap();
        assert_eq!(count.get(), 1);
    }

    #[test]
    fn defaults_are_the_last_rung() {
        let mut r = Resolver::builder().interactive(false).build();
        assert_eq!(
            r.get("links.mode").unwrap().unwrap(),
            ("auto".to_string(), Source::Default)
        );
    }

    #[test]
    fn describe_never_echoes_secrets() {
        let mut r = Resolver::builder()
            .env_map(env(&[("JIRED_TOKEN", "super-secret-value")]))
            .build();
        let rows = r.describe();
        let token = rows.iter().find(|(k, _, _)| *k == "auth.token").unwrap();
        assert_eq!(token.1.as_deref(), Some("<set>"));
        assert!(!format!("{rows:?}").contains("super-secret-value"));
    }
}
