//! Jira Data Center specifics [M1.6].
//!
//! Data Center runs REST v2, stores rich text as wiki markup, and identifies
//! users by username. Shares the transport with Cloud via [`crate::api::rest`].

use crate::api::client::SpecModule;
use crate::api::http::HttpClient;
use crate::api::rest::Rest;
use crate::error::{JiredError, Result};
use serde_json::{json, Value};

pub fn client(http: HttpClient) -> Rest {
    Rest::new(http)
}

/// Where the Move wizard starts for a sub-task.
const WIZARD_ENTRY: &str = "/secure/MoveSubTaskParent!default.jspa?id=";

/// What every step of that wizard has in its `action`, and nothing else on a
/// Jira page does.
const WIZARD: &str = "MoveSubTask";

/// The form field holding the new parent.
const PARENT_FIELD: &str = "parentIssue";

/// How many steps to follow before deciding the wizard is not one.
///
/// Jira's is two — choose the parent, confirm — and a version that adds one
/// still finishes well inside this. The bound is what stops a page that keeps
/// answering with a form from becoming a loop.
const MAX_STEPS: usize = 4;

/// Re-parent a sub-task by driving the wizard the Move dialog drives.
///
/// Data Center has no REST call for this. `PUT /issue` accepts a sub-task's
/// `parent`, answers 204 and leaves the parent where it was
/// ([JRASERVER-68763], open since 2018), and the only other route Atlassian
/// offers is a ScriptRunner endpoint — an app, not something jired can assume.
/// So this posts the same forms a browser would: fetch the step, send back
/// every field it declared with `parentIssue` filled in, and follow Jira's
/// redirect to the next one.
///
/// It is a web action, so it is unversioned and undocumented, and an upgrade
/// can move it. Two things make that survivable. The forms are read rather
/// than assumed — jired sends what the page says it wants, including the
/// `atl_token` that its XSRF check is made of — and success is never taken on
/// the wizard's word: `push` re-reads the issue afterwards and fails the
/// operation if the parent did not actually move.
///
/// [JRASERVER-68763]: https://jira.atlassian.com/browse/JRASERVER-68763
pub async fn move_subtask_parent(
    http: &HttpClient,
    key: &str,
    issue_id: &str,
    parent: &str,
) -> Result<()> {
    let mut page = http.get_page(&format!("{WIZARD_ENTRY}{issue_id}")).await?;
    // The first reply has to be the wizard. Only once a step has been posted
    // does a page without a form mean the wizard is over — before that it means
    // jired never reached one, and 0.4.0 read the difference the wrong way
    // round: an unauthenticated request lands on the session-timeout page,
    // Jira serves it as HTTP 200, and "no form here" was taken for "finished".
    if Form::find(&page.html, WIZARD).is_none() {
        return Err(not_the_wizard(key, &page.url));
    }

    // The new parent goes in on the step that asks for it; the steps after it
    // are confirmations, which carry the answer along in their hidden fields.
    let mut answer = Some(parent);
    let mut posted: Vec<String> = Vec::new();

    for _ in 0..MAX_STEPS {
        let Some(form) = Form::find(&page.html, WIZARD) else {
            // No wizard form left on the page: Jira has sent us back to the
            // issue, which is what finishing looks like.
            return Ok(());
        };
        // A step that answers with itself is Jira re-rendering the form around
        // its own complaint — the parent it will not accept, most often.
        if posted.contains(&form.action) {
            return Err(refused(key, parent, complaint(&page.html)));
        }
        let fields = form.submission(answer.take().map(|p| (PARENT_FIELD, p)));
        posted.push(form.action.clone());
        page = http.post_form(&form.action, &fields).await?;
    }

    Err(
        JiredError::unsupported_server(format!("{key}: Jira's Move wizard did not finish"))
            .with_detail("issue", key.to_string())
            .with_detail("parent", parent.to_string())
            .with_detail("steps", MAX_STEPS)
            .with_hint("move the sub-task in Jira and run `jired pull`"),
    )
}

