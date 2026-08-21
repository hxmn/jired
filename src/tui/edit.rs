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
const POINTS: usize = 7;
const PARENT: usize = 8;
const DESCRIPTION: usize = 9;

const SUMMARY_LABEL: &str = "Summary";
const STATUS_LABEL: &str = "Status";
const TYPE_LABEL: &str = "Type";
const PRIORITY_LABEL: &str = "Priority";
const ASSIGNEE_LABEL: &str = "Assignee";
const LABELS_LABEL: &str = "Labels";
const COMPONENTS_LABEL: &str = "Components";
const POINTS_LABEL: &str = "Points";
const PARENT_LABEL: &str = "Parent";
const DESCRIPTION_LABEL: &str = "Description";

const HELP: &[(&str, &str)] = &[
    ("tab / shift-tab", "move between fields"),
    ("↑ ↓", "move between fields"),
    ("← →", "change a choice"),
    ("^p or f2", "open the field as a list"),
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
                .with_hint("^p to pick someone this workspace has seen"),
            Field::line(LABELS_LABEL, "none")
                .with_items(catalog.labels.iter().map(Item::new).collect())
                .with_hint("comma-separated"),
            Field::line(COMPONENTS_LABEL, "none")
                .with_items(catalog.components.iter().map(Item::new).collect())
                .with_hint("comma-separated"),
            Field::line(POINTS_LABEL, "none").with_hint("a number, or empty to clear"),
            Field::line(PARENT_LABEL, "none")
                .with_items(issues)
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
        let points = self.fields[POINTS].text().trim().to_string();
        if !points.is_empty() && parse_points(&points).is_none() {
            self.focus = POINTS;
            self.error = Some(format!("`{points}` is not a number"));
            return Flow::Continue;
        }
        Flow::Done(self.edits())
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
        let mut picker = Picker::new(field.label, field.items.clone(), field.optional);
        if let Some(current) = field.filled() {
            picker.select(current);
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
        let key = self.fields[PARENT].filled()?.to_uppercase();
        self.fields[PARENT]
            .items
            .iter()
            .find(|i| i.value.eq_ignore_ascii_case(&key))
            .map(|i| i.detail.clone())
    }

    /// Who the assignee field names, so a mention is readable as a person.
    fn assignee_detail(&self) -> Option<String> {
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
            KeyCode::Tab => {
                self.move_focus(true);
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
                sprint: Some("Sprint 42".into()),
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
        assert_eq!(doc.frontmatter.sprint.as_deref(), Some("Sprint 42"));
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
    }
}
