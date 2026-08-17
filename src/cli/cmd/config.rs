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
                    println!("{v}");
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
            let ctx = Context::open(g, out)?;
            let mut file = if a.global {
                ctx.global_config_file()?
            } else {
                ctx.local_config()?
            };
            file.set(&a.key, &a.value)?;
            if a.global {
                let path = ctx
                    .global_config_path()
                    .ok_or_else(|| JiredError::generic("no user config directory available"))?;
                file.save_to(&path)?;
            } else {
                file.save_to(&ctx.workspace.config_path())?;
            }
            out.say(format!("{} = {}", a.key, a.value));
            out.emit(
                "config",
                &serde_json::json!({"key": a.key, "value": a.value, "scope":
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
            }
            out.emit("config-list", &serde_json::json!({ "settings": settings }));
        }
    }
    Ok(ExitCode::Success)
}

fn check_key(key: &str) -> Result<()> {
    if keys::lookup(key).is_some() {
        return Ok(());
    }
    Err(JiredError::usage(format!("unknown config key `{key}`"))
        .with_detail("key", key.to_string())
        .with_hint("run `jired config list --all` to see every key"))
}
