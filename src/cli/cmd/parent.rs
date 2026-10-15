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
    let to = crate::project::naming::key_from_input(&args.to)?;
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

    warn_about_the_credential(&ctx, &staged, out)?;

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

/// Say now that `push` will refuse this, rather than letting it be discovered
/// on the way out.
///
/// Data Center changes a sub-task's parent through a Jira web page, and a
/// personal access token authenticates the REST API and nothing else. The edit
/// itself is fine — it is what the file should say, and moving the sub-task in
/// the UI makes it true — so this is a warning and not a refusal. It is also
/// only about sub-tasks: an ordinary issue's `parent` is a field update, which
/// the same token sends perfectly well, and warning about those would be
/// noise that teaches people to ignore the warning.
fn warn_about_the_credential(ctx: &Context, staged: &[stage::Staged], out: Out) -> Result<()> {
    let session = crate::store::session::SessionStore::open(&ctx.workspace)?;
    let account = session.require_account()?;
    if account.deployment != crate::api::Deployment::DataCenter
        || account.auth_kind.opens_web_session()
    {
        return Ok(());
    }

    let state = crate::store::state::StateStore::open(&ctx.workspace)?;
    let sub_tasks: Vec<&str> = staged
        .iter()
        .filter(|s| s.changed)
        .filter(|s| {
            state
                .issue(&s.key)
                .ok()
                .flatten()
                .and_then(|row| row.base)
                .is_some_and(|base| {
                    base.raw
                        .pointer("/fields/issuetype/subtask")
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(false)
                })
        })
        .map(|s| s.key.as_str())
        .collect();
    if sub_tasks.is_empty() {
        return Ok(());
    }

    out.warn(format!(
        "{}: a sub-task's parent is changed through a Jira web page, which a {} \
         credential cannot open a session for — `push` will refuse it",
        sub_tasks.join(", "),
        account.auth_kind.as_str()
    ));
    out.hint(
        "re-authenticate with `jired init --force --auth basic` if the instance \
         allows it, or move the sub-task in Jira and run `jired pull`",
    );
    Ok(())
}