/// The wizard was asked for and something else answered.
///
/// The page jired landed on is the whole diagnosis, so it goes in the message
/// rather than into a debug log nobody has running when it happens: Jira
/// redirects an unauthenticated web request to its session-timeout page and
/// answers 200, so the status line says nothing and the URL says everything.
fn not_the_wizard(key: &str, landed: &str) -> JiredError {
    let signed_out = ["sessiontimeout", "login.jsp", "/login?", "os_destination"]
        .iter()
        .any(|marker| landed.to_ascii_lowercase().contains(marker));

    if signed_out {
        return JiredError::auth(format!(
            "{key}: the credential does not authenticate Jira's web pages — the \
             Move wizard answered with {landed}"
        ))
        .with_detail("issue", key.to_string())
        .with_detail("landed_on", landed.to_string())
        .with_hint(
            "a personal access token authenticates the REST API only; \
             re-authenticate with `jired init --force --auth basic` if the \
             instance allows it, or move the sub-task in Jira and run `jired pull`",
        );
    }

    JiredError::unsupported_server(format!(
        "{key}: Jira did not serve the Move wizard — the request for it answered \
         with {landed}"
    ))
    .with_detail("issue", key.to_string())
    .with_detail("landed_on", landed.to_string())
    .with_hint(
        "the account may lack the Move Issues permission, or this Jira spells \
         the wizard differently; move the sub-task in Jira and run `jired pull`",
    )
}

fn refused(key: &str, parent: &str, why: Option<String>) -> JiredError {
    let message = match &why {
        Some(why) => format!("{key}: Jira would not move it under {parent}: {why}"),
        None => format!("{key}: Jira would not move it under {parent}"),
    };
    JiredError::usage(message)
        .with_detail("issue", key.to_string())
        .with_detail("parent", parent.to_string())
        .with_hint(
            "check that the parent exists, is in the same project, and is not \
             itself a sub-task",
        )
}

/// One HTML form, as the page declared it.
#[derive(Debug, PartialEq, Eq)]
struct Form {
    action: String,
    /// Every named control, in the order they appeared.
    fields: Vec<(String, String)>,
}

impl Form {
    /// The first form on the page whose `action` names `wanted`.
    ///
    /// Jira pages carry forms jired has no business in — the header's search
    /// box, the dashboard's gadgets — so the wizard is found by what it posts
    /// to rather than by being first.
    fn find(html: &str, wanted: &str) -> Option<Self> {
        // Lowercased once and indexed in step with the original: tag and
        // attribute names are matched case-insensitively, values are not.
        let lower = html.to_ascii_lowercase();
        let mut at = 0;
        while let Some(start) = lower[at..].find("<form").map(|i| i + at) {
            // An unterminated tag is the end of anything readable on the page.
            let tag_end = lower[start..].find('>').map(|i| i + start)?;
            at = tag_end;
            let action = attr(&html[start..tag_end], "action").unwrap_or_default();
            if action.contains(wanted) {
                let end = lower[tag_end..]
                    .find("</form>")
                    .map(|i| i + tag_end)
                    .unwrap_or(html.len());
                return Some(Self {
                    action,
                    fields: controls(&html[tag_end..end]),
                });
            }
        }
        None
    }

    /// What to post back: everything the form declared, with `answer` written
    /// over the field it names.
    ///
    /// Sending the whole form rather than a hand-written payload is what keeps
    /// this working across versions: the `atl_token` the XSRF check wants, the
    /// issue id, and whatever else a given Jira threads through its wizard all
    /// travel because the page asked for them, not because jired knew to.
    fn submission(&self, answer: Option<(&str, &str)>) -> Vec<(String, String)> {
        let mut out: Vec<(String, String)> = self.fields.clone();
        if let Some((name, value)) = answer {
            match out.iter_mut().find(|(n, _)| n == name) {
                Some(field) => field.1 = value.to_string(),
                None => out.push((name.to_string(), value.to_string())),
            }
        }
        out
    }
}

