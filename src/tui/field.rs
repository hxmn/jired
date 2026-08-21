//! Form fields: the three shapes a screen needs, behind one interface.
//!
//! A field is either typed into (a line or a paragraph) or chosen from a fixed
//! set. Both kinds can also be opened as a [picker](super::picker), which is
//! how a long list stays usable without turning every field into a menu.

use crate::tui::input::TextInput;
use crate::tui::picker::Item;
use crossterm::event::KeyEvent;

/// A value picked from a fixed set, optionally empty.
#[derive(Debug, Clone, Default)]
pub struct Choice {
    options: Vec<String>,
    /// `None` is the empty value, which only exists when `optional`.
    index: Option<usize>,
    optional: bool,
}

impl Choice {
    pub fn new(options: Vec<String>, optional: bool) -> Self {
        let index = (!optional && !options.is_empty()).then_some(0);
        Self {
            options,
            index,
            optional,
        }
    }

    pub fn selected(&self) -> Option<&str> {
        self.index.map(|i| self.options[i].as_str())
    }

    /// Select `value`, adding it to the options when the workspace has not
    /// seen it before — a flag may name a type this project has never used.
    pub fn set(&mut self, value: Option<&str>) {
        let Some(value) = value.map(str::trim).filter(|v| !v.is_empty()) else {
            if self.optional {
                self.index = None;
            }
            return;
        };
        match self
            .options
            .iter()
            .position(|o| o.eq_ignore_ascii_case(value))
        {
            Some(i) => self.index = Some(i),
            None => {
                self.options.push(value.to_string());
                self.index = Some(self.options.len() - 1);
            }
        }
    }

    /// Step through the options, treating "empty" as one of them when allowed.
    pub fn step(&mut self, forward: bool) {
        if self.options.is_empty() {
            return;
        }
        let len = self.options.len() as isize;
        // The empty value sits at -1, before the first option.
        let first = if self.optional { -1 } else { 0 };
        let current = self.index.map_or(-1, |i| i as isize);
        let span = len - first;
        let next = (current - first + if forward { 1 } else { -1 }).rem_euclid(span) + first;
        self.index = (next >= 0).then_some(next as usize);
    }

    pub fn options(&self) -> &[String] {
        &self.options
    }
}

/// How a field holds its value.
pub enum Value {
    /// One line of free text.
    Line(TextInput),
    /// A paragraph; claims Enter and the vertical arrows.
    Body(TextInput),
    /// One of a fixed set; claims the horizontal arrows.
    Choice(Choice),
}

pub struct Field {
    pub label: &'static str,
    /// Shown dim in place of an empty value.
    pub placeholder: &'static str,
    pub value: Value,
    /// Rows the value occupies. Only a body is taller than one.
    pub height: u16,
    /// Rows offered when the field is opened as a list. Empty means it cannot
    /// be opened.
    pub items: Vec<Item>,
    /// Whether an empty value is acceptable.
    pub optional: bool,
    /// One dim line under the value: what the workspace already uses, or how
    /// the value is read.
    pub hint: String,
    /// Shown dim after the value: what the value points at, for a field that
    /// names something else.
    pub detail: Option<String>,
    /// Marked in the label when the value no longer matches what was loaded.
    pub changed: bool,
    /// Whether the field holds several values, ticked from its list.
    pub multiple: bool,
    /// Whether typing offers the best matching item as a completion.
    ///
    /// For a field whose values are opaque — an assignee is stored as
    /// `@[Ilya A](5b10a2)`, an id nobody can type — this is what makes the
    /// keyboard usable at all without opening the list.
    pub suggest: bool,
}

impl Field {
    pub fn line(label: &'static str, placeholder: &'static str) -> Self {
        Self {
            label,
            placeholder,
            value: Value::Line(TextInput::line("")),
            height: 1,
            items: Vec::new(),
            optional: true,
            hint: String::new(),
            detail: None,
            changed: false,
            multiple: false,
            suggest: false,
        }
    }

