//! A text editor small enough to reason about.
//!
//! One type covers both shapes a form needs: a single-line field that scrolls
//! sideways, and a body that soft-wraps at the width it was drawn at. Editing
//! moves by grapheme cluster and measures by display width, so an emoji is one
//! backspace and a CJK summary puts the cursor where the character actually is.
//!
//! Nothing here touches a terminal — the widget hands back rows of text and a
//! cursor position, and the screen decides where to paint them.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

/// One visual row: a byte range of the value plus whether it ends because the
/// text ended (or a newline did) rather than because the row filled up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Row {
    pub start: usize,
    pub end: usize,
    pub hard: bool,
}

#[derive(Debug, Clone, Default)]
pub struct TextInput {
    value: String,
    /// Byte offset of the caret; always on a grapheme boundary.
    cursor: usize,
    multiline: bool,
    /// Width the value was last laid out at, so `up`/`down` know what a visual
    /// line is. The screen sets it while rendering, which always precedes the
    /// next key press.
    width: usize,
}

impl TextInput {
    pub fn line(value: impl Into<String>) -> Self {
        let value = value.into();
        Self {
            cursor: value.len(),
            value,
            multiline: false,
            width: 1,
        }
    }

    pub fn body(value: impl Into<String>) -> Self {
        let value = value.into();
        Self {
            cursor: value.len(),
            value,
            multiline: true,
            width: 1,
        }
    }

    pub fn value(&self) -> &str {
        &self.value
    }

    pub fn is_empty(&self) -> bool {
        self.value.is_empty()
    }

    pub fn set_value(&mut self, value: impl Into<String>) {
        self.value = value.into();
        self.cursor = self.value.len();
    }

    /// Tell the widget how wide it is being drawn. Called from the render pass.
    pub fn set_width(&mut self, width: usize) {
        self.width = width.max(1);
    }

    /// Visual rows at the current width. A single-line input is always one row.
    pub fn rows(&self) -> Vec<Row> {
        if !self.multiline {
            return vec![Row {
                start: 0,
                end: self.value.len(),
                hard: true,
            }];
        }
        wrap(&self.value, self.width)
    }

    /// Caret position as `(row, column)` in display cells.
    pub fn cursor_at(&self) -> (usize, usize) {
        let rows = self.rows();
        for (i, row) in rows.iter().enumerate() {
            let last = i + 1 == rows.len();
            // A caret sitting exactly on a soft wrap belongs to the start of
            // the next row — that is where the next character will appear.
            if self.cursor < row.end || (self.cursor == row.end && (row.hard || last)) {
                return (i, self.value[row.start..self.cursor].width());
            }
        }
        (0, 0)
    }

    /// The slice of a single-line value to draw in `width` cells, scrolled so
    /// the caret is visible, with the caret's column inside that slice.
    pub fn line_view(&self, width: usize) -> (&str, usize) {
        let width = width.max(1);
        let before = self.value[..self.cursor].width();
        // Keep one cell for the caret itself, so typing at the end of a full
        // field still shows where the next character goes.
        let skip = before.saturating_sub(width.saturating_sub(1));
        let mut start = 0;
        let mut skipped = 0;
        for (idx, g) in self.value.grapheme_indices(true) {
            if skipped >= skip {
                start = idx;
                break;
            }
            skipped += g.width();
            start = idx + g.len();
        }
        let mut end = start;
        let mut used = 0;
        for (idx, g) in self.value[start..].grapheme_indices(true) {
            let w = g.width();
            if used + w > width {
                break;
            }
            used += w;
            end = start + idx + g.len();
        }
        (&self.value[start..end], before - skipped)
    }

