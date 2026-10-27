//! `jired filter` and `jired filters` — saved Jira filters.
//!
//! `jired search` could already run a JQL; saving one meant the browser, or a
//! hand-made call to `/rest/api/2/filter` with a token lifted out of the
//! session store, which is exactly what an agent should never be doing. These
//! are `jired release`'s neighbours and work the same way: a filter has no file
//! to stage in, so every write goes to the server there and then, and
//! `-n/--dry-run` prints the calls instead.
//!
//! A JQL is checked before it is saved, by asking the server how many issues
//! it matches. Jira stores a filter whose query it cannot run without a word,
//! and the first anyone hears of it is a board or a dashboard showing nothing.
//!
//! `FILTER` is an id or a name. Cloud finds a name among every filter the
//! account can see; Data Center has no route that searches filters at all, so
//! there a name is found only among the account's favourites — which is why a
//! filter made here is a favourite unless `--no-favourite` says otherwise.

use crate::api::filter::{self as plan, Call};
use crate::api::{Deployment, Filter, FilterInput, FilterListing, FilterUpdate, JiraApi};
use crate::api::{ShareInput, UserScope};
use crate::cli::cmd::prompt::confirm;
use crate::cli::cmd::search::{answer, ResultArgs};
use crate::cli::context::Context;
use crate::cli::GlobalArgs;
use crate::error::{ExitCode, JiredError, Result};
use crate::json::Out;
use crate::style::{self, CYAN, DIM};
use clap::{ArgGroup, Args, Subcommand};
use serde_json::{json, Value};

#[derive(Subcommand, Debug)]
pub enum FilterCmd {
    /// A filter's name, owner, JQL, sharing and link
    Show(ShowArgs),
    /// Save a JQL as a filter, checked against the server first
    Create(CreateArgs),
    /// Rename a filter, or change its JQL, description or star
    Edit(EditArgs),
    /// Share a filter with a project, a group, a person or everyone, or stop
    Share(ShareArgs),
    /// Delete a filter you own
    Delete(DeleteArgs),
    /// Run a filter's JQL; the same output as `jired search`
    Run(RunArgs),
}

#[derive(Args, Debug, Clone, Default)]
pub struct FiltersArgs {
    /// Only the filters this account owns
    #[arg(long, conflicts_with = "favourite")]
    pub mine: bool,

    /// Only the filters this account has starred
    #[arg(long)]
    pub favourite: bool,
}

#[derive(Args, Debug, Clone)]
pub struct ShowArgs {
    /// The filter: an id, or a name
    #[arg(value_name = "FILTER")]
    pub filter: String,
}

#[derive(Args, Debug, Clone)]
pub struct CreateArgs {
    /// What the filter is called
    #[arg(value_name = "NAME")]
    pub name: String,

    /// The query it saves
    #[arg(long, value_name = "JQL")]
    pub jql: String,

    /// What the filter is for
    #[arg(long, value_name = "TEXT")]
    pub description: Option<String>,

    /// Star it — the default, as Jira's own "Save as" does
    #[arg(long, overrides_with = "no_favourite")]
    pub favourite: bool,

    /// Do not star it. On Data Center a filter nobody starred can be named
    /// only by its id
    #[arg(long)]
    pub no_favourite: bool,

    /// Share it: `project:TSK`, `group:G`, `user:U`, `authenticated` or
    /// `global`; repeat for several
    #[arg(long, value_name = "SPEC")]
    pub share: Vec<String>,

    /// Show the API calls without sending them
    #[arg(long, short = 'n')]
    pub dry_run: bool,
}

#[derive(Args, Debug, Clone)]
pub struct EditArgs {
    /// The filter: an id, or a name
    #[arg(value_name = "FILTER")]
    pub filter: String,

    /// Rename it
    #[arg(long, value_name = "TEXT")]
    pub name: Option<String>,

    /// Replace its query, checked against the server first
    #[arg(long, value_name = "JQL")]
    pub jql: Option<String>,

    /// What the filter is for; `--description ''` clears it
    #[arg(long, value_name = "TEXT")]
    pub description: Option<String>,

    /// Star it
    #[arg(long, conflicts_with = "no_favourite")]
    pub favourite: bool,

    /// Take the star off
    #[arg(long)]
    pub no_favourite: bool,

