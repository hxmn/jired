//! The edit-issue screen [M4.7].
//!
//! `jired edit` changes the file, not the server: the form loads what the
//! working copy says, and saving writes it back so the change shows up in
//! `status` and `diff` and goes out on the next `push`, like every other local
//! edit. That is also why it can offer only the fields a push can actually
//! send — a form that let someone change a value nothing would transmit would
//! be worse than not offering it.
//!
//! Options come from [`Catalog`], which is built from snapshots already on
//! disk, so opening the form never waits on a network call. Fields the form
//! does not show — sprint, links, custom fields, comments — are untouched by
//! it: the screen edits a document the caller loaded and hands it back.

use crate::tui::catalog::Catalog;
use crate::tui::field::{Field, Value};
use crate::tui::form::{self, Card};
use crate::tui::picker::{self, Item, Picker};
use crate::tui::{theme, Flow, Screen};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::text::{Line, Span};
use ratatui::Frame;

/// What the form loads and what it gives back.
///
/// Only the fields a push can send. Everything else in the document is the
/// caller's business and travels around this screen untouched.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Edits {
    pub summary: String,
    pub status: String,
    pub issue_type: String,
    pub priority: Option<String>,
    pub assignee: Option<String>,
    pub labels: Vec<String>,
    pub components: Vec<String>,
    /// Sprints the issue belongs to, oldest first; empty is the backlog. Only
    /// the last one is somewhere a push can move it to — the rest is history.
    pub sprint: Vec<String>,
    pub story_points: Option<f64>,
    pub parent: Option<String>,
    pub description: String,
}

impl Edits {
    /// What the file currently says.
    pub fn of(doc: &crate::doc::IssueDoc) -> Self {
        let fm = &doc.frontmatter;
        Self {
            summary: fm.summary.clone(),
            status: fm.status.clone(),
            issue_type: fm.issue_type.clone(),
            priority: fm.priority.clone(),
            assignee: fm.assignee.clone(),
            labels: fm.labels.clone(),
            components: fm.components.clone(),
            sprint: fm.sprint.clone(),
            story_points: fm.story_points,
            parent: fm.parent.clone(),
            description: doc.description.trim_end().to_string(),
        }
    }

    /// Write the form's result back into the document, leaving everything it
    /// does not cover — comments, custom fields, links, the sync block —
    /// exactly as it was.
    pub fn apply(&self, doc: &mut crate::doc::IssueDoc) {
        let fm = &mut doc.frontmatter;
        fm.summary = self.summary.clone();
        fm.status = self.status.clone();
        fm.issue_type = self.issue_type.clone();
        fm.priority = self.priority.clone();
        fm.assignee = self.assignee.clone();
        fm.labels = self.labels.clone();
        fm.components = self.components.clone();
        fm.sprint = self.sprint.clone();
        fm.story_points = self.story_points;
        fm.parent = self.parent.clone();
        // The heading and the summary are two renderings of one thing.
        doc.heading = self.summary.clone();
        doc.description = self.description.clone();
    }
}

// Field order is the tab order, and the order they are drawn in.
const SUMMARY: usize = 0;
const STATUS: usize = 1;
const TYPE: usize = 2;
const PRIORITY: usize = 3;
const ASSIGNEE: usize = 4;
const LABELS: usize = 5;
const COMPONENTS: usize = 6;
const SPRINT: usize = 7;
const POINTS: usize = 8;
const PARENT: usize = 9;
const DESCRIPTION: usize = 10;

const SUMMARY_LABEL: &str = "Summary";
const STATUS_LABEL: &str = "Status";
const TYPE_LABEL: &str = "Type";
const PRIORITY_LABEL: &str = "Priority";
const ASSIGNEE_LABEL: &str = "Assignee";
const LABELS_LABEL: &str = "Labels";
const COMPONENTS_LABEL: &str = "Components";
const SPRINT_LABEL: &str = "Sprint";
const POINTS_LABEL: &str = "Points";
/// What an empty sprint field means, said in the placeholder rather than left
/// blank: an issue in no sprint is in the backlog, which is a place.
const BACKLOG: &str = "Backlog — in no sprint";
const PARENT_LABEL: &str = "Parent";
const DESCRIPTION_LABEL: &str = "Description";

