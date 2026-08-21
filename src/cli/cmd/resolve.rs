//! `jired resolve` [M3.8] — clear recorded conflicts.
//!
//! A default `pull` that hits a conflict writes nothing, which is safe but
//! leaves the workspace stuck until something decides. This is that something.
//!
//! Resolution re-runs the same three-way merge under a chosen strategy rather
//! than editing files a second way, so `resolve --theirs` and
//! `pull --strategy theirs` cannot drift apart.

use crate::cli::cmd::pull::{PullArgs, Strategy};
use crate::cli::context::Context;
use crate::cli::GlobalArgs;
use crate::error::{ExitCode, JiredError, Result};
use crate::json::Out;
use crate::sync::{merge, Engine};
use clap::Args;

#[derive(Args, Debug, Clone)]
pub struct ResolveArgs {
    /// Issues to resolve: keys or file paths (default: all conflicted)
    #[arg(value_delimiter = ',')]
    pub issues: Vec<String>,
    /// Keep the local value wherever both sides changed
    #[arg(long, conflicts_with_all = ["theirs", "done"])]
    pub ours: bool,
    /// Take the remote value wherever both sides changed
    #[arg(long, conflicts_with = "done")]
    pub theirs: bool,
    /// Accept the file as it stands, after editing the conflict markers by hand
    #[arg(long)]
    pub done: bool,
    /// Resolve only this field (listing only; per-field resolution is not built yet)
    #[arg(long, hide = true)]
    pub field: Option<String>,
    /// List conflicts without resolving anything
    #[arg(long)]
    pub list: bool,
}

pub fn run(g: &GlobalArgs, args: &ResolveArgs, out: Out) -> Result<ExitCode> {
    // Checked before anything else: `--field` narrows the *listing* but cannot
    // narrow a resolution, and finding that out after the conflicts have been
    // read and reported reads like the command half-worked.
    if args.field.is_some() && (args.ours || args.theirs || args.done) {
        return Err(crate::cli::not_yet(
            "jired resolve --field",
            "M3.8",
            "resolve the whole issue, or edit the file by hand and use --done",
        ));
    }

    let mut ctx = Context::open(g, out)?;
    let engine = Engine::open(&mut ctx)?;

    let wanted: Vec<String> = args
        .issues
        .iter()
        .map(|k| crate::project::naming::key_from_input(k))
        .collect();
    let conflicts: Vec<_> = engine
        .state
        .conflicts()?
        .into_iter()
        .filter(|c| wanted.is_empty() || wanted.contains(&c.issue_key))
        .filter(|c| args.field.as_ref().is_none_or(|f| &c.field == f))
        .collect();

    if conflicts.is_empty() {
        out.say("no recorded conflicts");
        out.emit(
            "resolve",
            &serde_json::json!({"resolved": [], "conflicts": []}),
        );
        return Ok(ExitCode::Success);
    }

    if args.list || !(args.ours || args.theirs || args.done) {
        for c in &conflicts {
            out.say(format!("{:<14} {}", c.issue_key, c.field));
        }
        out.emit("resolve", &serde_json::json!({ "conflicts": conflicts }));

        if args.list {
            return Ok(ExitCode::Success);
        }
        // Choosing for the user is exactly the mistake this command exists to
        // avoid, so an unqualified `resolve` reports and stops.
        return Err(JiredError::missing_value("no resolution chosen")
            .with_detail(
                "conflicts",
                serde_json::json!(conflicts
                    .iter()
                    .map(|c| format!("{}:{}", c.issue_key, c.field))
                    .collect::<Vec<_>>()),
            )
            .with_hint("pass --ours, --theirs, or --done after editing the file"));
    }

    let mut keys: Vec<String> = conflicts.iter().map(|c| c.issue_key.clone()).collect();
    keys.sort();
    keys.dedup();

    if args.done {
        return accept_as_edited(&mut ctx, &engine, &keys, out);
    }

    // Taking the server's copy throws away local edits, so name them first.
    if args.theirs
        && !crate::cli::cmd::prompt::confirm(
            &format!(
                "discard local changes to {} and take the server's copy?",
                keys.join(", ")
            ),
            ctx.yes || !ctx.resolver.interactive(),
            ctx.resolver.interactive(),
        )?
    {
        out.say("cancelled");
        return Ok(ExitCode::Success);
    }

    ctx.lock()?;
    let strategy = if args.ours {
        Strategy::Ours
    } else {
        Strategy::Theirs
    };
    let pull_args = PullArgs {
        no_fetch: true,
        strategy,
        ..Default::default()
    };
    let report = crate::sync::pull::run_scoped(&mut ctx, &pull_args, Some(&keys))?;

    out.say(format!(
        "resolved {} issue(s) in favour of {}",
        keys.len(),
        if args.ours {
            "the local copy"
        } else {
            "the server"
        }
    ));
    out.emit(
        "resolve",
        &serde_json::json!({
            "strategy": if args.ours { "ours" } else { "theirs" },
            "resolved": keys,
            "pull": report,
        }),
    );
    Ok(ExitCode::Success)
}

/// Accept files the user merged by hand.
fn accept_as_edited(
    ctx: &mut Context,
    engine: &Engine,
    keys: &[String],
    out: Out,
) -> Result<ExitCode> {
    ctx.lock()?;
    let files = engine.projector.scan()?;
    let mut unresolved = Vec::new();

    for key in keys {
        let Some(file) = files.get(key) else {
            continue;
        };
        let text = engine.projector.read_raw(&file.relative)?;
        // Accepting a file that still has markers would push `<<<<<<<` into
        // the issue, which is worse than the conflict was.
        if merge::has_markers(&text) {
            unresolved.push(file.relative.clone());
            continue;
        }
        engine.state.clear_conflicts(key)?;
        out.say(format!("accepted {key} as edited"));
    }

    if !unresolved.is_empty() {
        return Err(
            JiredError::conflict("some files still contain conflict markers")
                .with_detail("files", serde_json::json!(unresolved))
                .with_hint("remove the <<<<<<< ======= >>>>>>> lines, then re-run"),
        );
    }

    out.emit(
        "resolve",
        &serde_json::json!({ "strategy": "manual", "resolved": keys }),
    );
    Ok(ExitCode::Success)
}
