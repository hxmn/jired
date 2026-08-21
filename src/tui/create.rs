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
use crate::tui::picker::{self, Item, Picker};
use crate::tui::{theme, Flow, Screen};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::{Constraint, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::Frame;

/// Issue types every Jira has, used only when the workspace knows none.
const STOCK_TYPES: &[&str] = &["Task", "Story", "Bug", "Epic", "Sub-task"];

/// Priorities in Jira's default scheme, used the same way.
const STOCK_PRIORITIES: &[&str] = &["Highest", "High", "Medium", "Low", "Lowest"];

/// How many labels to name in the hint line before saying "and more".
const LABEL_HINT: usize = 6;

/// What the workspace already knows, offered as suggestions.
///
/// Every list is ordered by how often the workspace uses the value, so the
/// first option is usually the right one.
#[derive(Debug, Clone, Default)]
pub struct Catalog {
    pub projects: Vec<String>,
    pub types: Vec<String>,
    pub priorities: Vec<String>,
    pub labels: Vec<String>,
    pub issues: Vec<KnownIssue>,
}

#[derive(Debug, Clone)]
pub struct KnownIssue {
    pub key: String,
    pub summary: String,
    pub issue_type: String,
}

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
        let card = card_area(frame.area(), self.needed_height());
        let project = self.fields[PROJECT].text().to_string();
        let block = Block::bordered()
            .border_type(theme::BORDER)
            .border_style(theme::idle_frame())
            .title_top(Line::from(vec![
                Span::raw(" "),
                Span::styled("New issue", theme::title()),
                Span::raw(" "),
            ]))
            .title_top(
                Line::from(vec![
                    Span::raw(" "),
                    Span::styled(project, theme::dim()),
                    Span::raw(" "),
                ])
                .right_aligned(),
            );
        let inner = block.inner(card);
        frame.render_widget(Clear, card);
        frame.render_widget(block, card);

        // The last three rows are the status line and the key hints, with a
        // rule above them; the rest belongs to the fields.
        let footer_height = 3.min(inner.height);
        let fields = Rect {
            height: inner.height - footer_height,
            ..inner
        };
        let footer = Rect {
            y: inner.y + inner.height - footer_height,
            height: footer_height,
            ..inner
        };
        self.render_fields(frame, fields);
        self.render_footer(frame, footer);

        match &mut self.overlay {
            Some(Overlay::Pick { picker, .. }) => render_picker(frame, card, picker),
            Some(Overlay::Help) => render_help(frame, card),
            Some(Overlay::Discard) => render_discard(frame, card),
            None => {}
        }
    }
}

/// Width of the label gutter, including the focus marker.
const GUTTER: u16 = 13;

/// Widest the card gets; a form stretched across a 200-column terminal is
/// harder to read, not easier.
const MAX_WIDTH: u16 = 78;

const SUMMARY_LABEL: &str = "Summary";
const TYPE_LABEL: &str = "Type";
const PRIORITY_LABEL: &str = "Priority";
const LABELS_LABEL: &str = "Labels";
const PARENT_LABEL: &str = "Parent";
const PROJECT_LABEL: &str = "Project";
const DESCRIPTION_LABEL: &str = "Description";

impl CreateScreen {
    /// Rows the card wants: one per single-line field, the body's own height,
    /// the blank lines between groups, the footer, and the border.
    fn needed_height(&self) -> u16 {
        let fields: u16 = self.fields.iter().map(|f| f.height).sum();
        /// Blank rows separating the summary, the metadata and the body.
        const GAPS: u16 = 2;
        /// The description's label, which sits above its text.
        const BODY_LABEL: u16 = 1;
        /// The rule, the status line and the key hints.
        const FOOTER: u16 = 3;
        /// Top and bottom of the card.
        const BORDER: u16 = 2;
        fields + GAPS + BODY_LABEL + FOOTER + BORDER
    }

