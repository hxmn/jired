//! `jired set` — write any editable field on any number of issues.
//!
//! The general form of `assign`, `label` and `sprint add`: those three are worth
//! their own spelling because they are what people do most, but a flag per field
//! does not scale to `components`, `story_points` and an instance's own custom
//! fields. Without this, setting one of those across six issues meant a script
//! rewriting jired's YAML by hand — the caller parsing the file format, which
//! breaks the first time the format moves.
//!
//! Staged like the rest: the file is written, `status` and `diff` show it,
//! `push` sends it — or `--push` does, now.

use crate::cli::cmd::stage::{self, Wording};
use crate::cli::context::Context;
use crate::cli::GlobalArgs;
use crate::error::{ExitCode, JiredError, Result};
use crate::json::Out;
use clap::Args;

#[derive(Args, Debug, Clone, Default)]
pub struct SetArgs {
    /// Field to write: a frontmatter name, or a `customfield_*` id
    #[arg(long, value_name = "FIELD", required = true)]
    pub field: String,

    /// The value; empty clears the field. A list takes commas: `a,b`
    #[arg(
        long,
        value_name = "VALUE",
        required = true,
        allow_hyphen_values = true
    )]
    pub value: String,

    /// Issues to write it on: keys, or paths to their files
    #[arg(value_name = "ISSUES", required = true, value_delimiter = ',')]
    pub issues: Vec<String>,

    /// Apply immediately instead of leaving the change staged
    #[arg(long)]
    pub push: bool,

    /// Show the API calls a push would make, send nothing, and leave the
    /// files as they were
    #[arg(long)]
    pub dry_run: bool,
}

pub fn run(g: &GlobalArgs, args: &SetArgs, out: Out) -> Result<ExitCode> {
    let field = args.field.trim().to_string();
    // Both checks happen before a single file is opened: a name nobody can
    // write is a mistake about the command, not about one of the issues, and
    // finding out on the fourth file would leave three already edited.
    //
    // Identity first, because `key` would otherwise fall through to the
    // catch-all and be reported as unknown when the truth is that it is known
    // and refused.
    if crate::doc::frontmatter::is_identity(&field) {
        return Err(
            JiredError::usage(format!("`{field}` identifies the issue and cannot be set"))
                .with_hint("`jired create` makes an issue; `key` and `id` are the server's"),
        );
    }
    crate::doc::Frontmatter::default().set_field(&field, &args.value)?;

    let mut ctx = Context::open(g, out)?;
    ctx.lock()?;
    let (name, value) = (field.clone(), args.value.trim().to_string());
    let staged = stage::stage(&mut ctx, &args.issues, out, move |doc| {
        doc.frontmatter.set_field(&name, &value)
    })?;

    let shown = match args.value.trim() {
        "" => format!("`{field}` cleared"),
        v => format!("`{field}` = `{v}`"),
    };
    let outcome = stage::finish(
        &mut ctx,
        staged,
        &Wording {
            what: &shown,
            verb: "set",
            already: &format!("already has {shown}"),
        },
        args.push,
        args.dry_run,
        out,
        serde_json::json!({ "field": field, "value": args.value }),
    )?;
    out.emit("set", &outcome.doc);
    Ok(outcome.exit)
}
