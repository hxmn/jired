//! `jired create` [M3.5] — a local `NEW-*` issue that becomes real on push.
//!
//! Creating locally first keeps one rule true everywhere: the working copy is
//! authored offline and `push` is the only thing that talks to the server. The
//! temporary key is replaced with the real one once the server assigns it.

use crate::cli::context::Context;
use crate::cli::GlobalArgs;
use crate::doc::{Frontmatter, IssueDoc, SyncMeta};
use crate::error::{ExitCode, JiredError, Result};
use crate::json::Out;
use crate::sync::Engine;
use clap::Args;

#[derive(Args, Debug, Clone)]
pub struct CreateArgs {
    /// Issue type (Story, Bug, Task, …)
    #[arg(long, default_value = "Task")]
    pub r#type: String,
    /// Template file under .jired/templates
    #[arg(long)]
    pub template: Option<String>,
    /// Issue summary
    #[arg(long, short = 's')]
    pub summary: Option<String>,
    /// Parent issue key for a subtask
    #[arg(long)]
    pub parent: Option<String>,
    /// Description body
    #[arg(long, short = 'd')]
    pub description: Option<String>,
}

pub fn run(g: &GlobalArgs, args: &CreateArgs, out: Out) -> Result<ExitCode> {
    let mut ctx = Context::open(g, out)?;
    ctx.lock()?;

    let summary = args.summary.clone().ok_or_else(|| {
        JiredError::missing_value("a summary is required").with_hint("pass --summary \"...\"")
    })?;
    let project = ctx.resolver.require("workspace.project")?;
    let engine = Engine::open(&ctx)?;

    let key = new_key(&engine)?;
    let mut doc = template(&ctx, args)?;
    doc.frontmatter.key = key.clone();
    doc.frontmatter.summary = summary.clone();
    doc.frontmatter.issue_type = args.r#type.clone();
    if let Some(parent) = &args.parent {
        doc.frontmatter.parent = Some(parent.to_uppercase());
    }
    if let Some(description) = &args.description {
        doc.description = description.clone();
    }

    let outcome = engine.projector.write(&doc, None)?;
    // Recording it now is what makes `status` list it and `push` find it.
    engine
        .state
        .record_new_issue(&key, &project, &outcome.relative)?;

    out.say(format!(
        "created {key} in {} — run `jired push` to create it on the server",
        outcome.relative
    ));
    out.emit(
        "create",
        &serde_json::json!({
            "key": key,
            "file": outcome.relative,
            "project": project,
            "type": args.r#type,
            "summary": summary,
        }),
    );
    Ok(ExitCode::Success)
}

/// A short random key that cannot collide with a real one.
///
/// Short hex rather than a UUID: it appears in a filename, and it only has to
/// be unique within one workspace (decision D14).
fn new_key(engine: &Engine) -> Result<String> {
    use rand::Rng;
    let existing: std::collections::HashSet<String> =
        engine.state.issues()?.into_iter().map(|r| r.key).collect();

    let mut rng = rand::rng();
    for _ in 0..64 {
        let candidate = format!("NEW-{:06x}", rng.random_range(0..0xff_ffffu32));
        if !existing.contains(&candidate) {
            return Ok(candidate);
        }
    }
    Err(JiredError::generic("cannot find an unused temporary key"))
}

fn template(ctx: &Context, args: &CreateArgs) -> Result<IssueDoc> {
    let Some(name) = &args.template else {
        return Ok(blank());
    };
    let path = ctx.workspace.templates_dir().join(format!("{name}.md"));
    let text = std::fs::read_to_string(&path).map_err(|e| {
        JiredError::usage(format!("cannot read template {}: {e}", path.display()))
            .with_hint("templates live in .jired/templates/<name>.md")
    })?;
    // A template is a partial file: it has no key yet, so the identity fields
    // are filled in afterwards rather than parsed.
    let with_key = if text.starts_with("---") {
        text
    } else {
        format!("---\nkey: NEW-000000\nid: ''\njired: {{}}\n---\n\n{text}")
    };
    IssueDoc::parse(&with_key)
        .map_err(|e| JiredError::usage(format!("template {name}: {}", e.message)))
}

fn blank() -> IssueDoc {
    IssueDoc {
        frontmatter: Frontmatter {
            id: String::new(),
            status: String::new(),
            jired: SyncMeta::default(),
            ..Default::default()
        },
        heading: String::new(),
        preamble: String::new(),
        description: String::new(),
        checklist: Vec::new(),
        comments: Vec::new(),
        attachments: Vec::new(),
        extra_sections: Vec::new(),
    }
}