    /// Show the API calls without sending them
    #[arg(long, short = 'n')]
    pub dry_run: bool,
}

#[derive(Args, Debug, Clone)]
#[command(group(ArgGroup::new("change").required(true).multiple(true).args(["add", "remove"])))]
pub struct ShareArgs {
    /// The filter: an id, or a name
    #[arg(value_name = "FILTER")]
    pub filter: String,

    /// Share it: `project:TSK`, `group:G`, `user:U`, `authenticated` or
    /// `global`; repeat for several
    #[arg(long, value_name = "SPEC")]
    pub add: Vec<String>,

    /// Stop sharing it: a spec as `filter show` prints it, or the share's id
    #[arg(long, value_name = "SPEC")]
    pub remove: Vec<String>,

    /// Show the API calls without sending them
    #[arg(long, short = 'n')]
    pub dry_run: bool,
}

#[derive(Args, Debug, Clone)]
pub struct DeleteArgs {
    /// The filter: an id, or a name
    #[arg(value_name = "FILTER")]
    pub filter: String,

    /// Show the API call without sending it
    #[arg(long, short = 'n')]
    pub dry_run: bool,
}

#[derive(Args, Debug, Clone)]
pub struct RunArgs {
    /// The filter: an id, or a name
    #[arg(value_name = "FILTER")]
    pub filter: String,

    #[command(flatten)]
    pub view: ResultArgs,
}

pub fn run(g: &GlobalArgs, cmd: &FilterCmd, out: Out) -> Result<ExitCode> {
    match cmd {
        FilterCmd::Show(args) => show(g, args, out),
        FilterCmd::Create(args) => create(g, args, out),
        FilterCmd::Edit(args) => edit(g, args, out),
        FilterCmd::Share(args) => share(g, args, out),
        FilterCmd::Delete(args) => delete(g, args, out),
        FilterCmd::Run(args) => run_filter(g, args, out),
    }
}

/// What every one of these commands starts from: the workspace, a client, and
/// who "you" is, for the owner column and the delete check.
struct Session {
    ctx: Context,
    client: Box<dyn JiraApi>,
    me: Option<String>,
}

fn connect(g: &GlobalArgs, out: Out) -> Result<Session> {
    let mut ctx = Context::open(g, out)?;
    let session = crate::store::session::SessionStore::open(&ctx.workspace)?;
    let account = session.require_account()?;
    let client = crate::api::build_client(&mut ctx, &account)?;
    Ok(Session {
        ctx,
        client,
        me: account.account_id.clone(),
    })
}

// ---- filters ---------------------------------------------------------------

pub fn run_list(g: &GlobalArgs, args: &FiltersArgs, out: Out) -> Result<ExitCode> {
    let s = connect(g, out)?;
    let client = s.client.as_ref();
    let data_center = client.deployment() == Deployment::DataCenter;
    let me = s.me.as_deref().unwrap_or_default();

    let (listing, mut filters) = if args.favourite {
        (
            "favourite",
            crate::api::block_on(client.filters(FilterListing::Favourites))?,
        )
    } else if args.mine {
        (
            "mine",
            crate::api::block_on(client.filters(FilterListing::Owned(me)))?,
        )
    } else {
        let mut all = crate::api::block_on(client.filters(FilterListing::Favourites))?;
        // On Data Center "mine" is read out of the favourites, so asking again
        // would fetch the same list to learn nothing.
        if !data_center {
            for f in crate::api::block_on(client.filters(FilterListing::Owned(me)))? {
                if !all.iter().any(|a| a.id == f.id) {
                    all.push(f);
                }
            }
        }
        ("all", all)
    };
    filters.sort_by_key(|f| f.name.to_lowercase());

    if filters.is_empty() {
        out.say("no filters");
    }
    let me = s.me.as_deref();
    let width = crate::cli::cmd::list::column(filters.iter().map(|f| f.name.chars().count()), 40);
    let owners =
        crate::cli::cmd::list::column(filters.iter().map(|f| owner(f, me).chars().count()), 20);
    for f in &filters {
        out.say(row(f, me, width, owners));
    }
    if data_center && !args.favourite {
        out.hint(
            "Data Center lists only starred filters — one you own and have not starred \
             is reached by its id: `jired filter show ID`",
        );
    }
    let base = client.base_url().to_string();
    out.emit(
        "filters",
        &json!({
            "listing": listing,
            "filters": filters.iter().map(|f| doc(f, &base)).collect::<Vec<_>>(),
        }),
    );
    Ok(ExitCode::Success)
}