    /// Apply a key press, reporting whether it belonged to the editor.
    ///
    /// Keys the editor does not claim — `Tab`, `Enter` on a single-line field,
    /// anything with a control modifier it has no binding for — fall through to
    /// the screen, which is how one key can mean "next field" everywhere.
    pub fn handle(&mut self, key: KeyEvent) -> bool {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Char(c) if ctrl => match c {
                'a' => self.move_home(),
                'e' => self.move_end(),
                'u' => self.kill_to_start(),
                'k' => self.kill_to_end(),
                'w' => self.delete_word(),
                _ => return false,
            },
            KeyCode::Char(c) => self.insert(c),
            KeyCode::Backspace => self.backspace(),
            KeyCode::Delete => self.delete(),
            KeyCode::Left => self.move_left(),
            KeyCode::Right => self.move_right(),
            KeyCode::Home => self.move_home(),
            KeyCode::End => self.move_end(),
            KeyCode::Up if self.multiline => self.move_up(),
            KeyCode::Down if self.multiline => self.move_down(),
            KeyCode::Enter if self.multiline => self.insert('\n'),
            _ => return false,
        }
        true
    }

    pub fn insert(&mut self, c: char) {
        self.value.insert(self.cursor, c);
        self.cursor += c.len_utf8();
    }

    pub fn backspace(&mut self) {
        if let Some(prev) = self.prev_boundary() {
            self.value.replace_range(prev..self.cursor, "");
            self.cursor = prev;
        }
    }

    pub fn delete(&mut self) {
        if let Some(next) = self.next_boundary() {
            self.value.replace_range(self.cursor..next, "");
        }
    }

    pub fn move_left(&mut self) {
        if let Some(prev) = self.prev_boundary() {
            self.cursor = prev;
        }
    }

    pub fn move_right(&mut self) {
        if let Some(next) = self.next_boundary() {
            self.cursor = next;
        }
    }

    /// Home goes to the start of the visual row, then of the value — the same
    /// two-step every terminal editor does.
    pub fn move_home(&mut self) {
        let (row, col) = self.cursor_at();
        let rows = self.rows();
        match rows.get(row) {
            Some(r) if col > 0 => self.cursor = r.start,
            _ => self.cursor = 0,
        }
    }

    pub fn move_end(&mut self) {
        let (row, _) = self.cursor_at();
        let rows = self.rows();
        match rows.get(row) {
            Some(r) if self.cursor < r.end => self.cursor = r.end,
            _ => self.cursor = self.value.len(),
        }
    }

    pub fn move_up(&mut self) {
        let (row, col) = self.cursor_at();
        if row > 0 {
            self.cursor = self.offset_at(row - 1, col);
        }
    }

    pub fn move_down(&mut self) {
        let (row, col) = self.cursor_at();
        if row + 1 < self.rows().len() {
            self.cursor = self.offset_at(row + 1, col);
        }
    }

    pub fn kill_to_start(&mut self) {
        self.value.replace_range(..self.cursor, "");
        self.cursor = 0;
    }

    pub fn kill_to_end(&mut self) {
        self.value.replace_range(self.cursor.., "");
    }

    /// Delete the word before the caret, including the whitespace that led to
    /// it, so repeated presses walk back through a sentence.
    pub fn delete_word(&mut self) {
        let head = &self.value[..self.cursor];
        let trimmed = head.trim_end_matches(|c: char| c.is_whitespace());
        let start = match trimmed.rfind(|c: char| c.is_whitespace()) {
            Some(i) => i + self.value[i..].chars().next().map_or(1, char::len_utf8),
            None => 0,
        };
        self.value.replace_range(start..self.cursor, "");
        self.cursor = start;
    }

    /// Byte offset of the caret when placed at `col` display cells into `row`.
    fn offset_at(&self, row: usize, col: usize) -> usize {
        let rows = self.rows();
        let Some(r) = rows.get(row) else {
            return self.cursor;
        };
        let mut used = 0;
        for (idx, g) in self.value[r.start..r.end].grapheme_indices(true) {
            if used >= col {
                return r.start + idx;
            }
            used += g.width();
        }
        r.end
    }

    fn prev_boundary(&self) -> Option<usize> {
        self.value[..self.cursor]
            .grapheme_indices(true)
            .next_back()
            .map(|(i, _)| i)
    }

    fn next_boundary(&self) -> Option<usize> {
        self.value[self.cursor..]
            .graphemes(true)
            .next()
            .map(|g| self.cursor + g.len())
    }
}

/// Split a value into visual rows of at most `width` cells, breaking at spaces
/// where one is available and mid-word only when a word does not fit at all.
fn wrap(value: &str, width: usize) -> Vec<Row> {
    let width = width.max(1);
    let mut rows = Vec::new();
    let mut line_start = 0;
    // `split_inclusive` would keep the newline inside the row; ranges here
    // exclude it so a caret before and after a break are distinguishable.
    for line in value.split('\n') {
        let line_end = line_start + line.len();
        let mut start = line_start;
        loop {
            let (end, break_at) = fill(value, start, line_end, width);
            if end >= line_end {
                rows.push(Row {
                    start,
                    end: line_end,
                    hard: true,
                });
                break;
            }
            let cut = break_at.filter(|b| *b > start).unwrap_or(end);
            rows.push(Row {
                start,
                end: cut,
                hard: false,
            });
            start = cut;
        }
        line_start = line_end + 1;
    }
    rows
}

