//! `jired config get|set|list|unset` [M1.3].

use crate::cli::context::Context;
use crate::cli::GlobalArgs;
use crate::config::keys;
use crate::error::{ExitCode, JiredError, Result};
use crate::json::Out;
use clap::{Args, Subcommand};

#[derive(Subcommand, Debug)]
pub enum ConfigCmd {
    /// Print the effective value of a key
    Get(GetArgs),
    /// Write a key to the workspace (or global) config file
    Set(SetArgs),
    /// Remove a key from a config file
    Unset(UnsetArgs),
    /// Show every setting, its value, and where it came from
    List(ListArgs),
}

#[derive(Args, Debug)]
pub struct GetArgs {
    pub key: String,
}

#[derive(Args, Debug)]
pub struct SetArgs {
    pub key: String,
    pub value: String,
    /// Write to the user-wide config instead of this workspace
    #[arg(long)]
    pub global: bool,
}

#[derive(Args, Debug)]
pub struct UnsetArgs {
    pub key: String,
    #[arg(long)]
    pub global: bool,
}

#[derive(Args, Debug)]
pub struct ListArgs {
    /// Include settings that are unset
    #[arg(long)]
    pub all: bool,
}

#[derive(serde::Serialize)]
struct Setting {
    key: &'static str,
    value: Option<String>,
    source: &'static str,
    doc: &'static str,
    env: &'static str,
}

pub fn run(g: &GlobalArgs, cmd: &ConfigCmd, out: Out) -> Result<ExitCode> {
    match cmd {
        ConfigCmd::Get(a) => {
            let mut ctx = Context::open(g, out)?;
            check_key(&a.key)?;
            match ctx.resolver.get(&a.key)? {
                Some((v, src)) if !keys::is_secret(&a.key) => {
                    // Bare value for scripts that just want it; in JSON mode
                    // stdout is reserved for the one result document.
                    if !out.json {
                        println!("{v}");
                    }
                    out.emit(
                        "config",
                        &serde_json::json!({"key": a.key, "value": v, "source": src.as_str()}),
                    );
                }
                Some((_, src)) => {
                    // A secret's presence is reportable; its value is not.
                    out.say("<set>");
                    out.emit(
                        "config",
                        &serde_json::json!({"key": a.key, "value": "<set>", "source": src.as_str()}),
                    );
                }
                None => {
                    return Err(JiredError::usage(format!("`{}` is not set", a.key))
                        .with_detail("key", a.key.clone()))
                }
            }
        }
        ConfigCmd::Set(a) => {
            check_key(&a.key)?;
            // Settings with a fixed vocabulary are checked here, where the typo
            // is still on screen, rather than at the next command that reads them.
            let value = normalize(&a.key, &a.value)?;
            let ctx = Context::open(g, out)?;
            let mut file = if a.global {
                ctx.global_config_file()?
            } else {
                ctx.local_config()?
            };
            file.set(&a.key, &value)?;
            if a.global {
                let path = ctx
                    .global_config_path()
                    .ok_or_else(|| JiredError::generic("no user config directory available"))?;
                file.save_to(&path)?;
            } else {
                file.save_to(&ctx.workspace.config_path())?;
            }
            out.say(format!("{} = {}", a.key, value));
            if a.key == "creds.storage" {
                out.say("run `jired doctor` to move the stored credential there");
            }
            out.emit(
                "config",
                &serde_json::json!({"key": a.key, "value": value, "scope":
                    if a.global {"global"} else {"local"}}),
            );
        }
        ConfigCmd::Unset(a) => {
            check_key(&a.key)?;
            let ctx = Context::open(g, out)?;
            let (mut file, path) = if a.global {
                let path = ctx
                    .global_config_path()
                    .ok_or_else(|| JiredError::generic("no user config directory available"))?;
                (ctx.global_config_file()?, path)
            } else {
                (ctx.local_config()?, ctx.workspace.config_path())
            };
            let removed = file.unset(&a.key);
            file.save_to(&path)?;
            out.say(if removed {
                format!("unset {}", a.key)
            } else {
                format!("{} was not set", a.key)
            });
            out.emit(
                "config",
                &serde_json::json!({"key": a.key, "removed": removed}),
            );
        }
        ConfigCmd::List(a) => {
            let mut ctx = Context::open(g, out)?;
            let rows = ctx.resolver.describe();
            let settings: Vec<Setting> = rows
                .into_iter()
                .filter(|(_, v, _)| a.all || v.is_some())
                .map(|(key, value, source)| {
                    let def = keys::lookup(key).expect("described keys are defined");
                    Setting {
                        key,
                        value,
                        source: source.as_str(),
                        doc: def.doc,
                        env: def.env,
                    }
                })
                .collect();
            for s in &settings {
                out.say(format!(
                    "{:<32} {:<24} ({})",
                    s.key,
                    s.value.as_deref().unwrap_or("-"),
                    s.source
                ));
                // `--all` is what someone runs to find out what a setting is
                // for, so that listing carries the documentation with it.
                if a.all {
                    for line in wrap(s.doc, 72) {
                        out.say(format!("    {line}"));
                    }
                }
            }
            out.emit("config-list", &serde_json::json!({ "settings": settings }));
        }
    }
    Ok(ExitCode::Success)
}

/// Validate a value against the vocabulary its setting accepts, returning the
/// canonical spelling so the config file does not accumulate `local-db`,
/// `localdb` and `local_db` for the same thing.
fn normalize(key: &str, value: &str) -> Result<String> {
    Ok(match key {
        "creds.storage" => crate::auth::CredStore::parse(value)?.as_str().to_string(),
        "server.deployment" => crate::api::Deployment::parse(value)?.as_str().to_string(),
        "auth.kind" => crate::api::AuthKind::parse(value)?.as_str().to_string(),
        "links.mode" => crate::project::links::LinkMode::parse(value)?
            .as_str()
            .to_string(),
        _ => value.to_string(),
    })
}

/// Break documentation into lines that fit an ordinary terminal.
fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        if !line.is_empty() && line.chars().count() + 1 + word.chars().count() > width {
            lines.push(std::mem::take(&mut line));
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    if !line.is_empty() {
        lines.push(line);
    }
    lines
}

fn check_key(key: &str) -> Result<()> {
    if keys::lookup(key).is_some() {
        return Ok(());
    }
    Err(JiredError::usage(format!("unknown config key `{key}`"))
        .with_detail("key", key.to_string())
        .with_hint("run `jired config list --all` to see every key"))
}