/// One filter as a row: its id, its star, its name, whose it is, and who else
/// can run it.
fn row(f: &Filter, me: Option<&str>, width: usize, owners: usize) -> String {
    // Padded by characters, not bytes: a filter called `TSK 1.0.0 без
    // регулярных` is twice as long in bytes as it is on screen.
    let pad = |text: String, to: usize| {
        let n = to.saturating_sub(text.chars().count());
        text + &" ".repeat(n)
    };
    format!(
        "{} {} {} {}  {}",
        style::out(CYAN, format!("{:<8}", f.id)),
        if f.favourite { "★" } else { " " },
        pad(f.name.clone(), width),
        style::out(DIM, pad(owner(f, me), owners)),
        style::out(DIM, shares(f)),
    )
    .trim_end()
    .to_string()
}

fn owner(f: &Filter, me: Option<&str>) -> String {
    match &f.owner {
        Some(o) if me == Some(o.id.as_str()) => "you".to_string(),
        Some(o) => o.display_name.clone(),
        None => String::new(),
    }
}

fn shares(f: &Filter) -> String {
    if f.share_permissions.is_empty() {
        return "private".to_string();
    }
    f.share_permissions
        .iter()
        .map(|p| p.spec.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

/// A filter as the JSON documents carry it: the model, and where to open it.
fn doc(f: &Filter, base: &str) -> Value {
    let mut v = serde_json::to_value(f).unwrap_or(Value::Null);
    v["url"] = json!(plan::browse_url(base, &f.id));
    v
}

// ---- show, run -------------------------------------------------------------

fn show(g: &GlobalArgs, args: &ShowArgs, out: Out) -> Result<ExitCode> {
    let s = connect(g, out)?;
    let f = find(s.client.as_ref(), &args.filter)?;
    describe(&f, s.me.as_deref(), s.client.base_url(), out);
    out.emit("filter", &json!({ "filter": doc(&f, s.client.base_url()) }));
    Ok(ExitCode::Success)
}

/// Everything about a filter, a line each.
fn describe(f: &Filter, me: Option<&str>, base: &str, out: Out) {
    out.say(format!("{} {}", style::out(CYAN, &f.id), f.name));
    let who = owner(f, me);
    if !who.is_empty() {
        out.say(format!("owner:     {who}"));
    }
    out.say(format!(
        "favourite: {}",
        if f.favourite { "yes" } else { "no" }
    ));
    match f.share_permissions.as_slice() {
        [] => out.say("shared:    no one — private"),
        all => {
            for (i, p) in all.iter().enumerate() {
                let id = p.id.as_deref().map(|id| format!(" ({id})"));
                out.say(format!(
                    "{}{}{}",
                    if i == 0 { "shared:    " } else { "           " },
                    p.spec,
                    style::out(DIM, id.unwrap_or_default())
                ));
            }
        }
    }
    if let Some(text) = &f.description {
        out.say(format!("about:     {text}"));
    }
    out.say(format!("url:       {}", plan::browse_url(base, &f.id)));
    out.say(format!("jql:       {}", f.jql));
}

fn run_filter(g: &GlobalArgs, args: &RunArgs, out: Out) -> Result<ExitCode> {
    let s = connect(g, out)?;
    let f = find(s.client.as_ref(), &args.filter)?;
    if f.jql.trim().is_empty() {
        return Err(
            JiredError::usage(format!("filter {} has no JQL to run", f.id))
                .with_detail("filter_id", f.id.clone()),
        );
    }
    answer(s.client.as_ref(), &f.jql, &args.view, out)
}

// ---- create ----------------------------------------------------------------

fn create(g: &GlobalArgs, args: &CreateArgs, out: Out) -> Result<ExitCode> {
    let name = args.name.trim().to_string();
    if name.is_empty() {
        return Err(JiredError::missing_value("a filter needs a name")
            .with_hint("`jired filter create \"TSK 1.0.0\" --jql 'project = TSK'`"));
    }
    let jql = checked_jql(&args.jql)?;
    // Parsed before anything is asked of the server: a misspelt spec is a
    // mistake about the command, and finding it after the JQL check costs a
    // request to learn nothing.
    let specs: Vec<Spec> = args
        .share
        .iter()
        .map(|s| Spec::parse(s))
        .collect::<Result<_>>()?;

    let s = connect(g, out)?;
    let client = s.client.as_ref();
    let d = client.deployment();
    let shares: Vec<ShareInput> = specs
        .iter()
        .map(|spec| resolve(client, spec))
        .collect::<Result<_>>()?;

    // Jira keeps one filter of a name per owner, and refuses a second with a
    // 400 that does not say which filter is in the way.
    if let Some(me) = s.me.as_deref() {
        let named = crate::api::block_on(client.filters(FilterListing::Named(&name)))?;
        if let Some(there) = named
            .iter()
            .find(|f| f.owner.as_ref().is_some_and(|o| o.id == me))
        {
            return Err(JiredError::usage(format!(
                "you already have a filter called `{}`",
                there.name
            ))
            .with_detail("filter_id", there.id.clone())
            .with_hint(format!(
                "change it with `jired filter edit {} --jql …` instead",
                there.id
            )));
        }
    }

    let matches = check_jql(client, &jql)?;
    out.say(format!("the JQL matches {matches} issue(s)"));

    let input = FilterInput {
        name,
        jql,
        description: args.description.clone(),
        favourite: !args.no_favourite,
    };
    let planned = |id: &str| -> Vec<Call> {
        let mut calls = plan::create(d, &input, id);
        calls.extend(shares.iter().map(|sh| plan::add_share(d, id, sh)));
        calls
    };

    if args.dry_run {
        say_calls(&planned(plan::NEW), out)?;
        out.emit(
            "filter-create",
            &json!({
                "dry_run": true,
                "filter": Value::Null,
                "matches": matches,
                "requests": planned(plan::NEW),
            }),
        );
        return Ok(ExitCode::Success);
    }

    let made = crate::api::block_on(client.create_filter(&input))?;
    for share in &shares {
        // The filter exists by now, so a share that fails leaves it there,
        // private; saying so is the difference between retrying the share
        // and making a second filter.
        crate::api::block_on(client.add_filter_share(&made.id, share)).map_err(|e| {
            let hint = format!(
                "filter {} was saved; share it with `jired filter share {} --add {}`",
                made.id,
                made.id,
                share.spec()
            );
            e.with_detail("filter_id", made.id.clone()).with_hint(hint)
        })?;
    }
    let saved = match shares.is_empty() {
        true => made,
        false => crate::api::block_on(client.filter(&made.id))?,
    };

    // Data Center shares a new filter the way the account's default share
    // scope says before jired's own shares are added, and that default can
    // be "everyone".
    let asked: Vec<String> = shares.iter().map(|s| s.spec().to_lowercase()).collect();
    let extra: Vec<&str> = saved
        .share_permissions
        .iter()
        .filter(|p| !asked.contains(&p.spec.to_lowercase()))
        .map(|p| p.spec.as_str())
        .collect();
    if !extra.is_empty() {
        out.warn(format!(
            "filter {} is also shared with {} — the account's default sharing, not \
             anything asked for here; `jired filter share {} --remove {}` takes it off",
            saved.id,
            extra.join(", "),
            saved.id,
            extra[0]
        ));
    }

    describe(&saved, s.me.as_deref(), client.base_url(), out);
    out.emit(
        "filter-create",
        &json!({
            "dry_run": false,
            "filter": doc(&saved, client.base_url()),
            "matches": matches,
            "requests": planned(&saved.id),
        }),
    );
    Ok(ExitCode::Success)
}

// ---- edit ------------------------------------------------------------------

fn edit(g: &GlobalArgs, args: &EditArgs, out: Out) -> Result<ExitCode> {
    let mut update = FilterUpdate {
        name: match args.name.as_deref().map(str::trim) {
            Some("") => {
                return Err(
                    JiredError::usage("a filter cannot be renamed to nothing").with_hint(
                        "pass the new name — `jired filter edit 10400 --name \"TSK 1.0.1\"`",
                    ),
                )
            }
            other => other.map(str::to_string),
        },
        jql: args.jql.as_deref().map(checked_jql).transpose()?,
        // `--description ''` is how a description comes back off, so it is
        // sent as given rather than filtered for emptiness.
        description: args.description.clone(),
        favourite: match (args.favourite, args.no_favourite) {
            (true, _) => Some(true),
            (_, true) => Some(false),
            _ => None,
        },
    };
    // Checked before anything is opened, as `release edit` does: no flags is a
    // mistake about the command.
    if update.is_empty() {
        return Err(JiredError::missing_value("nothing to change").with_hint(
            "pass --name, --jql, --description, --favourite or --no-favourite; \
             sharing is `jired filter share`",
        ));
    }

    let s = connect(g, out)?;
    let client = s.client.as_ref();
    let f = find(client, &args.filter)?;
    if update.favourite == Some(f.favourite) {
        out.say(match f.favourite {
            true => format!("{} is already a favourite", f.name),
            false => format!("{} is already not a favourite", f.name),
        });
        update.favourite = None;
    }
    let matches = match &update.jql {
        Some(jql) => {
            let n = check_jql(client, jql)?;
            out.say(format!("the new JQL matches {n} issue(s)"));
            Some(n)
        }
        None => None,
    };
    let calls = plan::update(client.deployment(), &f, &update);
    change(&s, &f, &calls, matches, args.dry_run, out, || {
        crate::api::block_on(client.update_filter(&f, &update))
    })
}

/// Send a change to a filter that exists, or print it; then say where the
/// filter stands. Shared by `edit` and `share`, which publish one document.
fn change(
    s: &Session,
    f: &Filter,
    calls: &[Call],
    matches: Option<usize>,
    dry_run: bool,
    out: Out,
    send: impl FnOnce() -> Result<Filter>,
) -> Result<ExitCode> {
    let base = s.client.base_url();
    if calls.is_empty() {
        out.say("nothing to change");
    } else if dry_run {
        say_calls(calls, out)?;
    }
    if dry_run || calls.is_empty() {
        out.emit(
            "filter-update",
            &json!({
                "dry_run": dry_run,
                "filter": if dry_run { Value::Null } else { doc(f, base) },
                "matches": matches,
                "requests": calls,
            }),
        );
        return Ok(ExitCode::Success);
    }
    let updated = send()?;
    describe(&updated, s.me.as_deref(), base, out);
    out.emit(
        "filter-update",
        &json!({
            "dry_run": false,
            "filter": doc(&updated, base),
            "matches": matches,
            "requests": calls,
        }),
    );
    Ok(ExitCode::Success)
}

// ---- share -----------------------------------------------------------------

fn share(g: &GlobalArgs, args: &ShareArgs, out: Out) -> Result<ExitCode> {
    let specs: Vec<Spec> = args
        .add
        .iter()
        .map(|s| Spec::parse(s))
        .collect::<Result<_>>()?;

    let s = connect(g, out)?;
    let client = s.client.as_ref();
    let d = client.deployment();
    let f = find(client, &args.filter)?;

    let mut calls = Vec::new();
    let mut removed: Vec<String> = Vec::new();
    let mut removals: Vec<String> = Vec::new();
    for wanted in &args.remove {
        let wanted = wanted.trim();
        let hit = f
            .share_permissions
            .iter()
            .find(|p| p.spec.eq_ignore_ascii_case(wanted) || p.id.as_deref() == Some(wanted));
        let Some(hit) = hit else {
            let known: Vec<String> = f.share_permissions.iter().map(|p| p.spec.clone()).collect();
            return Err(JiredError::usage(format!(
                "filter {} is not shared with `{wanted}`",
                f.id
            ))
            .with_detail("filter_id", f.id.clone())
            .with_detail("known", json!(known))
            .with_hint(match known.is_empty() {
                true => "it is shared with no one".to_string(),
                false => format!("it is shared with: {}", known.join(", ")),
            }));
        };
        let Some(pid) = &hit.id else {
            return Err(JiredError::generic(format!(
                "the server named no id for the `{}` share, so it cannot be removed here",
                hit.spec
            )));
        };
        removed.push(hit.spec.to_lowercase());
        removals.push(pid.clone());
        calls.push(plan::remove_share(d, &f.id, pid));
    }

    let mut adds = Vec::new();
    for spec in &specs {
        let input = resolve(client, spec)?;
        let already = f.share_permissions.iter().any(|p| {
            p.spec.eq_ignore_ascii_case(&input.spec()) && !removed.contains(&p.spec.to_lowercase())
        });
        if already {
            out.say(format!(
                "{} is already shared with {}",
                f.name,
                input.spec()
            ));
            continue;
        }
        if input == ShareInput::Global && !f.share_permissions.is_empty() {
            out.warn(
                "sharing with everyone replaces every other share: Jira keeps nothing \
                 else on a public filter",
            );
        }
        calls.push(plan::add_share(d, &f.id, &input));
        adds.push(input);
    }

    change(&s, &f, &calls, None, args.dry_run, out, || {
        // In the order planned: removals first, so `--remove group:a --add
        // group:a` is a re-share rather than a share undone.
        for pid in &removals {
            crate::api::block_on(client.delete_filter_share(&f.id, pid))?;
        }
        for input in &adds {
            crate::api::block_on(client.add_filter_share(&f.id, input))?;
        }
        crate::api::block_on(client.filter(&f.id))
    })
}

// ---- delete ----------------------------------------------------------------

fn delete(g: &GlobalArgs, args: &DeleteArgs, out: Out) -> Result<ExitCode> {
    let s = connect(g, out)?;
    let client = s.client.as_ref();
    let f = find(client, &args.filter)?;
    let base = client.base_url();

    // Refused here rather than left to the server: a filter someone else owns
    // is usually one a board or a dashboard depends on, and the name that
    // matched it may have been meant for one of yours.
    let mine = match (&f.owner, s.me.as_deref()) {
        (Some(o), Some(me)) => o.id == me,
        _ => true,
    };
    if !mine {
        let by = f
            .owner
            .as_ref()
            .map(|o| o.display_name.clone())
            .unwrap_or_default();
        return Err(JiredError::auth(format!(
            "filter {} `{}` belongs to {by}; only its owner can delete it",
            f.id, f.name
        ))
        .with_detail("filter_id", f.id.clone())
        .with_detail("owner", by));
    }

    let call = plan::delete(client.deployment(), &f.id);
    let doc_of = |deleted: bool, dry_run: bool| {
        json!({
            "dry_run": dry_run,
            "deleted": deleted,
            "filter": doc(&f, base),
            "request": call,
        })
    };
    if args.dry_run {
        say_calls(std::slice::from_ref(&call), out)?;
        out.emit("filter-delete", &doc_of(false, true));
        return Ok(ExitCode::Success);
    }
    let question = format!("delete filter {} `{}`?", f.id, f.name);
    if !confirm(&question, s.ctx.yes, s.ctx.resolver.interactive())? {
        out.say("cancelled — nothing was deleted");
        out.emit("filter-delete", &doc_of(false, false));
        return Ok(ExitCode::Success);
    }
    crate::api::block_on(client.delete_filter(&f.id))?;
    out.say(format!("deleted filter {} `{}`", f.id, f.name));
    out.emit("filter-delete", &doc_of(true, false));
    Ok(ExitCode::Success)
}

// ---- shared ----------------------------------------------------------------

/// The filter an id or a name points at.
///
/// Matched the way `--sprint` is: `id:10400` is the id and nothing else, and a
/// bare number is taken only when every reading of it agrees — a filter called
/// `2026` and a filter whose id is 2026 are two filters, and picking either is
/// a guess. A name that more than one filter carries, which happens as soon as
/// two people save the same query under the same name, is refused with the ids.
fn find(client: &dyn JiraApi, wanted: &str) -> Result<Filter> {
    let wanted = wanted.trim();
    if wanted.is_empty() {
        return Err(JiredError::usage("no filter named")
            .with_hint("pass its id or its name — `jired filters` lists them"));
    }
    if let Some(id) = wanted.strip_prefix("id:").map(str::trim) {
        return by_id(client, id)?.ok_or_else(|| not_found(client, wanted));
    }

    let mut candidates: Vec<Filter> = Vec::new();
    if is_id(wanted) {
        candidates.extend(by_id(client, wanted)?);
    }
    for f in crate::api::block_on(client.filters(FilterListing::Named(wanted)))? {
        if !candidates.iter().any(|c| c.id == f.id) {
            candidates.push(f);
        }
    }
    match candidates.len() {
        0 => Err(not_found(client, wanted)),
        1 => Ok(candidates.remove(0)),
        _ => {
            let named: Vec<String> = candidates
                .iter()
                .map(|f| {
                    let by = f
                        .owner
                        .as_ref()
                        .map(|o| format!(", owned by {}", o.display_name))
                        .unwrap_or_default();
                    format!("id:{} ({}{by})", f.id, f.name)
                })
                .collect();
            Err(JiredError::usage(format!(
                "`{wanted}` could be any of {} filters",
                named.len()
            ))
            .with_detail("filter", wanted.to_string())
            .with_detail("ambiguous", true)
            .with_detail(
                "ids",
                json!(candidates.iter().map(|f| f.id.clone()).collect::<Vec<_>>()),
            )
            .with_detail("known", json!(named))
            .with_hint(format!("name one by its id: {}", named.join(", "))))
        }
    }
}

/// The filter with this id, or `None` when the server says there is none.
///
/// Jira answers a filter that does not exist and one the account may not see
/// with the same 400, and both mean "not this one" to a lookup.
fn by_id(client: &dyn JiraApi, id: &str) -> Result<Option<Filter>> {
    if !is_id(id) {
        return Ok(None);
    }
    match crate::api::block_on(client.filter(id)) {
        Ok(f) => Ok(Some(f)),
        Err(e) if matches!(status(&e), Some(400 | 404)) => Ok(None),
        Err(e) => Err(e),
    }
}

fn not_found(client: &dyn JiraApi, wanted: &str) -> JiredError {
    let err = JiredError::usage(format!("`{wanted}` is not a filter this account can see"))
        .with_detail("filter", wanted.to_string());
    match client.deployment() {
        Deployment::DataCenter => err.with_hint(
            "Data Center finds a filter by name only among the account's favourites — \
             pass its id, the number in `/issues/?filter=ID`",
        ),
        Deployment::Cloud => {
            err.with_hint("`jired filters` lists the ones you own or have starred")
        }
    }
}

/// How many issues a JQL matches, asked before it is saved.
///
/// One search with room for no issues: the server parses the whole query, and
/// names an unknown field or value, at the cost of a count.
fn check_jql(client: &dyn JiraApi, jql: &str) -> Result<usize> {
    let query = crate::api::SearchQuery {
        jql: jql.to_string(),
        start_at: 0,
        max_results: 0,
        fields: crate::api::FieldSet::Summary,
        expand_changelog: false,
    };
    match crate::api::block_on(client.search(&query)) {
        Ok(page) => Ok(page.total),
        // A 400 from search is the query, and the query is the user's input:
        // exit 2, with nothing saved.
        Err(e) if status(&e) == Some(400) => Err(JiredError::usage(format!(
            "Jira refused the JQL — {}",
            e.message
        ))
        .with_detail("jql", jql.to_string())
        .with_hint("nothing was saved; `jired search --count '…'` tries a query on its own")),
        Err(e) => Err(e),
    }
}

fn checked_jql(jql: &str) -> Result<String> {
    let jql = jql.trim();
    if jql.is_empty() {
        return Err(JiredError::missing_value("a filter needs a JQL")
            .with_hint("`--jql 'project = TSK AND fixVersion = \"1.0.0\"'`"));
    }
    Ok(jql.to_string())
}

fn status(e: &JiredError) -> Option<u64> {
    e.details.get("status").and_then(Value::as_u64)
}

fn is_id(text: &str) -> bool {
    !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit())
}

fn say_calls(calls: &[Call], out: Out) -> Result<()> {
    for call in calls {
        out.say(format!("{} {}", call.method, call.path));
        if !call.body.is_null() {
            out.say(serde_json::to_string_pretty(&call.body)?);
        }
    }
    Ok(())
}

/// A `--share` or `--add` value, before anything is looked up.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Spec {
    Project(String),
    Group(String),
    User(String),
    Authenticated,
    Global,
}

