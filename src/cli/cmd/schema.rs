//! `jired schema` — print the JSON Schemas agents validate against [M5.1].

use crate::error::{ExitCode, JiredError, Result};
use crate::json::Out;
use clap::Args;

#[derive(Args, Debug)]
pub struct SchemaArgs {
    /// Schema to print (omit to list all)
    pub name: Option<String>,
}

/// Embedded schema documents, keyed by their `jired/<name>/1` tag.
pub const SCHEMAS: &[(&str, &str)] = &[
    ("error", include_str!("../../../schemas/error.json")),
    (
        "frontmatter",
        include_str!("../../../schemas/frontmatter.json"),
    ),
    ("init", include_str!("../../../schemas/init.json")),
    ("fetch", include_str!("../../../schemas/fetch.json")),
    ("pull", include_str!("../../../schemas/pull.json")),
    ("push", include_str!("../../../schemas/push.json")),
    ("diff", include_str!("../../../schemas/diff.json")),
    ("status", include_str!("../../../schemas/status.json")),
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
            println!("{text}");
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
                println!("{}", serde_json::to_string_pretty(&doc)?);
            } else {
                for (name, _) in SCHEMAS {
                    println!("{name}");
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
