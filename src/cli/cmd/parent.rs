//! `jired parent` — move sub-tasks under a different parent.
//!
//! The frontmatter has always carried `parent:`, so this is the same edit a
//! text editor makes, on any number of issues at once: re-parenting the four
//! sub-tasks somebody filed under the wrong story is the case it exists for.
//!
//! Staged like `sprint add`, `label` and `assign`: the file is written,
//! `status` and `diff` show it, `push` sends it — or `--push` does, now. What
//! `push` then does with it differs by deployment, and neither this command nor
//! the file has to know: Cloud sends a field update, Data Center drives the
//! same Move wizard its UI does.

use crate::cli::cmd::stage::{self, Wording};
use crate::cli::context::Context;
use crate::cli::GlobalArgs;
use crate::error::{ExitCode, JiredError, Result};
use crate::json::Out;
use clap::Args;

#[derive(Args, Debug, Clone, Default)]
pub struct ParentArgs {
    /// Sub-tasks to move: keys, or paths to their files
    #[arg(value_name = "ISSUES", required = true, value_delimiter = ',')]
    pub issues: Vec<String>,

    /// The issue they should hang off: a key, or the path to its file
    #[arg(long, value_name = "ISSUE", required = true)]
    pub to: String,

    /// Apply immediately instead of leaving the change staged
    #[arg(long)]
    pub push: bool,

    /// With --push, show the API call without sending it
    #[arg(long, requires = "push")]
    pub dry_run: bool,
}

pub fn run(g: &GlobalArgs, args: &ParentArgs, out: Out) -> Result<ExitCode> {
    let mut ctx = Context::open(g, out)?;
    let to = crate::project::naming::key_from_input(&args.to);
    if !crate::doc::is_issue_key(&to) {
        return Err(
            JiredError::usage(format!("`{}` is not an issue key", args.to))
                .with_detail("parent", args.to.clone())
                .with_hint("pass --to PROJ-142, or the path to that issue's file"),
        );
    }

    ctx.lock()?;
    let staged = {
        let to = to.clone();
        stage::stage(&mut ctx, &args.issues, out, move |doc| {
            // An issue cannot be its own parent, and Jira answers that with a
            // page rather than a message. It is also the shape a mistyped
            // `--to` takes when the list was pasted from somewhere.
            if doc.frontmatter.key.eq_ignore_ascii_case(&to) {
                return Err(JiredError::usage(format!("{to} cannot be its own parent")));
            }
            if doc.frontmatter.parent.as_deref() == Some(to.as_str()) {
                return Ok(false);
            }
            doc.frontmatter.parent = Some(to.clone());
            Ok(true)
        })?
    };

    let outcome = stage::finish(
        &mut ctx,
        staged,
        &Wording {
            what: &to,
            verb: "moved",
            already: &format!("is already under {to}"),
        },
        args.push,
        args.dry_run,
        out,
        serde_json::json!({ "parent": to }),
    )?;
    out.emit("parent", &outcome.doc);
    Ok(outcome.exit)
}
