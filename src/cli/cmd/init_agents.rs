//! `jired init-agents` [M5.2] — set an agent up to work in this workspace:
//! the instructions it reads, and the permission to run the commands that only
//! read.
//!
//! Two halves, one source of truth each. The env-var table, exit codes and
//! command list in `CLAUDE.md`/`AGENTS.md` are rendered from the same
//! definitions the CLI itself uses, so the docs cannot drift from the binary;
//! the permission files are rendered from [`READ_ONLY`], so Claude's allow-list
//! and Codex's execpolicy rules cannot drift from each other.

use crate::cli::context::Context;
use crate::cli::GlobalArgs;
use crate::config::keys;
use crate::doc::frontmatter;
use crate::error::{ExitCode, JiredError, Result};
use crate::json::Out;
use clap::{Args, CommandFactory};
use std::path::Path;

/// The opening marker is located by this prefix, not by its full text: the
/// version stamped after it changes with every release, and a block whose
/// stamp no longer matches still has to be found before it can be replaced.
pub const BEGIN_PREFIX: &str = "<!-- BEGIN jired";
pub const END: &str = "<!-- END jired -->";

/// The two files the managed block is written into.
pub const DOCS: &[&str] = &["CLAUDE.md", "AGENTS.md"];

/// The opening marker this build writes: `<!-- BEGIN jired v0.7.0 -->`.
///
/// A generated doc reads as authoritative whether or not it still is, so the
/// block records which jired wrote it. That stamp is what lets `status` and
/// `doctor` say "this was written by 0.5.1 and you are running 0.6.1" instead
/// of leaving an agent to plan against instructions for a binary that is gone.
pub fn begin() -> String {
    format!("{BEGIN_PREFIX} v{} -->", env!("CARGO_PKG_VERSION"))
}

/// Sentinels for the Starlark rules file, whose comments start with `#`.
pub const RULES_BEGIN: &str = "# BEGIN jired";
pub const RULES_END: &str = "# END jired";

/// Claude Code reads its allow-list from here; the file is the project's, so
/// entries that are not ours are left alone.
pub const CLAUDE_SETTINGS: &str = ".claude/settings.json";
/// Codex loads every `.rules` file under a trusted project's `.codex/rules/`.
pub const CODEX_RULES: &str = ".codex/rules/default.rules";

/// A command an agent may run without stopping to ask, and why it is on the
/// list.
///
/// The line drawn here: the command sends nothing to Jira and writes nothing
/// into the working copy. Refreshing the read-only snapshot under `.jired/` is
/// allowed — that is machine state describing what the server holds, and no
/// file a person edits changes because of it.
pub struct ReadOnly {
    /// The argv prefix, `jired` included. Anything after it is still matched:
    /// both permission systems match on a prefix of the command line.
    pub argv: &'static [&'static str],
    /// Why running it changes nothing. Read by a person deciding whether to
    /// keep the entry, and by an agent's approval prompt.
    pub why: &'static str,
}

const fn ro(argv: &'static [&'static str], why: &'static str) -> ReadOnly {
    ReadOnly { argv, why }
}

pub const READ_ONLY: &[ReadOnly] = &[
    ro(
        &["jired", "status"],
        "reads the working copy and the snapshot: what is dirty, behind, or conflicted",
    ),
    // `st` is the same command under its visible alias, and a permission
    // system matching literal text does not know that.
    ro(&["jired", "st"], "the alias for `jired status`"),
    ro(
        &["jired", "diff"],
        "compares the working copy against the snapshot; sends nothing",
    ),
    ro(
        &["jired", "log"],
        "prints what changed, newest first; `--refresh` only updates the snapshot",
    ),
    ro(
        &["jired", "report"],
        "totals what the files on disk already say",
    ),
    ro(
        &["jired", "search"],
        "runs a JQL query and prints the matches",
    ),
    ro(
        &["jired", "fetch"],
        "downloads server state into the snapshot without touching a file",
    ),
    ro(
        &["jired", "pull", "--dry-run"],
        "reports what a pull would write, and writes none of it",
    ),
    ro(
        &["jired", "push", "--dry-run"],
        "prints the exact API calls, and sends none of them",
    ),
    ro(
        &["jired", "reset", "--dry-run"],
        "lists what a reset would drop, field by field",
    ),
    ro(
        &["jired", "clean", "--dry-run"],
        "lists the files a clean would remove",
    ),
    ro(
        &["jired", "doctor", "--dry-run"],
        "reports every repair it would make, and makes none",
    ),
    ro(
        &["jired", "resolve", "--list"],
        "names the issue and field behind each recorded conflict",
    ),
    ro(
        &["jired", "attachments", "list"],
        "says which attachments exist and whether their content is here",
    ),
    ro(&["jired", "whoami"], "reports the authenticated account"),
    ro(
        &["jired", "projects"],
        "lists the projects the account sees",
    ),
    ro(&["jired", "boards"], "lists boards"),
    ro(&["jired", "sprints"], "lists sprints"),
    ro(
        &["jired", "releases"],
        "lists the project's versions and refreshes the local copy of the list",
    ),
    ro(
        &["jired", "list"],
        "matches issues in the working copy; reads files and sends nothing",
    ),
    ro(
        &["jired", "show"],
        "prints one issue out of the working copy or the snapshot",
    ),
    ro(
        &["jired", "fields"],
        "names this instance's fields out of the cached definitions",
    ),
    ro(
        &["jired", "config", "get"],
        "prints one effective setting; the writes are `set`, `add`, `remove`, `unset`",
    ),
    ro(
        &["jired", "config", "list"],
        "prints every setting and where it came from",
    ),
    ro(
        &["jired", "config", "path"],
        "prints the path a config write would go to",
    ),
    ro(
        &["jired", "schema"],
        "prints JSON Schemas for the machine-readable output",
    ),
    ro(
        &["jired", "completion"],
        "prints a shell completion script on stdout",
    ),
    ro(
        &["jired", "version"],
        "prints which binary this is, and the changelog compiled into it",
    ),
    ro(
        &["jired", "init-agents", "--print"],
        "prints the files this command would write instead of writing them",
    ),
];

#[derive(Args, Debug, Clone, Default)]
pub struct InitAgentsArgs {
    /// Print what would be written instead of writing it
    #[arg(long)]
    pub print: bool,

