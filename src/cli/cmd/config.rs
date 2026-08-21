//! `jired config get|set|add|remove|unset|list|path|edit` [M1.3].
//!
//! Deliberately usable before `init`: `jired config set creds.storage
//! user_config --global` is a documented way to set up a machine, and it cannot
//! be if the command insists on a workspace first.

use crate::cli::context::Context;
use crate::cli::GlobalArgs;
use crate::config::keys;
use crate::error::{ExitCode, JiredError, Result};
use crate::json::Out;
use clap::{Args, Subcommand};
use std::path::PathBuf;

#[derive(Subcommand, Debug)]
pub enum ConfigCmd {
    /// Print the effective value of a key
    Get(GetArgs),
    /// Write a key to the workspace (or global) config file
    Set(SetArgs),
    /// Append a value to a setting that holds a list
    Add(AddArgs),
    /// Take one value back out of a setting that holds a list
    Remove(AddArgs),
    /// Remove a key from a config file
    Unset(UnsetArgs),
    /// Show every setting, its value, and where it came from
    List(ListArgs),
    /// Print the path of the config file a write would go to
    Path(ScopeArgs),
    /// Open a config file in $EDITOR
    Edit(ScopeArgs),
}

/// Names of every setting, so `--help`, errors and shell completion all agree.
fn key_parser() -> clap::builder::PossibleValuesParser {
    clap::builder::PossibleValuesParser::new(keys::KEYS.iter().map(|k| k.key))
}

#[derive(Args, Debug)]
pub struct GetArgs {
    /// Setting to read, e.g. server.url
    #[arg(value_parser = key_parser())]
    pub key: String,
}

#[derive(Args, Debug)]
pub struct SetArgs {
    /// Setting to write
    #[arg(value_parser = key_parser())]
    pub key: String,
    /// Value to store
    pub value: String,
    /// Write to the user-wide config instead of this workspace
    #[arg(long)]
    pub global: bool,
}

#[derive(Args, Debug)]
pub struct AddArgs {
    pub key: String,
    pub value: String,
    /// Edit the user-wide config instead of this workspace
    #[arg(long)]
    pub global: bool,
}

#[derive(Args, Debug)]
pub struct UnsetArgs {
    /// Setting to remove
    #[arg(value_parser = key_parser())]
    pub key: String,
    /// Remove from the user-wide config instead of this workspace
    #[arg(long)]
    pub global: bool,
}

#[derive(Args, Debug)]
pub struct ScopeArgs {
    /// Act on the user-wide config instead of this workspace
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
    /// File the value came from, when it came from one.
    #[serde(skip_serializing_if = "Option::is_none")]
    file: Option<String>,
    doc: &'static str,
    env: &'static str,
}

