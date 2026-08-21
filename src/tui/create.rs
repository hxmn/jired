//! The new-issue screen [M4.7].
//!
//! `jired create` is offline — the issue becomes real on push — so this form
//! never talks to a server. Everything it offers comes from what the workspace
//! has already pulled: the types this project actually uses, its priorities and
//! labels, and its issues as parent candidates. A workspace with nothing in it
//! yet falls back to Jira's stock issue types, which is the worst case rather
//! than the usual one.
//!
//! The screen owns no terminal state. [`CreateScreen::handle`] takes a key and
//! [`CreateScreen::render`] draws a frame, which is what lets the tests below
//! drive a whole session through a `TestBackend`.

use crate::tui::field::{Field, Value};
use crate::tui::form::{self, Card};
use crate::tui::picker::{self, Item, Picker};
use crate::tui::{theme, Flow, Screen};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::text::{Line, Span};
use ratatui::Frame;

pub use crate::tui::catalog::{Catalog, KnownIssue};

/// Issue types every Jira has, used only when the workspace knows none.
const STOCK_TYPES: &[&str] = &["Task", "Story", "Bug", "Epic", "Sub-task"];

/// Priorities in Jira's default scheme, used the same way.
const STOCK_PRIORITIES: &[&str] = &["Highest", "High", "Medium", "Low", "Lowest"];

/// How many labels to name in the hint line before saying "and more".
const LABEL_HINT: usize = 6;

/// Values the command line already supplied; the form starts from these.
#[derive(Debug, Clone, Default)]
pub struct Seed {
    pub project: Option<String>,
    pub issue_type: Option<String>,
    pub summary: Option<String>,
    pub description: Option<String>,
    pub parent: Option<String>,
    pub priority: Option<String>,
    pub labels: Vec<String>,
}

/// A finished form: exactly what `create` needs to write a file.
#[derive(Debug, Clone, PartialEq)]
pub struct Draft {
    pub project: String,
    pub issue_type: String,
    pub summary: String,
    pub description: String,
    pub priority: Option<String>,
    pub labels: Vec<String>,
    pub parent: Option<String>,
}

// Field order is the tab order, and the order they are drawn in.
const SUMMARY: usize = 0;
const TYPE: usize = 1;
const PRIORITY: usize = 2;
const LABELS: usize = 3;
const PARENT: usize = 4;
const PROJECT: usize = 5;
const DESCRIPTION: usize = 6;

enum Overlay {
    /// A list open over the field at this index.
    Pick {
        field: usize,
        picker: Picker,
    },
    Help,
    /// Esc was pressed with something typed; confirm before throwing it away.
    Discard,
}

pub struct CreateScreen {
    fields: Vec<Field>,
    focus: usize,
    overlay: Option<Overlay>,
    /// A validation failure, cleared as soon as anything is typed.
    error: Option<String>,
    /// Temporary key, so the screen can show the filename that will be written.
    key: String,
    /// First visible row of the description, when it has more than fits.
    body_scroll: usize,
}