impl Spec {
    fn parse(text: &str) -> Result<Self> {
        let text = text.trim();
        let (kind, value) = match text.split_once(':') {
            Some((k, v)) => (k.trim().to_ascii_lowercase(), v.trim()),
            None => (text.to_ascii_lowercase(), ""),
        };
        let needs = |what: &str| {
            JiredError::usage(format!("`{text}` names no {what}"))
                .with_hint(format!("`{kind}:{}`", what.to_ascii_uppercase()))
        };
        Ok(match kind.as_str() {
            "project" if value.contains('/') => {
                return Err(JiredError::usage(format!(
                    "`{text}` is a project role, which jired cannot share with"
                ))
                .with_hint(
                    "share with the whole project — `project:KEY` — or set the role in Jira",
                ))
            }
            "project" if value.is_empty() => return Err(needs("project")),
            "project" => Spec::Project(value.to_ascii_uppercase()),
            "group" if value.is_empty() => return Err(needs("group")),
            "group" => Spec::Group(value.to_string()),
            "user" if value.is_empty() => return Err(needs("user")),
            "user" => Spec::User(value.to_string()),
            // Jira's own name for it, on the way out of the API, is accepted
            // too: it is what a payload read elsewhere would say.
            "authenticated" | "loggedin" if value.is_empty() => Spec::Authenticated,
            "global" if value.is_empty() => Spec::Global,
            _ => return Err(
                JiredError::usage(format!("`{text}` is not a way to share a filter")).with_hint(
                    "one of `project:KEY`, `group:NAME`, `user:NAME`, `authenticated`, `global`",
                ),
            ),
        })
    }
}

