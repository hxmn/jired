//! What every form screen does with a key, once.
//!
//! The create, edit, logwork and sprint screens each carried their own copy
//! of focus, the overlay over a field and the keys they share — sixty lines of
//! dispatch with the same comments in four places — so a key binding fixed in
//! one stayed broken in three. A screen now owns its fields' meaning (what is
//! dirty, what submitting checks and produces, what each field shows); the
//! [`FormController`] owns how keys move through them.

use crate::tui::field::Field;
use crate::tui::form;
use crate::tui::picker::{self, Picker};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;
use ratatui::Frame;

/// What is open over the form.
pub enum Overlay {
    /// A list open over the field at this index.
    Pick {
        field: usize,
        picker: Picker,
    },
    Help,
    /// Esc was pressed with something typed; confirm before throwing it away.
    Discard,
}

/// What a key asks of the screen, once the controller has done its part.
#[derive(Debug, PartialEq, Eq)]
pub enum Step {
    Continue,
    /// Finish: the screen checks the form and produces its result.
    Submit,
    Cancel,
}

pub struct FormController {
    pub fields: Vec<Field>,
    pub focus: usize,
    pub overlay: Option<Overlay>,
    /// A validation failure, cleared as soon as anything is typed.
    pub error: Option<String>,
    /// First visible row of the body field, when it has more than fits.
    pub body_scroll: usize,
    /// The field of several lines, which keeps Enter for itself.
    pub body: Option<usize>,
    /// Whether Tab completes what is being typed before it moves on.
    pub tab_completes: bool,
}

impl FormController {
    pub fn new(fields: Vec<Field>, body: Option<usize>) -> Self {
        Self {
            fields,
            focus: 0,
            overlay: None,
            error: None,
            body_scroll: 0,
            body,
            tab_completes: false,
        }
    }

    /// Tab completes the focused field first, the way a shell does.
    pub fn completing(mut self) -> Self {
        self.tab_completes = true;
        self
    }

    /// Handle one key. `dirty` says whether Esc has something to lose.
    pub fn handle(&mut self, key: KeyEvent, dirty: bool) -> Step {
        if let Some(overlay) = self.overlay.take() {
            return self.handle_overlay(key, overlay);
        }

        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            // Always a way out, even mid-edit: this is the key people reach for.
            KeyCode::Char('c') if ctrl => return Step::Cancel,
            KeyCode::Char('s') if ctrl => return Step::Submit,
            KeyCode::Char('l') if ctrl => {
                self.open_list();
                return Step::Continue;
            }
            KeyCode::F(2) => {
                self.open_list();
                return Step::Continue;
            }
            KeyCode::F(1) => {
                self.overlay = Some(Overlay::Help);
                return Step::Continue;
            }
            KeyCode::Esc => {
                return if dirty {
                    self.overlay = Some(Overlay::Discard);
                    Step::Continue
                } else {
                    Step::Cancel
                }
            }
            KeyCode::Tab if self.tab_completes && self.fields[self.focus].complete() => {
                self.error = None;
                return Step::Continue;
            }
            KeyCode::Tab => {
                self.move_focus(true);
                return Step::Continue;
            }
            KeyCode::BackTab => {
                self.move_focus(false);
                return Step::Continue;
            }
            // Enter finishes the form. The body field claims it first, below,
            // because a paragraph needs a way to hold more than one line.
            KeyCode::Enter if Some(self.focus) != self.body => return Step::Submit,
            _ => {}
        }

        if self.fields[self.focus].handle(key) {
            // Any edit invalidates the complaint that produced the error.
            self.error = None;
            return Step::Continue;
        }

        // Whatever the field did not want moves between fields, so the arrows
        // work everywhere they are not already doing something.
        match key.code {
            KeyCode::Down => self.move_focus(true),
            KeyCode::Up => self.move_focus(false),
            _ => {}
        }
        Step::Continue
    }

    /// Refuse to finish while `field` is empty: the cursor goes to it and the
    /// form says what is missing.
    pub fn require(&mut self, field: usize, what: &str) -> bool {
        if self.fields[field].filled().is_some() {
            return true;
        }
        self.focus = field;
        self.error = Some(format!("{what} is required"));
        false
    }

    pub fn move_focus(&mut self, forward: bool) {
        let len = self.fields.len();
        self.focus = if forward {
            (self.focus + 1) % len
        } else {
            (self.focus + len - 1) % len
        };
    }

    /// Open the focused field as a list, when it has one.
    pub fn open_list(&mut self) {
        let field = &self.fields[self.focus];
        if !field.openable() {
            return;
        }
        let mut picker = if field.multiple {
            Picker::multi(field.label, field.items.clone(), split_list(field.text()))
        } else {
            Picker::new(field.label, field.items.clone(), field.optional)
        };
        // Start the list on the value the field already holds.
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

    fn handle_overlay(&mut self, key: KeyEvent, overlay: Overlay) -> Step {
        match overlay {
            // Any key closes help; it is a reference card, not a mode.
            Overlay::Help => self.overlay = None,
            Overlay::Discard => match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') => return Step::Cancel,
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
                    if self.fields[field].accumulate {
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
        Step::Continue
    }

    /// Draw whatever is open over the form, inside `area`.
    pub fn render_overlay(
        &mut self,
        frame: &mut Frame,
        area: Rect,
        help: &[(&str, &str)],
        discard: &str,
    ) {
        match &mut self.overlay {
            Some(Overlay::Pick { picker, .. }) => form::render_picker(frame, area, picker),
            Some(Overlay::Help) => form::render_help(frame, area, help),
            Some(Overlay::Discard) => form::render_confirm(frame, area, discard),
            None => {}
        }
    }

    /// The focused field, unless something is open over the form.
    pub fn visible_focus(&self) -> Option<usize> {
        self.overlay.is_none().then_some(self.focus)
    }
}

/// A comma-separated value as its parts, each once.
pub fn split_list(text: &str) -> Vec<String> {
    let mut seen: Vec<String> = Vec::new();
    for value in text.split(',') {
        let value = value.trim();
        if !value.is_empty() && !seen.iter().any(|s| s == value) {
            seen.push(value.to_string());
        }
    }
    seen
}

/// The key events the screen tests type with, in one place: each screen's
/// tests declared their own.
#[cfg(test)]
pub(crate) mod keys {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    pub fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    pub fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    /// Type `text` into a screen a key at a time.
    pub fn typed<S: crate::tui::Screen>(screen: &mut S, text: &str) {
        for c in text.chars() {
            screen.handle(key(KeyCode::Char(c)));
        }
    }
}