    fn render_fields(&mut self, frame: &mut Frame, area: Rect) {
        let mut y = area.y;
        for index in 0..self.fields.len() {
            // A blank line separates the summary from the metadata, and the
            // metadata from the body.
            if matches!(index, TYPE | DESCRIPTION) {
                y += 1;
            }
            let height = if index == DESCRIPTION {
                self.fields[index].height + 1
            } else {
                1
            };
            if y + height > area.y + area.height {
                break;
            }
            let row = Rect {
                x: area.x,
                y,
                width: area.width,
                height,
            };
            self.render_field(frame, row, index);
            y += height;
        }
    }

    fn render_field(&mut self, frame: &mut Frame, area: Rect, index: usize) {
        let focused = index == self.focus && self.overlay.is_none();
        // Read out of the field before anything borrows it mutably.
        let label = self.fields[index].label;
        let placeholder = self.fields[index].placeholder;
        let marker = Span::styled(
            if focused { "▌" } else { " " },
            if focused {
                theme::focus_frame()
            } else {
                theme::dim()
            },
        );
        let label_style = if focused {
            theme::label_focused()
        } else {
            theme::label()
        };

        // The description's label sits on its own row with the text beneath it;
        // everything else shares one row with its label.
        if index == DESCRIPTION {
            frame.render_widget(
                Paragraph::new(Line::from(vec![
                    marker,
                    Span::raw(" "),
                    Span::styled(label, label_style),
                ])),
                Rect { height: 1, ..area },
            );
            let body = Rect {
                x: area.x + 2,
                y: area.y + 1,
                width: area.width.saturating_sub(3),
                height: area.height.saturating_sub(1),
            };
            self.render_body(frame, body, index, focused);
            return;
        }

        let value_x = area.x + GUTTER;
        let value_width = area.width.saturating_sub(GUTTER + 1);
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                marker,
                Span::raw(" "),
                Span::styled(label, label_style),
            ])),
            area,
        );
        let value_area = Rect {
            x: value_x,
            width: value_width,
            ..area
        };

        match &mut self.fields[index].value {
            Value::Choice(choice) => {
                let text = choice.selected().unwrap_or("—").to_string();
                frame.render_widget(
                    Paragraph::new(Line::from(vec![
                        Span::styled("‹ ", theme::dim()),
                        Span::styled(
                            text,
                            if choice.selected().is_some() {
                                theme::value()
                            } else {
                                theme::placeholder()
                            },
                        ),
                        Span::styled(" ›", theme::dim()),
                    ])),
                    value_area,
                );
            }
            Value::Line(input) | Value::Body(input) => {
                let (text, col) = input.line_view(value_width as usize);
                let span = if text.is_empty() {
                    Span::styled(placeholder, theme::placeholder())
                } else {
                    Span::styled(text.to_string(), theme::value())
                };
                frame.render_widget(Paragraph::new(Line::from(span)), value_area);
                if focused {
                    frame.set_cursor_position((value_x + col as u16, area.y));
                }
            }
        }

        // The parent field shows what it points at, which is the check the
        // user actually wants: did I pick the right issue?
        if index == PARENT {
            if let Some(detail) = self.parent_detail() {
                let used = self.fields[index].text().chars().count() as u16 + 2;
                if value_width > used + 4 {
                    frame.render_widget(
                        Paragraph::new(Span::styled(detail, theme::dim())),
                        Rect {
                            x: value_x + used,
                            width: value_width - used,
                            ..area
                        },
                    );
                }
            }
        }
    }

    fn render_body(&mut self, frame: &mut Frame, area: Rect, index: usize, focused: bool) {
        let width = area.width.max(1) as usize;
        let height = area.height.max(1) as usize;
        let placeholder = self.fields[index].placeholder;
        let Some(input) = self.fields[index].input_mut() else {
            return;
        };
        input.set_width(width);
        let rows = input.rows();
        let (cursor_row, cursor_col) = input.cursor_at();

        if cursor_row < self.body_scroll {
            self.body_scroll = cursor_row;
        } else if cursor_row >= self.body_scroll + height {
            self.body_scroll = cursor_row + 1 - height;
        }

        let input = self.fields[index].input().expect("a body is a text input");
        let value = input.value();
        if value.is_empty() {
            frame.render_widget(
                Paragraph::new(Span::styled(placeholder, theme::placeholder())),
                Rect { height: 1, ..area },
            );
        }
        for (offset, row) in rows.iter().skip(self.body_scroll).take(height).enumerate() {
            frame.render_widget(
                Paragraph::new(Span::styled(&value[row.start..row.end], theme::value())),
                Rect {
                    y: area.y + offset as u16,
                    height: 1,
                    ..area
                },
            );
        }
        if focused {
            frame.set_cursor_position((
                area.x + cursor_col as u16,
                area.y + (cursor_row - self.body_scroll) as u16,
            ));
        }
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

    fn render_footer(&self, frame: &mut Frame, area: Rect) {
        if area.height == 0 {
            return;
        }
        frame.render_widget(
            Block::new()
                .borders(Borders::TOP)
                .border_style(theme::idle_frame()),
            area,
        );
        if area.height < 2 {
            return;
        }

        // The status line is whichever is most useful right now: a complaint,
        // then the file that will be written, then help for this field.
        let status = if let Some(error) = &self.error {
            Line::from(Span::styled(format!("✗ {error}"), theme::error()))
        } else if !self.filename().is_empty() {
            Line::from(vec![
                Span::styled("writes ", theme::dim()),
                Span::styled(self.filename(), theme::value()),
            ])
        } else {
            Line::from(Span::styled(
                self.fields[self.focus].hint.clone(),
                theme::dim(),
            ))
        };
        frame.render_widget(
            Paragraph::new(status),
            Rect {
                x: area.x + 1,
                y: area.y + 1,
                width: area.width.saturating_sub(2),
                height: 1,
            },
        );

        if area.height < 3 {
            return;
        }
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
        frame.render_widget(
            Paragraph::new(key_hints(&keys)),
            Rect {
                x: area.x + 1,
                y: area.y + 2,
                width: area.width.saturating_sub(2),
                height: 1,
            },
        );
    }
}