/// A spec with the ids the server takes in place of the names people use.
fn resolve(client: &dyn JiraApi, spec: &Spec) -> Result<ShareInput> {
    Ok(match spec {
        Spec::Project(key) => {
            let p = crate::api::block_on(client.project(key)).map_err(|e| {
                JiredError::usage(format!("`{key}` is not a project this account can see"))
                    .with_detail("project", key.clone())
                    .with_source(e)
            })?;
            ShareInput::Project {
                id: p.id,
                key: p.key,
            }
        }
        Spec::Group(name) => ShareInput::Group(name.clone()),
        Spec::User(query) => {
            let found = crate::api::block_on(client.search_users(query, UserScope::Any))?;
            // The id or the full name, exactly, before anything looser: `ann`
            // is a prefix of `anna`, and sharing with the wrong one is a leak.
            let exact: Vec<_> = found
                .iter()
                .filter(|u| u.id == *query || u.display_name.eq_ignore_ascii_case(query))
                .collect();
            let pick = match (exact.as_slice(), found.as_slice()) {
                ([one], _) => *one,
                ([], [one]) => one,
                ([], []) => {
                    return Err(JiredError::usage(format!("no user matches `{query}`"))
                        .with_detail("query", query.clone()))
                }
                _ => {
                    let names: Vec<String> = found
                        .iter()
                        .map(|u| format!("{} ({})", u.display_name, u.id))
                        .collect();
                    return Err(JiredError::usage(format!(
                        "`{query}` matches {} users",
                        found.len()
                    ))
                    .with_detail("matches", json!(names))
                    .with_hint(format!("pass the id — `user:{}`", found[0].id)));
                }
            };
            ShareInput::User(pick.id.clone())
        }
        Spec::Authenticated => ShareInput::Authenticated,
        Spec::Global => ShareInput::Global,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_share_spec_is_read_the_way_filter_show_prints_it() {
        assert_eq!(
            Spec::parse("project:tsk").unwrap(),
            Spec::Project("TSK".into())
        );
        assert_eq!(
            Spec::parse("group:jira-users").unwrap(),
            Spec::Group("jira-users".into())
        );
        assert_eq!(Spec::parse(" authenticated ").unwrap(), Spec::Authenticated);
        assert_eq!(Spec::parse("loggedin").unwrap(), Spec::Authenticated);
        assert_eq!(Spec::parse("GLOBAL").unwrap(), Spec::Global);

        for bad in ["project:", "everyone", "project:TSK/Developers", "global:x"] {
            let err = Spec::parse(bad).unwrap_err();
            assert_eq!(err.exit_code().as_i32(), 2, "{bad}");
        }
    }

    #[test]
    fn only_digits_are_an_id() {
        assert!(is_id("10400"));
        assert!(!is_id("TSK 1.0.0"));
        assert!(!is_id(""));
        assert!(!is_id("-1"));
    }
}