/// The named controls of a form body: inputs and buttons, submit included.
///
/// The submit button matters as much as the hidden fields do — a webwork action
/// dispatches on which one was pressed, so a payload without it is a step Jira
/// does not recognize. Only one is taken, and never a cancel: a form that
/// offers both would otherwise be submitted as both at once.
fn controls(body: &str) -> Vec<(String, String)> {
    let lower = body.to_ascii_lowercase();
    let mut out: Vec<(String, String)> = Vec::new();
    let mut submitted = false;
    let mut at = 0;
    while let Some(start) = next_control(&lower, at) {
        let end = lower[start..]
            .find('>')
            .map(|i| i + start)
            .unwrap_or(body.len());
        let tag = &body[start..end];
        at = end;

        let Some(name) = attr(tag, "name").filter(|n| !n.is_empty()) else {
            continue;
        };
        let kind = attr(tag, "type").unwrap_or_default().to_ascii_lowercase();
        let value = attr(tag, "value").unwrap_or_default();
        match kind.as_str() {
            "submit" | "button" if submitted || name.eq_ignore_ascii_case("cancel") => {}
            "submit" | "button" => {
                submitted = true;
                out.push((name, value));
            }
            // An unticked checkbox is not submitted by a browser either, and
            // sending one would turn a default off into a default on.
            "checkbox" | "radio" if attr(tag, "checked").is_none() => {}
            _ => out.push((name, value)),
        }
    }
    out
}

/// The value of one attribute of an HTML tag.
///
/// Attribute names are matched on a word boundary, so `action` does not find
/// itself inside `formaction`.
fn attr(tag: &str, name: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();
    let mut from = 0;
    while let Some(at) = lower[from..].find(name).map(|i| i + from) {
        from = at + name.len();
        let before_ok = tag[..at]
            .chars()
            .next_back()
            .is_some_and(char::is_whitespace);
        let after = tag[from..].trim_start();
        if !before_ok {
            continue;
        }
        // A bare attribute — `checked`, `disabled` — has no value and is
        // reported as an empty one, which is what makes it findable at all.
        let Some(after) = after.strip_prefix('=') else {
            return Some(String::new());
        };
        let after = after.trim_start();
        let value = match after.chars().next() {
            Some(quote @ ('"' | '\'')) => after[1..].split(quote).next().unwrap_or_default(),
            _ => after.split_whitespace().next().unwrap_or_default(),
        };
        return Some(unescape(value));
    }
    None
}