impl CreateScreen {
    pub fn new(key: impl Into<String>, catalog: &Catalog, seed: &Seed) -> Self {
        let types = or_stock(&catalog.types, STOCK_TYPES);
        let priorities = or_stock(&catalog.priorities, STOCK_PRIORITIES);
        let parents: Vec<Item> = catalog
            .issues
            .iter()
            .map(|i| {
                Item::new(&i.key).with_detail(if i.issue_type.is_empty() {
                    i.summary.clone()
                } else {
                    format!("{}  ·  {}", i.summary, i.issue_type)
                })
            })
            .collect();

        // A workspace that has pulled nothing knows no project keys, and a
        // list with nothing in it cannot be corrected; that case gets a field
        // the key can simply be typed into.
        let project = if catalog.projects.is_empty() {
            Field::line(PROJECT_LABEL, "PROJ").required()
        } else {
            Field::choice(PROJECT_LABEL, catalog.projects.clone(), false)
        };

        let mut fields = vec![
            Field::line(SUMMARY_LABEL, "one line saying what needs doing").required(),
            Field::choice(TYPE_LABEL, types, false),
            Field::choice(PRIORITY_LABEL, priorities, true),
            Field::line(LABELS_LABEL, "none")
                .with_items(catalog.labels.iter().map(Item::new).collect())
                .with_hint(label_hint(&catalog.labels)),
            Field::line(PARENT_LABEL, "none")
                .with_items(parents)
                .with_hint("the epic or story this belongs under"),
            project.with_hint("the project the issue is created in when you push"),
            Field::body(
                DESCRIPTION_LABEL,
                "what someone picking this up needs to know",
                6,
            ),
        ];

        // Seeded values win over the catalog's ordering: they were asked for
        // explicitly on the command line.
        fields[SUMMARY].set(seed.summary.as_deref());
        fields[DESCRIPTION].set(seed.description.as_deref());
        fields[PARENT].set(seed.parent.as_deref());
        fields[PRIORITY].set(seed.priority.as_deref());
        if !seed.labels.is_empty() {
            fields[LABELS].set(Some(&seed.labels.join(", ")));
        }
        // A choice only takes a seed when there is one: setting `None` would
        // clear the sensible default the catalog just chose.
        if let Some(t) = &seed.issue_type {
            fields[TYPE].set(Some(t));
        }
        if let Some(p) = &seed.project {
            fields[PROJECT].set(Some(p));
        }

        Self {
            // Starting on the summary means the first keystroke goes where the
            // user was already going to type.
            focus: SUMMARY,
            fields,
            overlay: None,
            error: None,
            key: key.into(),
            body_scroll: 0,
        }
    }

    /// The form's current contents.
    pub fn draft(&self) -> Draft {
        Draft {
            project: self.fields[PROJECT].text().trim().to_uppercase(),
            issue_type: self.fields[TYPE].text().to_string(),
            summary: self.fields[SUMMARY].text().trim().to_string(),
            description: self.fields[DESCRIPTION].text().trim().to_string(),
            priority: self.fields[PRIORITY].filled().map(str::to_string),
            labels: split_labels(self.fields[LABELS].text()),
            parent: self.fields[PARENT]
                .filled()
                .map(|p| p.trim().to_uppercase()),
        }
    }

    /// The file `create` will write, shown while the summary is being typed.
    fn filename(&self) -> String {
        let summary = self.fields[SUMMARY].text().trim();
        if summary.is_empty() {
            return String::new();
        }
        crate::project::naming::file_name(&self.key, summary)
    }

    /// Whether anything has been typed that would be lost by cancelling.
    fn dirty(&self) -> bool {
        [SUMMARY, DESCRIPTION, LABELS, PARENT]
            .iter()
            .any(|&i| self.fields[i].filled().is_some())
    }

    /// Refuse to finish while something required is missing, and put the
    /// cursor on the field that has to be fixed.
    fn submit(&mut self) -> Flow<Draft> {
        for (i, label) in [(SUMMARY, "a summary"), (PROJECT, "a project")] {
            if self.fields[i].filled().is_none() {
                self.focus = i;
                self.error = Some(format!("{label} is required"));
                return Flow::Continue;
            }
        }
        Flow::Done(self.draft())
    }

    fn move_focus(&mut self, forward: bool) {
        let len = self.fields.len();
        self.focus = if forward {
            (self.focus + 1) % len
        } else {
            (self.focus + len - 1) % len
        };
    }

    /// Open the focused field as a list, when it has one.
    fn open_list(&mut self) {
        let field = &self.fields[self.focus];
        if !field.openable() {
            return;
        }
        let mut picker = Picker::new(field.label, field.items.clone(), field.optional);
        // Start the list on the value the field already holds.
        if let Some(current) = field.filled() {
            picker.select(current);
        }
        self.overlay = Some(Overlay::Pick {
            field: self.focus,
            picker,
        });
    }

    fn handle_overlay(&mut self, key: KeyEvent, overlay: Overlay) -> Flow<Draft> {
        match overlay {
            Overlay::Help => {
                // Any key closes help; it is a reference card, not a mode.
                self.overlay = None;
            }
            Overlay::Discard => match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') => return Flow::Cancel,
                _ => self.overlay = None,
            },
            Overlay::Pick { field, mut picker } => match picker.handle(key) {
                picker::Outcome::Continue => {
                    self.overlay = Some(Overlay::Pick { field, picker });
                }
                picker::Outcome::Cancel => self.overlay = None,
                picker::Outcome::Chose(value) => {
                    self.fields[field].set(value.as_deref());
                    self.error = None;
                    self.overlay = None;
                }
            },
        }
        Flow::Continue
    }
}