/// How far a row starting at `start` reaches, and the last space it could
/// break at instead.
fn fill(value: &str, start: usize, limit: usize, width: usize) -> (usize, Option<usize>) {
    let mut used = 0;
    let mut end = start;
    let mut break_at = None;
    for (idx, g) in value[start..limit].grapheme_indices(true) {
        let abs = start + idx;
        let w = g.width();
        if used + w > width && abs > start {
            return (end, break_at);
        }
        used += w;
        end = abs + g.len();
        if g.chars().all(char::is_whitespace) {
            break_at = Some(end);
        }
    }
    (limit, break_at)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    #[test]
    fn typing_and_erasing_track_the_caret() {
        let mut input = TextInput::line("");
        for c in "hello".chars() {
            input.handle(key(KeyCode::Char(c)));
        }
        assert_eq!(input.value(), "hello");
        input.handle(key(KeyCode::Backspace));
        assert_eq!(input.value(), "hell");
        input.handle(key(KeyCode::Home));
        input.handle(key(KeyCode::Delete));
        assert_eq!(input.value(), "ell");
    }

    #[test]
    fn editing_moves_by_grapheme_not_by_byte() {
        // A family emoji is several code points; one backspace must remove all
        // of it rather than leaving a fragment behind.
        let mut input = TextInput::line("ok 👨‍👩‍👧");
        input.handle(key(KeyCode::Backspace));
        assert_eq!(input.value(), "ok ");

        let mut accented = TextInput::line("café");
        accented.handle(key(KeyCode::Left));
        accented.handle(key(KeyCode::Backspace));
        assert_eq!(accented.value(), "café".replace('f', ""));
    }

    #[test]
    fn control_keys_do_the_usual_readline_things() {
        let mut input = TextInput::line("one two three");
        input.handle(ctrl('w'));
        assert_eq!(input.value(), "one two ");
        input.handle(ctrl('w'));
        assert_eq!(input.value(), "one ");
        input.handle(ctrl('u'));
        assert_eq!(input.value(), "");

        let mut kill = TextInput::line("keep this");
        kill.handle(ctrl('a'));
        kill.handle(ctrl('k'));
        assert_eq!(kill.value(), "");
    }

    #[test]
    fn a_single_line_field_does_not_claim_enter_or_tab() {
        let mut input = TextInput::line("x");
        assert!(!input.handle(key(KeyCode::Enter)));
        assert!(!input.handle(key(KeyCode::Tab)));
        assert_eq!(input.value(), "x");

        // A body does claim enter: that is how a paragraph gets a second line.
        let mut body = TextInput::body("x");
        assert!(body.handle(key(KeyCode::Enter)));
        assert_eq!(body.value(), "x\n");
    }

    #[test]
    fn wrapping_breaks_at_spaces_and_keeps_hard_breaks() {
        let rows = wrap("the quick brown fox", 10);
        let text: Vec<&str> = rows
            .iter()
            .map(|r| &"the quick brown fox"[r.start..r.end])
            .collect();
        assert_eq!(text, vec!["the quick ", "brown fox"]);
        assert!(!rows[0].hard, "the first row wrapped rather than ended");
        assert!(rows[1].hard);

        // A word longer than the row has to be cut somewhere.
        let long = wrap("supercalifragilistic", 8);
        assert_eq!(long.len(), 3);

        // An empty logical line is still a row, or the caret has nowhere to sit.
        let blank = wrap("a\n\nb", 10);
        assert_eq!(blank.len(), 3);
        assert_eq!(blank[1].start, blank[1].end);
    }

    #[test]
    fn the_caret_lands_on_the_row_the_next_character_would() {
        let mut body = TextInput::body("the quick brown fox");
        body.set_width(10);
        // End of the value: last row, after "fox".
        assert_eq!(body.cursor_at(), (1, 9));

        body.set_value("ab\ncd");
        body.move_home();
        assert_eq!(body.cursor_at(), (1, 0));
        body.move_up();
        assert_eq!(body.cursor_at(), (0, 0));
        body.move_end();
        // A hard break keeps the caret at the end of its own row.
        assert_eq!(body.cursor_at(), (0, 2));
    }

    #[test]
    fn up_and_down_keep_the_column() {
        let mut body = TextInput::body("first line\nsecond line");
        body.set_width(40);
        body.move_home();
        for _ in 0..4 {
            body.move_right();
        }
        assert_eq!(body.cursor_at(), (1, 4));
        body.move_up();
        assert_eq!(body.cursor_at(), (0, 4));
        body.move_down();
        assert_eq!(body.cursor_at(), (1, 4));
    }

    #[test]
    fn a_long_line_scrolls_to_keep_the_caret_in_view() {
        let input = TextInput::line("abcdefghij");
        let (text, col) = input.line_view(5);
        assert_eq!(text, "ghij");
        assert_eq!(col, 4, "the caret sits just past the last character");

        let short = TextInput::line("ab");
        assert_eq!(short.line_view(10), ("ab", 2));
    }

    #[test]
    fn wide_characters_are_measured_in_cells() {
        let input = TextInput::line("日本語");
        // Three characters, six cells.
        assert_eq!(input.line_view(10).1, 6);
    }
}