    /// Only refresh CLAUDE.md and AGENTS.md, not the permission files
    #[arg(long, conflicts_with = "no_docs")]
    pub no_permissions: bool,

    /// Only refresh the permission files, not CLAUDE.md and AGENTS.md
    #[arg(long)]
    pub no_docs: bool,
}

pub fn run(g: &GlobalArgs, args: &InitAgentsArgs, out: Out) -> Result<ExitCode> {
    let ctx = Context::open(g, out)?;
    let body = render();
    // `out.say` rather than `println!`: a reader that closed the pipe
    // (`jired init-agents --print | head`) should end the run, not panic in the
    // middle of it.
    if args.print {
        if !args.no_docs {
            out.say("==> CLAUDE.md, AGENTS.md\n");
            out.say(&body);
        }
        if !args.no_permissions {
            out.say(format!("\n==> {CLAUDE_SETTINGS}\n"));
            out.say(claude_settings(&serde_json::json!({}))?);
            out.say(format!("\n==> {CODEX_RULES}\n"));
            out.say(codex_rules());
        }
        return Ok(ExitCode::Success);
    }

    let root = ctx.workspace.root();
    let mut written = Vec::new();
    if !args.no_docs {
        for name in DOCS {
            write_managed_block(&root.join(name), &body)?;
            written.push((*name).to_string());
        }
    }
    if !args.no_permissions {
        write_permissions(root)?;
        written.push(CLAUDE_SETTINGS.to_string());
        written.push(CODEX_RULES.to_string());
    }
    for name in &written {
        out.say(format!("wrote {name}"));
    }
    if !args.no_permissions {
        out.say(format!(
            "{} read-only command(s) may now run without asking",
            READ_ONLY.len()
        ));
        // Codex ignores a project's `.codex/` until the project is trusted, so
        // a rules file that silently does nothing is the likely first
        // experience unless this is said out loud.
        out.say("codex applies these rules once you trust this project's .codex/ layer");
    }
    out.emit(
        "init-agents",
        &serde_json::json!({
            "files": written,
            "permissions": permission_entries(),
        }),
    );
    Ok(ExitCode::Success)
}

/// Write both permission files, returning whether either of them changed.
pub fn write_permissions(root: &Path) -> Result<bool> {
    let claude = write_claude_settings(&root.join(CLAUDE_SETTINGS))?;
    let codex = write_block(
        &root.join(CODEX_RULES),
        &codex_rules(),
        RULES_BEGIN,
        RULES_BEGIN,
        RULES_END,
    )?;
    Ok(claude || codex)
}

/// Replace only our sentinel block so user-authored notes survive regeneration.
pub fn write_managed_block(path: &Path, body: &str) -> Result<()> {
    write_block(path, body, &begin(), BEGIN_PREFIX, END)?;
    Ok(())
}

/// How the managed block in one workspace file compares with this build's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DocState {
    /// This build's block, word for word.
    Current,
    /// No managed block in the file: it was never written, or it was taken out.
    Missing,
    /// A block some other jired wrote, carrying its version where it had one.
    /// `generated_by` is `None` for a block from before the stamp existed.
    Stale { generated_by: Option<String> },
}

/// The version stamped into a file's opening marker, if it carries one.
///
/// `None` covers both "no block here" and "a block older than the stamp"; the
/// caller separates those with [`DocState`].
pub fn stamped_version(text: &str) -> Option<String> {
    let start = text.find(BEGIN_PREFIX)?;
    let line = text[start..].lines().next()?;
    line.strip_prefix(BEGIN_PREFIX)?
        .trim()
        .strip_suffix("-->")?
        .trim()
        .strip_prefix('v')
        .map(str::to_string)
}

/// Compare every generated doc under `root` with what this build would write.
///
/// Rendered once for both files, because [`render`] builds the whole document.
pub fn doc_states(root: &Path) -> Vec<(&'static str, DocState)> {
    let body = render();
    let body = body.trim();
    DOCS.iter()
        .map(|name| {
            let text = std::fs::read_to_string(root.join(name)).unwrap_or_default();
            let state = if !text.contains(BEGIN_PREFIX) {
                DocState::Missing
            } else if text.contains(body)
                && stamped_version(&text).as_deref() == Some(env!("CARGO_PKG_VERSION"))
            {
                DocState::Current
            } else {
                // Body drift counts as well as a stamp mismatch: a build from
                // the same version with a changed template is still not what
                // the file says it is.
                DocState::Stale {
                    generated_by: stamped_version(&text),
                }
            };
            (*name, state)
        })
        .collect()
}

/// Rewrite the region between `begin` and `end`, leaving the rest of the file
/// as it was. Returns whether anything on disk changed.
///
/// The existing block is located by `begin_prefix` rather than by `begin`, so a
/// block an older jired opened with a different version stamp is replaced in
/// place instead of being left behind with a second one appended under it.
pub fn write_block(
    path: &Path,
    body: &str,
    begin: &str,
    begin_prefix: &str,
    end: &str,
) -> Result<bool> {
    let existing = std::fs::read_to_string(path).unwrap_or_default();
    let block = format!("{begin}\n{body}\n{end}\n");
    // The closing marker is looked for after the opening one, so a file whose
    // prose happens to mention `END jired` above the block cannot be cut at
    // that mention instead — which would leave the tail of the old block
    // orphaned below the new one.
    let start = existing.find(begin_prefix);
    let stop = start.and_then(|s| existing[s..].find(end).map(|e| s + e));
    let updated = match (start, stop) {
        (Some(s), Some(e)) if e > s => {
            let e_end = e + end.len();
            let e_end = existing[e_end..]
                .find('\n')
                .map(|n| e_end + n + 1)
                .unwrap_or(existing.len());
            format!("{}{}{}", &existing[..s], block, &existing[e_end..])
        }
        _ if existing.trim().is_empty() => block,
        _ => format!("{}\n{}", existing.trim_end(), block),
    };
    if updated == existing {
        return Ok(false);
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, updated)?;
    Ok(true)
}

/// The allow-list entries Claude Code understands, one per read-only command.
pub fn permission_entries() -> Vec<String> {
    READ_ONLY
        .iter()
        .map(|r| format!("Bash({}:*)", r.argv.join(" ")))
        .collect()
}