impl Screen for CreateScreen {
    type Output = Draft;

    fn handle(&mut self, key: KeyEvent) -> Flow<Draft> {
        if let Some(overlay) = self.overlay.take() {
            return self.handle_overlay(key, overlay);
        }

        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            // Always a way out, even mid-edit: this is the key people reach for.
            KeyCode::Char('c') if ctrl => return Flow::Cancel,
            KeyCode::Char('s') if ctrl => return self.submit(),
            KeyCode::Char('p') if ctrl => {
                self.open_list();
                return Flow::Continue;
            }
            KeyCode::F(2) => {
                self.open_list();
                return Flow::Continue;
            }
            KeyCode::F(1) => {
                self.overlay = Some(Overlay::Help);
                return Flow::Continue;
            }
            KeyCode::Esc => {
                return if self.dirty() {
                    self.overlay = Some(Overlay::Discard);
                    Flow::Continue
                } else {
                    Flow::Cancel
                }
            }
            KeyCode::Tab => {
                self.move_focus(true);
                return Flow::Continue;
            }
            KeyCode::BackTab => {
                self.move_focus(false);
                return Flow::Continue;
            }
            // Enter finishes the form. The description claims it first, below,
            // because a paragraph needs a way to hold more than one line.
            KeyCode::Enter if self.focus != DESCRIPTION => return self.submit(),
            _ => {}
        }

        if self.fields[self.focus].handle(key) {
            // Any edit invalidates the complaint that produced the error.
            self.error = None;
            return Flow::Continue;
        }

        // Whatever the field did not want moves between fields, so the arrows
        // work everywhere they are not already doing something.
        match key.code {
            KeyCode::Down => self.move_focus(true),
            KeyCode::Up => self.move_focus(false),
            _ => {}
        }
        Flow::Continue
    }

    fn render(&mut self, frame: &mut Frame) {
        // The parent field shows the summary of what it points at, which is the
        // check the user actually wants: did I pick the right issue?
        self.fields[PARENT].detail = self.parent_detail();
        let card = Card {
            title: "New issue",
            subtitle: self.fields[PROJECT].text().to_string(),
            gaps: &[TYPE, DESCRIPTION],
            body: Some(DESCRIPTION),
            status: self.status_line(),
            keys: self.key_hints(),
        };
        let focus = self.overlay.is_none().then_some(self.focus);
        let area = form::render(frame, &card, &mut self.fields, focus, &mut self.body_scroll);

        match &mut self.overlay {
            Some(Overlay::Pick { picker, .. }) => form::render_picker(frame, area, picker),
            Some(Overlay::Help) => form::render_help(frame, area, HELP),
            Some(Overlay::Discard) => form::render_confirm(frame, area, "Discard this issue?"),
            None => {}
        }
    }
}

const HELP: &[(&str, &str)] = &[
    ("tab / shift-tab", "move between fields"),
    ("↑ ↓", "move between fields"),
    ("← →", "change a choice"),
    ("^p or f2", "open the field as a list"),
    ("⏎", "create the issue"),
    ("^s", "create from the description"),
    ("^a ^e ^w ^u ^k", "edit the line like a shell does"),
    ("esc", "cancel"),
];

const SUMMARY_LABEL: &str = "Summary";
const TYPE_LABEL: &str = "Type";
const PRIORITY_LABEL: &str = "Priority";
const LABELS_LABEL: &str = "Labels";
const PARENT_LABEL: &str = "Parent";
const PROJECT_LABEL: &str = "Project";
const DESCRIPTION_LABEL: &str = "Description";

impl CreateScreen {
    /// The summary of the issue the parent field names, when the workspace
    /// knows it.
    fn parent_detail(&self) -> Option<String> {
        let key = self.fields[PARENT].filled()?.to_uppercase();
        self.fields[PARENT]
            .items
            .iter()
            .find(|i| i.value.eq_ignore_ascii_case(&key))
            .map(|i| i.detail.clone())
    }

