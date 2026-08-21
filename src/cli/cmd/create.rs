//! `jired create` [M3.5] — a local `NEW-*` issue that becomes real on push.
//!
//! Creating locally first keeps one rule true everywhere: the working copy is
//! authored offline and `push` is the only thing that talks to the server. The
//! temporary key is replaced with the real one once the server assigns it.
//!
//! Everything the issue needs can come from flags, and with a summary among
//! them nothing is asked. Without one there are three rungs, in the order the
//! terminal can support them: the full-screen form [M4.7], line prompts, and —
//! when neither is allowed — exit 7 naming what is missing.

use crate::cli::cmd::prompt::Ui;
use crate::cli::context::Context;
use crate::cli::GlobalArgs;
use crate::doc::{Frontmatter, IssueDoc, SyncMeta};
use crate::error::{ExitCode, JiredError, Result};
use crate::json::Out;
use crate::sync::Engine;
use crate::tui::catalog::Catalog;
use crate::tui::create::{CreateScreen, Draft, Seed};
use clap::Args;

/// Issue type used when neither a flag, a template, nor a person says otherwise.
const DEFAULT_TYPE: &str = "Task";

#[derive(Args, Debug, Clone, Default)]
pub struct CreateArgs {
    /// Issue type (Story, Bug, Task, …) [default: Task]
    #[arg(long)]
    pub r#type: Option<String>,
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
    /// Fill in the form even when the flags would have been enough
    #[arg(long, short = 'i')]
    pub interactive: bool,
}

pub fn run(g: &GlobalArgs, args: &CreateArgs, out: Out) -> Result<ExitCode> {
    let mut ctx = Context::open(g, out)?;
    ctx.lock()?;

    let engine = Engine::open(&ctx)?;
    let key = new_key(&engine)?;
    // The template is the starting point for both the form and the flags-only
    // path, so it is read before either decides anything.
    let mut doc = template(&ctx, args)?;
    let seed = seed(&mut ctx, args, &doc)?;

    let Some(draft) = draft(&mut ctx, &engine, &key, args, seed)? else {
        return Err(JiredError::generic("cancelled — no issue was created"));
    };

    doc.frontmatter.key = key.clone();
    doc.frontmatter.summary = draft.summary.clone();
    doc.frontmatter.issue_type = draft.issue_type.clone();
    doc.frontmatter.parent = draft.parent.clone();
    doc.frontmatter.priority = draft.priority.clone();
    doc.frontmatter.labels = draft.labels.clone();
    doc.description = draft.description.clone();

    let outcome = engine.projector.write(&doc, None)?;
    // Recording it now is what makes `status` list it and `push` find it.
    engine
        .state
        .record_new_issue(&key, &draft.project, &outcome.relative)?;

    out.say(format!(
        "created {key} in {} — run `jired push` to create it on the server",
        outcome.relative
    ));
    out.emit(
        "create",
        &serde_json::json!({
            "key": key,
            "file": outcome.relative,
            "project": draft.project,
            "type": draft.issue_type,
            "summary": draft.summary,
        }),
    );
    Ok(ExitCode::Success)
}

/// Everything already decided before anyone is asked anything: the flags, and
/// whatever the template supplied where a flag was absent.
fn seed(ctx: &mut Context, args: &CreateArgs, doc: &IssueDoc) -> Result<Seed> {
    let fm = &doc.frontmatter;
    Ok(Seed {
        project: ctx.resolver.get_quiet("workspace.project")?,
        issue_type: args.r#type.clone().or_else(|| some(&fm.issue_type)),
        summary: args.summary.clone().or_else(|| some(&fm.summary)),
        description: args.description.clone().or_else(|| some(&doc.description)),
        parent: args.parent.clone().or_else(|| fm.parent.clone()),
        priority: fm.priority.clone(),
        labels: fm.labels.clone(),
    })
}

/// Resolve the seed into a complete issue, asking for what is missing.
///
/// `None` means the user backed out.
fn draft(
    ctx: &mut Context,
    engine: &Engine,
    key: &str,
    args: &CreateArgs,
    seed: Seed,
) -> Result<Option<Draft>> {
    let interactive = ctx.resolver.interactive();
    if args.interactive && !interactive {
        return Err(
            JiredError::missing_value("--interactive needs a terminal to ask on")
                .with_hint("supply --summary and the other fields as flags instead"),
        );
    }

    // With a summary in hand there is nothing that has to be asked, so nothing
    // is: a scripted `create` behaves the same whether or not a terminal is
    // attached. `--interactive` is how someone opts back into the form.
    if interactive && (args.interactive || seed.summary.is_none()) {
        let catalog = Catalog::from_rows(&engine.state.issues()?, seed.project.as_deref());
        if crate::tui::available() {
            return crate::tui::run(&mut CreateScreen::new(key, &catalog, &seed));
        }
        // No alternate screen here — over a dumb terminal the line prompts are
        // the whole interface rather than a consolation prize.
        let ui = Ui::new(true, !ctx.out.json && !ctx.out.quiet);
        return ask(&ui, &catalog, seed).map(Some);
    }

    let summary = seed.summary.ok_or_else(|| {
        JiredError::missing_value("a summary is required")
            .with_hint("pass --summary \"...\", or run `jired create` in a terminal")
    })?;
    Ok(Some(Draft {
        project: ctx.resolver.require("workspace.project")?.to_uppercase(),
        issue_type: seed.issue_type.unwrap_or_else(|| DEFAULT_TYPE.to_string()),
        summary,
        description: seed.description.unwrap_or_default(),
        priority: seed.priority,
        labels: seed.labels,
        parent: seed.parent.map(|p| p.to_uppercase()),
    }))
}

/// The line-based form, for terminals the full-screen one cannot run on.
///
/// It asks for the two things an issue cannot do without and takes the rest
/// from the seed; anything more would be a worse version of the screen.
fn ask(ui: &Ui, catalog: &Catalog, seed: Seed) -> Result<Draft> {
    ui.title("new issue");
    let summary = match seed.summary {
        Some(s) => s,
        None => ui.ask("Summary", Some("one line saying what needs doing"), None)?,
    };

    let types = if catalog.types.is_empty() {
        vec![DEFAULT_TYPE.to_string()]
    } else {
        catalog.types.clone()
    };
    let issue_type = match seed.issue_type {
        Some(t) => t,
        None => {
            let chosen = ui.choose("Type", &types, 0)?;
            types[chosen].clone()
        }
    };

    let project = match seed.project {
        Some(p) => p,
        None if catalog.projects.len() == 1 => catalog.projects[0].clone(),
        None => ui.ask(
            "Project",
            Some("the key of the project to create it in"),
            None,
        )?,
    };

    Ok(Draft {
        project: project.to_uppercase(),
        issue_type,
        summary,
        description: seed.description.unwrap_or_default(),
        priority: seed.priority,
        labels: seed.labels,
        parent: seed.parent.map(|p| p.to_uppercase()),
    })
}

fn some(value: &str) -> Option<String> {
    Some(value.trim())
        .filter(|v| !v.is_empty())
        .map(str::to_string)
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