fn key_hints(keys: &[(&str, &str)]) -> Line<'static> {
    let mut spans = Vec::new();
    for (i, (key, what)) in keys.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled("  ·  ", theme::dim()));
        }
        spans.push(Span::styled(key.to_string(), theme::key()));
        spans.push(Span::raw(" "));
        spans.push(Span::styled(what.to_string(), theme::dim()));
    }
    Line::from(spans)
}

/// Centre the card, capping its size so it stays readable, and let it shrink
/// rather than overflow on a small terminal.
fn card_area(area: Rect, wanted_height: u16) -> Rect {
    let width = area.width.min(MAX_WIDTH);
    let height = area.height.min(wanted_height);
    area.centered(Constraint::Length(width), Constraint::Length(height))
}

fn render_picker(frame: &mut Frame, card: Rect, picker: &mut Picker) {
    // Sit above the footer rather than across it: the key hints are what tell
    // the user how to get out of the list they just opened.
    let over = Rect {
        height: card.height.saturating_sub(3).max(3),
        ..card
    };
    let area = over.centered(
        Constraint::Length(card.width.saturating_sub(6).max(20)),
        Constraint::Length(over.height.saturating_sub(2).max(3)),
    );
    let block = Block::bordered()
        .border_type(theme::BORDER)
        .border_style(theme::focus_frame())
        .title_top(Line::from(vec![
            Span::raw(" "),
            Span::styled(picker.title.clone(), theme::title()),
            Span::raw(" "),
        ]))
        .title_bottom(
            Line::from(vec![
                Span::raw(" "),
                Span::styled(
                    format!("{} of {}", picker.matched(), picker.total()),
                    theme::dim(),
                ),
                Span::raw(" "),
            ])
            .right_aligned(),
        );
    let inner = block.inner(area);
    frame.render_widget(Clear, area);
    frame.render_widget(block, area);
    if inner.height == 0 {
        return;
    }

    // The filter is the top row and always has the cursor: typing is the
    // primary way to drive a list.
    let (text, col) = picker
        .filter()
        .line_view(inner.width.saturating_sub(2) as usize);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("/ ", theme::key()),
            if text.is_empty() {
                Span::styled("type to filter", theme::placeholder())
            } else {
                Span::styled(text.to_string(), theme::value())
            },
        ])),
        Rect { height: 1, ..inner },
    );
    frame.set_cursor_position((inner.x + 2 + col as u16, inner.y));

    let list = Rect {
        y: inner.y + 1,
        height: inner.height - 1,
        ..inner
    };
    picker.scroll_to_fit(list.height as usize);
    if picker.is_empty() {
        frame.render_widget(
            Paragraph::new(Span::styled(
                "nothing matches — ⏎ uses what you typed",
                theme::dim(),
            )),
            list,
        );
        return;
    }
    for (offset, (item, selected)) in picker.visible(list.height as usize).iter().enumerate() {
        let row = Rect {
            y: list.y + offset as u16,
            height: 1,
            ..list
        };
        let style = if *selected {
            theme::selected()
        } else {
            Style::default()
        };
        let line = match item {
            Some(item) => Line::from(vec![
                Span::styled(format!(" {:<12}", item.label), style),
                Span::styled(
                    item.detail.clone(),
                    if *selected { style } else { theme::dim() },
                ),
            ]),
            None => Line::from(Span::styled(
                " (leave empty)",
                if *selected {
                    style
                } else {
                    theme::placeholder()
                },
            )),
        };
        frame.render_widget(Paragraph::new(line), row);
    }
}

