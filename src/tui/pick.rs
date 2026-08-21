//! Choosing one issue out of the workspace [M4.7].
//!
//! The same filtered list the forms open over a field, standing on its own as a
//! screen: `jired edit` with no key shows every issue in the working copy and
//! narrows as you type. The filter is a subsequence match, so `pl142` finds
//! `PLAT-142` and `sso ro` finds the SSO rollout epic — you type what you
//! remember rather than what the key happens to be.
//!
//! Everything it lists is already on disk. There is no "search the server"
//! here on purpose: `edit` works on files, and offering an issue the workspace
//! does not have would produce a key nothing could open.

use crate::tui::catalog::KnownIssue;
use crate::tui::picker::{Item, Outcome, Picker};
use crate::tui::{form, theme, Flow, Screen};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;
use ratatui::text::Span;
use ratatui::widgets::Paragraph;
use ratatui::Frame;

pub struct PickScreen {
    picker: Picker,
    /// Shown under the list; the keys that drive it are not obvious from a
    /// filter box alone.
    hint: &'static str,
}

impl PickScreen {
    /// A screen listing `issues`, in the order given.
    pub fn new(title: impl Into<String>, issues: &[KnownIssue]) -> Self {
        let items = issues.iter().map(row).collect();
        Self {
            // Not clearable: this list exists to answer "which issue", and
            // "none" is what Esc says.
            picker: Picker::new(title, items, false),
            hint: "type to filter  ·  ↑↓ move  ·  ⏎ open  ·  esc cancel",
        }
    }

    pub fn is_empty(&self) -> bool {
        self.picker.total() == 0
    }
}

/// One row: the key, then what tells two issues apart at a glance.
fn row(issue: &KnownIssue) -> Item {
    let mut detail = issue.summary.clone();
    let aside: Vec<&str> = [issue.status.as_str(), issue.issue_type.as_str()]
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect();
    if !aside.is_empty() {
        detail.push_str("  ·  ");
        detail.push_str(&aside.join("  ·  "));
    }
    Item::new(&issue.key).with_detail(detail)
}

impl Screen for PickScreen {
    /// The key of the chosen issue.
    type Output = String;

    fn handle(&mut self, key: KeyEvent) -> Flow<String> {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return Flow::Cancel;
        }
        match self.picker.handle(key) {
            Outcome::Continue => Flow::Continue,
            Outcome::Cancel | Outcome::Chose(None) => Flow::Cancel,
            // A filter that matched nothing is taken at face value by the
            // picker; the caller decides whether such a key exists.
            Outcome::Chose(Some(value)) => Flow::Done(value),
        }
    }

    fn render(&mut self, frame: &mut Frame) {
        let area = frame.area();
        form::render_picker(frame, area, &mut self.picker);
        if area.height >= 2 {
            frame.render_widget(
                Paragraph::new(Span::styled(self.hint, theme::dim())).centered(),
                Rect {
                    y: area.y + area.height - 1,
                    height: 1,
                    ..area
                },
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::render_lines;

    fn issues() -> Vec<KnownIssue> {
        vec![
            KnownIssue {
                key: "PROJ-142".into(),
                summary: "Fix login flow".into(),
                issue_type: "Story".into(),
                status: "In Progress".into(),
            },
            KnownIssue {
                key: "PROJ-100".into(),
                summary: "SSO rollout".into(),
                issue_type: "Epic".into(),
                status: "To Do".into(),
            },
            KnownIssue {
                key: "NEW-8bf7b6".into(),
                summary: "Забота о сотрудниках".into(),
                issue_type: "Task".into(),
                status: String::new(),
            },
        ]
    }

    fn screen() -> PickScreen {
        PickScreen::new("Edit which issue?", &issues())
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn typed(screen: &mut PickScreen, text: &str) {
        for c in text.chars() {
            screen.handle(key(KeyCode::Char(c)));
        }
    }

    fn chose(flow: Flow<String>) -> String {
        match flow {
            Flow::Done(key) => key,
            _ => panic!("the screen did not choose anything"),
        }
    }

    #[test]
    fn enter_opens_the_selected_issue() {
        let mut screen = screen();
        assert_eq!(chose(screen.handle(key(KeyCode::Enter))), "PROJ-142");
    }

    /// The point of a fuzzy filter: type what you remember, in any order.
    #[test]
    fn typing_narrows_the_list_by_subsequence() {
        let mut by_summary = screen();
        typed(&mut by_summary, "sso");
        assert_eq!(chose(by_summary.handle(key(KeyCode::Enter))), "PROJ-100");

        let mut by_key = screen();
        typed(&mut by_key, "p142");
        assert_eq!(chose(by_key.handle(key(KeyCode::Enter))), "PROJ-142");

        // The summary is searched too, including a non-ASCII one.
        let mut cyrillic = screen();
        typed(&mut cyrillic, "забота");
        assert_eq!(chose(cyrillic.handle(key(KeyCode::Enter))), "NEW-8bf7b6");
    }

    #[test]
    fn escape_chooses_nothing() {
        let mut screen = screen();
        assert!(matches!(screen.handle(key(KeyCode::Esc)), Flow::Cancel));
    }

    #[test]
    fn the_arrows_move_the_selection() {
        let mut stepped = screen();
        stepped.handle(key(KeyCode::Down));
        assert_eq!(chose(stepped.handle(key(KeyCode::Enter))), "PROJ-100");
        // Up from the top wraps to the end.
        let mut wrapped = screen();
        wrapped.handle(key(KeyCode::Up));
        assert_eq!(chose(wrapped.handle(key(KeyCode::Enter))), "NEW-8bf7b6");
    }

    #[test]
    fn the_list_shows_what_tells_two_issues_apart() {
        let mut screen = screen();
        let lines = render_lines(&mut screen, 80, 20).join("\n");
        assert!(lines.contains("Edit which issue?"), "{lines}");
        assert!(lines.contains("PROJ-142"), "{lines}");
        assert!(lines.contains("Fix login flow"), "{lines}");
        assert!(lines.contains("In Progress"), "{lines}");
        assert!(lines.contains("3 of 3"), "the count is shown:\n{lines}");
        assert!(lines.contains("esc cancel"), "{lines}");
    }

    /// Print the picker for looking at:
    /// `cargo test --lib pick::tests::preview -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn preview() {
        let mut s = screen();
        for l in render_lines(&mut s, 80, 16) {
            println!("|{l}");
        }
    }

    #[test]
    fn a_small_terminal_renders_without_panicking() {
        for (w, h) in [(20u16, 4u16), (40, 8), (80, 24), (200, 60)] {
            let mut screen = screen();
            typed(&mut screen, "zzz");
            render_lines(&mut screen, w, h);
        }
    }
}