const HELP: &[(&str, &str)] = &[
    ("tab / shift-tab", "move between fields"),
    ("↑ ↓", "move between fields"),
    ("← →", "change a choice"),
    ("^p or f2", "open the field as a list"),
    ("space", "tick a sprint in the list"),
    ("tab", "take the suggestion, where there is one"),
    ("⏎", "save to the file"),
    ("^s", "save from the description"),
    ("^a ^e ^w ^u ^k", "edit the line like a shell does"),
    ("esc", "cancel"),
];

enum Overlay {
    Pick {
        field: usize,
        picker: Picker,
    },
    Help,
    /// Esc with unsaved changes; confirm before throwing them away.
    Discard,
}

pub struct EditScreen {
    key: String,
    fields: Vec<Field>,
    focus: usize,
    overlay: Option<Overlay>,
    error: Option<String>,
    /// What was loaded, to mark what has moved since and to answer "anything
    /// to save?" without comparing rendered text.
    original: Edits,
    body_scroll: usize,
}

impl EditScreen {
    pub fn new(key: impl Into<String>, catalog: &Catalog, current: &Edits) -> Self {
        let people: Vec<Item> = catalog
            .people
            .iter()
            .map(|p| Item::new(&p.mention).with_detail(p.display_name.clone()))
            .collect();
        let issues: Vec<Item> = catalog
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

        // The issue's own values belong in every list, whether or not the rest
        // of the workspace happens to use them.
        // Every sprint the issue is in belongs among the options, closed ones
        // included: an issue that rolled over has been through sprints nobody
        // can move it into any more, and the list has to show where it has been.
        let mut sprints = catalog.sprints.clone();
        for name in &current.sprint {
            if !sprints.iter().any(|s| s.eq_ignore_ascii_case(name)) {
                sprints.insert(0, name.clone());
            }
        }

        let statuses = with_current(&catalog.statuses, Some(&current.status));
        let types = with_current(&catalog.types, Some(&current.issue_type));
        let priorities = with_current(&catalog.priorities, current.priority.as_deref());

        let mut fields = vec![
            Field::line(SUMMARY_LABEL, "one line saying what needs doing").required(),
            Field::choice(STATUS_LABEL, statuses, false)
                .with_hint("saved as a transition, applied when you push"),
            Field::choice(TYPE_LABEL, types, false),
            Field::choice(PRIORITY_LABEL, priorities, true),
            Field::line(ASSIGNEE_LABEL, "nobody")
                .with_items(people)
                .suggesting()
                .with_hint("type a name — tab completes it — or ^p for the list"),
            Field::line(LABELS_LABEL, "none")
                .with_items(catalog.labels.iter().map(Item::new).collect())
                .with_hint("comma-separated"),
            Field::line(COMPONENTS_LABEL, "none")
                .with_items(catalog.components.iter().map(Item::new).collect())
                .with_hint("comma-separated"),
            Field::line(SPRINT_LABEL, BACKLOG)
                .with_items(sprints.iter().map(Item::new).collect())
                .multiple()
                .with_hint("^p to tick sprints; the last one is where it moves to"),
            Field::line(POINTS_LABEL, "none").with_hint("a number, or empty to clear"),
            Field::line(PARENT_LABEL, "none")
                .with_items(issues)
                .suggesting()
                .with_hint("the epic or story this belongs under"),
            Field::body(
                DESCRIPTION_LABEL,
                "what someone picking this up needs to know",
                6,
            ),
        ];

        fields[SUMMARY].set(Some(&current.summary));
        fields[STATUS].set(Some(&current.status));
        fields[TYPE].set(Some(&current.issue_type));
        fields[PRIORITY].set(current.priority.as_deref());
        fields[ASSIGNEE].set(current.assignee.as_deref());
        fields[LABELS].set(Some(&current.labels.join(", ")));
        fields[COMPONENTS].set(Some(&current.components.join(", ")));
        fields[SPRINT].set(Some(&current.sprint.join(", ")));
        fields[POINTS].set(current.story_points.map(format_points).as_deref());
        fields[PARENT].set(current.parent.as_deref());
        fields[DESCRIPTION].set(Some(&current.description));

        Self {
            key: key.into(),
            fields,
            focus: SUMMARY,
            overlay: None,
            error: None,
            original: current.clone(),
            body_scroll: 0,
        }
    }