fn render_help(frame: &mut Frame, card: Rect) {
    let rows: &[(&str, &str)] = &[
        ("tab / shift-tab", "move between fields"),
        ("↑ ↓", "move between fields"),
        ("← →", "change a choice"),
        ("^p or f2", "open the field as a list"),
        ("⏎", "create the issue"),
        ("^s", "create from the description"),
        ("^a ^e ^w ^u ^k", "edit the line like a shell does"),
        ("esc", "cancel"),
    ];
    let area = card.centered(
        Constraint::Length(card.width.saturating_sub(8).max(24)),
        Constraint::Length(rows.len() as u16 + 2),
    );
    let block = Block::bordered()
        .border_type(theme::BORDER)
        .border_style(theme::focus_frame())
        .title_top(Line::from(vec![
            Span::raw(" "),
            Span::styled("Keys", theme::title()),
            Span::raw(" "),
        ]));
    let inner = block.inner(area);
    frame.render_widget(Clear, area);
    frame.render_widget(block, area);
    for (i, (key, what)) in rows.iter().enumerate() {
        if i as u16 >= inner.height {
            break;
        }
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(format!(" {key:<16}"), theme::key()),
                Span::styled(*what, theme::dim()),
            ])),
            Rect {
                y: inner.y + i as u16,
                height: 1,
                ..inner
            },
        );
    }
}

fn render_discard(frame: &mut Frame, card: Rect) {
    let area = card.centered(
        Constraint::Length(40.min(card.width)),
        Constraint::Length(4),
    );
    let block = Block::bordered()
        .border_type(theme::BORDER)
        .border_style(theme::error());
    let inner = block.inner(area);
    frame.render_widget(Clear, area);
    frame.render_widget(block, area);
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled(" Discard this issue?", theme::value())),
            key_hints(&[("y", "discard"), ("any other key", "keep editing")]),
        ]),
        inner,
    );
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
