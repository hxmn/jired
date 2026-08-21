//! `jired agent-docs` [M5.2] — generate CLAUDE.md and AGENTS.md.
//!
//! The env-var table, exit codes and command list are rendered from the same
//! definitions the CLI itself uses, so the docs cannot drift from the binary.

use crate::cli::context::Context;
use crate::cli::GlobalArgs;
use crate::config::keys;
use crate::doc::frontmatter;
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

/// A field-name list as prose: `` `a`, `b`, `c` ``.
fn field_list(fields: &[&str]) -> String {
    fields
        .iter()
        .map(|f| format!("`{f}`"))
        .collect::<Vec<_>>()
        .join(", ")
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
    s.push_str(
        "jired pull --json              # exit 5 means conflicts; see \"Conflicts\" below\n",
    );
    s.push_str("#   `updated[].changes` says what each issue's pull brought in\n");
    s.push_str("#   ... edit the .md files ...\n");
    s.push_str("jired diff --json              # confirm only intended changes\n");
    s.push_str("jired push --dry-run --json    # exact API calls, nothing sent\n");
    s.push_str("jired push --json              # apply\n");
    s.push_str("```\n\n");

    s.push_str("## Files and folders\n\n");
    s.push_str("| Path | Meaning |\n|---|---|\n");
    s.push_str("| `PROJ-0142. Summary.md` | One issue. The key prefix is its identity; the summary part is cosmetic. The number is zero-padded so files sort numerically — the key itself is `PROJ-142`, which is what the frontmatter and every command use. |\n");
    s.push_str("| `.PROJ-0142. Summary/` | Attachments for that issue. New files here are uploaded on push. |\n");
    s.push_str("| `by-parent/`, `sprints/`, `boards/` | Grouping links onto the same issue files, regenerated from the database on every pull. Edits are lost, with one exception: moving an entry between `boards/<board>/<column>/` folders stages a transition to that column's status, the way dragging the card would. An explicit `status:` edit in the file wins if the two disagree. |\n");
    s.push_str("| `.jired/` | Machine state: credentials, sync database, templates, logs. Never edit or commit, except `.jired/templates/<name>.md` for `jired create --template <name>`. |\n\n");

    s.push_str("## File structure\n\n");
    s.push_str(
        "YAML frontmatter, then fixed sections in this order: `# <summary>`, \
         `## Description`, `## Checklist`, `## Children`, `## Comments`, \
         `## Worklogs`, `## Attachments`. Keep the section order and heading \
         text intact.\n\n",
    );
    // Rendered from the same constants the parser enforces. A hand-copied list
    // silently becomes a lie the first time a field is added, and an agent that
    // trusts it either skips an editable field or edits a rejected one.
    s.push_str(&format!(
        "Editable frontmatter fields: {}. A difference in one of these becomes an API \
         call on the next push.\n\n",
        field_list(frontmatter::EDITABLE)
    ));
    s.push_str(&format!(
        "Read-only fields: {}; `jired:` is the whole sync block. They only report \
         server state, so an edit is dropped with a warning on the next write.\n\n",
        field_list(frontmatter::READ_ONLY)
    ));
    s.push_str(&format!(
        "Identity fields: {}. Changing one is an error, not a warning — it would write \
         one issue's content over another's.\n\n",
        field_list(frontmatter::IDENTITY)
    ));
    s.push_str(
        "Any other key you add to the header is preserved verbatim and never sent. \
         Frontmatter `summary` wins over the `# ` heading if they disagree.\n\n",
    );

    s.push_str("## Custom fields\n\n");
    s.push_str(
        "`custom:` holds fields keyed by their Jira id, written in their simple \
         form: a select list as `Platform`, a multi-select as a YAML list, a user \
         as `@[Name](id)`. Edit those values directly — push rebuilds the shape \
         Jira wants (`{\"value\": …}`, `{\"name\": …}`, `{\"accountId\": …}`) from what \
         the server last sent for that field. A value that could not be \
         simplified is shown as raw JSON and is sent back verbatim; leave it \
         alone unless you know the field's format. Deleting an entry clears that \
         field on the next push.\n\n",
    );
    s.push_str(
        "A field nobody here works with can be left out of the files entirely: \
         `jired config add ignored_custom_fields customfield_10000` (an id or the \
         field's name), then `jired doctor` to take it out of the files that already \
         carry it. Ignoring hides a field, it does not clear it — the value stays on \
         the server and in the database, `push` refuses to send one, and removing the \
         entry plus another `jired doctor` brings it back.\n\n",
    );

    s.push_str("## Comments\n\n");
    s.push_str("- Existing: `### [c-10812] Jane Doe — 2026-08-15T10:32:00Z`. Edit the body below it to update the comment.\n");
    s.push_str("- New: add a `### [new]` section; push creates it and rewrites the heading with the real id.\n");
    s.push_str("- Deleting a section only deletes the comment with `--allow-comment-delete`.\n\n");

    s.push_str("## Worklogs\n\n");
    s.push_str(
        "`## Worklogs` is the time booked against the issue, oldest first, one \
         `### ` heading each: the duration, then who booked it, then when the \
         work started — `### [w-10500] 2h 30m — Jane Doe — 2026-08-15T09:00:00Z`. \
         Text under a heading is that entry's note.\n\n",
    );
    s.push_str(
        "- Book time with `jired logwork KEY \"2h 30m\" -m \"what it went on\"`, \
         which stages `### [new] 2h 30m — <when>` for the next push; writing that \
         heading by hand does the same thing.\n",
    );
    s.push_str(
        "- The duration is Jira's own notation — weeks, days, hours, minutes — \
         and is sent as written, because how long a day is is an instance \
         setting. A bare number is refused rather than guessed at.\n",
    );
    s.push_str(
        "- Editing a heading or its note updates that worklog; deleting a section \
         only deletes it with `--allow-worklog-delete`.\n\n",
    );

    s.push_str("## Children and checklists\n\n");
    s.push_str("Two checkbox sections, two different things.\n\n");
    s.push_str(
        "- `## Children` lists the subtasks as `- [ ] PROJ-143 Title`, each of them an \
         issue of its own. Ticking a box applies a done-transition only when \
         `children.done_transition` is configured; otherwise it is reported and ignored. \
         Adding a line does not create a subtask — use `jired create --parent PROJ-142`.\n",
    );
    s.push_str(
        "- `## Checklist` is the checklist plugin's list inside this issue, as \
         `- [ ] `IN PROGRESS` Title` — a status is written in backticks ahead of the \
         title, and only when the checkbox does not already say it. `### ` lines group \
         items; indented lines under an item are its note. It is read-only: an edit is \
         reported by push rather than sent, and the next write from the server \
         replaces it.\n\n",
    );

    s.push_str("## New issues\n\n");
    s.push_str(
        "`jired create` writes `NEW-<hex>. Summary.md` with a placeholder key: the real \
         one only exists once Jira has been asked. Push creates the issue, renames the \
         file, and reports the mapping in `results[].new_key`. Locally created issues \
         are pushed before existing ones, so an issue may name a `NEW-…` key as its \
         `parent` or `epic`. `jired create --template <name>` starts from \
         `.jired/templates/<name>.md`, a partial file with no identity fields.\n\n",
    );

    s.push_str("## Credentials\n\n");
    s.push_str(
        "The token is never in `config.toml`. `creds.storage` says where it is: \
         `keychain`, `local_db` (`.jired/session.db`), or `user_config` \
         (`credentials.toml` in the user config directory, shared by every \
         workspace on the machine). The file-backed stores encrypt it with \
         `JIRED_SECRET_KEY`. Changing the setting and running `jired doctor` \
         moves the credential; it never has to be entered twice.\n\n",
    );

    s.push_str("## Editing\n\n");
    s.push_str(
        "Edit the `.md` file directly — that is the interface. `jired edit KEY` \
         opens the same fields in a form for a person at a terminal, and needs \
         one: it exits 7 without it. The key may also be a file name or a path, \
         and with none given it searches. `comment`, `transition` and `assign` are \
         the scriptable single-field equivalents.\n\n",
    );

    s.push_str("## Sprints\n\n");
    s.push_str(
        "`sprint:` names the sprints an issue belongs to, oldest first — one as \
         a string, several as a list, because an issue that rolls over stays in \
         the sprint it came from. The last one is where it is now; removing them \
         all puts it in the backlog. Push turns a change to that last one into a \
         board move rather than a field update, resolving the name against the \
         sprints the workspace has fetched. The earlier entries are the server's \
         record of what happened and cannot be edited: changing them is reported \
         and not sent.\n\n",
    );

    s.push_str("## Repairing a workspace\n\n");
    s.push_str(
        "`jired doctor` checks the layout, the config file, the credentials, the filenames, \
         the sections inside each issue file and the database, and repairs what it can; \
         `jired doctor --dry-run` reports without changing anything. It surveys before \
         repairing and asks first at a terminal, so a non-interactive run (or `--yes`) \
         proceeds without the question. It is the migration path when a convention \
         changes — a filename shape, a section heading, a renamed setting — so run it \
         after upgrading jired. It exits 1 when something needs a human.\n\n",
    );

    s.push_str("## Lossy regions\n\n");
    s.push_str(
        "Blocks between `<!--jired:raw:ID-->` and `<!--/jired:raw:ID-->` are Jira content \
         Markdown cannot represent (panels, macros, media). They are read-only: leave them \
         untouched and they push back byte-identical. Editing one makes `push` fail with \
         exit code 6 unless you pass `--allow-lossy`, which discards the original markup. \
         Files containing such a region have `lossy: true` in the `jired:` block.\n\n",
    );

    s.push_str("## Conflicts\n\n");
    s.push_str(
        "Exit 5 from `pull` means a field changed on both sides. The default strategy \
         (`abort`) writes nothing; `pull --strategy merge` writes git-style \
         `<<<<<<< LOCAL` / `=======` / `>>>>>>> REMOTE` markers into the file **and \
         still exits 5**, so a non-zero status never implies the working copy is \
         unchanged. `jired status` and `jired resolve --list` say which issues and \
         fields are affected.\n\n",
    );
    s.push_str("```sh\n");
    s.push_str("jired resolve --list --json     # issue key and field for each conflict\n");
    s.push_str("jired resolve --ours --json     # keep the local value wherever both changed\n");
    s.push_str("jired resolve --theirs --json   # take the server's (discards local edits)\n");
    s.push_str("#   or: edit the markers out of the file by hand, then\n");
    s.push_str("jired resolve --done --json     # accept the file as it now stands\n");
    s.push_str("```\n\n");
    s.push_str(
        "`--ours` and `--theirs` re-run the same merge under that strategy rather than \
         editing the file a second way. `--done` refuses any file that still contains \
         markers and exits 5 listing them, so `<<<<<<<` cannot reach Jira.\n\n",
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
            "conflict detected (pull aborted, or `--strategy merge` wrote conflict \
             markers; push refused stale issues)",
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
    s.push_str("| `--dry-run` (`-n`) | On `push`: the exact API calls, nothing sent. On `pull`: what would be written, nothing touched. |\n\n");
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
    // No config key and no flag, but they still change what jired does —
    // `JIRED_SECRET_KEY` is what makes a headless run possible at all.
    for (env, doc) in keys::ENV_ONLY {
        s.push_str(&format!("| `{env}` | — | {doc} |\n"));
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

    /// The docs are generated from the CLI's own definitions, so this catches
    /// the case they exist to prevent: a new command, flag or setting that the
    /// docs never mention, leaving an agent unable to discover it.
    #[test]
    fn generated_docs_mention_every_command_and_setting() {
        let doc = render();
        let cmd = crate::cli::Cli::command();

        for sub in cmd.get_subcommands() {
            assert!(
                doc.contains(&format!("| `jired {}` |", sub.get_name())),
                "`{}` is missing from the generated docs",
                sub.get_name()
            );
        }
        for def in keys::KEYS {
            assert!(
                doc.contains(def.env),
                "{} is missing from the generated docs",
                def.env
            );
            assert!(doc.contains(def.key), "{} is missing", def.key);
        }
        // Not settings, but a headless run is impossible without knowing about
        // JIRED_SECRET_KEY, and it appears in no other table.
        for (env, _) in keys::ENV_ONLY {
            assert!(
                doc.contains(env),
                "{env} is missing from the generated docs"
            );
        }
    }

    /// The editability rules are the part an agent acts on directly, and they
    /// used to be a hand-copied sentence. Rendering them from the constants the
    /// parser enforces makes drift impossible; this test proves the rendering
    /// really covers all three lists, including a field added later.
    #[test]
    fn field_lists_cannot_drift_from_the_parser() {
        let doc = render();
        for field in frontmatter::EDITABLE
            .iter()
            .chain(frontmatter::READ_ONLY)
            .chain(frontmatter::IDENTITY)
        {
            assert!(
                doc.contains(&format!("`{field}`")),
                "frontmatter field `{field}` is missing from the generated docs"
            );
        }
        // Each list has to appear under its own heading, or "editable" and
        // "read-only" could be swapped and every field would still be present.
        for (label, fields) in [
            ("Editable frontmatter fields: ", frontmatter::EDITABLE),
            ("Read-only fields: ", frontmatter::READ_ONLY),
            ("Identity fields: ", frontmatter::IDENTITY),
        ] {
            assert!(
                doc.contains(&format!("{label}{}", field_list(fields))),
                "{label:?} does not list exactly {fields:?}"
            );
        }
    }

    /// Exit 5 is the one code whose meaning depends on the flags: `--strategy
    /// merge` writes markers *and* exits 5, so "nothing was written" would send
    /// an agent past a half-merged file.
    #[test]
    fn conflict_recovery_is_documented() {
        let doc = render();
        for needle in [
            "## Conflicts",
            "jired resolve --list",
            "--ours",
            "--theirs",
            "--done",
            "<<<<<<< LOCAL",
        ] {
            assert!(doc.contains(needle), "generated docs missing {needle:?}");
        }
        assert!(
            !doc.contains("exit 5 means conflicts; nothing was written"),
            "the exit-5 gloss must not claim nothing was written"
        );
    }

    /// A board-column move is a staged transition (`tests/e2e.rs`:
    /// `dragging_a_card_between_columns_stages_a_transition`), so telling an
    /// agent the folders are never editable would hide a supported workflow.
    #[test]
    fn board_moves_are_documented_as_editable() {
        let doc = render();
        assert!(doc.contains("stages a transition"));
        assert!(!doc.contains("Never edit as source of truth"));
    }

    /// Every exit code the binary can return has to be documented, or an agent
    /// cannot tell a conflict from a crash.
    #[test]
    fn generated_docs_cover_every_exit_code() {
        let doc = render();
        for code in 0..=10 {
            assert!(
                doc.contains(&format!("| {code} |")),
                "exit code {code} is undocumented"
            );
        }
    }

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
