//! `jired link` and `jired unlink` — write `links:` lines without the YAML.
//!
//! A link is a line in the file — `- is blocked by: TSK-739`, or
//! `- web: https://wiki.example/x "Release notes"` — and `push` already turns
//! an added or removed line into the call that makes it true. What was missing
//! was a way to write the line that did not mean editing the frontmatter by
//! hand: the agent that created five chained release tasks did it with a
//! Python script rewriting `links:`.
//!
//! Staged like `label` and `sprint add`: the file is written, `status` and
//! `diff` show it, `push` sends it — or `--push` does, now.

use crate::cli::cmd::stage::{self, Wording};
use crate::cli::context::Context;
use crate::cli::GlobalArgs;
use crate::doc::frontmatter::{parse_web, web_value, WEB};
use crate::doc::Frontmatter;
use crate::error::{ExitCode, JiredError, Result};
use crate::json::Out;
use clap::Args;
use std::collections::BTreeMap;

#[derive(Args, Debug, Clone, Default)]
pub struct LinkArgs {
    /// The issue, how it relates, and the other issue: `TSK-740 is blocked by
    /// TSK-739`
    ///
    /// The direction is the words between the two keys, spelled the way the
    /// instance's link types read (`blocks`, `is blocked by`, `relates to`).
    /// Either end can be several keys, comma-separated. With `--web`, only the
    /// issue.
    #[arg(value_name = "ISSUE DIRECTION OTHER", required = true, num_args = 1..)]
    pub words: Vec<String>,

    /// Link a web page instead of another issue
    #[arg(long, value_name = "URL")]
    pub web: Option<String>,

    /// The web link's title (defaults to the URL)
    #[arg(long, value_name = "TEXT", requires = "web")]
    pub title: Option<String>,

    /// Apply immediately instead of leaving the change staged
    #[arg(long)]
    pub push: bool,

    /// With --push, show the API calls without sending them
    #[arg(long, requires = "push")]
    pub dry_run: bool,
}

#[derive(Args, Debug, Clone, Default)]
pub struct UnlinkArgs {
    /// The issue, optionally how it relates, and the other issue: `TSK-740
    /// TSK-739` removes every link between the two, `TSK-740 is blocked by
    /// TSK-739` only that one. With `--web`, only the issue.
    #[arg(value_name = "ISSUE [DIRECTION] OTHER", required = true, num_args = 1..)]
    pub words: Vec<String>,

    /// Remove the web link to this URL instead
    #[arg(long, value_name = "URL")]
    pub web: Option<String>,

    /// Apply immediately instead of leaving the change staged
    #[arg(long)]
    pub push: bool,

    /// With --push, show the API calls without sending them
    #[arg(long, requires = "push")]
    pub dry_run: bool,
}

/// What one invocation writes or removes, before it is applied to any file.
#[derive(Debug, Clone, PartialEq)]
enum Target {
    /// `(direction, key)`. An empty direction, for `unlink`, is any.
    Issue(String, String),
    /// `(url, title)`.
    Web(String, String),
}

impl Target {
    fn line(&self) -> String {
        match self {
            Target::Issue(d, k) if d.is_empty() => k.clone(),
            Target::Issue(d, k) => format!("{d}: {k}"),
            Target::Web(url, title) => format!("{WEB}: {}", web_value(url, title)),
        }
    }

    fn json(&self) -> serde_json::Value {
        match self {
            Target::Issue(d, k) => serde_json::json!({"direction": d, "key": k}),
            Target::Web(url, title) => serde_json::json!({"web": url, "title": title}),
        }
    }
}

pub fn run_link(g: &GlobalArgs, args: &LinkArgs, out: Out) -> Result<ExitCode> {
    let mut ctx = Context::open(g, out)?;
    ctx.lock()?;
    let (issues, mut targets) = parse(&args.words, args.web.as_deref(), true)?;
    if let Some(url) = &args.web {
        targets = vec![Target::Web(
            url.trim().to_string(),
            args.title.clone().unwrap_or_default().trim().to_string(),
        )];
    }
    let state = crate::store::state::StateStore::open(&ctx.workspace)?;
    let types = state.link_types()?;
    let targets: Vec<Target> = targets
        .into_iter()
        .map(|t| canonical(t, &types))
        .collect::<Result<_>>()?;
    for t in &targets {
        if let Target::Issue(_, other) = t {
            if issues.contains(other) {
                return Err(
                    JiredError::usage(format!("{other} cannot be linked to itself"))
                        .with_hint("name a different issue on the right"),
                );
            }
        }
    }

    let adding = targets.clone();
    let staged = stage::stage(&mut ctx, &issues, out, move |doc| {
        Ok(add(&mut doc.frontmatter, &adding))
    })?;
    finish(
        &mut ctx,
        staged,
        "link",
        &targets,
        args.push,
        args.dry_run,
        out,
    )
}