    /// The form's current contents.
    pub fn edits(&self) -> Edits {
        Edits {
            summary: self.fields[SUMMARY].text().trim().to_string(),
            status: self.fields[STATUS].text().to_string(),
            issue_type: self.fields[TYPE].text().to_string(),
            priority: self.fields[PRIORITY].filled().map(str::to_string),
            assignee: self.fields[ASSIGNEE].filled().map(str::to_string),
            labels: split_list(self.fields[LABELS].text()),
            components: split_list(self.fields[COMPONENTS].text()),
            sprint: split_list(self.fields[SPRINT].text()),
            story_points: parse_points(self.fields[POINTS].text()),
            parent: self.fields[PARENT].filled().map(|p| p.to_uppercase()),
            description: self.fields[DESCRIPTION].text().trim_end().to_string(),
        }
    }

    /// Whether anything would be written.
    pub fn dirty(&self) -> bool {
        self.edits() != self.original
    }

    /// Which fields have moved since the form loaded, for the label markers and
    /// the count in the status line.
    fn mark_changes(&mut self) -> usize {
        let now = self.edits();
        let before = &self.original;
        let moved = [
            now.summary != before.summary,
            now.status != before.status,
            now.issue_type != before.issue_type,
            now.priority != before.priority,
            now.assignee != before.assignee,
            now.labels != before.labels,
            now.components != before.components,
            now.sprint != before.sprint,
            now.story_points != before.story_points,
            now.parent != before.parent,
            now.description != before.description,
        ];
        for (field, changed) in self.fields.iter_mut().zip(moved) {
            field.changed = changed;
        }
        moved.iter().filter(|m| **m).count()
    }

    /// Refuse to finish while something required is missing, and put the cursor
    /// on the field that has to be fixed.
    fn submit(&mut self) -> Flow<Edits> {
        if self.fields[SUMMARY].filled().is_none() {
            self.focus = SUMMARY;
            self.error = Some("a summary is required".into());
            return Flow::Continue;
        }
        // A half-typed name is not an assignee: Jira wants the id inside the
        // mention, so what is on screen has to become one before it is saved.
        // The suggestion is right there next to the field, so taking it is what
        // the user was looking at when they pressed enter.
        if !self.fields[ASSIGNEE].complete() && !self.assignee_is_resolved() {
            self.focus = ASSIGNEE;
            self.error = Some(format!(
                "nobody here matches `{}` — ^p to pick, or clear it",
                self.fields[ASSIGNEE].text().trim()
            ));
            return Flow::Continue;
        }
        let points = self.fields[POINTS].text().trim().to_string();
        if !points.is_empty() && parse_points(&points).is_none() {
            self.focus = POINTS;
            self.error = Some(format!("`{points}` is not a number"));
            return Flow::Continue;
        }
        Flow::Done(self.edits())
    }

    /// Whether the assignee field holds something a push could send: a mention,
    /// or nothing at all.
    fn assignee_is_resolved(&self) -> bool {
        let Some(text) = self.fields[ASSIGNEE].filled() else {
            return true;
        };
        // A mention that came from the file names someone this workspace may
        // never have seen in any other issue, so its own value counts too.
        text.starts_with("@[")
            || self.original.assignee.as_deref() == Some(text)
            || self.fields[ASSIGNEE]
                .items
                .iter()
                .any(|i| i.value.eq_ignore_ascii_case(text))
    }

    fn move_focus(&mut self, forward: bool) {
        let len = self.fields.len();
        self.focus = if forward {
            (self.focus + 1) % len
        } else {
            (self.focus + len - 1) % len
        };
    }

    fn open_list(&mut self) {
        let field = &self.fields[self.focus];
        if !field.openable() {
            return;
        }
        let mut picker = if field.multiple {
            Picker::multi(field.label, field.items.clone(), split_list(field.text()))
        } else {
            Picker::new(field.label, field.items.clone(), field.optional)
        };
        if !field.multiple {
            if let Some(current) = field.filled() {
                picker.select(current);
            }
        }
        self.overlay = Some(Overlay::Pick {
            field: self.focus,
            picker,
        });
    }

