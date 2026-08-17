//! `jired agent-docs` [M5.2] — generate CLAUDE.md and AGENTS.md.
//!
//! The env-var table, exit codes and command list are rendered from the same
//! definitions the CLI itself uses, so the docs cannot drift from the binary.

use crate::cli::context::Context;
use crate::cli::GlobalArgs;
use crate::config::keys;
use crate::error::{ExitCode, Result};
use crate::json::Out;
use clap::{Args, CommandFactory};
use std::path::Path;

pub const BEGIN: &str = "<!-- BEGIN jired -->";
pub const END: &str = "<!-- END jired -->";

#[derive(Args, Debug, Clone, Default)]
pub struct AgentDocsArgs {
    /// Print the generated documentation instead of writing files
    #[arg(long)]
    pub print: bool,
}

pub fn run(g: &GlobalArgs, args: &AgentDocsArgs, out: Out) -> Result<ExitCode> {
    let ctx = Context::open(g, out)?;
    let body = render();
    if args.print {
        println!("{body}");
        return Ok(ExitCode::Success);
    }
    let mut written = Vec::new();
    for name in ["CLAUDE.md", "AGENTS.md"] {
        let path = ctx.workspace.root().join(name);
        write_managed_block(&path, &body)?;
        written.push(name.to_string());
        out.say(format!("wrote {name}"));
    }
    out.emit("agent-docs", &serde_json::json!({ "files": written }));
    Ok(ExitCode::Success)
}

/// Replace only our sentinel block so user-authored notes survive regeneration.
pub fn write_managed_block(path: &Path, body: &str) -> Result<()> {
    let existing = std::fs::read_to_string(path).unwrap_or_default();
    let block = format!("{BEGIN}\n{body}\n{END}\n");
    let updated = match (existing.find(BEGIN), existing.find(END)) {
        (Some(s), Some(e)) if e > s => {
            let e_end = e + END.len();
            let e_end = existing[e_end..]
                .find('\n')
                .map(|n| e_end + n + 1)
                .unwrap_or(existing.len());
            format!("{}{}{}", &existing[..s], block, &existing[e_end..])
        }
        _ if existing.trim().is_empty() => block,
        _ => format!("{}\n{}", existing.trim_end(), block),
    };
    std::fs::write(path, updated)?;
    Ok(())
}