pub fn run_unlink(g: &GlobalArgs, args: &UnlinkArgs, out: Out) -> Result<ExitCode> {
    let mut ctx = Context::open(g, out)?;
    ctx.lock()?;
    let (issues, mut targets) = parse(&args.words, args.web.as_deref(), false)?;
    if let Some(url) = &args.web {
        targets = vec![Target::Web(url.trim().to_string(), String::new())];
    }
    let removing = targets.clone();
    let staged = stage::stage(&mut ctx, &issues, out, move |doc| {
        Ok(remove(&mut doc.frontmatter, &removing))
    })?;
    finish(
        &mut ctx,
        staged,
        "unlink",
        &targets,
        args.push,
        args.dry_run,
        out,
    )
}

fn finish(
    ctx: &mut Context,
    staged: Vec<stage::Staged>,
    operation: &str,
    targets: &[Target],
    push: bool,
    dry_run: bool,
    out: Out,
) -> Result<ExitCode> {
    let lines: Vec<String> = targets.iter().map(Target::line).collect();
    let what = format!("`{}`", lines.join("`, `"));
    let (verb, already) = match operation {
        "link" => ("linked", format!("already has {what}")),
        _ => ("unlinked", format!("has no link {what}")),
    };
    let outcome = stage::finish(
        ctx,
        staged,
        &Wording {
            what: &what,
            verb,
            already: &already,
        },
        push,
        dry_run,
        out,
        serde_json::json!({
            "operation": operation,
            "links": targets.iter().map(Target::json).collect::<Vec<_>>(),
        }),
    )?;
    out.emit("link", &outcome.doc);
    Ok(outcome.exit)
}

/// Split the words into the issues on the left and the targets on the right.
///
/// The direction is whatever sits between the two ends, so it needs no quoting:
/// `jired link TSK-740 is blocked by TSK-739`. `needs_direction` is `link`'s
/// rule; `unlink` may leave it out to mean every link between the two.
fn parse(
    words: &[String],
    web: Option<&str>,
    needs_direction: bool,
) -> Result<(Vec<String>, Vec<Target>)> {
    let keys = |word: &str| -> Result<Vec<String>> {
        word.split(',')
            .map(str::trim)
            .filter(|k| !k.is_empty())
            .map(crate::project::naming::key_from_input)
            .collect()
    };
    let Some(first) = words.first() else {
        return Err(JiredError::usage("no issue given"));
    };
    let issues = keys(first)?;
    if web.is_some() {
        if words.len() > 1 {
            return Err(JiredError::usage(format!(
                "with --web, name only the issue; `{}` is left over",
                words[1..].join(" ")
            ))
            .with_hint("`jired link TSK-740 --web https://wiki.example/x --title \"Notes\"`"));
        }
        return Ok((issues, Vec::new()));
    }
    let usage = || {
        JiredError::usage("expected the issue, how it relates, and the other issue")
            .with_hint("`jired link TSK-740 is blocked by TSK-739`, or `--web URL` for a page")
    };
    if words.len() < 2 || (needs_direction && words.len() < 3) {
        return Err(usage());
    }
    let others = keys(words.last().expect("checked"))?;
    let direction = words[1..words.len() - 1].join(" ").trim().to_string();
    if direction.eq_ignore_ascii_case(WEB) {
        return Err(JiredError::usage("`web` is not a link direction")
            .with_hint("link a page with `--web URL`"));
    }
    let targets = others
        .into_iter()
        .map(|k| Target::Issue(direction.clone(), k))
        .collect();
    Ok((issues, targets))
}

/// The direction as the instance spells it, or a refusal naming the ones it
/// has — when the last fetch listed them. Without that list nothing is
/// checked here, and push refuses an unknown one at plan time as it always has.
fn canonical(target: Target, types: &[crate::api::IssueLinkType]) -> Result<Target> {
    let Target::Issue(direction, key) = target else {
        return Ok(target);
    };
    if types.is_empty() {
        return Ok(Target::Issue(direction, key));
    }
    let found = types
        .iter()
        .flat_map(|t| [&t.outward, &t.inward])
        .find(|d| d.eq_ignore_ascii_case(&direction));
    match found {
        Some(spelled) => Ok(Target::Issue(spelled.clone(), key)),
        None => {
            let mut known: Vec<String> = types
                .iter()
                .flat_map(|t| [t.outward.clone(), t.inward.clone()])
                .collect();
            known.sort();
            known.dedup();
            Err(JiredError::usage(format!(
                "`{direction}` is not a link direction this Jira knows"
            ))
            .with_detail("direction", direction)
            .with_detail("known", serde_json::json!(known))
            .with_hint(format!("one of: {}", known.join(", "))))
        }
    }
}

