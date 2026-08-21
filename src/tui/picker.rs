//! A filtered list, shown as an overlay over whatever screen opened it.
//!
//! The filter is a subsequence match rather than a substring one, so `pl142`
//! finds `PLAT-142` and `sso ro` finds `SSO rollout`. Scoring prefers matches
//! that are contiguous and that start at word boundaries, which is what makes
//! the first row usually be the one wanted.

use crate::tui::input::TextInput;
use crossterm::event::{KeyCode, KeyEvent};
use unicode_segmentation::UnicodeSegmentation;

/// One row of a picker: what is chosen, and what the user reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    /// The value handed back on selection.
    pub value: String,
    /// The left-hand column; usually the value itself.
    pub label: String,
    /// Dim right-hand context, e.g. an issue's summary.
    pub detail: String,
}

impl Item {
    pub fn new(value: impl Into<String>) -> Self {
        let value = value.into();
        Self {
            label: value.clone(),
            value,
            detail: String::new(),
        }
    }

    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = detail.into();
        self
    }
}

/// What a key press did to the picker.
pub enum Outcome {
    Continue,
    /// The user chose a row; `None` means they chose the "no value" row.
    Chose(Option<String>),
    /// A multi-select list was confirmed, with everything ticked in it.
    ChoseMany(Vec<String>),
    Cancel,
}

pub struct Picker {
    pub title: String,
    items: Vec<Item>,
    /// Values ticked, for a list that takes several answers. `None` for the
    /// single-answer lists, which is most of them.
    ticked: Option<Vec<String>>,
    /// Offered as the first row when the field is allowed to have no value.
    clearable: bool,
    filter: TextInput,
    /// Indices into `items`, best match first; `usize::MAX` marks the clear row.
    matches: Vec<usize>,
    selected: usize,
    /// First visible row, moved only when the selection would leave the view.
    scroll: usize,
}

/// The clear row's stand-in index. Real indices can never reach it.
const CLEAR: usize = usize::MAX;

impl Picker {
    pub fn new(title: impl Into<String>, items: Vec<Item>, clearable: bool) -> Self {
        let mut picker = Self {
            title: title.into(),
            items,
            ticked: None,
            clearable,
            filter: TextInput::line(""),
            matches: Vec::new(),
            selected: 0,
            scroll: 0,
        };
        picker.refilter();
        picker
    }

    /// A list that answers with several values rather than one.
    ///
    /// Space ticks a row, enter confirms the lot. Used where the underlying
    /// field genuinely holds a set — the sprints an issue has been through —
    /// rather than one value out of many.
    pub fn multi(title: impl Into<String>, items: Vec<Item>, ticked: Vec<String>) -> Self {
        let mut picker = Self::new(title, items, false);
        picker.ticked = Some(ticked);
        picker
    }

    pub fn is_multi(&self) -> bool {
        self.ticked.is_some()
    }

    /// Whether a row is ticked, for the renderer to draw its box.
    pub fn is_ticked(&self, item: &Item) -> bool {
        self.ticked
            .as_ref()
            .is_some_and(|t| t.iter().any(|v| v == &item.value))
    }

    fn toggle(&mut self) {
        let Some(&index) = self.matches.get(self.selected) else {
            return;
        };
        if index == CLEAR {
            return;
        }
        let value = self.items[index].value.clone();
        let Some(ticked) = self.ticked.as_mut() else {
            return;
        };
        match ticked.iter().position(|v| v == &value) {
            Some(at) => {
                ticked.remove(at);
            }
            // Kept in the order they were ticked, which for sprints is the
            // order they happened.
            None => ticked.push(value),
        }
    }

    pub fn filter(&self) -> &TextInput {
        &self.filter
    }

    pub fn is_empty(&self) -> bool {
        self.matches.is_empty()
    }

    pub fn total(&self) -> usize {
        self.items.len()
    }

    /// How many real items the filter kept. The "leave empty" row is a way to
    /// clear the field, not something the filter matched.
    pub fn matched(&self) -> usize {
        self.matches.iter().filter(|&&m| m != CLEAR).count()
    }

