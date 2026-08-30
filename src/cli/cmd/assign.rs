//! `jired assign` [M3.6].

use crate::cli::cmd::edit::{edit_issue, push_one, report_push};
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
    /// Issue to assign: a key, or the path to its file
    pub issue: String,
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
    let key = crate::project::naming::key_from_input(&args.issue);

    let assignee = resolve_assignee(&mut ctx, args, &key, out)?;

    ctx.lock()?;
    let value = assignee.clone();
    let file = edit_issue(&mut ctx, &key, |doc| {
        doc.frontmatter.assignee = value.clone();
        Ok(())
    })?;

    let pushed = if args.push {
        let report = push_one(&mut ctx, &key, args.dry_run)?;
        report_push(&report, out);
        Some(report)
    } else {
        None
    };

    let who = assignee.as_deref().unwrap_or("nobody");
    match &pushed {
        Some(r) if r.dry_run => {}
        Some(r) if r.results.iter().all(|x| x.ok) => out.say(format!("{key} assigned to {who}")),
        Some(_) => out.say(format!(
            "{key} → {who} is staged in {file}, but the push failed — \
             fix the cause and run `jired push`"
        )),
        None => out.say(format!(
            "staged {key} → {who} in {file} — run `jired push` to apply it"
        )),
    }
    out.emit(
        "assign",
        &serde_json::json!({
            "key": key,
            "assignee": assignee,
            "file": file,
            "pushed": pushed.is_some(),
            "push": pushed,
        }),
    );
    Ok(match pushed {
        Some(report) => report.exit_code(),
        None => ExitCode::Success,
    })
}

/// Work out the `@[Name](id)` value to write, or `None` to unassign.
fn resolve_assignee(
    ctx: &mut Context,
    args: &AssignArgs,
    key: &str,
    out: Out,
) -> Result<Option<String>> {
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
        JiredError::usage("no assignee given")
            .with_detail("issue", args.issue.clone())
            .with_hint("pass --me, --user <name>, or --none")
    })?;

    let engine = Engine::open(ctx)?;
    let client = engine.client(ctx)?;
    // Scoped to the issue: the question is who can be assigned *this*, which is
    // a smaller set than who exists, and offering a name the server will then
    // refuse is worse than offering none.
    let matches =
        crate::api::block_on(client.search_users(query, crate::api::UserScope::Issue(key)))?;

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
