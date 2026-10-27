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
    /// Template to start from: a name under .jired/templates, or a path
    #[arg(long)]
    pub template: Option<String>,
    /// Model the new issue on an existing one
    ///
    /// Copies type, priority, labels, assignee, components and custom fields.
    /// Never its identity, status, description, comments or worklogs — those
    /// belong to the issue that already exists.
    #[arg(long, value_name = "ISSUE")]
    pub like: Option<String>,
    /// Issue summary (asked for when omitted at a terminal)
    #[arg(long, short = 's')]
    pub summary: Option<String>,
    /// Parent issue key for a subtask
    #[arg(long)]
    pub parent: Option<String>,
    /// Description body (opens $EDITOR when omitted at a terminal)
    #[arg(long, short = 'd')]
    pub description: Option<String>,
    /// Assign it: a name, an email, or an account id
    #[arg(long)]
    pub assignee: Option<String>,
    /// Sprint to put it in: a name, an id, or the number that ends its name
    #[arg(long)]
    pub sprint: Option<String>,
    /// Labels, comma-separated
    #[arg(long, value_name = "LABELS", value_delimiter = ',')]
    pub labels: Vec<String>,
    /// Priority (Highest, High, Medium, …)
    #[arg(long)]
    pub priority: Option<String>,
    /// Component; repeat the flag for several
    #[arg(long = "component", value_name = "COMPONENT")]
    pub components: Vec<String>,
    /// Epic this belongs under
    #[arg(long)]
    pub epic: Option<String>,
    /// Any other field: `--set story_points=5`, `--set customfield_10456=Platform`
    ///
    /// Repeatable, and the escape hatch for the fields with no flag of their
    /// own — including an instance's custom fields, by id.
    #[arg(long = "set", value_name = "FIELD=VALUE")]
    pub set: Vec<String>,

    /// Fill in the form even when the flags would have been enough
    #[arg(long, short = 'i')]
    pub interactive: bool,
    /// Create it on the server now instead of leaving it staged
    #[arg(long)]
    pub push: bool,
    /// With --push, show the API call without sending it
    #[arg(long, requires = "push")]
    pub dry_run: bool,
}

pub fn run(g: &GlobalArgs, args: &CreateArgs, out: Out) -> Result<ExitCode> {
    let mut ctx = Context::open(g, out)?;
    ctx.lock()?;

    let engine = Engine::open(&mut ctx)?;
    let key = new_key(&engine)?;
    // Resolved before anything is written, to the name the file carries and
    // push moves by. Written as typed, `--sprint 17` named no sprint, the push
    // moved the issue nowhere, and it landed in the backlog.
    let sprint = match &args.sprint {
        Some(wanted) => Some(crate::cli::cmd::sprint::resolve_sprint(&ctx, wanted)?.name),
        None => None,
    };
    // The template is the starting point for both the form and the flags-only
    // path, so it is read before either decides anything.
    let mut doc = template(&ctx, args)?;
    // After the template and before the flags: "like that one, but…" is what
    // `--like` means, and the flags are the "but".
    if let Some(model) = &args.like {
        let key = crate::project::naming::key_from_input(model)?;
        let (from, _) = crate::cli::cmd::lookup::doc(&engine, &key)?;
        copy_from(&mut doc.frontmatter, &from.frontmatter);
    }
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
    // The fields the form does not show, and the general `--set`. Applied after
    // the draft so a flag beats what a template said, and through the same
    // setter `jired set` uses so the two cannot disagree about what a field is
    // called or how its value is spelled.
    apply_flags(&mut doc.frontmatter, args, sprint.as_deref())?;

    let outcome = engine.projector.write(&doc, None)?;
    // Recording it now is what makes `status` list it and `push` find it.
    engine
        .state
        .record_new_issue(&key, &draft.project, &outcome.relative)?;

    let pushed = if args.push {
        let report = crate::cli::cmd::edit::push_one(&mut ctx, &key, args.dry_run)?;
        crate::cli::cmd::edit::report_push(&report, out);
        Some(report)
    } else {
        None
    };
    // The draft existed only so the dry run had something to plan from. Left
    // behind, it was a `NEW-*` issue nobody asked to stage, which the next
    // plain `push` would have created.
    if args.dry_run {
        engine.projector.remove(&outcome.relative)?;
        engine.state.forget_issue(&key)?;
        engine.state.forget_new_issue_project(&key)?;
    }

    let created_as = pushed
        .as_ref()
        .and_then(|r| r.results.first())
        .and_then(|r| r.new_key.clone());
    match (&pushed, &created_as) {
        (Some(r), _) if r.dry_run => out.say("nothing was created, and no draft was kept"),
        (Some(_), Some(real)) => out.say(format!("created {real}")),
        (Some(_), None) => out.say(format!(
            "{key} is staged in {}, but the push failed — \
             fix the cause and run `jired push`",
            outcome.relative
        )),
        (None, _) => out.say(format!(
            "created {key} in {} — run `jired push` to create it on the server",
            outcome.relative
        )),
    }

    let staged = !args.dry_run && created_as.is_none();
    out.emit(
        "create",
        &serde_json::json!({
            "key": key,
            "new_key": created_as,
            "file": outcome.relative,
            "staged": staged,
            "project": draft.project,
            "type": draft.issue_type,
            "summary": draft.summary,
            "pushed": pushed.is_some(),
            "push": pushed,
        }),
    );
    Ok(match pushed {
        Some(report) => report.exit_code(),
        None => ExitCode::Success,
    })
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
        priority: args.priority.clone().or_else(|| fm.priority.clone()),
        labels: if args.labels.is_empty() {
            fm.labels.clone()
        } else {
            args.labels.clone()
        },
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
    // Every candidate `NEW-xxxxxx` collided, which after this many tries means
    // something is wrong with the workspace rather than with luck.
    Err(
        JiredError::generic("cannot allocate a temporary key for the new issue")
            .with_hint("run `jired push` to send the issues already staged, then try again"),
    )
}

