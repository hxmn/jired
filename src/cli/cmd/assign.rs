//! `jired assign` [M3.6].
//!
//! Staged like `label` and `set`: the files are written, `status` and `diff`
//! show it, `push` sends it — or `--push` does, now. Several issues at once,
//! because handing work over is rarely about one ticket: `jired list --keys`
//! exists to be piped into exactly this.

use crate::cli::cmd::stage::{self, Wording};
use crate::cli::context::Context;
use crate::cli::GlobalArgs;
use crate::doc::frontmatter::format_user;
use crate::error::{ExitCode, JiredError, Result};
use crate::json::Out;
use crate::store::session::SessionStore;
use crate::sync::Engine;
use clap::Args;

/// One of these three is required; clap says so in `--help` and in the error,
/// rather than the command discovering it at run time.
#[derive(Args, Debug, Clone)]
#[command(group = clap::ArgGroup::new("who").required(true).args(["me", "user", "none"]))]
pub struct AssignArgs {
    /// Issues to assign: keys, or paths to their files
    #[arg(value_name = "ISSUES", required = true, value_delimiter = ',')]
    pub issues: Vec<String>,
    /// Assign to the authenticated account
    #[arg(long)]
    pub me: bool,
    /// Assign to a user (name, email, or account id)
    #[arg(long)]
    pub user: Option<String>,
    /// Remove the current assignee
    #[arg(long)]
    pub none: bool,
    /// Apply immediately instead of leaving the change staged
    #[arg(long)]
    pub push: bool,
    /// With --push, show the API call without sending it
    #[arg(long, requires = "push")]
    pub dry_run: bool,
}

pub fn run(g: &GlobalArgs, args: &AssignArgs, out: Out) -> Result<ExitCode> {
    let mut ctx = Context::open(g, out)?;

    let assignee = resolve_assignee(&mut ctx, args, out)?;

    ctx.lock()?;
    let value = assignee.clone();
    let staged = stage::stage(&mut ctx, &args.issues, out, move |doc| {
        let before = doc.frontmatter.assignee.clone();
        doc.frontmatter.assignee = value.clone();
        Ok(before != doc.frontmatter.assignee)
    })?;

    let who = assignee.as_deref().unwrap_or("nobody").to_string();
    let already = match &assignee {
        Some(w) => format!("is already assigned to {w}"),
        None => "already has no assignee".to_string(),
    };
    let outcome = stage::finish(
        &mut ctx,
        staged,
        &Wording {
            what: &who,
            verb: "assigned",
            already: &already,
        },
        args.push,
        args.dry_run,
        out,
        serde_json::json!({ "assignee": assignee }),
    )?;
    out.emit("assign", &outcome.doc);
    Ok(outcome.exit)
}

/// Work out the `@[Name](id)` value to write, or `None` to unassign.
fn resolve_assignee(ctx: &mut Context, args: &AssignArgs, out: Out) -> Result<Option<String>> {
    if args.none {
        return Ok(None);
    }

    let session = SessionStore::open(&ctx.workspace)?;
    let account = session.require_account()?;

    if args.me {
        let id = account.account_id.clone().ok_or_else(|| {
            JiredError::auth("the stored account has no id")
                .with_hint("run `jired whoami --refresh`")
        })?;
        return Ok(format_user(Some(&crate::api::UserRef {
            id,
            display_name: account.display_name.clone().unwrap_or_else(|| "me".into()),
        })));
    }

    let query = args.user.as_ref().ok_or_else(|| {
        JiredError::usage("no assignee given").with_hint("pass --me, --user <name>, or --none")
    })?;

    let engine = Engine::open(ctx)?;
    let client = engine.client(ctx)?;
    // Scoped to the issue: the question is who can be assigned *this*, which is
    // a smaller set than who exists, and offering a name the server will then
    // refuse is worse than offering none. With several issues the first one
    // scopes the search — one person is being written everywhere, and an issue
    // that person cannot hold gets the server's own refusal at push time.
    let key = crate::project::naming::key_from_input(&args.issues[0]);
    let matches =
        crate::api::block_on(client.search_users(query, crate::api::UserScope::Issue(&key)))?;

    match matches.len() {
        0 => Err(JiredError::usage(format!("no user matches `{query}`"))
            .with_detail("query", query.clone())),
        1 => Ok(format_user(matches.first())),
        _ => {
            // Picking one silently could assign the wrong person, which is
            // exactly the sort of mistake nobody reviews.
            let names: Vec<String> = matches
                .iter()
                .map(|u| format!("{} ({})", u.display_name, u.id))
                .collect();
            for n in &names {
                out.say(format!("  {n}"));
            }
            Err(
                JiredError::usage(format!("`{query}` matches {} users", matches.len()))
                    .with_detail("matches", serde_json::json!(names))
                    .with_hint("pass --user with the account id shown above"),
            )
        }
    }
}