pub fn run(g: &GlobalArgs, cmd: &ConfigCmd, out: Out) -> Result<ExitCode> {
    match cmd {
        ConfigCmd::Get(a) => {
            let mut ctx = Context::open_optional(g, out)?;
            check_key(&a.key)?;
            match ctx.resolver.get(&a.key)? {
                Some((v, src)) if !keys::is_secret(&a.key) => {
                    // Bare value for scripts that just want it; in JSON mode
                    // stdout is reserved for the one result document.
                    out.say(&v);
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
                        .with_detail("key", a.key.clone())
                        .with_hint(format!("set it with `jired config set {} <value>`", a.key)))
                }
            }
        }
        ConfigCmd::Set(a) => {
            check_key(&a.key)?;
            // Settings with a fixed vocabulary are checked here, where the typo
            // is still on screen, rather than at the next command that reads them.
            let value = keys::canonicalize(&a.key, &a.value)?;
            let ctx = Context::open_optional(g, out)?;
            let (mut file, path) = target_file(&ctx, a.global)?;
            file.set(&a.key, &value)?;
            file.save_to(&path)?;
            out.say(format!("{} = {}  ({})", a.key, value, path.display()));
            if a.key == "creds.storage" {
                out.hint("run \"jired doctor\" to move the stored credential there");
            }
            out.emit(
                "config",
                &serde_json::json!({"key": a.key, "value": value, "file": path.display().to_string(),
                    "scope": if a.global {"global"} else {"local"}}),
            );
        }
        ConfigCmd::Add(a) => edit_list(g, out, a, true)?,
        ConfigCmd::Remove(a) => edit_list(g, out, a, false)?,
        ConfigCmd::Unset(a) => {
            check_key(&a.key)?;
            let ctx = Context::open_optional(g, out)?;
            let (mut file, path) = target_file(&ctx, a.global)?;
            let removed = file.unset(&a.key);
            file.save_to(&path)?;
            out.say(if removed {
                format!("unset {} in {}", a.key, path.display())
            } else {
                format!("{} was not set in {}", a.key, path.display())
            });
            out.emit(
                "config",
                &serde_json::json!({"key": a.key, "removed": removed,
                    "file": path.display().to_string()}),
            );
        }
        ConfigCmd::Path(a) => {
            let ctx = Context::open_optional(g, out)?;
            let path = target_path(&ctx, a.global)?;
            out.say(path.display().to_string());
            out.emit(
                "config",
                &serde_json::json!({"file": path.display().to_string(),
                    "scope": if a.global {"global"} else {"local"},
                    "exists": path.exists()}),
            );
        }
        ConfigCmd::Edit(a) => {
            let ctx = Context::open_optional(g, out)?;
            let path = target_path(&ctx, a.global)?;
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).map_err(crate::error::io_ctx("create", parent))?;
            }
            if !path.exists() {
                std::fs::write(&path, "").map_err(crate::error::io_ctx("create", &path))?;
            }
            open_editor(&path)?;
            // Reading it back turns a typo into an error now rather than at the
            // next command, which is the whole reason to validate at write time.
            let reloaded = crate::config::file::ConfigFile::load(&path)?;
            for def in keys::KEYS {
                if let Some(v) = reloaded.get(def.key) {
                    keys::canonicalize(def.key, &v)?;
                }
            }
            out.say(format!("saved {}", path.display()));
            out.emit(
                "config",
                &serde_json::json!({"file": path.display().to_string(), "edited": true}),
            );
        }
        ConfigCmd::List(a) => {
            let mut ctx = Context::open_optional(g, out)?;
            let local = ctx.workspace.config_path();
            let global = ctx.global_config_path();
            let rows = ctx.resolver.describe();
            let settings: Vec<Setting> = rows
                .into_iter()
                .filter(|(_, v, _)| a.all || v.is_some())
                .map(|(key, value, source)| {
                    let def = keys::lookup(key).expect("described keys are defined");
                    // Which *file* to edit is the question `config list` is
                    // usually asked; the source kind alone does not answer it
                    // when a workspace, a global config and --config are in play.
                    let file = match source {
                        keys::Source::LocalConfig => Some(local.display().to_string()),
                        keys::Source::GlobalConfig => {
                            global.as_ref().map(|p| p.display().to_string())
                        }
                        _ => None,
                    };
                    Setting {
                        key,
                        value,
                        source: source.as_str(),
                        file,
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
            if a.all {
                out.say("\nenvironment only:");
                for (name, doc) in keys::ENV_ONLY {
                    out.say(format!("{name:<32} {doc}"));
                }
            }
            out.emit(
                "config-list",
                &serde_json::json!({
                    "settings": settings,
                    "env_only": keys::ENV_ONLY.iter()
                        .map(|(n, d)| serde_json::json!({"env": n, "doc": d}))
                        .collect::<Vec<_>>(),
                }),
            );
        }
    }
    Ok(ExitCode::Success)
}

/// The config file a write is aimed at, and where it will be saved.
fn target_file(ctx: &Context, global: bool) -> Result<(crate::config::file::ConfigFile, PathBuf)> {
    let path = target_path(ctx, global)?;
    let file = if global {
        ctx.global_config_file()?
    } else {
        ctx.local_config()?
    };
    Ok((file, path))
}

/// Add or remove one value of a list setting.
///
/// The file being edited is what the new list is built from, never the resolved
/// value: adding a field to the workspace list must not silently copy the
/// global one into it.
fn edit_list(g: &GlobalArgs, out: Out, a: &AddArgs, add: bool) -> Result<()> {
    check_key(&a.key)?;
    if !keys::is_list(&a.key) {
        return Err(
            JiredError::usage(format!("`{}` holds a single value", a.key))
                .with_detail("key", a.key.clone())
                .with_hint(format!("use `jired config set {} <value>`", a.key)),
        );
    }
    let value = a.value.trim().to_string();
    if value.is_empty() || value.contains(',') {
        return Err(
            JiredError::usage("a list value cannot be empty or contain a comma")
                .with_detail("value", a.value.clone())
                .with_hint("add one value per command"),
        );
    }

    let ctx = Context::open_optional(g, out)?;
    let (mut file, path) = target_file(&ctx, a.global)?;
    let mut values = file
        .get(&a.key)
        .as_deref()
        .map(crate::config::split_list)
        .unwrap_or_default();

    let changed = if add {
        let missing = !values.contains(&value);
        if missing {
            values.push(value.clone());
        }
        missing
    } else {
        let before = values.len();
        values.retain(|v| v != &value);
        before != values.len()
    };

    file.set_list(&a.key, &values)?;
    file.save_to(&path)?;

    out.say(format!(
        "{} = {}",
        a.key,
        if values.is_empty() {
            "(empty)".to_string()
        } else {
            values.join(", ")
        }
    ));
    if !changed {
        out.say(if add {
            format!("`{value}` was already there")
        } else {
            format!("`{value}` was not there")
        });
    } else if a.key == "ignored_custom_fields" {
        out.say("run `jired doctor` to bring the issue files into line");
    }
    out.emit(
        "config",
        &serde_json::json!({
            "key": a.key,
            "value": values.join(","),
            "values": values,
            "changed": changed,
            "scope": if a.global {"global"} else {"local"},
        }),
    );
    Ok(())
}

/// The file a write in this scope lands in.
fn target_path(ctx: &Context, global: bool) -> Result<PathBuf> {
    if global {
        return ctx.global_config_path().ok_or_else(|| {
            JiredError::generic("no user config directory available")
                .with_hint("set XDG_CONFIG_HOME, or use a workspace config instead of --global")
        });
    }
    if !ctx.workspace.is_initialized() {
        return Err(JiredError::usage("this directory is not a jired workspace")
            .with_hint("run `jired init` first, or pass --global to write the user-wide config"));
    }
    Ok(ctx.workspace.config_path())
}

fn open_editor(path: &std::path::Path) -> Result<()> {
    let editor = std::env::var("VISUAL")
        .or_else(|_| std::env::var("EDITOR"))
        .map_err(|_| {
            JiredError::missing_value("no editor configured")
                .with_hint("set $EDITOR, or use `jired config set <key> <value>`")
        })?;
    let status = std::process::Command::new("sh")
        .arg("-c")
        .arg(format!("{editor} \"$1\""))
        .arg("sh")
        .arg(path)
        .status()
        .map_err(|e| {
            JiredError::generic(format!("cannot run $EDITOR ({editor})")).with_source(e)
        })?;
    if !status.success() {
        return Err(JiredError::generic(format!(
            "$EDITOR ({editor}) exited with an error"
        )));
    }
    Ok(())
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
    let near = keys::nearest(key);
    let hint = if near.is_empty() {
        "run `jired config list --all` to see every key".to_string()
    } else {
        format!("did you mean {}?", near.join(", "))
    };
    Err(JiredError::usage(format!("unknown config key `{key}`"))
        .with_detail("key", key.to_string())
        .with_hint(hint))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_near_miss_key_is_suggested() {
        let err = check_key("server.urls").unwrap_err();
        assert!(
            err.hint.as_deref().unwrap_or("").contains("server.url"),
            "expected a suggestion, got {:?}",
            err.hint
        );
    }

    #[test]
    fn values_are_checked_against_the_settings_vocabulary() {
        assert_eq!(
            keys::canonicalize("links.mode", "SYM-LINK")
                .unwrap_err()
                .exit_code()
                .as_i32(),
            2
        );
        assert_eq!(
            keys::canonicalize("links.mode", "SYMLINK").unwrap(),
            "symlink"
        );
        assert_eq!(keys::canonicalize("tls.insecure", "yes").unwrap(), "true");
        // A number key used to accept anything and fail at the next pull.
        assert!(keys::canonicalize("attachments.max_upload", "lots").is_err());
    }
}