    pub fn body(label: &'static str, placeholder: &'static str, height: u16) -> Self {
        Self {
            label,
            placeholder,
            value: Value::Body(TextInput::body("")),
            height: height.max(1),
            items: Vec::new(),
            optional: true,
            hint: String::new(),
            detail: None,
            changed: false,
            multiple: false,
            suggest: false,
        }
    }

    pub fn choice(label: &'static str, options: Vec<String>, optional: bool) -> Self {
        let items = options.iter().map(Item::new).collect();
        Self {
            label,
            placeholder: "—",
            value: Value::Choice(Choice::new(options, optional)),
            height: 1,
            items,
            optional,
            hint: String::new(),
            detail: None,
            changed: false,
            multiple: false,
            suggest: false,
        }
    }

    pub fn required(mut self) -> Self {
        self.optional = false;
        self
    }

    pub fn with_items(mut self, items: Vec<Item>) -> Self {
        self.items = items;
        self
    }

    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = hint.into();
        self
    }

    /// Hold several of the field's items rather than one of them.
    pub fn multiple(mut self) -> Self {
        self.multiple = true;
        self
    }

    /// Offer the best matching item while this field is being typed into.
    pub fn suggesting(mut self) -> Self {
        self.suggest = true;
        self
    }

    /// The item that completing right now would choose.
    ///
    /// `None` once the text is already one of the items: there is nothing left
    /// to complete, and offering to "complete" what is already chosen would put
    /// a suggestion on screen that does nothing.
    pub fn suggestion(&self) -> Option<&Item> {
        if !self.suggest {
            return None;
        }
        let typed = self.filled()?;
        if self
            .items
            .iter()
            .any(|i| i.value.eq_ignore_ascii_case(typed))
        {
            return None;
        }
        crate::tui::picker::best_match(&self.items, typed)
    }

    /// Take the suggestion, if there is one. Reports whether anything changed.
    pub fn complete(&mut self) -> bool {
        let Some(value) = self.suggestion().map(|i| i.value.clone()) else {
            return false;
        };
        self.set(Some(&value));
        true
    }

    /// The current value as text; empty when there is none.
    pub fn text(&self) -> &str {
        match &self.value {
            Value::Line(i) | Value::Body(i) => i.value(),
            Value::Choice(c) => c.selected().unwrap_or(""),
        }
    }

    /// The current value, or `None` when the field is empty.
    pub fn filled(&self) -> Option<&str> {
        Some(self.text().trim()).filter(|t| !t.is_empty())
    }

    pub fn set(&mut self, value: Option<&str>) {
        match &mut self.value {
            Value::Line(i) | Value::Body(i) => i.set_value(value.unwrap_or("")),
            Value::Choice(c) => c.set(value),
        }
    }

    pub fn input(&self) -> Option<&TextInput> {
        match &self.value {
            Value::Line(i) | Value::Body(i) => Some(i),
            Value::Choice(_) => None,
        }
    }

    pub fn input_mut(&mut self) -> Option<&mut TextInput> {
        match &mut self.value {
            Value::Line(i) | Value::Body(i) => Some(i),
            Value::Choice(_) => None,
        }
    }

    /// Whether the field can be opened as a list.
    pub fn openable(&self) -> bool {
        !self.items.is_empty()
    }

    /// Apply a key, reporting whether the field claimed it.
    pub fn handle(&mut self, key: KeyEvent) -> bool {
        use crossterm::event::KeyCode;
        match &mut self.value {
            Value::Line(i) | Value::Body(i) => i.handle(key),
            Value::Choice(c) => match key.code {
                KeyCode::Left => {
                    c.step(false);
                    true
                }
                KeyCode::Right => {
                    c.step(true);
                    true
                }
                // Typing a letter jumps to the first option starting with it,
                // the way a native select does. Chords are left for the screen:
                // ^l means "open the list", not "jump to Labels".
                KeyCode::Char(ch)
                    if !ch.is_control()
                        && !key.modifiers.intersects(
                            crossterm::event::KeyModifiers::CONTROL
                                | crossterm::event::KeyModifiers::ALT,
                        ) =>
                {
                    let hit = c.options().iter().position(|o| {
                        o.chars()
                            .next()
                            .is_some_and(|f| f.eq_ignore_ascii_case(&ch))
                    });
                    match hit {
                        Some(i) => {
                            let value = c.options()[i].clone();
                            c.set(Some(&value));
                            true
                        }
                        None => false,
                    }
                }
                _ => false,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyModifiers};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn a_required_choice_starts_on_its_first_option_and_cycles() {
        let mut c = Choice::new(vec!["Task".into(), "Bug".into()], false);
        assert_eq!(c.selected(), Some("Task"));
        c.step(true);
        assert_eq!(c.selected(), Some("Bug"));
        c.step(true);
        assert_eq!(c.selected(), Some("Task"), "cycling wraps");
        c.step(false);
        assert_eq!(c.selected(), Some("Bug"));
    }

    #[test]
    fn an_optional_choice_includes_the_empty_value_in_the_cycle() {
        let mut c = Choice::new(vec!["High".into(), "Low".into()], true);
        assert_eq!(c.selected(), None);
        c.step(true);
        assert_eq!(c.selected(), Some("High"));
        c.step(true);
        assert_eq!(c.selected(), Some("Low"));
        c.step(true);
        assert_eq!(c.selected(), None, "the empty value comes back round");
        c.step(false);
        assert_eq!(c.selected(), Some("Low"));
    }

    #[test]
    fn setting_a_value_the_workspace_has_not_seen_adds_it() {
        let mut c = Choice::new(vec!["Task".into()], false);
        c.set(Some("Spike"));
        assert_eq!(c.selected(), Some("Spike"));
        assert_eq!(c.options().len(), 2);
        // A known option is matched rather than duplicated, case-insensitively.
        c.set(Some("task"));
        assert_eq!(c.selected(), Some("Task"));
        assert_eq!(c.options().len(), 2);
    }

    #[test]
    fn a_choice_field_jumps_to_an_option_by_first_letter() {
        let mut f = Field::choice("Type", vec!["Task".into(), "Bug".into()], false);
        assert!(f.handle(key(KeyCode::Char('b'))));
        assert_eq!(f.text(), "Bug");
        assert!(
            !f.handle(key(KeyCode::Char('z'))),
            "no option starts with z"
        );
    }

    /// The assignee case: a person types a name and gets the mention that names
    /// them, because the mention is not something anyone could type.
    #[test]
    fn a_suggesting_field_completes_to_the_best_match() {
        let people = vec![
            Item::new("@[Ilya A](5b10a2)").with_detail("Ilya A"),
            Item::new("@[Jane D](5b1af9)").with_detail("Jane D"),
        ];
        let mut f = Field::line("Assignee", "nobody")
            .with_items(people)
            .suggesting();

        assert_eq!(f.suggestion(), None, "nothing typed, nothing to suggest");
        f.set(Some("jane"));
        assert_eq!(
            f.suggestion().map(|i| i.value.as_str()),
            Some("@[Jane D](5b1af9)")
        );
        assert!(f.complete());
        assert_eq!(f.text(), "@[Jane D](5b1af9)");
        assert_eq!(f.suggestion(), None, "already chosen");
        assert!(!f.complete());

        f.set(Some("nobody by that name"));
        assert_eq!(f.suggestion(), None);
    }

    #[test]
    fn a_field_that_does_not_suggest_never_completes() {
        let mut f = Field::line("Labels", "none").with_items(vec![Item::new("auth")]);
        f.set(Some("au"));
        assert_eq!(f.suggestion(), None);
        assert!(!f.complete());
    }

    #[test]
    fn a_line_field_reports_emptiness_by_trimmed_content() {
        let mut f = Field::line("Summary", "what needs doing").required();
        assert_eq!(f.filled(), None);
        f.set(Some("   "));
        assert_eq!(f.filled(), None);
        f.set(Some("  Fix login  "));
        assert_eq!(f.filled(), Some("Fix login"));
    }
}