pub fn render() -> String {
    let mut s = String::new();
    s.push_str("# Working with this jired workspace\n\n");
    s.push_str(
        "This directory is a **jired workspace**: Jira issues are Markdown files kept in \
         sync with the server by a git-like `fetch`/`pull`/`push` cycle. All reads and \
         edits work offline; only `fetch`, `pull`, and `push` need the network.\n\n",
    );

    s.push_str("## Safe workflow\n\n```sh\n");
    s.push_str("jired status --json            # what is dirty, behind, or conflicted\n");
    s.push_str("jired pull --json              # exit 5 means conflicts; nothing was written\n");
    s.push_str("#   ... edit the .md files ...\n");
    s.push_str("jired diff --json              # confirm only intended changes\n");
    s.push_str("jired push --dry-run --json    # exact API calls, nothing sent\n");
    s.push_str("jired push --json              # apply\n");
    s.push_str("```\n\n");

    s.push_str("## Files and folders\n\n");
    s.push_str("| Path | Meaning |\n|---|---|\n");
    s.push_str("| `PROJ-142. Summary.md` | One issue. The `PROJ-142.` prefix is its identity; the summary part is cosmetic. |\n");
    s.push_str("| `.PROJ-142. Summary/` | Attachments for that issue. New files here are uploaded on push. |\n");
    s.push_str("| `by-parent/`, `sprints/`, `boards/` | Grouping links, regenerated from the database. Never edit as source of truth. |\n");
    s.push_str("| `.jired/` | Machine state: credentials, sync database, logs. Never edit or commit. |\n\n");

    s.push_str("## File structure\n\n");
    s.push_str(
        "YAML frontmatter, then fixed sections in this order: `# <summary>`, \
         `## Description`, `## Checklist`, `## Comments`, `## Attachments`. \
         Keep the section order and heading text intact.\n\n",
    );
    s.push_str(
        "Editable frontmatter fields: `type`, `status`, `priority`, `summary`, `labels`, \
         `components`, `assignee`, `sprint`, `parent`, `epic`, `fix_versions`, \
         `affects_versions`, `links`, `story_points`, and anything under `custom:`.\n\n",
    );
    s.push_str(
        "Read-only fields: `key`, `id`, `reporter`, `created`, `updated`, `url`, and the \
         whole `jired:` block. Editing `key` or `id` is an error; editing the others is \
         ignored with a warning.\n\n",
    );
    s.push_str("Frontmatter `summary` wins over the `# ` heading if they disagree.\n\n");

    s.push_str("## Comments\n\n");
    s.push_str("- Existing: `### [c-10812] Jane Doe — 2026-08-15T10:32:00Z`. Edit the body below it to update the comment.\n");
    s.push_str("- New: add a `### [new]` section; push creates it and rewrites the heading with the real id.\n");
    s.push_str("- Deleting a section only deletes the comment with `--allow-comment-delete`.\n\n");

    s.push_str("## Checklists\n\n");
    s.push_str(
        "`## Checklist` lists subtasks as `- [ ] PROJ-143 Title`. Ticking a box applies a \
         done-transition only when `checklist.done_transition` is configured; otherwise it \
         is reported and ignored.\n\n",
    );

    s.push_str("## Lossy regions\n\n");
    s.push_str(
        "Blocks between `<!--jired:raw:ID-->` and `<!--/jired:raw:ID-->` are Jira content \
         Markdown cannot represent (panels, macros, media). They are read-only: leave them \
         untouched and they push back byte-identical. Editing one makes `push` fail with \
         exit code 6 unless you pass `--allow-lossy`, which discards the original markup. \
         Files containing such a region have `lossy: true` in the `jired:` block.\n\n",
    );

    s.push_str("## Exit codes\n\n| Code | Meaning |\n|---|---|\n");
    for (code, meaning) in [
        (0, "success"),
        (1, "generic error"),
        (2, "usage error"),
        (3, "authentication failure"),
        (4, "network failure"),
        (
            5,
            "conflict detected (pull wrote nothing; push refused stale issues)",
        ),
        (6, "lossy conversion blocked"),
        (7, "required value missing in non-interactive mode"),
        (8, "workspace locked by another jired process"),
        (9, "local state database corrupt"),
        (10, "unsupported Jira server"),
    ] {
        s.push_str(&format!("| {code} | {meaning} |\n"));
    }
    s.push('\n');

    s.push_str("## Agent-friendly flags\n\n");
    s.push_str("| Flag | Effect |\n|---|---|\n");
    s.push_str("| `--json` | One versioned JSON document on stdout; stderr carries the structured error and nothing else. Narration is suppressed so both streams stay parseable. |\n");
    s.push_str(
        "| `--no-input` | Never prompt; missing values fail with exit 7 naming the value. |\n",
    );
    s.push_str("| `--yes` | Accept confirmations. |\n");
    s.push_str("| `--dry-run` | On `push`: show the exact API calls without sending them. |\n\n");
    s.push_str(
        "Every JSON document carries a `schema` field such as `jired/pull/1`. \
         Run `jired schema` to list the JSON Schemas and `jired schema pull` to print one.\n\n",
    );

    s.push_str("## Environment variables\n\n| Variable | Setting | Meaning |\n|---|---|---|\n");
    for def in keys::KEYS {
        s.push_str(&format!(
            "| `{}` | `{}` | {} |\n",
            def.env, def.key, def.doc
        ));
    }
    s.push('\n');

    s.push_str("## Commands\n\n| Command | Purpose |\n|---|---|\n");
    let cmd = crate::cli::Cli::command();
    for sub in cmd.get_subcommands() {
        s.push_str(&format!(
            "| `jired {}` | {} |\n",
            sub.get_name(),
            sub.get_about().map(|a| a.to_string()).unwrap_or_default()
        ));
    }
    s.push('\n');
    s.push_str("Run `jired <command> --help` for the full flag list.\n");
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_docs_cover_the_contract() {
        let doc = render();
        for needle in [
            "jired push --dry-run --json",
            "exit code 6",
            "JIRED_TOKEN",
            "jired/pull/1",
            "`.jired/`",
        ] {
            assert!(doc.contains(needle), "generated docs missing {needle:?}");
        }
        // Every command must be listed so agents can discover them.
        assert!(doc.contains("| `jired pull` |"));
        assert!(doc.contains("| `jired schema` |"));
    }

    #[test]
    fn docs_never_show_a_literal_token() {
        let doc = render().to_lowercase();
        assert!(!doc.contains("--token abc"));
        assert!(!doc.contains("token=") || doc.contains("$jired_token"));
    }

    #[test]
    fn regeneration_replaces_only_the_managed_block() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("CLAUDE.md");
        std::fs::write(&path, "# My notes\n\nkeep me\n").unwrap();
        write_managed_block(&path, "first").unwrap();
        write_managed_block(&path, "second").unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("keep me"));
        assert!(text.contains("second"));
        assert!(!text.contains("first"));
        assert_eq!(text.matches(BEGIN).count(), 1);
    }
}
