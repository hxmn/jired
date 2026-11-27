//! `jired pull` [M1.11, M2.4] — fetch, then merge into the working copy.

use crate::cli::cmd::fetch::FetchArgs;
use crate::cli::context::Context;
use crate::cli::GlobalArgs;
use crate::error::{ExitCode, Result};
use crate::json::Out;
use clap::{Args, ValueEnum};

pub use crate::sync::pull::Strategy;

// By hand rather than derived, so the engine's type carries no clap: the
// spellings and help here are the flag's, and the engine never sees them.
impl ValueEnum for Strategy {
    fn value_variants<'a>() -> &'a [Self] {
        &[
            Strategy::Abort,
            Strategy::Merge,
            Strategy::Ours,
            Strategy::Theirs,
        ]
    }

    fn to_possible_value(&self) -> Option<clap::builder::PossibleValue> {
        use clap::builder::PossibleValue;
        Some(match self {
            Strategy::Abort => PossibleValue::new("abort")
                .help("Stop before writing anything if any conflict is detected"),
            Strategy::Merge => PossibleValue::new("merge")
                .help("Write conflict markers and record conflicts for `jired resolve`"),
            Strategy::Ours => {
                PossibleValue::new("ours").help("Keep the local value wherever both sides changed")
            }
            Strategy::Theirs => PossibleValue::new("theirs")
                .help("Take the remote value wherever both sides changed"),
        })
    }
}

#[derive(Args, Debug, Clone, Default)]
pub struct PullArgs {
    #[command(flatten)]
    pub scope: FetchArgs,

    /// Merge the existing snapshot without contacting the server
    #[arg(long)]
    pub no_fetch: bool,

    /// How to handle fields changed on both sides
    #[arg(long, value_enum, default_value_t = Strategy::Abort)]
    pub strategy: Strategy,

    /// Overwrite local changes with remote state (equivalent to --strategy theirs)
    #[arg(long)]
    pub force: bool,

    /// Show what would be written without touching any file
    #[arg(long, short = 'n')]
    pub dry_run: bool,
}

impl PullArgs {
    /// What the engine is asked to do.
    pub fn options(&self) -> crate::sync::pull::PullOptions {
        crate::sync::pull::PullOptions {
            scope: self.scope.options(),
            no_fetch: self.no_fetch,
            strategy: self.effective_strategy(),
            dry_run: self.dry_run,
        }
    }

    pub fn effective_strategy(&self) -> Strategy {
        if self.force {
            Strategy::Theirs
        } else {
            self.strategy
        }
    }
}

pub fn run(g: &GlobalArgs, args: &PullArgs, out: Out) -> Result<ExitCode> {
    let mut ctx = Context::open(g, out)?;
    ctx.lock()?;

    // `theirs` discards local edits. Name the issues that would lose them before
    // doing it, rather than after.
    if !args.dry_run && matches!(args.effective_strategy(), Strategy::Theirs) {
        let dirty = crate::sync::status::dirty_keys(&mut ctx)?;
        if !dirty.is_empty() {
            for key in &dirty {
                out.say(format!("  {key} has local edits that will be discarded"));
            }
            if !crate::cli::cmd::prompt::confirm(
                &format!("overwrite {} issue(s) with the server's copy?", dirty.len()),
                ctx.yes || !ctx.resolver.interactive(),
                ctx.resolver.interactive(),
            )? {
                out.say("cancelled — nothing was written");
                return Ok(ExitCode::Success);
            }
        }
    }

    let mut report = crate::sync::pull::run(&mut ctx, &args.options())?;
    report.render(out);
    report.state = closing_state(&mut ctx, &report);
    if let Some(state) = &report.state {
        state.render(out);
    }
    out.emit("pull", &report);
    // Conflicts are a distinct, scriptable outcome rather than an error. An
    // issue that could not be converted is one: the rest were written, and
    // exit 0 would say the workspace is level with the server when one issue
    // is not.
    Ok(if !report.conflicts.is_empty() {
        ExitCode::Conflict
    } else if !report.failed.is_empty() {
        ExitCode::Generic
    } else {
        ExitCode::Success
    })
}

/// The workspace's state once a pull is written, for the line that ends it.
///
/// Not after a dry run, which wrote nothing, and not after a pull with
/// conflicts of its own: under the default strategy those are not recorded,
/// and a closing "nothing to push, no conflicts" under "2 conflicting issues"
/// would contradict the line above it. Best-effort, because the pull itself
/// succeeded and a state that cannot be read is no reason to say otherwise.
pub fn closing_state(
    ctx: &mut Context,
    report: &crate::sync::pull::PullReport,
) -> Option<crate::sync::status::WorkspaceState> {
    if report.dry_run || !report.conflicts.is_empty() {
        return None;
    }
    crate::sync::status::WorkspaceState::now(ctx)
        .map_err(|e| tracing::debug!("cannot read the workspace state: {}", e.message))
        .ok()
}