    /// Whichever is most useful right now: a complaint, then the file that will
    /// be written, then help for this field.
    fn status_line(&self) -> Line<'static> {
        if let Some(error) = &self.error {
            return Line::from(Span::styled(format!("✗ {error}"), theme::error()));
        }
        if !self.filename().is_empty() {
            return Line::from(vec![
                Span::styled("writes ", theme::dim()),
                Span::styled(self.filename(), theme::value()),
            ]);
        }
        Line::from(Span::styled(
            self.fields[self.focus].hint.clone(),
            theme::dim(),
        ))
    }

    fn key_hints(&self) -> Vec<(&'static str, &'static str)> {
        let mut keys = vec![("tab", "field")];
        if self.focus == DESCRIPTION {
            keys.push(("^s", "create"));
        } else {
            keys.push(("⏎", "create"));
        }
        if self.fields[self.focus].openable() {
            keys.push(("^p", "list"));
        } else if matches!(self.fields[self.focus].value, Value::Choice(_)) {
            keys.push(("←→", "change"));
        }
        keys.push(("esc", "cancel"));
        keys.push(("f1", "keys"));
        keys
    }
}

/// Prefer what the workspace uses; fall back to Jira's stock list.
fn or_stock(known: &[String], stock: &[&str]) -> Vec<String> {
    if known.is_empty() {
        return stock.iter().map(|s| s.to_string()).collect();
    }
    known.to_vec()
}

fn label_hint(labels: &[String]) -> String {
    if labels.is_empty() {
        return "comma-separated".to_string();
    }
    let shown: Vec<&str> = labels.iter().take(LABEL_HINT).map(String::as_str).collect();
    let rest = labels.len().saturating_sub(shown.len());
    let mut hint = format!("in use: {}", shown.join(", "));
    if rest > 0 {
        hint.push_str(&format!(", and {rest} more"));
    }
    hint
}

