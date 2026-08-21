//! `jired attach` and `jired attachments …` [M3.3].

use crate::cli::context::Context;
use crate::cli::GlobalArgs;
use crate::error::{ExitCode, JiredError, Result};
use crate::json::Out;
use crate::sync::attachments::{self, STUB_SUFFIX};
use crate::sync::Engine;
use clap::{Args, Subcommand};
use std::path::PathBuf;

#[derive(Args, Debug, Clone)]
pub struct AttachArgs {
    pub issue: String,
    /// Files to stage for upload
    #[arg(required = true)]
    pub files: Vec<PathBuf>,
    /// Upload immediately instead of leaving the files staged
    #[arg(long)]
    pub push: bool,
}

#[derive(Subcommand, Debug)]
pub enum AttachmentsCmd {
    /// Download attachment contents, replacing local stubs
    Pull(AttachmentsPullArgs),
    /// List known attachments and whether their content is present
    List(AttachmentsListArgs),
}

#[derive(Args, Debug, Clone)]
pub struct AttachmentsPullArgs {
    /// Issues to download for (default: all)
    pub issues: Vec<String>,
    /// Skip attachments larger than this many bytes
    #[arg(long)]
    pub max_size: Option<u64>,
}

#[derive(Args, Debug, Clone)]
pub struct AttachmentsListArgs {
    pub issues: Vec<String>,
}

pub fn run(g: &GlobalArgs, args: &AttachArgs, out: Out) -> Result<ExitCode> {
    let mut ctx = Context::open(g, out)?;
    ctx.lock()?;
    let key = crate::project::naming::key_from_input(&args.issue);

    let engine = Engine::open(&ctx)?;
    let max_upload = ctx
        .resolver
        .get_u64("attachments.max_upload")?
        .unwrap_or(50 * 1024 * 1024);

    let files = engine.projector.scan()?;
    let found = files.get(&key).ok_or_else(|| {
        JiredError::usage(format!("{key} is not in this workspace"))
            .with_detail("issue", key.clone())
            .with_hint("run `jired pull` first, or check the key")
    })?;
    let dir = engine.projector.attachment_dir(&found.relative);
    std::fs::create_dir_all(&dir)?;

    let mut staged = Vec::new();
    for source in &args.files {
        // Read through the size check first: copying a file the server will
        // reject just leaves rubbish in the folder.
        let bytes = attachments::check_upload_size(source, max_upload)?;
        let name = source
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| JiredError::usage(format!("{} has no filename", source.display())))?;
        std::fs::write(dir.join(name), &bytes)?;
        staged.push(name.to_string());
        out.say(format!(
            "staged {name} ({})",
            crate::sync::render::human_size(bytes.len() as u64)
        ));
    }

    let pushed = if args.push {
        Some(crate::cli::cmd::edit::push_one(&mut ctx, &key)?)
    } else {
        out.say("run `jired push` to upload");
        None
    };

    out.emit(
        "attach",
        &serde_json::json!({
            "key": key,
            "staged": staged,
            "pushed": pushed.is_some(),
            "push": pushed,
        }),
    );
    Ok(match pushed {
        Some(report) => report.exit_code(),
        None => ExitCode::Success,
    })
}

pub fn run_attachments(g: &GlobalArgs, cmd: &AttachmentsCmd, out: Out) -> Result<ExitCode> {
    match cmd {
        AttachmentsCmd::List(args) => list(g, args, out),
        AttachmentsCmd::Pull(args) => pull(g, args, out),
    }
}

fn list(g: &GlobalArgs, args: &AttachmentsListArgs, out: Out) -> Result<ExitCode> {
    let ctx = Context::open(g, out)?;
    let engine = Engine::open(&ctx)?;
    let files = engine.projector.scan()?;
    let wanted: Vec<String> = args
        .issues
        .iter()
        .map(|k| crate::project::naming::key_from_input(k))
        .collect();

    let mut rows = Vec::new();
    for row in engine.state.issues()? {
        if !wanted.is_empty() && !wanted.contains(&row.key) {
            continue;
        }
        let Some(file) = files.get(&row.key) else {
            continue;
        };
        let dir = engine.projector.attachment_dir(&file.relative);
        for a in engine.state.attachments(&row.key)? {
            let present = dir.join(&a.filename).is_file();
            out.say(format!(
                "{:<14} {:<28} {:>10}  {}",
                row.key,
                a.filename,
                crate::sync::render::human_size(a.size),
                if present { "downloaded" } else { "stub" }
            ));
            rows.push(serde_json::json!({
                "key": row.key,
                "id": a.id,
                "filename": a.filename,
                "size": a.size,
                "mime": a.mime,
                "downloaded": present,
            }));
        }
    }
    out.emit("attachments", &serde_json::json!({ "attachments": rows }));
    Ok(ExitCode::Success)
}

fn pull(g: &GlobalArgs, args: &AttachmentsPullArgs, out: Out) -> Result<ExitCode> {
    let mut ctx = Context::open(g, out)?;
    ctx.lock()?;

    let engine = Engine::open(&ctx)?;
    let client = engine.client(&mut ctx)?;
    let files = engine.projector.scan()?;
    let wanted: Vec<String> = args
        .issues
        .iter()
        .map(|k| crate::project::naming::key_from_input(k))
        .collect();
    let max = args.max_size.unwrap_or(u64::MAX);

    let mut downloaded = Vec::new();
    let mut skipped = Vec::new();

    for row in engine.state.issues()? {
        if !wanted.is_empty() && !wanted.contains(&row.key) {
            continue;
        }
        let Some(file) = files.get(&row.key) else {
            continue;
        };
        let dir = engine.projector.attachment_dir(&file.relative);

        for a in engine.state.attachments(&row.key)? {
            if dir.join(&a.filename).is_file() {
                continue;
            }
            if a.size > max {
                skipped.push(serde_json::json!({
                    "key": row.key, "filename": a.filename, "size": a.size
                }));
                continue;
            }
            attachments::download_one(&engine.state, client.as_ref(), &row.key, &a, &dir)?;
            // The stub has served its purpose once the content is there.
            let _ = std::fs::remove_file(attachments::stub_path(&dir, &a.filename));
            out.say(format!("{:<14} {}", row.key, a.filename));
            downloaded.push(serde_json::json!({
                "key": row.key, "filename": a.filename, "size": a.size
            }));
        }
    }

    out.say(format!(
        "downloaded {} attachment(s){}",
        downloaded.len(),
        if skipped.is_empty() {
            String::new()
        } else {
            format!(", skipped {} over --max-size", skipped.len())
        }
    ));
    out.emit(
        "attachments-pull",
        &serde_json::json!({ "downloaded": downloaded, "skipped": skipped }),
    );
    Ok(ExitCode::Success)
}

/// True for a placeholder file rather than real content.
pub fn is_stub(name: &str) -> bool {
    name.ends_with(STUB_SUFFIX)
}