/// The entities an attribute value can carry. Jira writes `&#39;` for an
/// apostrophe and `&amp;` for the ampersand in a query string, and a token that
/// arrived with either intact is a token the server will not recognize.
fn unescape(s: &str) -> String {
    s.replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&#x27;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

/// Where the next `<input>` or `<button>` starts, whichever comes first.
fn next_control(lower: &str, from: usize) -> Option<usize> {
    let find = |tag: &str| lower[from..].find(tag).map(|i| i + from);
    match (find("<input"), find("<button")) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

/// What a re-rendered wizard step is complaining about.
///
/// Best effort by design: the message is the difference between "Jira would not
/// move it" and "Jira would not move it: the parent must be in the same
/// project", and a version whose markup this does not recognize still gets the
/// first half.
fn complaint(html: &str) -> Option<String> {
    let lower = html.to_ascii_lowercase();
    let mut from = 0;
    while let Some(at) = lower[from..].find("error").map(|i| i + from) {
        from = at + "error".len();
        // Inside a class attribute, which is where Jira marks its messages.
        let Some(open) = html[..at].rfind('<') else {
            continue;
        };
        let Some(tag_end) = html[at..].find('>').map(|i| i + at) else {
            continue;
        };
        if !html[open..at].contains("class") {
            continue;
        }
        let text = strip_tags(&html[tag_end + 1..]);
        if !text.is_empty() {
            return Some(crate::project::naming::truncate(&text, 200));
        }
    }
    None
}

/// The first line of readable text in a fragment of HTML.
fn strip_tags(html: &str) -> String {
    let mut text = String::new();
    let mut depth = 0usize;
    for c in html.chars() {
        match c {
            '<' => depth += 1,
            '>' => depth = depth.saturating_sub(1),
            _ if depth == 0 => text.push(c),
            _ => {}
        }
        // Enough for a message; a whole page of markup is not one.
        if text.len() > 400 {
            break;
        }
    }
    unescape(text.split_whitespace().collect::<Vec<_>>().join(" ").trim())
}

/// What Jersey answers a WADL request with, and what it refuses.
///
/// The WADL resource produces XML only; the `application/json` on every other
/// jired request comes back a 406, so this `Accept` is not a preference but the
/// condition of getting an answer at all.
const WADL_ACCEPT: &str = "application/vnd.sun.wadl+xml, application/xml;q=0.9";

/// The REST modules a Data Center instance describes about itself.
///
/// Atlassian publishes no OpenAPI document for Data Center — the request for one
/// has been open for years — but every Jira mounts its REST modules on Jersey,
/// which serves each one's WADL beside it. These are the three jired speaks: the
/// platform API, the Agile API that the board and sprint commands live on, and
/// the session endpoints. Each is fetched separately because a Jira that has
/// stopped serving one still answers for the others.
pub fn spec_modules() -> Vec<SpecModule> {
    ["api/2", "agile/1.0", "auth/1"]
        .into_iter()
        .map(|module| SpecModule {
            name: module.replace('/', "-"),
            path: format!("/rest/{module}/application.wadl"),
            accept: WADL_ACCEPT.to_string(),
            extension: "wadl".to_string(),
        })
        .collect()
}

/// Assignee/reporter payload for Data Center: `name`, and an explicit null
/// clears the field.
pub fn user_field_value(user_id: Option<&str>) -> Value {
    match user_id {
        Some(id) => json!({ "name": id }),
        None => Value::Null,
    }
}

/// Wiki-markup mention syntax.
pub fn mention_markup(user_id: &str) -> String {
    format!("[~{user_id}]")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assignee_uses_username() {
        assert_eq!(user_field_value(Some("ilya")), json!({"name": "ilya"}));
    }

    #[test]
    fn mentions_use_wiki_syntax() {
        assert_eq!(mention_markup("ilya"), "[~ilya]");
    }

    /// A page shaped like the one Jira serves: a header form jired must not
    /// touch, then the wizard step.
    const STEP_ONE: &str = r##"
        <html><body>
        <form action="/secure/QuickSearch.jspa" method="get">
          <input type="text" name="searchString" value="">
          <input type="submit" name="Search" value="Search">
        </form>
        <form name="jiraform" action="MoveSubTaskParent.jspa" method="post">
          <input type="hidden" name="atl_token" value="BXTU-9RCF-BJLC-QQ7M|3f2&amp;b|lout">
          <input type="hidden" name="id" value="10412">
          <input type="text" name="parentIssue" id="parentIssue" value="DC-77">
          <input type="checkbox" name="sendMail" value="true">
          <input type="submit" name="Next" id="next_submit" value="Next &gt;&gt;">
          <input type="submit" name="cancel" value="Cancel">
        </form>
        </body></html>"##;

    #[test]
    fn the_wizard_form_is_found_past_the_ones_around_it() {
        let form = Form::find(STEP_ONE, WIZARD).expect("the wizard form");
        assert_eq!(form.action, "MoveSubTaskParent.jspa");
        assert_eq!(
            form.fields,
            vec![
                // The token arrives unescaped, which is the only form the
                // server's own XSRF check will match.
                ("atl_token".into(), "BXTU-9RCF-BJLC-QQ7M|3f2&b|lout".into()),
                ("id".into(), "10412".into()),
                ("parentIssue".into(), "DC-77".into()),
                // An unticked box is not submitted, and Cancel is not the
                // button being pressed.
                ("Next".into(), "Next >>".into()),
            ]
        );
    }

    /// What actually goes out: the page's own fields, with the new parent
    /// written over the one it came with.
    #[test]
    fn the_answer_replaces_the_field_it_names() {
        let form = Form::find(STEP_ONE, WIZARD).unwrap();
        let sent = form.submission(Some((PARENT_FIELD, "DC-78")));
        assert!(
            sent.contains(&("parentIssue".into(), "DC-78".into())),
            "{sent:?}"
        );
        assert_eq!(
            sent.iter().filter(|(n, _)| n == PARENT_FIELD).count(),
            1,
            "the old value is replaced, not accompanied: {sent:?}"
        );
        assert!(sent.iter().any(|(n, _)| n == "atl_token"), "{sent:?}");

        // The confirm step is submitted exactly as it arrived.
        assert_eq!(form.submission(None), form.fields);
    }

    /// A step whose field the page does not declare still goes out: a version
    /// that renames the picker is a failure jired can see, not one it causes.
    #[test]
    fn an_undeclared_field_is_added_rather_than_dropped() {
        let html = r#"<form action="MoveSubTaskConfirm.jspa">
                        <input type="hidden" name="id" value="1">
                      </form>"#;
        let sent = Form::find(html, WIZARD)
            .unwrap()
            .submission(Some(("parentIssue", "DC-78")));
        assert_eq!(
            sent,
            vec![
                ("id".into(), "1".into()),
                ("parentIssue".into(), "DC-78".into())
            ]
        );
    }

    #[test]
    fn a_page_without_the_wizard_is_the_end_of_it() {
        assert_eq!(Form::find("<html><body>DC-79</body></html>", WIZARD), None);
        // The browse page a finished move lands on carries forms of its own.
        let browse = r#"<form action="/secure/CommentAssignIssue!default.jspa"></form>"#;
        assert_eq!(Form::find(browse, WIZARD), None);
    }

    #[test]
    fn attributes_are_read_whole_and_on_word_boundaries() {
        let tag = r#"<input formaction="/other" name='parentIssue' value="DC-1" checked>"#;
        assert_eq!(attr(tag, "action").as_deref(), None);
        assert_eq!(attr(tag, "formaction").as_deref(), Some("/other"));
        assert_eq!(attr(tag, "name").as_deref(), Some("parentIssue"));
        assert_eq!(attr(tag, "checked").as_deref(), Some(""));
        assert_eq!(attr(tag, "type").as_deref(), None);
    }

    /// A ticked box is what the user's Jira has configured, and dropping it
    /// would quietly turn "notify watchers" off on every move.
    #[test]
    fn a_ticked_box_is_submitted() {
        let html = r#"<form action="MoveSubTaskParent.jspa">
            <input type="checkbox" name="sendMail" value="true" checked="checked">
        </form>"#;
        let form = Form::find(html, WIZARD).unwrap();
        assert_eq!(form.fields, vec![("sendMail".into(), "true".into())]);
    }

    /// The reported failure: a PAT reaches Jira, Jira bounces the web action to
    /// its session-timeout page, and serves that as HTTP 200. What names the
    /// cause is where the request landed, so that is what the error carries.
    #[test]
    fn a_session_timeout_page_is_named_as_the_credential_it_is() {
        let e = not_the_wizard(
            "TSK-148",
            "https://jira.example/secure/SessionTimeoutMessage.jspa",
        );
        assert_eq!(e.kind.as_str(), "auth");
        assert!(e.message.contains("SessionTimeoutMessage.jspa"), "{e}");
        assert!(e.message.contains("web pages"), "{e}");
        assert!(e.hint.as_deref().unwrap().contains("--auth basic"), "{e}");

        // Anything else that is not the wizard is a different problem, and
        // guessing "sign in again" at it would send the reader the wrong way.
        let other = not_the_wizard("TSK-148", "https://jira.example/browse/TSK-148");
        assert_eq!(other.kind.as_str(), "unsupported_server");
        assert!(other.message.contains("/browse/TSK-148"), "{other}");
    }

    #[test]
    fn the_servers_complaint_is_pulled_out_of_the_re_rendered_form() {
        let html = r#"<div class="aui-message error">
                        <p>The parent issue <b>DC-900</b> does not exist.</p>
                      </div>"#;
        assert_eq!(
            complaint(html).as_deref(),
            Some("The parent issue DC-900 does not exist.")
        );
        assert_eq!(complaint("<div>all fine</div>"), None);
    }

    /// The paths are the contract with the server, and a slug is what names the
    /// file on disk, so both are spelled out rather than merely derived.
    #[test]
    fn spec_modules_name_the_wadl_endpoints() {
        let modules = spec_modules();
        let named: Vec<(&str, &str)> = modules
            .iter()
            .map(|m| (m.name.as_str(), m.path.as_str()))
            .collect();
        assert_eq!(
            named,
            [
                ("api-2", "/rest/api/2/application.wadl"),
                ("agile-1.0", "/rest/agile/1.0/application.wadl"),
                ("auth-1", "/rest/auth/1/application.wadl"),
            ]
        );
        assert!(modules.iter().all(|m| m.extension == "wadl"));
        // Asking for JSON here is a 406, so the Accept must never inherit the
        // client-wide default.
        assert!(modules.iter().all(|m| !m.accept.contains("json")));
    }
}