    /// Start the list on the value the field already holds, so opening it and
    /// pressing enter changes nothing.
    pub fn select(&mut self, value: &str) {
        let hit = self
            .matches
            .iter()
            .position(|&m| m != CLEAR && self.items[m].value.eq_ignore_ascii_case(value.trim()));
        if let Some(i) = hit {
            self.selected = i;
        }
    }

    /// Move the window so the selected row is inside it. Called once per frame,
    /// before [`visible`](Self::visible), because only the renderer knows how
    /// many rows fit.
    pub fn scroll_to_fit(&mut self, height: usize) {
        let height = height.max(1);
        if self.selected < self.scroll {
            self.scroll = self.selected;
        } else if self.selected >= self.scroll + height {
            self.scroll = self.selected + 1 - height;
        }
    }

    /// The rows to draw, as `(item, is_selected)`. `None` for the item is the
    /// "leave this empty" row.
    pub fn visible(&self, height: usize) -> Vec<(Option<&Item>, bool)> {
        self.matches
            .iter()
            .enumerate()
            .skip(self.scroll)
            .take(height.max(1))
            .map(|(i, &m)| {
                let item = (m != CLEAR).then(|| &self.items[m]);
                (item, i == self.selected)
            })
            .collect()
    }

    pub fn handle(&mut self, key: KeyEvent) -> Outcome {
        // A multi-select answers with everything ticked, and space is what ticks.
        if let Some(ticked) = &self.ticked {
            match key.code {
                KeyCode::Esc => return Outcome::Cancel,
                KeyCode::Enter => return Outcome::ChoseMany(ticked.clone()),
                KeyCode::Char(' ') if self.filter.is_empty() => {
                    self.toggle();
                    return Outcome::Continue;
                }
                KeyCode::Tab => {
                    // With a filter typed, space belongs to the filter; tab is
                    // the way to tick without clearing what was typed.
                    self.toggle();
                    return Outcome::Continue;
                }
                _ => {}
            }
        }
        match key.code {
            KeyCode::Esc => return Outcome::Cancel,
            KeyCode::Enter => {
                return match self.matches.get(self.selected) {
                    Some(&CLEAR) => Outcome::Chose(None),
                    Some(&i) => Outcome::Chose(Some(self.items[i].value.clone())),
                    // Nothing matched, so the filter itself is the best guess
                    // at what the user meant.
                    None if !self.filter.is_empty() => {
                        Outcome::Chose(Some(self.filter.value().trim().to_string()))
                    }
                    None => Outcome::Cancel,
                };
            }
            KeyCode::Up => self.move_by(-1),
            KeyCode::Down => self.move_by(1),
            KeyCode::PageUp => self.move_by(-10),
            KeyCode::PageDown => self.move_by(10),
            _ => {
                if self.filter.handle(key) {
                    self.refilter();
                }
            }
        }
        Outcome::Continue
    }

    fn move_by(&mut self, delta: isize) {
        if self.matches.is_empty() {
            return;
        }
        let last = self.matches.len() as isize - 1;
        let next = self.selected as isize + delta;
        // Wrap at the ends for single steps; a page jump just clamps.
        self.selected = if delta.abs() == 1 {
            next.rem_euclid(last + 1) as usize
        } else {
            next.clamp(0, last) as usize
        };
    }

    fn refilter(&mut self) {
        let needle = self.filter.value().trim().to_lowercase();
        let mut scored: Vec<(i32, usize)> = self
            .items
            .iter()
            .enumerate()
            .filter_map(|(i, item)| {
                score(
                    &format!("{} {}", item.label, item.detail).to_lowercase(),
                    &needle,
                )
                .map(|s| (s, i))
            })
            .collect();
        // Sort by score, then by original order so equal matches keep the
        // order the caller chose to present them in.
        scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        self.matches = scored.into_iter().map(|(_, i)| i).collect();
        if self.clearable && needle.is_empty() {
            self.matches.insert(0, CLEAR);
        }
        self.selected = 0;
        self.scroll = 0;
    }
}

