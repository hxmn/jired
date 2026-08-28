//! `jired schema` — print the JSON Schemas agents validate against [M5.1].

use crate::error::{ExitCode, JiredError, Result};
use crate::json::Out;
use clap::Args;

#[derive(Args, Debug)]
pub struct SchemaArgs {
    /// Schema to print (omit to list all)
    #[arg(value_parser = name_parser())]
    pub name: Option<String>,
}

/// Every schema name, so `--help` and shell completion list them.
fn name_parser() -> clap::builder::PossibleValuesParser {
    clap::builder::PossibleValuesParser::new(SCHEMAS.iter().map(|(n, _)| *n))
}

/// Embedded schema documents, keyed by their `jired/<name>/1` tag.
pub const SCHEMAS: &[(&str, &str)] = &[
    ("assign", include_str!("../../../schemas/assign.json")),
    ("attach", include_str!("../../../schemas/attach.json")),
    (
        "attachments",
        include_str!("../../../schemas/attachments.json"),
    ),
    (
        "attachments-pull",
        include_str!("../../../schemas/attachments-pull.json"),
    ),
    ("boards", include_str!("../../../schemas/boards.json")),
    ("clean", include_str!("../../../schemas/clean.json")),
    ("comment", include_str!("../../../schemas/comment.json")),
    ("config", include_str!("../../../schemas/config.json")),
    (
        "config-list",
        include_str!("../../../schemas/config-list.json"),
    ),
    ("create", include_str!("../../../schemas/create.json")),
    ("diff", include_str!("../../../schemas/diff.json")),
    ("edit", include_str!("../../../schemas/edit.json")),
    ("doctor", include_str!("../../../schemas/doctor.json")),
    ("error", include_str!("../../../schemas/error.json")),
    ("fetch", include_str!("../../../schemas/fetch.json")),
    (
        "frontmatter",
        include_str!("../../../schemas/frontmatter.json"),
    ),
    ("init", include_str!("../../../schemas/init.json")),
    (
        "init-agents",
        include_str!("../../../schemas/init-agents.json"),
    ),
    ("label", include_str!("../../../schemas/label.json")),
    ("log", include_str!("../../../schemas/log.json")),
    ("logwork", include_str!("../../../schemas/logwork.json")),
    ("open", include_str!("../../../schemas/open.json")),
    ("projects", include_str!("../../../schemas/projects.json")),
    ("pull", include_str!("../../../schemas/pull.json")),
    ("push", include_str!("../../../schemas/push.json")),
    (
        "report-time",
        include_str!("../../../schemas/report-time.json"),
    ),
    ("reset", include_str!("../../../schemas/reset.json")),
    ("resolve", include_str!("../../../schemas/resolve.json")),
    ("search", include_str!("../../../schemas/search.json")),
    ("set", include_str!("../../../schemas/set.json")),
    (
        "sprint-create",
        include_str!("../../../schemas/sprint-create.json"),
    ),
    (
        "sprint-move",
        include_str!("../../../schemas/sprint-move.json"),
    ),
    ("sprints", include_str!("../../../schemas/sprints.json")),
    ("status", include_str!("../../../schemas/status.json")),
    (
        "transition",
        include_str!("../../../schemas/transition.json"),
    ),
    (
        "transitions",
        include_str!("../../../schemas/transitions.json"),
    ),
    ("version", include_str!("../../../schemas/version.json")),
    ("whoami", include_str!("../../../schemas/whoami.json")),
];

pub fn get(name: &str) -> Option<&'static str> {
    SCHEMAS.iter().find(|(n, _)| *n == name).map(|(_, s)| *s)
}

pub fn run(args: &SchemaArgs, out: Out) -> Result<ExitCode> {
    match &args.name {
        Some(name) => {
            let text = get(name).ok_or_else(|| {
                JiredError::usage(format!("unknown schema `{name}`"))
                    .with_detail("name", name.clone())
                    .with_hint(format!(
                        "available: {}",
                        SCHEMAS
                            .iter()
                            .map(|(n, _)| *n)
                            .collect::<Vec<_>>()
                            .join(", ")
                    ))
            })?;
            out.say(text);
        }
        None => {
            if out.json {
                let all: serde_json::Map<String, serde_json::Value> = SCHEMAS
                    .iter()
                    .map(|(n, s)| {
                        (
                            n.to_string(),
                            serde_json::from_str(s).unwrap_or(serde_json::Value::Null),
                        )
                    })
                    .collect();
                let doc = serde_json::json!({
                    "schema": "jired/schemas/1",
                    "schemas": all,
                });
                // Printed directly: this *is* the result document, and `emit`
                // would wrap it in a second envelope.
                let mut stdout = std::io::stdout().lock();
                let _ = std::io::Write::write_all(
                    &mut stdout,
                    serde_json::to_string_pretty(&doc)?.as_bytes(),
                );
                let _ = std::io::Write::write_all(&mut stdout, b"\n");
            } else {
                for (name, _) in SCHEMAS {
                    out.say(name);
                }
            }
        }
    }
    Ok(ExitCode::Success)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_embedded_schema_is_valid_json_schema() {
        for (name, text) in SCHEMAS {
            let value: serde_json::Value = serde_json::from_str(text)
                .unwrap_or_else(|e| panic!("schema {name} is not valid JSON: {e}"));
            jsonschema::validator_for(&value)
                .unwrap_or_else(|e| panic!("schema {name} is not a valid JSON Schema: {e}"));
            assert_eq!(
                value["$id"].as_str(),
                Some(format!("jired/{name}/1").as_str()),
                "schema {name} has a mismatched $id"
            );
        }
    }
}