    fn handle_overlay(&mut self, key: KeyEvent, overlay: Overlay) -> Flow<Edits> {
        match overlay {
            Overlay::Help => self.overlay = None,
            Overlay::Discard => match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') => return Flow::Cancel,
                _ => self.overlay = None,
            },
            Overlay::Pick { field, mut picker } => match picker.handle(key) {
                picker::Outcome::Continue => {
                    self.overlay = Some(Overlay::Pick { field, picker });
                }
                picker::Outcome::Cancel => self.overlay = None,
                picker::Outcome::ChoseMany(values) => {
                    self.fields[field].set(Some(&values.join(", ")));
                    self.error = None;
                    self.overlay = None;
                }
                picker::Outcome::Chose(value) => {
                    // A list of several values adds to what is there rather
                    // than replacing it: picking two labels should give two.
                    if matches!(field, LABELS | COMPONENTS) {
                        let mut current = split_list(self.fields[field].text());
                        match value.as_deref().map(str::trim).filter(|v| !v.is_empty()) {
                            Some(v) if !current.iter().any(|c| c == v) => {
                                current.push(v.to_string());
                            }
                            Some(_) => {}
                            None => current.clear(),
                        }
                        self.fields[field].set(Some(&current.join(", ")));
                    } else {
                        self.fields[field].set(value.as_deref());
                    }
                    self.error = None;
                    self.overlay = None;
                }
            },
        }
        Flow::Continue
    }

    /// Whichever is most useful right now: a complaint, then what would be
    /// saved, then help for this field.
    fn status_line(&self, changed: usize) -> Line<'static> {
        if let Some(error) = &self.error {
            return Line::from(Span::styled(format!("✗ {error}"), theme::error()));
        }
        if changed > 0 {
            return Line::from(vec![
                Span::styled(
                    format!(
                        "{changed} field{} changed",
                        if changed == 1 { "" } else { "s" }
                    ),
                    theme::value(),
                ),
                Span::styled(" — saved to the file, sent on push", theme::dim()),
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
            keys.push(("^s", "save"));
        } else {
            keys.push(("⏎", "save"));
        }
        if self.fields[self.focus].suggestion().is_some() {
            keys.push(("tab", "complete"));
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

    /// The summary of the issue the parent field names, when the workspace
    /// knows it.
    fn parent_detail(&self) -> Option<String> {
        if let Some(item) = self.fields[PARENT].suggestion() {
            return Some(format!("→ {}  {}   tab", item.label, item.detail));
        }
        let key = self.fields[PARENT].filled()?.to_uppercase();
        self.fields[PARENT]
            .items
            .iter()
            .find(|i| i.value.eq_ignore_ascii_case(&key))
            .map(|i| i.detail.clone())
    }

    /// What sits dim after the assignee: who the mention names once one is
    /// chosen, and what tab would complete to before that.
    fn assignee_detail(&self) -> Option<String> {
        if let Some(item) = self.fields[ASSIGNEE].suggestion() {
            return Some(format!("→ {}   tab", item.detail));
        }
        let value = self.fields[ASSIGNEE].filled()?;
        self.fields[ASSIGNEE]
            .items
            .iter()
            .find(|i| i.value == value)
            .map(|i| i.detail.clone())
    }
}

impl Screen for EditScreen {
    type Output = Edits;

    fn handle(&mut self, key: KeyEvent) -> Flow<Edits> {
        if let Some(overlay) = self.overlay.take() {
            return self.handle_overlay(key, overlay);
        }

        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
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
            // Tab completes what is being typed before it moves on, the way a
            // shell does; with nothing to complete it is just the next field.
            KeyCode::Tab => {
                if !self.fields[self.focus].complete() {
                    self.move_focus(true);
                }
                return Flow::Continue;
            }
            KeyCode::BackTab => {
                self.move_focus(false);
                return Flow::Continue;
            }
            // Enter saves. The description claims it first, below, because a
            // paragraph needs a way to hold more than one line.
            KeyCode::Enter if self.focus != DESCRIPTION => return self.submit(),
            _ => {}
        }

        if self.fields[self.focus].handle(key) {
            self.error = None;
            return Flow::Continue;
        }

        match key.code {
            KeyCode::Down => self.move_focus(true),
            KeyCode::Up => self.move_focus(false),
            _ => {}
        }
        Flow::Continue
    }

    fn render(&mut self, frame: &mut Frame) {
        let changed = self.mark_changes();
        self.fields[PARENT].detail = self.parent_detail();
        self.fields[ASSIGNEE].detail = self.assignee_detail();

        let card = Card {
            title: "Edit issue",
            subtitle: self.key.clone(),
            gaps: &[STATUS, DESCRIPTION],
            body: Some(DESCRIPTION),
            status: self.status_line(changed),
            keys: self.key_hints(),
        };
        let focus = self.overlay.is_none().then_some(self.focus);
        let area = form::render(frame, &card, &mut self.fields, focus, &mut self.body_scroll);

        match &mut self.overlay {
            Some(Overlay::Pick { picker, .. }) => form::render_picker(frame, area, picker),
            Some(Overlay::Help) => form::render_help(frame, area, HELP),
            Some(Overlay::Discard) => form::render_confirm(frame, area, "Discard these changes?"),
            None => {}
        }
    }
}

/// The options a field offers, with the issue's own value guaranteed among them.
fn with_current(known: &[String], current: Option<&str>) -> Vec<String> {
    let mut options = known.to_vec();
    if let Some(value) = current.map(str::trim).filter(|v| !v.is_empty()) {
        if !options.iter().any(|o| o.eq_ignore_ascii_case(value)) {
            options.insert(0, value.to_string());
        }
    }
    options
}

/// Split what was typed into labels or components.
///
/// Labels cannot contain spaces, so both separators mean the same thing there;
/// components can, so only a comma separates them.
fn split_list(text: &str) -> Vec<String> {
    let mut seen: Vec<String> = Vec::new();
    for value in text.split(',') {
        let value = value.trim();
        if !value.is_empty() && !seen.iter().any(|s| s == value) {
            seen.push(value.to_string());
        }
    }
    seen
}

/// Story points as Jira stores them: a number, usually whole.
fn parse_points(text: &str) -> Option<f64> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    text.parse::<f64>().ok().filter(|n| n.is_finite())
}