/// The one item `needle` matches best, by the same scoring the list uses.
///
/// Shared so that typing into a field and typing into its list agree about what
/// the best match is: a suggestion that differs from the first row of the list
/// it came from would be its own small betrayal.
pub fn best_match<'a>(items: &'a [Item], needle: &str) -> Option<&'a Item> {
    let needle = needle.trim().to_lowercase();
    if needle.is_empty() {
        return None;
    }
    items
        .iter()
        .filter_map(|item| {
            score(
                &format!("{} {}", item.label, item.detail).to_lowercase(),
                &needle,
            )
            .map(|s| (s, item))
        })
        // `max_by_key` keeps the last of equal scores; the first is the one the
        // caller ordered highest, so the comparison is reversed here.
        .fold(None, |best: Option<(i32, &Item)>, (s, item)| match best {
            Some((top, _)) if top >= s => best,
            _ => Some((s, item)),
        })
        .map(|(_, item)| item)
}

/// Score `needle` as a subsequence of `haystack`, or `None` if it is not one.
///
/// Both sides are already lowercased by the caller.
fn score(haystack: &str, needle: &str) -> Option<i32> {
    if needle.is_empty() {
        return Some(0);
    }
    let hay: Vec<&str> = haystack.graphemes(true).collect();
    let mut wanted = needle.graphemes(true).peekable();
    let mut total = 0;
    let mut previous: Option<usize> = None;
    for (i, g) in hay.iter().enumerate() {
        let Some(next) = wanted.peek() else { break };
        if g != next {
            continue;
        }
        wanted.next();
        total += match previous {
            // Adjacent to the last match: this is a real substring, not a
            // scattering of letters that happen to be in order.
            Some(p) if p + 1 == i => 8,
            _ => 0,
        };
        // The start of a word, which is where people aim.
        if i == 0 || hay[i - 1].chars().all(|c| !c.is_alphanumeric()) {
            total += 4;
        }
        previous = Some(i);
    }
    if wanted.peek().is_some() {
        return None;
    }
    // Shorter haystacks matched the same way are the tighter match.
    Some(total - (hay.len() as i32 / 8))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyModifiers;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn typed(picker: &mut Picker, text: &str) {
        for c in text.chars() {
            picker.handle(key(KeyCode::Char(c)));
        }
    }

    fn issues() -> Vec<Item> {
        vec![
            Item::new("PLAT-142").with_detail("Fix login flow"),
            Item::new("PLAT-150").with_detail("Rotate IdP certificate"),
            Item::new("OPS-7").with_detail("SSO rollout"),
        ]
    }

    #[test]
    fn filtering_matches_a_subsequence_of_label_or_detail() {
        let mut picker = Picker::new("Parent", issues(), false);
        typed(&mut picker, "pl142");
        assert_eq!(picker.matches.len(), 1);
        assert_eq!(picker.items[picker.matches[0]].value, "PLAT-142");

        let mut by_detail = Picker::new("Parent", issues(), false);
        typed(&mut by_detail, "sso ro");
        assert_eq!(by_detail.items[by_detail.matches[0]].value, "OPS-7");
    }

    #[test]
    fn the_best_match_is_the_one_the_list_would_put_first() {
        let items = issues();
        assert_eq!(
            best_match(&items, "pl142").map(|i| i.value.as_str()),
            Some("PLAT-142")
        );
        assert_eq!(
            best_match(&items, "sso").map(|i| i.value.as_str()),
            Some("OPS-7")
        );
        assert!(best_match(&items, "zzz").is_none());
        assert!(
            best_match(&items, "  ").is_none(),
            "nothing typed, nothing meant"
        );

        // Equal scores keep the caller's order, which is the list's order.
        let same = vec![Item::new("Ilya A"), Item::new("Ilya B")];
        assert_eq!(
            best_match(&same, "ilya").map(|i| i.value.as_str()),
            Some("Ilya A")
        );
    }

    #[test]
    fn a_contiguous_match_outranks_a_scattered_one() {
        let items = vec![Item::new("s-o-m-e-t-h-i-n-g"), Item::new("something else")];
        let mut picker = Picker::new("x", items, false);
        typed(&mut picker, "something");
        assert_eq!(picker.items[picker.matches[0]].value, "something else");
    }

    #[test]
    fn enter_returns_the_selected_value_and_esc_returns_nothing() {
        let mut picker = Picker::new("Parent", issues(), false);
        picker.handle(key(KeyCode::Down));
        assert!(matches!(
            picker.handle(key(KeyCode::Enter)),
            Outcome::Chose(Some(v)) if v == "PLAT-150"
        ));
        assert!(matches!(
            Picker::new("Parent", issues(), false).handle(key(KeyCode::Esc)),
            Outcome::Cancel
        ));
    }

    /// Sprints are a set, so the list that edits them has to be one too.
    #[test]
    fn a_multi_select_ticks_rows_and_answers_with_all_of_them() {
        let items = vec![
            Item::new("Sprint 42"),
            Item::new("Sprint 43"),
            Item::new("Sprint 44"),
        ];
        let mut picker = Picker::multi("Sprints", items, vec!["Sprint 42".into()]);
        assert!(picker.is_multi());
        assert!(picker.is_ticked(&Item::new("Sprint 42")));
        assert!(!picker.is_ticked(&Item::new("Sprint 43")));

        // Space ticks the selected row; the list stays open.
        picker.handle(key(KeyCode::Down));
        assert!(matches!(
            picker.handle(key(KeyCode::Char(' '))),
            Outcome::Continue
        ));
        match picker.handle(key(KeyCode::Enter)) {
            Outcome::ChoseMany(values) => assert_eq!(values, ["Sprint 42", "Sprint 43"]),
            _ => panic!("a multi-select answers with every ticked value"),
        }

        // Ticking something already ticked takes it off again.
        picker.handle(key(KeyCode::Up));
        picker.handle(key(KeyCode::Char(' ')));
        match picker.handle(key(KeyCode::Enter)) {
            Outcome::ChoseMany(values) => assert_eq!(values, ["Sprint 43"]),
            _ => panic!("a multi-select answers with every ticked value"),
        }
    }

    /// Space types a space while filtering, so tab is what ticks then.
    #[test]
    fn a_typed_filter_keeps_the_space_bar_and_tab_ticks_instead() {
        let items = vec![Item::new("Sprint 42"), Item::new("Sprint 43")];
        let mut picker = Picker::multi("Sprints", items, Vec::new());
        typed(&mut picker, "sprint 4");
        assert_eq!(picker.filter().value(), "sprint 4");

        picker.handle(key(KeyCode::Tab));
        match picker.handle(key(KeyCode::Enter)) {
            Outcome::ChoseMany(values) => assert_eq!(values, ["Sprint 42"]),
            _ => panic!("tab ticks while a filter is typed"),
        }
    }

    #[test]
    fn a_clearable_picker_offers_an_empty_row_first() {
        let mut picker = Picker::new("Priority", vec![Item::new("High")], true);
        assert!(matches!(
            picker.handle(key(KeyCode::Enter)),
            Outcome::Chose(None)
        ));
    }

    #[test]
    fn an_unmatched_filter_is_taken_at_face_value() {
        // Parent keys that are not in the workspace still have to be typeable.
        let mut picker = Picker::new("Parent", issues(), false);
        typed(&mut picker, "ZZZ-9");
        assert!(picker.is_empty());
        assert!(matches!(
            picker.handle(key(KeyCode::Enter)),
            Outcome::Chose(Some(v)) if v == "ZZZ-9"
        ));
    }

    #[test]
    fn the_selection_wraps_and_the_view_follows_it() {
        let mut picker = Picker::new("Parent", issues(), false);
        picker.handle(key(KeyCode::Up));
        assert_eq!(picker.selected, 2, "up from the top wraps to the end");

        // With room for two rows, the third selection scrolls the window.
        picker.scroll_to_fit(2);
        let rows = picker.visible(2);
        assert_eq!(rows.len(), 2);
        assert!(rows[1].1, "the selected row is the last visible one");
        assert_eq!(rows[0].0.unwrap().value, "PLAT-150");
    }
}
