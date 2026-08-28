//! `jired fields` — what this instance's fields are called.
//!
//! `custom:` keys a field by its Jira id, which is the only name that is stable
//! across instances and the only one nobody can read. Finding out what
//! `customfield_10015` is meant opening `.jired/state.db` in `sqlite3` and
//! reading `field_meta` — an agent reverse-engineering a private schema to
//! answer a question the tool already knows, which is the sort of thing that
//! becomes a habit and then becomes a bug report about a schema that changed.

use crate::cli::context::Context;
use crate::cli::GlobalArgs;
use crate::error::{ExitCode, Result};
use crate::json::Out;
use crate::style::{self, CYAN, DIM};
use crate::sync::Engine;
use clap::Args;

#[derive(Args, Debug, Clone, Default)]
pub struct FieldsArgs {
    /// Only fields whose name or id contains this text
    #[arg(value_name = "TEXT")]
    pub filter: Option<String>,

    /// Only the instance's custom fields
    #[arg(long)]
    pub custom: bool,
}

pub fn run(g: &GlobalArgs, args: &FieldsArgs, out: Out) -> Result<ExitCode> {
    let mut ctx = Context::open(g, out)?;
    let engine = Engine::open(&mut ctx)?;

    let mut fields = engine.state.field_meta()?;
    fields.sort_by_key(|f| f.name.to_lowercase());
    let wanted: Vec<_> = fields
        .into_iter()
        .filter(|f| !args.custom || f.id.starts_with("customfield_"))
        .filter(|f| match &args.filter {
            None => true,
            Some(text) => {
                let text = text.to_lowercase();
                f.name.to_lowercase().contains(&text) || f.id.to_lowercase().contains(&text)
            }
        })
        .collect();

    if wanted.is_empty() {
        out.say(match &args.filter {
            Some(text) => format!("no field here is called `{text}`"),
            None => "this workspace has no field definitions yet".to_string(),
        });
        out.hint("run `jired fetch` to pick them up");
    }
    let width = crate::cli::cmd::list::column(wanted.iter().map(|f| f.id.len()), 22);
    for f in &wanted {
        let kind = f.schema_type.as_deref().unwrap_or("");
        out.say(format!(
            "{} {} {}",
            style::out(CYAN, format!("{:<width$}", f.id)),
            style::out(DIM, format!("{kind:<10}")),
            f.name
        ));
    }

    out.emit(
        "fields",
        &serde_json::json!({ "fields": wanted, "count": wanted.len() }),
    );
    Ok(ExitCode::Success)
}
