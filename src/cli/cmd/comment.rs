//! `jired comment` [M3.2] — append a comment without hand-editing the file.

use crate::cli::cmd::edit::{edit_issue, message_or_stdin, push_one};
use crate::cli::context::Context;
use crate::cli::GlobalArgs;
use crate::doc::{CommentBlock, CommentId};
use crate::error::{ExitCode, Result};
use crate::json::Out;
use clap::Args;

#[derive(Args, Debug, Clone)]
pub struct CommentArgs {
    /// Issue to comment on
    pub issue: String,
    /// Comment body (reads stdin when omitted)
    #[arg(short = 'm', long = "message")]
    pub message: Option<String>,
    /// Send the comment immediately instead of leaving it staged
    #[arg(long)]
    pub push: bool,
}

pub fn run(g: &GlobalArgs, args: &CommentArgs, out: Out) -> Result<ExitCode> {
    let mut ctx = Context::open(g, out)?;
    ctx.lock()?;

    let body = message_or_stdin(&args.message, "comment body")?;
    let key = args.issue.to_uppercase();

    let file = edit_issue(&mut ctx, &key, |doc| {
        doc.comments.push(CommentBlock {
            id: CommentId::New,
            author: String::new(),
            timestamp: String::new(),
            note: String::new(),
            body: body.clone(),
        });
        Ok(())
    })?;

    let pushed = if args.push {
        Some(push_one(&mut ctx, &key)?)
    } else {
        None
    };

    match &pushed {
        Some(report) if report.results.iter().all(|r| r.ok) => {
            out.say(format!("commented on {key}"))
        }
        Some(_) => out.say(format!("staged a comment on {key}; the push failed")),
        None => out.say(format!(
            "staged a comment on {key} in {file} — run `jired push` to send it"
        )),
    }

    out.emit(
        "comment",
        &serde_json::json!({
            "key": key,
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