fn format_points(points: f64) -> String {
    if points.fract() == 0.0 {
        format!("{points:.0}")
    } else {
        points.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::catalog::{KnownIssue, KnownUser};
    use crate::tui::render_lines;

    fn catalog() -> Catalog {
        Catalog {
            types: vec!["Story".into(), "Bug".into()],
            statuses: vec!["To Do".into(), "In Progress".into(), "Done".into()],
            priorities: vec!["High".into(), "Medium".into()],
            labels: vec!["auth".into(), "backend".into(), "sso".into()],
            components: vec!["web".into(), "api".into()],
            sprints: vec!["Sprint 42".into(), "Sprint 43".into()],
            people: vec![
                KnownUser {
                    mention: "@[Ilya A](5b10a2)".into(),
                    display_name: "Ilya A".into(),
                },
                KnownUser {
                    mention: "@[Jane D](5b1af9)".into(),
                    display_name: "Jane D".into(),
                },
            ],
            issues: vec![KnownIssue {
                key: "PROJ-100".into(),
                summary: "SSO rollout".into(),
                issue_type: "Epic".into(),
                status: "To Do".into(),
            }],
            ..Catalog::default()
        }
    }

    fn current() -> Edits {
        Edits {
            summary: "Fix login flow".into(),
            status: "In Progress".into(),
            issue_type: "Story".into(),
            priority: Some("High".into()),
            assignee: Some("@[Ilya A](5b10a2)".into()),
            labels: vec!["auth".into(), "backend".into()],
            components: vec!["web".into()],
            sprint: vec!["Sprint 42".into()],
            story_points: Some(5.0),
            parent: Some("PROJ-100".into()),
            description: "Login fails when SSO is enabled.".into(),
        }
    }

    fn screen() -> EditScreen {
        EditScreen::new("PROJ-142", &catalog(), &current())
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    fn typed(screen: &mut EditScreen, text: &str) {
        for c in text.chars() {
            screen.handle(key(KeyCode::Char(c)));
        }
    }

    fn done(flow: Flow<Edits>) -> Edits {
        match flow {
            Flow::Done(edits) => edits,
            _ => panic!("the screen did not finish"),
        }
    }

    #[test]
    fn the_form_opens_on_what_the_file_already_says() {
        let screen = screen();
        assert_eq!(screen.fields[SUMMARY].text(), "Fix login flow");
        assert_eq!(screen.fields[STATUS].text(), "In Progress");
        assert_eq!(screen.fields[TYPE].text(), "Story");
        assert_eq!(screen.fields[PRIORITY].text(), "High");
        assert_eq!(screen.fields[ASSIGNEE].text(), "@[Ilya A](5b10a2)");
        assert_eq!(screen.fields[LABELS].text(), "auth, backend");
        assert_eq!(screen.fields[POINTS].text(), "5");
        assert_eq!(screen.fields[PARENT].text(), "PROJ-100");
    }

    /// Opening the form and saving it must be a no-op, or every edit session
    /// would leave the file looking dirty.
    #[test]
    fn saving_without_touching_anything_changes_nothing() {
        let mut screen = screen();
        assert!(!screen.dirty());
        assert_eq!(done(screen.handle(key(KeyCode::Enter))), current());
    }

    #[test]
    fn editing_a_field_shows_up_in_the_result() {
        let mut screen = screen();
        screen.focus = STATUS;
        screen.handle(key(KeyCode::Right));
        assert_eq!(screen.fields[STATUS].text(), "Done");

        screen.focus = POINTS;
        screen.handle(ctrl('u')); // clear the line
        typed(&mut screen, "8");

        let edits = done(screen.handle(key(KeyCode::Enter)));
        assert_eq!(edits.status, "Done");
        assert_eq!(edits.story_points, Some(8.0));
        assert_eq!(edits.summary, "Fix login flow", "the rest is untouched");
    }

    #[test]
    fn a_status_the_workspace_has_never_seen_is_still_offered() {
        let mut current = current();
        current.status = "Awaiting Review".into();
        let screen = EditScreen::new("PROJ-142", &catalog(), &current);
        assert_eq!(screen.fields[STATUS].text(), "Awaiting Review");
    }

    #[test]
    fn story_points_have_to_be_a_number() {
        let mut screen = screen();
        screen.focus = POINTS;
        screen.handle(ctrl('u'));
        typed(&mut screen, "lots");
        assert!(matches!(screen.handle(key(KeyCode::Enter)), Flow::Continue));
        assert_eq!(screen.focus, POINTS);
        assert!(screen.error.is_some());

        screen.handle(ctrl('u'));
        let edits = done(screen.handle(key(KeyCode::Enter)));
        assert_eq!(edits.story_points, None, "empty clears the field");
    }

    #[test]
    fn an_empty_summary_is_refused() {
        let mut screen = screen();
        screen.focus = SUMMARY;
        screen.handle(ctrl('u'));
        assert!(matches!(screen.handle(key(KeyCode::Enter)), Flow::Continue));
        assert_eq!(screen.error.as_deref(), Some("a summary is required"));
    }

    /// An issue can be in several sprints, so the field is a ticked list.
    #[test]
    fn sprints_are_ticked_from_a_list_and_several_can_be_held_at_once() {
        let mut screen = screen();
        assert_eq!(screen.fields[SPRINT].text(), "Sprint 42");

        screen.focus = SPRINT;
        screen.handle(ctrl('p'));
        screen.handle(key(KeyCode::Down));
        screen.handle(key(KeyCode::Char(' ')));
        screen.handle(key(KeyCode::Enter));
        assert_eq!(screen.fields[SPRINT].text(), "Sprint 42, Sprint 43");

        let edits = done(screen.handle(key(KeyCode::Enter)));
        assert_eq!(edits.sprint, ["Sprint 42", "Sprint 43"]);
    }

    #[test]
    fn unticking_every_sprint_puts_the_issue_in_the_backlog() {
        let mut screen = screen();
        screen.focus = SPRINT;
        screen.handle(ctrl('p'));
        screen.handle(key(KeyCode::Char(' '))); // untick the one it is in
        screen.handle(key(KeyCode::Enter));
        assert_eq!(screen.fields[SPRINT].filled(), None);

        let edits = done(screen.handle(key(KeyCode::Enter)));
        assert!(edits.sprint.is_empty(), "the backlog is no sprint at all");
    }

    /// An issue that rolled over has been through sprints nobody can move it
    /// into any more; the list still has to show where it has been.
    #[test]
    fn a_sprint_that_is_no_longer_open_is_still_shown() {
        let mut current = current();
        current.sprint = vec!["Sprint 7".into(), "Sprint 42".into()];
        let mut screen = EditScreen::new("PROJ-142", &catalog(), &current);
        assert_eq!(screen.fields[SPRINT].text(), "Sprint 7, Sprint 42");
        assert!(
            screen.fields[SPRINT]
                .items
                .iter()
                .any(|i| i.value == "Sprint 7"),
            "a closed sprint the issue is in is still an option"
        );
        assert_eq!(
            done(screen.handle(key(KeyCode::Enter))).sprint,
            current.sprint
        );
    }

    #[test]
    fn an_issue_with_no_sprint_shows_the_backlog_as_its_placeholder() {
        let mut current = current();
        current.sprint = Vec::new();
        let mut screen = EditScreen::new("PROJ-142", &catalog(), &current);
        assert_eq!(screen.fields[SPRINT].filled(), None);
        let lines = render_lines(&mut screen, 80, 26).join("\n");
        assert!(lines.contains("Backlog"), "{lines}");
    }

    /// The mention is an id nobody can type, so typing a name has to be enough.
    #[test]
    fn typing_a_name_completes_to_the_mention_that_names_them() {
        let mut screen = screen();
        screen.focus = ASSIGNEE;
        screen.handle(ctrl('u'));
        typed(&mut screen, "jane");
        assert_eq!(
            screen.fields[ASSIGNEE]
                .suggestion()
                .map(|i| i.value.as_str()),
            Some("@[Jane D](5b1af9)")
        );

        // Tab takes it, and does not move on while there is something to take.
        screen.handle(key(KeyCode::Tab));
        assert_eq!(screen.focus, ASSIGNEE);
        assert_eq!(screen.fields[ASSIGNEE].text(), "@[Jane D](5b1af9)");

        // With nothing left to complete, tab is the next field again.
        screen.handle(key(KeyCode::Tab));
        assert_eq!(screen.focus, LABELS);

        let edits = done(screen.handle(key(KeyCode::Enter)));
        assert_eq!(edits.assignee.as_deref(), Some("@[Jane D](5b1af9)"));
    }

    /// Enter with a half-typed name takes the suggestion on screen rather than
    /// saving text that no push could send.
    #[test]
    fn saving_resolves_a_typed_name_and_refuses_an_unknown_one() {
        let mut screen = screen();
        screen.focus = ASSIGNEE;
        screen.handle(ctrl('u'));
        typed(&mut screen, "jane");
        let edits = done(screen.handle(key(KeyCode::Enter)));
        assert_eq!(edits.assignee.as_deref(), Some("@[Jane D](5b1af9)"));

        let mut screen = self::tests::screen();
        screen.focus = ASSIGNEE;
        screen.handle(ctrl('u'));
        typed(&mut screen, "somebody else");
        assert!(matches!(screen.handle(key(KeyCode::Enter)), Flow::Continue));
        assert_eq!(screen.focus, ASSIGNEE);
        assert!(
            screen.error.as_deref().unwrap().contains("somebody else"),
            "{:?}",
            screen.error
        );

        // Clearing it is always allowed: nobody assigned is a valid answer.
        screen.handle(ctrl('u'));
        let edits = done(screen.handle(key(KeyCode::Enter)));
        assert_eq!(edits.assignee, None);
    }

    /// A mention loaded from the file names someone the rest of the workspace
    /// may never have seen; saving must not demand they be in the list.
    #[test]
    fn an_assignee_the_file_already_had_survives_a_save() {
        let mut current = current();
        current.assignee = Some("@[Someone Else](abc123)".into());
        let mut screen = EditScreen::new("PROJ-142", &catalog(), &current);
        let edits = done(screen.handle(key(KeyCode::Enter)));
        assert_eq!(edits.assignee.as_deref(), Some("@[Someone Else](abc123)"));
    }

    #[test]
    fn the_suggestion_is_shown_next_to_the_field() {
        let mut screen = screen();
        screen.focus = ASSIGNEE;
        screen.handle(ctrl('u'));
        typed(&mut screen, "jane");
        let lines = render_lines(&mut screen, 80, 26).join("\n");
        assert!(lines.contains("→ Jane D"), "{lines}");
        assert!(lines.contains("tab complete"), "{lines}");
    }

    #[test]
    fn picking_a_label_adds_to_the_list_rather_than_replacing_it() {
        let mut screen = screen();
        screen.focus = LABELS;
        screen.handle(ctrl('p'));
        typed(&mut screen, "sso");
        screen.handle(key(KeyCode::Enter));
        assert_eq!(screen.fields[LABELS].text(), "auth, backend, sso");

        // And picking the same one twice does not duplicate it.
        screen.handle(ctrl('p'));
        typed(&mut screen, "sso");
        screen.handle(key(KeyCode::Enter));
        assert_eq!(screen.fields[LABELS].text(), "auth, backend, sso");
    }

    #[test]
    fn escape_asks_before_throwing_away_an_edit_but_not_otherwise() {
        let mut untouched = screen();
        assert!(matches!(untouched.handle(key(KeyCode::Esc)), Flow::Cancel));

        let mut screen = screen();
        typed(&mut screen, "!");
        assert!(matches!(screen.handle(key(KeyCode::Esc)), Flow::Continue));
        assert!(matches!(screen.overlay, Some(Overlay::Discard)));
        assert!(matches!(
            screen.handle(key(KeyCode::Char('y'))),
            Flow::Cancel
        ));
    }

    #[test]
    fn the_screen_shows_the_issue_and_marks_what_moved() {
        let mut screen = screen();
        let lines = render_lines(&mut screen, 80, 26).join("\n");
        assert!(lines.contains("Edit issue"), "{lines}");
        assert!(lines.contains("PROJ-142"), "{lines}");
        assert!(lines.contains("Fix login flow"), "{lines}");
        assert!(lines.contains("‹ In Progress ›"), "{lines}");
        assert!(
            lines.contains("Ilya A"),
            "the mention resolves to a name:\n{lines}"
        );

        screen.focus = SUMMARY;
        typed(&mut screen, "!");
        let lines = render_lines(&mut screen, 80, 26).join("\n");
        assert!(
            lines.contains("Summary •"),
            "the moved field is marked:\n{lines}"
        );
        assert!(lines.contains("1 field changed"), "{lines}");
    }

    #[test]
    fn a_short_terminal_scrolls_to_the_focused_field_without_panicking() {
        for (w, h) in [(20u16, 6u16), (40, 8), (80, 12), (200, 60)] {
            let mut screen = screen();
            render_lines(&mut screen, w, h);
            screen.focus = DESCRIPTION;
            let lines = render_lines(&mut screen, w, h).join("\n");
            if w >= 40 && h >= 12 {
                assert!(
                    lines.contains("Description"),
                    "the focused field has to be visible at {w}x{h}:\n{lines}"
                );
            }
            screen.handle(key(KeyCode::F(1)));
            render_lines(&mut screen, w, h);
            screen.handle(key(KeyCode::Esc));
            screen.focus = PARENT;
            screen.handle(ctrl('p'));
            render_lines(&mut screen, w, h);
        }
    }

    /// The form covers ten fields; a document holds far more. Everything it
    /// does not offer has to come out the other side untouched.
    #[test]
    fn applying_the_form_leaves_the_rest_of_the_document_alone() {
        use crate::doc::{CommentBlock, CommentId, Frontmatter, IssueDoc, SyncMeta};

        let mut doc = IssueDoc {
            frontmatter: Frontmatter {
                key: "PROJ-142".into(),
                id: "10023".into(),
                summary: "Fix login flow".into(),
                status: "In Progress".into(),
                issue_type: "Story".into(),
                labels: vec!["auth".into()],
                sprint: vec!["Sprint 42".into()],
                epic: Some("PROJ-90".into()),
                custom: [(
                    "customfield_10456".to_string(),
                    serde_json::json!("Platform"),
                )]
                .into_iter()
                .collect(),
                jired: SyncMeta {
                    base_updated: "2026-08-16T12:03:00Z".into(),
                    ..Default::default()
                },
                ..Default::default()
            },
            heading: "Fix login flow".into(),
            preamble: String::new(),
            description: "Login fails when SSO is enabled.".into(),
            checklist: vec![],
            children: vec![],
            comments: vec![CommentBlock {
                id: CommentId::Existing("c1".into()),
                author: "Jane D".into(),
                timestamp: "2026-08-15T10:32:00Z".into(),
                note: String::new(),
                body: "Repro confirmed.".into(),
            }],
            attachments: vec![],
            extra_sections: vec![],
        };

        let loaded = Edits::of(&doc);
        assert_eq!(loaded.summary, "Fix login flow");
        assert_eq!(loaded.status, "In Progress");

        let mut edited = loaded.clone();
        edited.summary = "Fix the SSO login flow".into();
        edited.status = "In Review".into();
        edited.apply(&mut doc);

        assert_eq!(doc.frontmatter.summary, "Fix the SSO login flow");
        assert_eq!(doc.heading, "Fix the SSO login flow", "the heading follows");
        assert_eq!(doc.frontmatter.status, "In Review");
        // Untouched by the form, and still here.
        assert_eq!(doc.frontmatter.sprint, ["Sprint 42"]);
        assert_eq!(doc.frontmatter.epic.as_deref(), Some("PROJ-90"));
        assert_eq!(doc.frontmatter.custom.len(), 1);
        assert_eq!(doc.comments.len(), 1);
        assert_eq!(doc.frontmatter.jired.base_updated, "2026-08-16T12:03:00Z");

        // And loading it again sees exactly what was applied.
        assert_eq!(Edits::of(&doc), edited);
    }

    /// Print the screen for looking at:
    /// `cargo test --lib edit::tests::preview -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn preview() {
        let mut s = screen();
        for l in render_lines(&mut s, 90, 26) {
            println!("|{l}");
        }
        println!();
        s.focus = SPRINT;
        s.handle(ctrl('p'));
        for l in render_lines(&mut s, 90, 26) {
            println!("|{l}");
        }
    }
}