/// Whether `settings` already grants every read-only command.
pub fn claude_settings_current(settings: &serde_json::Value) -> bool {
    let allow = settings
        .get("permissions")
        .and_then(|p| p.get("allow"))
        .and_then(|a| a.as_array());
    let Some(allow) = allow else {
        return false;
    };
    permission_entries()
        .iter()
        .all(|entry| allow.iter().any(|v| v.as_str() == Some(entry.as_str())))
}

/// Add the missing entries to a settings document, pretty-printed.
///
/// Additive on purpose. The file belongs to the project, not to jired: an entry
/// someone added by hand — including one for a command jired stopped
/// generating — is a decision, and regenerating docs is no reason to overrule
/// it.
pub fn claude_settings(existing: &serde_json::Value) -> Result<String> {
    let mut doc = match existing {
        serde_json::Value::Object(_) => existing.clone(),
        serde_json::Value::Null => serde_json::json!({}),
        other => {
            return Err(JiredError::generic(format!(
                "{CLAUDE_SETTINGS} holds {other}, not an object"
            )))
        }
    };
    let allow = doc
        .as_object_mut()
        .expect("just built as an object")
        .entry("permissions")
        .or_insert_with(|| serde_json::json!({}))
        .as_object_mut()
        .ok_or_else(|| {
            JiredError::generic(format!("{CLAUDE_SETTINGS}: `permissions` is not an object"))
        })?
        .entry("allow")
        .or_insert_with(|| serde_json::json!([]));
    let allow = allow.as_array_mut().ok_or_else(|| {
        JiredError::generic(format!(
            "{CLAUDE_SETTINGS}: `permissions.allow` is not an array"
        ))
    })?;
    for entry in permission_entries() {
        if !allow.iter().any(|v| v.as_str() == Some(entry.as_str())) {
            allow.push(serde_json::Value::String(entry));
        }
    }
    Ok(serde_json::to_string_pretty(&doc)?)
}

fn write_claude_settings(path: &Path) -> Result<bool> {
    let existing = std::fs::read_to_string(path).unwrap_or_default();
    let parsed: serde_json::Value = if existing.trim().is_empty() {
        serde_json::json!({})
    } else {
        serde_json::from_str(&existing).map_err(|e| {
            JiredError::generic(format!("{CLAUDE_SETTINGS} is not valid JSON: {e}"))
                .with_hint("fix or delete the file, then run `jired init-agents` again")
        })?
    };
    let updated = format!("{}\n", claude_settings(&parsed)?);
    if updated == existing {
        return Ok(false);
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, updated)?;
    Ok(true)
}