/// Take from `model` the fields that describe *what kind of issue this is*.
///
/// Not its identity, which belongs to it; not its status, which is where it has
/// got to; not its summary, description, comments or worklogs, which are what
/// it is about. What is left is the shape a team gives issues of this sort, and
/// copying that by hand — read the model, read off six fields, type them back —
/// is the whole of what "create a task like TSK-259" used to mean.
fn copy_from(fm: &mut Frontmatter, model: &Frontmatter) {
    fm.issue_type = model.issue_type.clone();
    fm.priority = model.priority.clone();
    fm.labels = model.labels.clone();
    fm.assignee = model.assignee.clone();
    fm.components = model.components.clone();
    fm.custom = model.custom.clone();
}

/// Write the flags that name a field, and then whatever `--set` names.
///
/// `--set` is applied last so it can override a flag as well as a template,
/// which is what makes one template plus overrides a way to stamp out issues.
fn apply_flags(fm: &mut Frontmatter, args: &CreateArgs, sprint: Option<&str>) -> Result<()> {
    for (field, value) in [
        ("assignee", args.assignee.clone()),
        ("sprint", sprint.map(str::to_string)),
        ("epic", args.epic.clone()),
    ] {
        if let Some(v) = value {
            fm.set_field(field, &v)?;
        }
    }
    if !args.components.is_empty() {
        fm.set_field("components", &args.components.join(","))?;
    }

    for pair in &args.set {
        let (field, value) = pair.split_once('=').ok_or_else(|| {
            JiredError::usage(format!("`{pair}` is not `field=value`"))
                .with_hint("`--set story_points=5`, `--set customfield_10456=Platform`")
        })?;
        fm.set_field(field.trim(), value)?;
    }
    Ok(())
}

fn template(ctx: &Context, args: &CreateArgs) -> Result<IssueDoc> {
    let Some(name) = &args.template else {
        return Ok(blank());
    };
    // A name means one of the workspace's templates; anything that exists as a
    // path is that file, so a skeleton can live in the repository being worked
    // on rather than having to be copied into every workspace first.
    let named = ctx.workspace.templates_dir().join(format!("{name}.md"));
    let as_path = std::path::Path::new(name);
    let path = if named.is_file() || !as_path.is_file() {
        named
    } else {
        as_path.to_path_buf()
    };
    let text = std::fs::read_to_string(&path).map_err(|e| {
        JiredError::usage(format!("cannot read template {}: {e}", path.display()))
            .with_hint("templates live in .jired/templates/<name>.md, or name a path")
    })?;
    IssueDoc::parse(&with_identity(&text))
        .map_err(|e| JiredError::usage(format!("template {name}: {}", e.message)))
}

/// A template with the fields the parser insists on, added if they are absent.
///
/// A template is a partial file — the README calls it "an issue file without
/// the identity fields" — and `create` fills the real key in afterwards. Making
/// every template carry a placeholder `key:` to get past the parser contradicts
/// that, and is the sort of thing nobody discovers until their first template
/// fails to load.
fn with_identity(text: &str) -> String {
    let placeholder = "key: NEW-000000\nid: ''\njired: {}\n";
    let Some(rest) = text.strip_prefix("---\n") else {
        // No header at all: the whole file is the body.
        return format!("---\n{placeholder}---\n\n{text}");
    };
    let (header, body) = match rest.split_once("\n---") {
        Some((h, b)) => (h, b),
        // An opening fence and no closing one is not a header; leave it to the
        // parser to say so, rather than guessing at where it ends.
        None => return text.to_string(),
    };
    let mut missing = String::new();
    for (field, line) in [
        ("key:", "key: NEW-000000\n"),
        ("id:", "id: ''\n"),
        ("jired:", "jired: {}\n"),
    ] {
        if !header.lines().any(|l| l.starts_with(field)) {
            missing.push_str(line);
        }
    }
    format!("---\n{missing}{header}\n---{body}")
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
        children: Vec::new(),
        comments: Vec::new(),
        worklogs: Vec::new(),
        attachments: Vec::new(),
        extra_sections: Vec::new(),
    }
}