/// Write the lines that are not there yet, reporting whether any were.
///
/// Appended rather than re-sorted, so a file someone ordered by hand keeps its
/// order; an issue link goes before the first web line, where a pull would
/// have put it.
fn add(fm: &mut Frontmatter, targets: &[Target]) -> bool {
    let mut changed = false;
    for t in targets {
        let (kind, value) = match t {
            Target::Issue(d, k) => (d.clone(), k.clone()),
            Target::Web(url, title) => (WEB.to_string(), web_value(url, title)),
        };
        let present = fm.link_pairs().iter().any(|(d, v)| match t {
            Target::Issue(..) => d.eq_ignore_ascii_case(&kind) && v.eq_ignore_ascii_case(&value),
            // The same page under a new title is the same link, renamed.
            Target::Web(url, _) => d == WEB && parse_web(v).0 == *url && *v == value,
        });
        if present {
            continue;
        }
        if let Target::Web(url, _) = t {
            // A title change replaces the line rather than adding a second
            // link to the same page.
            fm.links
                .retain(|m| !m.iter().any(|(d, v)| d == WEB && parse_web(v).0 == *url));
        }
        let line = BTreeMap::from([(kind.clone(), value)]);
        let at = match kind == WEB {
            true => fm.links.len(),
            false => fm
                .links
                .iter()
                .position(|m| m.contains_key(WEB))
                .unwrap_or(fm.links.len()),
        };
        fm.links.insert(at, line);
        changed = true;
    }
    changed
}

/// Take the matching lines out, reporting whether any were there.
fn remove(fm: &mut Frontmatter, targets: &[Target]) -> bool {
    let before = fm.links.len();
    fm.links.retain(|m| {
        !m.iter().any(|(d, v)| {
            targets.iter().any(|t| match t {
                Target::Issue(dir, key) => {
                    d != WEB
                        && v.eq_ignore_ascii_case(key)
                        && (dir.is_empty() || d.eq_ignore_ascii_case(dir))
                }
                Target::Web(url, _) => d == WEB && parse_web(v).0 == *url,
            })
        })
    });
    fm.links.len() != before
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(s: &str) -> Vec<String> {
        s.split(' ').map(str::to_string).collect()
    }

    #[test]
    fn the_direction_is_the_words_between_the_keys() {
        let (issues, targets) =
            parse(&words("TSK-740 is blocked by TSK-739,TSK-738"), None, true).unwrap();
        assert_eq!(issues, ["TSK-740"]);
        assert_eq!(
            targets,
            [
                Target::Issue("is blocked by".into(), "TSK-739".into()),
                Target::Issue("is blocked by".into(), "TSK-738".into()),
            ]
        );
        assert!(parse(&words("TSK-740 TSK-739"), None, true).is_err());
        // `unlink` may leave the direction out.
        let (_, any) = parse(&words("TSK-740 TSK-739"), None, false).unwrap();
        assert_eq!(any, [Target::Issue(String::new(), "TSK-739".into())]);
        assert!(parse(&words("TSK-740 web TSK-739"), None, true).is_err());
        assert!(parse(&words("TSK-740 blocks"), Some("https://x"), true).is_err());
    }

    #[test]
    fn a_direction_takes_the_instance_spelling_or_is_refused() {
        let types = vec![crate::api::IssueLinkType {
            id: "1".into(),
            name: "Blocks".into(),
            inward: "is blocked by".into(),
            outward: "blocks".into(),
        }];
        let t = canonical(Target::Issue("Is Blocked By".into(), "A-1".into()), &types).unwrap();
        assert_eq!(t, Target::Issue("is blocked by".into(), "A-1".into()));
        let err = canonical(Target::Issue("depends on".into(), "A-1".into()), &types).unwrap_err();
        assert!(err.hint.as_deref().unwrap_or("").contains("is blocked by"));
        // No list, no check: push still refuses at plan time.
        assert!(canonical(Target::Issue("depends on".into(), "A-1".into()), &[]).is_ok());
    }

    #[test]
    fn lines_are_added_once_issue_links_before_web_ones_and_removed_by_match() {
        let mut fm = Frontmatter::default();
        let web = Target::Web("https://wiki.example/x".into(), "Notes".into());
        let blocks = Target::Issue("blocks".into(), "A-2".into());
        assert!(add(&mut fm, std::slice::from_ref(&web)));
        assert!(add(&mut fm, std::slice::from_ref(&blocks)));
        assert!(!add(&mut fm, &[blocks.clone(), web.clone()]), "nothing new");
        assert_eq!(fm.links[0].get("blocks").map(String::as_str), Some("A-2"));
        assert_eq!(
            fm.links[1].get(WEB).map(String::as_str),
            Some("https://wiki.example/x \"Notes\"")
        );
        // A new title for the same page replaces the line.
        assert!(add(
            &mut fm,
            &[Target::Web(
                "https://wiki.example/x".into(),
                "Renamed".into()
            )]
        ));
        assert_eq!(
            fm.web_links(),
            [("https://wiki.example/x".to_string(), "Renamed".to_string())]
        );

        assert!(remove(
            &mut fm,
            &[Target::Issue(String::new(), "A-2".into())]
        ));
        assert!(!remove(
            &mut fm,
            &[Target::Issue(String::new(), "A-2".into())]
        ));
        assert!(remove(
            &mut fm,
            &[Target::Web("https://wiki.example/x".into(), String::new())]
        ));
        assert!(fm.links.is_empty());
    }
}