/// The Starlark body of the Codex execpolicy file, sentinels excluded.
pub fn codex_rules() -> String {
    let mut s = String::new();
    s.push_str("# jired commands that only read. Every one of them sends nothing to\n");
    s.push_str("# Jira and writes nothing into the working copy; some refresh the\n");
    s.push_str("# read-only snapshot under .jired/, which no edited file depends on.\n");
    s.push_str("# Matching is on the start of the command line, so `jired status --json`\n");
    s.push_str("# is covered and `jired --json status` is not.\n");
    s.push_str("# Regenerate with `jired init-agents`; edits inside this block are lost.\n");
    for r in READ_ONLY {
        let pattern = r
            .argv
            .iter()
            .map(|t| format!("\"{t}\""))
            .collect::<Vec<_>>()
            .join(", ");
        s.push_str(&format!(
            "\nprefix_rule(\n    pattern = [{pattern}],\n    decision = \"allow\",\n    justification = \"{}\",\n)\n",
            r.why.replace('\\', "\\\\").replace('"', "\\\"")
        ));
    }
    s
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
    s.push_str("jired diff --stat              # the same, as one printed line per issue\n");
    s.push_str("jired diff --exit-code         # exit 1 when there is anything to send\n");
    s.push_str("jired push --dry-run --json    # exact API calls, nothing sent\n");
    s.push_str("jired push --json              # apply\n");
    s.push_str("```\n\n");
    s.push_str(
        "`--stat` shortens what is *printed* to one line per issue — how many \
         fields changed, how many lines were added and removed — and a total. It \
         does not change the `--json` document, which carries those same totals \
         in `stat` and is the same either way, so there is no reason to pass both. \
         `--exit-code` turns \"is there anything to send\" into a status code, \
         which is what a script should branch on rather than on whether some \
         array came back empty.\n\n",
    );

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
         text intact. A description, comment or worklog can hold headings of its \
         own — Jira's `h2.` is a `## ` here — and they are part of that text: \
         inside one, only those section names end it.\n\n",
    );
    // Rendered from the same constants the parser enforces. A hand-copied list
    // silently becomes a lie the first time a field is added, and an agent that
    // trusts it either skips an editable field or edits a rejected one.
    s.push_str(&format!(
        "Editable frontmatter fields: {}. A difference in one of these becomes an API \
         call on the next push. The one exception is `epic`, which jired cannot send \
         yet: a change to it is warned about rather than sent.\n\n",
        field_list(frontmatter::EDITABLE)
    ));
    s.push_str(
        "`links:` is a list of single-entry maps, one link per line, spelled with the \
         instance's own directed wording: `- is blocked by: PROJ-1`, `- blocks: PROJ-2`. \
         Adding a line creates the link on the next push, deleting one deletes it; a \
         direction this Jira does not define is refused at plan time, naming the ones \
         it does.\n\n",
    );
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
         heading by hand does the same thing. Given both values it never asks \
         anything, so it is safe to script; with either missing it wants a \
         terminal to ask on and exits 7 without one.\n",
    );
    s.push_str(
        "- A start time that names its zone (`2026-08-15T09:00:00Z`) means \
         exactly that; one that does not (`2026-08-15 09:00`) is read in the \
         machine's local timezone. Prefer the first when scripting — it is the \
         only one that means the same thing on every machine.\n",
    );
    s.push_str(
        "- The duration is Jira's own notation — weeks, days, hours, minutes — \
         and is sent as written, because how long a day is is an instance \
         setting. A bare number is refused rather than guessed at.\n",
    );
    s.push_str(
        "- Editing a heading or its note updates that worklog; deleting a section \
         only deletes it with `--allow-worklog-delete`.\n",
    );
    s.push_str(
        "- `jired report time --days 7 --json` totals the booked time per person, \
         per day and per issue: `dates` is a day per column and every `by_day` \
         array is in that order and of that length. It reads these files rather \
         than the server, so `pull` first; the document says when the snapshot \
         was last refreshed and how much of the total is still staged.\n\n",
    );

    s.push_str("## Children and checklists\n\n");
    s.push_str("Two checkbox sections, two different things.\n\n");
    s.push_str(
        "- `## Children` lists the subtasks as `- [ ] PROJ-143 Title`, each of them an \
         issue of its own. Ticking a box applies a done-transition only when \
         `children.done_transition` is configured; otherwise it is reported and ignored. \
         Adding a line does not create a subtask — use `jired create --parent PROJ-142`; \
         the push that creates it adds it to the parent's file, so there is nothing to pull \
         afterwards.\n",
    );
    s.push_str(
        "- `## Checklist` is the checklist plugin's list inside this issue, as \
         `- [ ] `IN PROGRESS` Title` — a status is written in backticks ahead of the \
         title, and only when the checkbox does not already say it. `### ` lines group \
         items; indented lines under an item are its note. Editing it is a push: the \
         list goes back in the plugin's own syntax, so an issue can be created and its \
         checklist filled in without leaving the CLI. Only what the section shows is \
         writable — the four statuses its markers spell (`TO DO`, `IN PROGRESS`, \
         `DONE`, `SKIPPED`), groups, notes. Anything else in the field (a mandatory \
         item, a status the instance configured itself) makes push refuse the whole \
         section and say so, rather than send it with that part dropped.\n\n",
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
         and with none given it searches.\n\n",
    );
    s.push_str(
        "For a script, these write one field without a form. Each stages the \
         change into the file, exactly as editing it by hand would, and `push` \
         sends it; `--push` sends it at once, and `--push --dry-run` prints that \
         call without sending it.\n\n",
    );
    s.push_str("```sh\n");
    s.push_str("jired transition KEY[,KEY…] --to STATUS [--field K=V]… [--push]\n");
    s.push_str("jired transition KEY --list      # the transitions available from here\n");
    s.push_str("jired assign KEY[,KEY…] --user NAME|--me|--none [--push]\n");
    s.push_str("jired set --field FIELD --value VALUE KEY[,KEY…] [--push]\n");
    s.push_str("jired comment KEY -m TEXT [--push]\n");
    s.push_str("jired label add|remove|set LABEL KEY[,KEY…] [--push]\n");
    s.push_str("jired logwork KEY \"2h 30m\" -m NOTE\n");
    s.push_str("```\n\n");
    s.push_str(
        "The status goes after `--to`, never as a bare word — `jired transition \
         PROJ-142 Done` is exit 2. `--list` is how to find the spelling this \
         issue's workflow accepts, and `--field` carries anything the transition \
         screen demands. `transition` and `assign` take several issues, which is \
         what `jired list --keys` pipes into; each issue is validated against \
         its own workflow, and one that cannot move is a warning beside the \
         ones that were staged. `jired set` is the general one: `--field` takes a \
         frontmatter name or a `customfield_*` id, an empty `--value` clears the \
         field, and a list value takes commas. `label set` replaces every label \
         at once and takes them comma-separated, where `add` and `remove` take \
         one and leave the rest alone. `assign --user` searches only the people \
         who can be assigned the first issue named, so a name it does not find \
         may exist on the instance without being assignable here.\n\n",
    );
    s.push_str(
        "An operation Jira accepted is not an operation Jira performed — it \
         answers success to some payloads it has no intention of applying — so \
         `push` re-reads the issue and checks the change is in it before it \
         rewrites the file. One that is not is `\"status\": \"failed\"` with the \
         request that was sent, the issue is `\"ok\": false`, and base does not \
         advance: nothing is overwritten and the edit is still in the file for \
         the next `push`.\n\n",
    );
    s.push_str(
        "Those operations stay on the queue afterwards, and `jired status` \
         reports them in `pending_ops` — one entry per operation, with \
         `\"status\": \"failed\"` and the error where there was one. They are the \
         record of an interrupted or refused push, not a second copy of the \
         work: the edit is still in the file. `jired push --resume` replays them \
         and clears the queue; it re-plans from the file, so a queue whose edits \
         you have since reset or pushed another way comes back empty. `jired \
         reset` clears them too, along with the edits behind them.\n\n",
    );
    s.push_str(
        "A push refused as stale — exit 5, the issue named in `stale` — is one \
         the server changed since the last sync. `push --refresh-stale` re-fetches \
         those issues and retries the ones that still merge, which is the right \
         answer when somebody logged time against an issue whose labels you are \
         sending. An incoming change touching a field you also changed is a real \
         conflict and stays refused; `push --force` sends anyway and overwrites \
         it.\n\n",
    );

    s.push_str("## Finding issues\n\n");
    s.push_str(
        "`jired list` matches the issue files already on disk — no network, and \
         nothing is sent. Filters combine, and an issue has to match all of \
         them. Bare, it prints a line per matching issue — key, status, \
         summary; `--columns` picks the fields instead, `--keys` prints the key \
         alone, and `--json` the lot.\n\n",
    );
    s.push_str("| Flag | Matches |\n|---|---|\n");
    s.push_str("| `--sprint S` | issues whose *current* sprint is this one |\n");
    s.push_str("| `--status S` | issues in this status; repeat the flag for \"any of\" |\n");
    s.push_str("| `--not-status S` | issues in any status but this one; repeatable |\n");
    s.push_str("| `--category new\\|indeterminate\\|done` | Jira's own bucket behind the status, the same across renamed workflows |\n");
    s.push_str("| `--assignee U` | any part of the name, or the id |\n");
    s.push_str("| `--label L` | issues carrying this label |\n");
    s.push_str("| `--issue-type T` | issues of exactly this type, ignoring case — *not* a substring, so `sub-task` does not match `Analytics sub-task` |\n");
    s.push_str("| `--summary-contains TEXT` | case-insensitive substring of the summary |\n");
    s.push_str("| `--columns key,type,status,assignee,summary` | print these fields, in this order, instead of the default line |\n");
    s.push_str("| `--keys` | print only the keys, one per line |\n\n");
    s.push_str(
        "\"Not finished yet\" is `--category new` plus `--category indeterminate` \
         asked separately, or simpler, `--not-status Done` — do not enumerate \
         the unfinished statuses by hand.\n\n",
    );
    s.push_str(
        "`--keys` is what makes the rest composable: `jired list --sprint \
         \"Sprint 7\" --status \"To Do\" --keys` feeds `jired set`, `jired label`, \
         `jired transition` or `jired sprint add`, all of which take several \
         keys — as separate arguments: one argument holding several keys, which \
         is what an unsplit shell variable gives, is exit 2. \
         Searching the *server* rather than the working copy is `jired \
         search JQL`.\n\n",
    );
    s.push_str(
        "`--columns` takes any of `key`, `type`, `status`, `assignee`, \
         `summary`, `sprint`, `labels` and `file`, in any order, and is \
         tab-separated whenever stdout is not a terminal — `jired list \
         --columns key,type | grep -P '\\tBug$'` needs no JSON parser. The \
         values are the ones `--json` carries, except for the two the table \
         shortens: `assignee` is the name out of `@[Name](id)`, and `sprint` is \
         the *last* of the sprints the issue has been in — where it is now — \
         while `--json` carries the whole history as an array, the way the file \
         has it. Splitting carry-over from new work is that array, so reach for \
         `--json` for it, for the assignee id, for a field no column offers, or \
         when you want a document rather than a table.\n\n",
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
         and not sent. `status` and `diff` follow the same rule, so an issue \
         whose only difference is sprint history is not reported as changed; \
         where there is a real change too, `diff` keeps the sprint row and marks \
         it `\"sendable\": false`, out of the `stat` counts.\n\n",
    );
    s.push_str(
        "A sub-task is not a board item: it takes its sprint from its parent, and \
         a `sprint:` on one is exit 2 naming the parent to move instead. It is \
         refused at plan time, so `push --dry-run` shows it, and nothing is sent \
         and nothing is overwritten — the edit stays in the file.\n\n",
    );
    s.push_str(
        "On Data Center, `push` checks the fields it is about to send against \
         the issue's edit screen, and `create` against the create screen for \
         its project and type. A field the screen does not carry is exit 2 at \
         plan time, named in the error and visible in `--dry-run`, rather than \
         a 400 after the rest of the push has gone. Nothing is refused when the \
         server describes no screen.\n\n",
    );
    s.push_str(
        "`assignee:` is sent as its own operation on both deployments, shown in \
         `--dry-run` as `assign` against `/issue/{key}/assignee` rather than \
         inside a field update — assigning is a permission of its own, and an \
         instance whose edit screen has no assignee on it refuses the field and \
         accepts the route. Read `body.accountId` or `body.name` for what is \
         being sent, not `body.fields.assignee`.\n\n",
    );
    s.push_str(
        "`jired parent KEY… --to ISSUE` moves sub-tasks under a different parent, \
         and stages like `sprint add` does; editing `parent:` in the file is the \
         same change. Cloud sends a field update. Data Center has no REST call \
         for it — `PUT /issue` accepts the payload and ignores it — so `push` \
         drives the same Move wizard the UI does, shown in `--dry-run` as \
         `move_parent`, and then re-reads the issue: a parent that did not \
         actually move fails the push with the edit still in the file. Three \
         things are exit 2 at plan time, so `--dry-run` shows them: leaving a \
         sub-task with no parent, a parent in another project, and a credential \
         that cannot open a web session. That last one is the common case on \
         Data Center — the wizard is a Jira page, and a personal access token \
         authenticates the REST API only, so this operation needs \
         `auth.kind = basic`. `jired parent` warns about it while staging; the \
         staged line is still correct, and moving the sub-task in Jira's UI \
         makes it true.\n\n",
    );
    s.push_str(
        "A push that moves an issue between sprints, columns or parents rebuilds \
         `sprints/`, `boards/` and `by-parent/` before it returns, so the folder \
         for a sprint you just created appears with the first issue you move \
         into it — including a `future` sprint, which is a folder like any \
         other. Only closed sprints are left out, until \
         `sprints.include_closed`.\n\n",
    );
    s.push_str(
        "`jired sprint add KEY… --sprint S` and `jired sprint remove KEY…` are the \
         scriptable way to move issues, and stage like `transition` and `assign` do: \
         they write `sprint:` and `push` sends it, or `--push` does immediately. \
         `--sprint` takes a name or an id and is checked against the sprints already \
         fetched, so a name no board knows is exit 2 rather than a skipped push. An \
         issue already in the sprint is reported, not staged.\n\n",
    );
    s.push_str(
        "`jired sprint create NAME [--board B] [--start TS] [--end TS] [--goal TEXT] \
         [--issues K1,K2] [--push]` makes a sprint. It is the one write that does not go \
         through `push` — a sprint has no file to stage it in — so `-n/--dry-run` prints \
         the call instead of sending it. `--board` takes a name or an id and is only \
         needed when the project has more than one; the dates are read in the local \
         timezone unless they name one, and Jira takes both or neither. `--issues` \
         moves those issues into it as `sprint add` would.\n\n",
    );
    s.push_str(
        "`jired sprint start SPRINT`, `jired sprint edit SPRINT` and `jired sprint \
         close SPRINT` are the rest of a sprint's life, and go straight to the \
         server for the same reason, with the same `-n`. They are one endpoint \
         underneath and it *merges*: only the flags you pass are sent, and what is \
         not sent is what the sprint keeps. `edit --goal TEXT` is the only way to \
         set a goal after the sprint exists — `create --goal` sets one at creation \
         and nothing else reaches the field — and `--goal ''` clears it; \
         `--name`, `--start` and `--end` change the rest. `SPRINT` is a name or an \
         id, matched like `--sprint` is.\n\n",
    );
    s.push_str(
        "Two refusals happen before anything is sent, as exit 2 rather than a 400 \
         that names nothing: a sprint with no dates cannot be started (pass \
         `--start` and `--end` to set them and start it in one call), and a board \
         runs one sprint at a time, so a second is refused naming the one in the \
         way.\n\n",
    );
    s.push_str(
        "`jired sprint close SPRINT` always names the issues it would leave \
         unfinished — the ones that would otherwise point at a closed sprint with \
         nothing carried over — and `--move-incomplete-to SPRINT` moves them into \
         the next sprint first, pushing the moves before the close, because an \
         issue cannot be got out of a sprint that is already closed. \"Unfinished\" \
         is the status *category* rather than a status name, so it reads the same \
         on a workflow that says `Done` and one that does not.\n\n",
    );
    s.push_str("## Versions\n\n");
    s.push_str(
        "`fix_versions:` and `affects_versions:` name the project's versions. \
         `jired releases [--all]` lists them; `jired release create NAME \
         [--start D] [--release-date D] [--description T] [--issues K1,K2] [--push]` \
         makes one, `jired release edit VERSION` renames or re-dates it, `jired \
         release ship VERSION [--date D] [--move-unfixed-to VERSION]` releases it and \
         `jired release archive VERSION` archives it. Like the sprint commands they \
         go straight to the server and take `-n/--dry-run`; `VERSION` is a name or an \
         id. Dates are days, `2026-10-29`. `ship` names the issues in the version \
         that are not done, by status category, and `--move-unfixed-to` moves them \
         on and pushes that before releasing.\n\n",
    );
    s.push_str(
        "A version name a push adds to `fix_versions:` or `affects_versions:` is \
         checked against the project's versions before anything is sent: an unknown \
         one is exit 2 at plan time, shown by `push --dry-run`, with the known names \
         in the hint. Create it with `jired release create` first — the new version \
         is usable immediately, with no fetch in between.\n\n",
    );
    s.push_str(
        "`jired boards --json` names the project a board is filed under in \
         `project_key`, which is what makes a board selectable without a human. \
         Where the listing leaves it out — every board, on Data Center — it is \
         asked of the board itself. Treat `null` as \"unknown\" rather than \
         \"no project\": a board whose filter spans several projects has no one \
         answer, and one the account cannot read stays unplaced. \
         `jired sprints` needs `--board` only when the project's boards would \
         answer differently — boards sharing one sprint pool answer the same, \
         and then the answer is simply given. The sprint a `sprint create` \
         returns is usable in a `sprint:` immediately; no fetch is needed \
         first.\n\n",
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
    // The sentinels are deliberately not reproduced here. A body containing its
    // own end marker would be cut short at the wrong place the next time the
    // block is rewritten, leaving half of the old one behind it.
    s.push_str(&format!(
        "This section of `CLAUDE.md` and `AGENTS.md` is generated, between the \
         `BEGIN jired` and `END jired` HTML comments that bracket it; anything \
         written outside them survives a regeneration. The opening comment \
         carries the version of the jired that wrote it — **{}** here. `jired \
         status` reports a block written by some other version in \
         `stale_agent_docs` and says so on the way past; `jired init-agents` \
         rewrites it, and `jired doctor` does the same alongside its other \
         repairs. Trust this document only while those agree: it describes the \
         binary that generated it, not necessarily the one you are about to \
         run.\n\n",
        env!("CARGO_PKG_VERSION")
    ));

    s.push_str("## Lossy regions\n\n");
    s.push_str(
        "Blocks between `<!--jired:raw:ID-->` and `<!--/jired:raw:ID-->` are Jira content \
         Markdown cannot represent (panels, macros, media). They are read-only: leave them \
         untouched and they push back byte-identical. Editing one makes `push` fail with \
         exit code 6 unless you pass `--allow-lossy`, which sends what the file says \
         instead, as plain Markdown, and discards the original markup. The rule covers a \
         region wherever it sits — the description, a comment, a worklog note — and a \
         region indented inside a list or a quote is not an edit. Files containing such a \
         region have `lossy: true` in the `jired:` block.\n\n",
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

    s.push_str("## Undoing local changes\n\n");
    s.push_str(
        "`jired reset [ISSUES…]` puts files back to the state of the last sync — the \
         same state `push` diffs against — so a reset issue has nothing left to send. \
         It is offline and does not need the file to parse, which makes it the way out \
         of an edit that broke one. Run `--dry-run` first: it lists every issue and \
         names the fields that would go. Non-interactively it needs `--yes`, or it \
         exits 7 rather than dropping work on a script's say-so.\n\n",
    );
    s.push_str("```sh\n");
    s.push_str("jired reset --dry-run --json   # what would be dropped, field by field\n");
    s.push_str("jired reset PROJ-142 --yes --json\n");
    s.push_str("```\n\n");
    s.push_str(
        "Two things it reports rather than deletes, because the last sync cannot bring \
         them back: staged attachment uploads, whose only copy is the file in the \
         issue's folder, and `NEW-*` issues, which have no earlier state at all \
         (`--include-new` deletes those). `jired reset` goes backwards to the last \
         sync; `jired pull --force` goes forwards to whatever the server holds now.\n\n",
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
        (
            10,
            "unsupported Jira server, including a change it accepted and did not apply",
        ),
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
    s.push_str("| `--silent` | Say nothing unless the command fails: `--quiet` plus the warnings and the retry log. The error and the exit code survive it, and `--json` keeps its document, warnings and all. |\n");
    s.push_str("| `--dry-run` (`-n`) | On `push`: the exact API calls, nothing sent. On `pull`: what would be written, nothing touched. |\n");
    s.push_str("| `--fields <PATHS>` | Keep only these dotted paths of the `--json` document: `results.key,results.ops.summary`. Cheaper than parsing the whole thing. |\n");
    s.push_str("| `--json-compact` | Drop null and empty values from the `--json` document. |\n\n");
    s.push_str(
        "A full document is a complete record, which is the wrong size when you \
         wanted three values. `--fields` takes dotted paths, comma-separated or \
         repeated; a path walks objects and *maps over* arrays, so `results.ok` \
         keeps the `ok` of every result rather than needing an index. Both flags \
         are opt-in because a projected or compacted document no longer matches \
         its published schema — which is the point of asking for one.\n\n",
    );
    s.push_str("```sh\n");
    s.push_str("jired status --json --fields dirty,new_issues\n");
    s.push_str("jired push --json --fields summary,results.key,results.ok\n");
    s.push_str("jired list --sprint \"Sprint 7\" --json --json-compact\n");
    s.push_str("```\n\n");
    s.push_str(
        "Every JSON document carries a `schema` field such as `jired/pull/1`, and \
         `schema` survives a projection. Run `jired schema` to list the JSON \
         Schemas and `jired schema pull` to print one. `jired push --json` leads \
         with `summary` — `{\"ok\": 5, \"failed\": 0, \"stale\": [\"PROJ-194\"]}` — so \
         the outcome is one read rather than a walk over every operation of every \
         issue.\n\n",
    );

    // The same list the permission files are built from, so an agent reading
    // these docs knows what it can run before it tries and gets stopped.
    s.push_str("## Commands you may run without asking\n\n");
    s.push_str(&format!(
        "`jired init-agents` grants these in `{CLAUDE_SETTINGS}` and `{CODEX_RULES}`. \
         Each one sends nothing to Jira and writes nothing into the working copy; \
         some refresh the read-only snapshot under `.jired/`, which no file you edit \
         depends on. Everything else asks first, which is the point.\n\n"
    ));
    s.push_str("| Command | Why it is safe |\n|---|---|\n");
    for r in READ_ONLY {
        s.push_str(&format!("| `{}` | {} |\n", r.argv.join(" "), r.why));
    }
    s.push_str(
        "\nBoth permission systems match the start of the command line, so put the \
         global flags after the subcommand — `jired status --json`, not \
         `jired --json status` — or the grant does not apply.\n\n",
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

    /// `READ_ONLY` is written by hand beside a command table that moves, and
    /// 0.2.0 proved the two drift: `list` shipped read-only and `doctor` would
    /// not grant it, so an agent stopped to ask about a command that sends
    /// nothing and writes nothing.
    ///
    /// Every command has to be accounted for on one side or the other. A new
    /// one fails here until somebody decides which.
    #[test]
    fn every_command_is_either_granted_or_a_known_writer() {
        // What each of these writes, so that putting a read-only command here
        // to quiet the test would have to be written down as a lie first.
        const WRITES: &[(&str, &str)] = &[
            ("init", "creates the workspace and stores credentials"),
            ("comment", "stages a comment in the file"),
            ("logwork", "stages a worklog in the file"),
            ("create", "writes a new issue file"),
            ("edit", "writes the fields the form changed"),
            ("transition", "stages a status change"),
            ("assign", "stages an assignee"),
            ("label", "stages labels"),
            ("parent", "stages a sub-task's parent"),
            ("set", "stages any editable field"),
            ("attach", "stages a file for upload"),
            ("open", "launches a browser"),
            (
                "fetch-spec",
                "writes specification files into a directory the caller names",
            ),
            (
                "sprint",
                "stages a sprint move, and creates, starts, edits or closes a \
                 sprint on the server",
            ),
            (
                "release",
                "creates, edits, releases or archives a version on the server, \
                 and stages `fix_versions:` edits",
            ),
            (
                "watch",
                "adds this account to an issue's watchers on the server, or \
                 takes it off",
            ),
        ];

        let cli = crate::cli::Cli::command();
        let known: Vec<&str> = cli
            .get_subcommands()
            .flat_map(|s| {
                std::iter::once(s.get_name())
                    .chain(s.get_visible_aliases())
                    .collect::<Vec<_>>()
            })
            .collect();

        for name in known {
            assert!(
                READ_ONLY.iter().any(|r| r.argv.get(1) == Some(&name))
                    || WRITES.iter().any(|(w, _)| *w == name),
                "`jired {name}` is in neither list: grant it in READ_ONLY, or \
                 record what it writes in this test"
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

    /// The list grants commands by name, so a command that was renamed or
    /// removed would leave a rule granting nothing — or, worse, a rule granting
    /// whatever later takes that name.
    #[test]
    fn every_granted_command_still_exists() {
        let cmd = crate::cli::Cli::command();
        for r in READ_ONLY {
            assert_eq!(r.argv[0], "jired", "{:?} must start with `jired`", r.argv);
            let name = r.argv[1];
            let sub = cmd
                .get_subcommands()
                .find(|s| s.get_name() == name || s.get_all_aliases().any(|a| a == name))
                .unwrap_or_else(|| panic!("`jired {name}` is not a command"));
            // A second word is either a subcommand of the first or a flag it
            // takes; either way it has to be spelled the way clap spells it.
            if let Some(word) = r.argv.get(2) {
                let known = if let Some(flag) = word.strip_prefix("--") {
                    sub.get_arguments().any(|a| a.get_long() == Some(flag))
                } else {
                    sub.get_subcommands().any(|s| s.get_name() == *word)
                };
                assert!(known, "`jired {name} {word}` is not a thing");
            }
            assert!(!r.why.is_empty(), "{:?} has no justification", r.argv);
        }
    }

    /// The two permission systems have to grant the same set, or an agent's
    /// behaviour would depend on which one it happens to be.
    #[test]
    fn both_permission_files_grant_the_same_commands() {
        let claude = permission_entries();
        let codex = codex_rules();
        let docs = render();
        assert_eq!(claude.len(), READ_ONLY.len());
        for r in READ_ONLY {
            // The docs are the third copy of the list, and the one an agent
            // reads before it decides what to try.
            assert!(
                docs.contains(&format!("| `{}` |", r.argv.join(" "))),
                "the generated docs do not list {:?}",
                r.argv
            );
            let pattern = r
                .argv
                .iter()
                .map(|t| format!("\"{t}\""))
                .collect::<Vec<_>>()
                .join(", ");
            assert!(
                codex.contains(&format!("pattern = [{pattern}]")),
                "codex rules do not grant {:?}",
                r.argv
            );
            assert!(claude.contains(&format!("Bash({}:*)", r.argv.join(" "))));
        }
    }

    /// Anything that writes must stay off the list: a granted `push` or `pull`
    /// would let an agent change Jira without anyone being asked.
    #[test]
    fn nothing_that_writes_is_granted() {
        for r in READ_ONLY {
            let line = r.argv.join(" ");
            let bare = r.argv.len() == 2;
            for writer in [
                "init",
                "pull",
                "push",
                "create",
                "edit",
                "comment",
                "logwork",
                "transition",
                "assign",
                "attach",
                "reset",
                "resolve",
                "clean",
                "doctor",
                "open",
            ] {
                assert!(
                    !(bare && r.argv[1] == writer),
                    "`{line}` writes and must not be granted unqualified"
                );
            }
            // The qualified ones earn their place with the flag that makes them
            // read-only, and only that flag.
            if !bare {
                assert!(
                    !line.contains("--dry-run") || r.argv.last() == Some(&"--dry-run"),
                    "`{line}` puts something after --dry-run"
                );
            }
        }
    }

    /// Someone else's entry in the settings file is someone else's decision.
    #[test]
    fn claude_settings_merge_keeps_what_was_already_there() {
        let existing = serde_json::json!({
            "model": "opus",
            "permissions": { "allow": ["Bash(git status:*)"], "deny": ["Bash(rm:*)"] }
        });
        let merged: serde_json::Value =
            serde_json::from_str(&claude_settings(&existing).unwrap()).unwrap();
        assert_eq!(merged["model"], "opus");
        assert_eq!(merged["permissions"]["deny"][0], "Bash(rm:*)");
        let allow = merged["permissions"]["allow"].as_array().unwrap();
        assert_eq!(allow[0], "Bash(git status:*)");
        assert!(claude_settings_current(&merged));
        // And merging twice adds nothing a second time.
        let again: serde_json::Value =
            serde_json::from_str(&claude_settings(&merged).unwrap()).unwrap();
        assert_eq!(again, merged);
    }

    #[test]
    fn an_empty_settings_file_gets_the_whole_list() {
        let fresh: serde_json::Value =
            serde_json::from_str(&claude_settings(&serde_json::json!({})).unwrap()).unwrap();
        assert!(claude_settings_current(&fresh));
        assert!(!claude_settings_current(&serde_json::json!({})));
    }

    /// A rules file jired regenerates must not swallow rules jired did not
    /// write.
    #[test]
    fn regenerating_the_rules_keeps_hand_written_ones() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".codex/rules/default.rules");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "prefix_rule(pattern = [\"make\", \"test\"])\n").unwrap();
        assert!(write_permissions(dir.path()).unwrap());
        // Idempotent: a second run finds both files already current.
        assert!(!write_permissions(dir.path()).unwrap());
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("[\"make\", \"test\"]"));
        assert!(text.contains("[\"jired\", \"status\"]"));
        assert_eq!(text.matches(RULES_BEGIN).count(), 1);
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
        assert_eq!(text.matches(&begin()).count(), 1);
    }

    /// The whole point of the stamp is telling an agent the instructions it is
    /// reading are not this binary's, so every state has to be distinguishable:
    /// a file this build wrote, one an older build wrote, one from before the
    /// stamp existed, and one that was never generated at all.
    #[test]
    fn a_block_from_another_jired_is_reported_as_stale() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let states = || -> Vec<DocState> { doc_states(root).into_iter().map(|(_, s)| s).collect() };

        // Nothing written yet: missing, not stale. A workspace that never asked
        // for instructions is not carrying wrong ones.
        assert_eq!(states(), vec![DocState::Missing, DocState::Missing]);

        for name in DOCS {
            write_managed_block(&root.join(name), &render()).unwrap();
        }
        assert_eq!(states(), vec![DocState::Current, DocState::Current]);
        assert_eq!(
            stamped_version(&std::fs::read_to_string(root.join("CLAUDE.md")).unwrap()).as_deref(),
            Some(env!("CARGO_PKG_VERSION"))
        );

        // An older jired's stamp, and a block from before stamping.
        std::fs::write(
            root.join("CLAUDE.md"),
            format!("{BEGIN_PREFIX} v0.0.1 -->\nold body\n{END}\n"),
        )
        .unwrap();
        std::fs::write(
            root.join("AGENTS.md"),
            format!("<!-- BEGIN jired -->\nold body\n{END}\n"),
        )
        .unwrap();
        assert_eq!(
            states(),
            vec![
                DocState::Stale {
                    generated_by: Some("0.0.1".into())
                },
                DocState::Stale { generated_by: None },
            ]
        );

        // And regenerating replaces the differently-stamped block in place
        // rather than appending a second one under it.
        let path = root.join("CLAUDE.md");
        write_managed_block(&path, &render()).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(text.matches(BEGIN_PREFIX).count(), 1);
        assert!(!text.contains("old body"));
        assert_eq!(doc_states(root)[0].1, DocState::Current);
    }

    /// The same version with a changed template is still not what the file
    /// says it is, so the body is compared as well as the stamp.
    #[test]
    fn an_edited_block_is_stale_even_under_the_right_stamp() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("CLAUDE.md");
        std::fs::write(&path, format!("{}\nnot the docs\n{END}\n", begin())).unwrap();
        assert_eq!(
            doc_states(dir.path())[0].1,
            DocState::Stale {
                generated_by: Some(env!("CARGO_PKG_VERSION").into())
            }
        );
    }

    /// The flags a scripted caller reaches for first. Each was in `--help` and
    /// in no generated table, which left the obvious move — parsing the whole
    /// `--json` document by hand — looking like the only one.
    #[test]
    fn the_flags_a_scripted_caller_needs_are_documented() {
        let doc = render();
        for needle in [
            "`--fields <PATHS>`",
            "--json-compact",
            "jired diff --stat",
            "--exit-code",
            "--summary-contains",
            "--issue-type",
            "--keys",
            "jired transition KEY[,KEY…] --to STATUS",
            "jired assign KEY[,KEY…] --user NAME|--me|--none",
            "jired set --field FIELD --value VALUE",
            "jired comment KEY -m TEXT",
            "pending_ops",
            "jired push --resume",
            "--refresh-stale",
        ] {
            assert!(doc.contains(needle), "generated docs missing {needle:?}");
        }
    }

    /// Every filter `jired list` takes has to appear, or an agent filters by
    /// hand over the whole working copy the way it would with no filters at
    /// all.
    #[test]
    fn every_list_filter_is_documented() {
        use clap::Args;
        let doc = render();
        let cmd = crate::cli::cmd::list_issues::ListArgs::augment_args(clap::Command::new("list"));
        for arg in cmd.get_arguments() {
            let Some(long) = arg.get_long() else { continue };
            assert!(
                doc.contains(&format!("`--{long}")),
                "`jired list --{long}` is missing from the generated docs"
            );
        }
    }
}
