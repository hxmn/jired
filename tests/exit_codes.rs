//! The exit-code table, held to everything that repeats it.
//!
//! `ExitCode::ALL` is the one written table; the error schema's enums, the
//! README's table and the specification's line used to be kept in step by
//! hand, and nothing noticed when one moved.

use jired::error::ExitCode;
use std::path::PathBuf;

fn read(name: &str) -> String {
    std::fs::read_to_string(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(name))
        .unwrap()
        .replace("\r\n", "\n")
}

#[test]
fn the_error_schema_lists_exactly_the_codes_and_kinds() {
    let schema: serde_json::Value = serde_json::from_str(&read("schemas/error.json")).unwrap();
    let codes: Vec<i64> = schema["properties"]["code"]["enum"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_i64().unwrap())
        .collect();
    let kinds: Vec<&str> = schema["properties"]["kind"]["enum"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    let failures: Vec<_> = ExitCode::ALL
        .iter()
        .filter(|(_, kind, _)| kind.is_some())
        .collect();
    assert_eq!(
        codes,
        failures
            .iter()
            .map(|(c, _, _)| i64::from(c.as_i32()))
            .collect::<Vec<_>>()
    );
    assert_eq!(
        kinds,
        failures
            .iter()
            .map(|(_, k, _)| k.unwrap())
            .collect::<Vec<_>>()
    );
}

#[test]
fn the_readme_table_is_the_table() {
    let readme = read("README.md");
    for (code, _, meaning) in ExitCode::ALL {
        let row = format!("| {} | {meaning} |", code.as_i32());
        assert!(readme.contains(&row), "README.md has no row `{row}`");
    }
}

#[test]
fn the_specification_names_every_code() {
    let spec = read("docs/plan/commands.md");
    let line = spec
        .lines()
        .skip_while(|l| !l.starts_with("**Exit codes:**"))
        .take(3)
        .collect::<Vec<_>>()
        .join(" ");
    for (code, kind, _) in ExitCode::ALL {
        let name = kind.unwrap_or("ok").replace('_', " ");
        let short = name.split(' ').next().unwrap();
        assert!(
            line.contains(&format!("`{}`", code.as_i32())) && line.contains(short),
            "commands.md's exit-code line lacks {} ({name}): {line}",
            code.as_i32()
        );
    }
}