/// Split what was typed into labels. Jira labels cannot contain spaces, so both
/// separators people reach for mean the same thing.
fn split_labels(text: &str) -> Vec<String> {
    let mut seen = Vec::new();
    for label in text.split([',', ' ', '\t']) {
        let label = label.trim();
        if !label.is_empty() && !seen.iter().any(|s: &String| s.as_str() == label) {
            seen.push(label.to_string());
        }
    }
    seen
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::render_lines;

    fn catalog() -> Catalog {
        Catalog {
            projects: vec!["PROJ".into(), "OPS".into()],
            types: vec!["Story".into(), "Bug".into(), "Task".into()],
            priorities: vec!["High".into(), "Medium".into()],
            labels: vec!["auth".into(), "backend".into()],
            issues: vec![
                KnownIssue {
                    key: "PROJ-100".into(),
                    summary: "SSO rollout".into(),
                    issue_type: "Epic".into(),
                },
                KnownIssue {
                    key: "PROJ-142".into(),
                    summary: "Fix login flow".into(),
                    issue_type: "Story".into(),
                },
            ],
            ..Catalog::default()
        }
    }

    fn screen() -> CreateScreen {
        CreateScreen::new("NEW-a1b2c3", &catalog(), &Seed::default())
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    fn typed(screen: &mut CreateScreen, text: &str) {
        for c in text.chars() {
            screen.handle(key(KeyCode::Char(c)));
        }
    }

    fn done(flow: Flow<Draft>) -> Draft {
        match flow {
            Flow::Done(draft) => draft,
            _ => panic!("the screen did not finish"),
        }
    }

    #[test]
    fn the_form_starts_on_the_summary_with_the_workspaces_own_defaults() {
        let screen = screen();
        assert_eq!(screen.focus, SUMMARY);
        // Most-used first, straight from the catalog rather than a stock list.
        assert_eq!(screen.fields[TYPE].text(), "Story");
        assert_eq!(screen.fields[PROJECT].text(), "PROJ");
        assert_eq!(screen.fields[PRIORITY].filled(), None);
    }

    #[test]
    fn an_empty_workspace_falls_back_to_jiras_own_types() {
        let screen = CreateScreen::new("NEW-1", &Catalog::default(), &Seed::default());
        assert_eq!(screen.fields[TYPE].text(), "Task");
        assert_eq!(screen.fields[PRIORITY].filled(), None);
    }

    #[test]
    fn flags_seed_the_form_and_survive_to_the_draft() {
        let seed = Seed {
            project: Some("ops".into()),
            issue_type: Some("Bug".into()),
            summary: Some("Rotate the certificate".into()),
            description: Some("It expires on Friday.".into()),
            parent: Some("proj-100".into()),
            priority: Some("High".into()),
            labels: vec!["auth".into(), "backend".into()],
        };
        let mut screen = CreateScreen::new("NEW-1", &catalog(), &seed);
        let draft = done(screen.handle(ctrl('s')));
        assert_eq!(draft.summary, "Rotate the certificate");
        assert_eq!(draft.issue_type, "Bug");
        assert_eq!(draft.description, "It expires on Friday.");
        assert_eq!(draft.project, "OPS", "keys are normalized on the way out");
        assert_eq!(draft.parent.as_deref(), Some("PROJ-100"));
    }

    #[test]
    fn typing_a_summary_and_pressing_enter_produces_a_draft() {
        let mut screen = screen();
        typed(&mut screen, "Fix the login flow");
        let draft = done(screen.handle(key(KeyCode::Enter)));
        assert_eq!(draft.summary, "Fix the login flow");
        assert_eq!(draft.issue_type, "Story");
        assert_eq!(draft.project, "PROJ");
        assert!(draft.description.is_empty());
        assert!(draft.labels.is_empty());
    }

    #[test]
    fn an_empty_summary_is_refused_and_the_cursor_goes_back_to_it() {
        let mut screen = screen();
        screen.handle(key(KeyCode::Tab));
        assert!(matches!(screen.handle(key(KeyCode::Enter)), Flow::Continue));
        assert_eq!(screen.focus, SUMMARY);
        assert_eq!(screen.error.as_deref(), Some("a summary is required"));

        // Typing clears the complaint rather than leaving it up to argue with.
        typed(&mut screen, "x");
        assert_eq!(screen.error, None);
    }

    #[test]
    fn the_description_keeps_enter_for_itself() {
        let mut screen = screen();
        typed(&mut screen, "Summary");
        screen.focus = DESCRIPTION;
        typed(&mut screen, "one");
        assert!(matches!(screen.handle(key(KeyCode::Enter)), Flow::Continue));
        typed(&mut screen, "two");
        // ^s is the way out from the body, and it still validates.
        let draft = done(screen.handle(ctrl('s')));
        assert_eq!(draft.description, "one\ntwo");
    }

    #[test]
    fn arrows_change_a_choice_and_tab_moves_on() {
        let mut screen = screen();
        screen.handle(key(KeyCode::Tab));
        assert_eq!(screen.focus, TYPE);
        screen.handle(key(KeyCode::Right));
        assert_eq!(screen.fields[TYPE].text(), "Bug");
        screen.handle(key(KeyCode::Left));
        assert_eq!(screen.fields[TYPE].text(), "Story");
        // Down is not a choice's key, so it falls through to the next field.
        screen.handle(key(KeyCode::Down));
        assert_eq!(screen.focus, PRIORITY);
        screen.handle(key(KeyCode::BackTab));
        assert_eq!(screen.focus, TYPE);
    }

    #[test]
    fn labels_accept_either_separator_and_never_repeat() {
        assert_eq!(split_labels("auth, backend"), vec!["auth", "backend"]);
        assert_eq!(split_labels("auth backend auth"), vec!["auth", "backend"]);
        assert!(split_labels("  ,  ").is_empty());
    }

    #[test]
    fn a_parent_is_chosen_from_the_workspaces_own_issues() {
        let mut screen = screen();
        typed(&mut screen, "Summary");
        screen.focus = PARENT;
        screen.handle(ctrl('p'));
        assert!(matches!(screen.overlay, Some(Overlay::Pick { .. })));
        typed(&mut screen, "100");
        screen.handle(key(KeyCode::Enter));
        assert_eq!(screen.fields[PARENT].text(), "PROJ-100");
        assert!(screen.overlay.is_none());

        let draft = done(screen.handle(key(KeyCode::Enter)));
        assert_eq!(draft.parent.as_deref(), Some("PROJ-100"));
    }

    #[test]
    fn escape_asks_before_throwing_away_typed_input() {
        let mut screen = screen();
        // Nothing typed: escape means escape.
        assert!(matches!(screen.handle(key(KeyCode::Esc)), Flow::Cancel));

        typed(&mut screen, "half an idea");
        assert!(matches!(screen.handle(key(KeyCode::Esc)), Flow::Continue));
        assert!(matches!(screen.overlay, Some(Overlay::Discard)));
        // Anything but y goes back to editing.
        screen.handle(key(KeyCode::Char('n')));
        assert!(screen.overlay.is_none());

        screen.handle(key(KeyCode::Esc));
        assert!(matches!(
            screen.handle(key(KeyCode::Char('y'))),
            Flow::Cancel
        ));
    }

    #[test]
    fn ctrl_c_always_leaves_immediately() {
        let mut screen = screen();
        typed(&mut screen, "work in progress");
        assert!(matches!(screen.handle(ctrl('c')), Flow::Cancel));
    }

    #[test]
    fn the_screen_draws_its_fields_the_summary_and_the_file_it_will_write() {
        let mut screen = screen();
        typed(&mut screen, "Fix the login flow");
        let lines = render_lines(&mut screen, 80, 24).join("\n");

        assert!(lines.contains("New issue"), "{lines}");
        for label in [
            "Summary",
            "Type",
            "Priority",
            "Labels",
            "Parent",
            "Project",
            "Description",
        ] {
            assert!(lines.contains(label), "{label} is missing from\n{lines}");
        }
        assert!(lines.contains("Fix the login flow"), "{lines}");
        assert!(
            lines.contains("NEW-a1b2c3. Fix the login flow.md"),
            "the filename preview is missing from\n{lines}"
        );
        assert!(lines.contains("‹ Story ›"), "{lines}");
    }

    #[test]
    fn the_open_list_draws_over_the_form() {
        let mut screen = screen();
        screen.focus = PARENT;
        screen.handle(ctrl('p'));
        let lines = render_lines(&mut screen, 80, 24).join("\n");
        assert!(lines.contains("PROJ-100"), "{lines}");
        assert!(lines.contains("SSO rollout"), "{lines}");
        assert!(lines.contains("type to filter"), "{lines}");
    }

    #[test]
    fn help_lists_the_keys_and_any_key_closes_it() {
        let mut screen = screen();
        screen.handle(key(KeyCode::F(1)));
        let lines = render_lines(&mut screen, 80, 24).join("\n");
        assert!(lines.contains("Keys"), "{lines}");
        assert!(lines.contains("create the issue"), "{lines}");
        screen.handle(key(KeyCode::Char('q')));
        assert!(screen.overlay.is_none());
    }

    /// Print the screen, its list and its help, for looking at.
    ///
    /// Layout is the one thing assertions are bad at judging, so this exists to
    /// be read rather than to pass:
    /// `cargo test --lib preview -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn preview() {
        let mut s = screen();
        typed(&mut s, "Login fails when SSO is enabled");
        s.focus = DESCRIPTION;
        typed(
            &mut s,
            "Repro: sign in with the corporate IdP while the assertion is stale.",
        );
        s.focus = LABELS;
        typed(&mut s, "auth, backend");
        s.focus = PARENT;
        typed(&mut s, "PROJ-100");
        s.focus = SUMMARY;
        for l in render_lines(&mut s, 90, 26) {
            println!("|{l}");
        }
        println!();
        s.focus = PARENT;
        s.handle(ctrl('p'));
        for l in render_lines(&mut s, 90, 26) {
            println!("|{l}");
        }
        println!();
        s.handle(key(KeyCode::Esc));
        s.handle(key(KeyCode::F(1)));
        for l in render_lines(&mut s, 90, 26) {
            println!("|{l}");
        }
    }

    #[test]
    fn a_short_terminal_still_renders_without_panicking() {
        // Every rect here is computed, so the small sizes are worth pinning:
        // an off-by-one in the footer maths would be a panic in production.
        for (w, h) in [(20u16, 6u16), (40, 8), (80, 10), (200, 60)] {
            let mut screen = screen();
            typed(&mut screen, "Fix the login flow");
            render_lines(&mut screen, w, h);
            screen.handle(key(KeyCode::F(1)));
            render_lines(&mut screen, w, h);
            screen.handle(key(KeyCode::Esc));
            screen.focus = PARENT;
            screen.handle(ctrl('p'));
            render_lines(&mut screen, w, h);
        }
    }
}
