//! Which issues this account watches, as a list of boxes to tick.
//!
//! Jira offers watching one issue at a time, from the issue's own page. What
//! somebody actually wants to know is "am I watching the things that moved this
//! week", and that is a question about a *list* — so this is a list, with the
//! state it starts from taken from the server rather than guessed at.
//!
//! Nothing is sent while the screen is open. Ticking is editing a plan, the
//! same way editing a file is: `⏎` sends what the ticks add up to, and `esc`
//! throws it away. That is what makes `a` — tick everything — safe to offer as
//! one key, and it is why the screen shows what the plan adds and removes
//! before anything leaves the machine.

use crate::tui::pick::fit;
use crate::tui::when::short;
use crate::tui::{theme, Flow, Screen};
use chrono::{DateTime, Utc};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

/// One issue the screen offers, as the server has it now.
#[derive(Debug, Clone)]
pub struct Row {
    pub key: String,
    pub summary: String,
    pub updated: Option<DateTime<Utc>>,
    /// Whether this account watches it — the state the ticks start from.
    pub watching: bool,
}

/// What the screen produces: the issues whose watch state is to change.
///
/// A plan rather than a new state: an issue nobody touched is in neither list,
/// so a screen closed without changes sends nothing at all.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Changes {
    pub watch: Vec<String>,
    pub unwatch: Vec<String>,
}

impl Changes {
    pub fn is_empty(&self) -> bool {
        self.watch.is_empty() && self.unwatch.is_empty()
    }

    pub fn len(&self) -> usize {
        self.watch.len() + self.unwatch.len()
    }
}

pub struct WatchScreen {
    rows: Vec<Row>,
    /// Ticked state, parallel to `rows`; `rows[i].watching` is what it started
    /// as, and the difference between the two is the plan.
    ticked: Vec<bool>,
    cursor: usize,
    scroll: usize,
    /// Right-aligned in the title row: the workspace this list came from.
    project: String,
    now: DateTime<Utc>,
}

impl WatchScreen {
    pub fn new(project: &str, rows: Vec<Row>, now: DateTime<Utc>) -> Self {
        let ticked = rows.iter().map(|r| r.watching).collect();
        Self {
            rows,
            ticked,
            cursor: 0,
            scroll: 0,
            project: project.to_string(),
            now,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// The plan as it stands: what ticking has added, and what it has taken off.
    pub fn changes(&self) -> Changes {
        let mut changes = Changes::default();
        for (row, ticked) in self.rows.iter().zip(&self.ticked) {
            match (row.watching, ticked) {
                (false, true) => changes.watch.push(row.key.clone()),
                (true, false) => changes.unwatch.push(row.key.clone()),
                _ => {}
            }
        }
        changes
    }

    fn move_by(&mut self, delta: isize) {
        if self.rows.is_empty() {
            return;
        }
        let last = self.rows.len() as isize - 1;
        self.cursor = (self.cursor as isize + delta).clamp(0, last) as usize;
    }

    fn toggle(&mut self) {
        if let Some(ticked) = self.ticked.get_mut(self.cursor) {
            *ticked = !*ticked;
        }
    }

    /// One key for the thing the screen exists to make easy.
    ///
    /// It toggles rather than only ticking: pressing it twice is how somebody
    /// who hit it by accident gets back to where they were, and a key that
    /// only ever adds would make that a row-by-row undo.
    fn toggle_all(&mut self) {
        let want = !self.ticked.iter().all(|t| *t);
        self.ticked.iter_mut().for_each(|t| *t = want);
    }

    /// Keep the cursor on screen, and the list under it as still as it can be.
    fn scroll_to_fit(&mut self, height: usize) {
        if height == 0 {
            return;
        }
        if self.cursor < self.scroll {
            self.scroll = self.cursor;
        } else if self.cursor >= self.scroll + height {
            self.scroll = self.cursor + 1 - height;
        }
    }
}

impl Screen for WatchScreen {
    type Output = Changes;

    fn handle(&mut self, key: KeyEvent) -> Flow<Changes> {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => return Flow::Cancel,
            KeyCode::Char('c') if ctrl => return Flow::Cancel,
            KeyCode::Enter => return Flow::Done(self.changes()),
            KeyCode::Up | KeyCode::Char('k') => self.move_by(-1),
            KeyCode::Down | KeyCode::Char('j') => self.move_by(1),
            KeyCode::PageUp => self.move_by(-10),
            KeyCode::PageDown => self.move_by(10),
            KeyCode::Home => self.cursor = 0,
            KeyCode::End => self.cursor = self.rows.len().saturating_sub(1),
            KeyCode::Char(' ') | KeyCode::Char('x') => self.toggle(),
            KeyCode::Char('a') => self.toggle_all(),
            _ => {}
        }
        Flow::Continue
    }

    fn render(&mut self, frame: &mut Frame) {
        let area = frame.area();
        if area.height == 0 || area.width == 0 {
            return;
        }
        let row_at = |y: u16| Rect {
            y,
            height: 1,
            ..area
        };
        let mut y = area.y;

        let watching = self.ticked.iter().filter(|t| **t).count();
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::raw(" "),
                Span::styled("Watching", theme::title()),
                Span::styled(
                    format!("  {watching} of {} ticked", self.rows.len()),
                    theme::dim(),
                ),
            ])),
            row_at(y),
        );
        frame.render_widget(
            Paragraph::new(Span::styled(format!("{} ", self.project), theme::dim()))
                .right_aligned(),
            row_at(y),
        );
        y += 1;

        // What `⏎` would send, said before it is sent rather than after: the
        // whole reason the ticks are a plan and not a series of writes.
        if y < area.y + area.height {
            let changes = self.changes();
            let line = if changes.is_empty() {
                Line::from(Span::styled(
                    " nothing to send — tick what you want to watch",
                    theme::placeholder(),
                ))
            } else {
                Line::from(vec![
                    Span::raw(" "),
                    Span::styled(format!("+{}", changes.watch.len()), theme::key()),
                    Span::styled(" to watch  ", theme::dim()),
                    Span::styled(format!("-{}", changes.unwatch.len()), theme::error()),
                    Span::styled(" to stop watching", theme::dim()),
                ])
            };
            frame.render_widget(Paragraph::new(line), row_at(y));
            y += 1;
        }

        let bottom = area.y + area.height;
        let hint_y = (bottom > y).then(|| bottom - 1);
        let list = Rect {
            y,
            height: hint_y.unwrap_or(bottom).saturating_sub(y),
            ..area
        };
        if let Some(hint_y) = hint_y {
            frame.render_widget(Paragraph::new(hint()).centered(), row_at(hint_y));
        }
        if list.height == 0 {
            return;
        }
        if self.rows.is_empty() {
            frame.render_widget(
                Paragraph::new(Span::styled(
                    " this workspace knows no issues yet — run `jired pull`",
                    theme::dim(),
                )),
                list,
            );
            return;
        }

        self.scroll_to_fit(list.height as usize);
        // The summary takes what the fixed columns leave, never less than a
        // word: a narrow terminal should still say which issue a row is.
        let key_width = self
            .rows
            .iter()
            .map(|r| r.key.chars().count())
            .max()
            .unwrap_or(8)
            .clamp(6, 20);
        let summary_width = (area.width as usize).saturating_sub(key_width + 14).max(8);

        for (offset, index) in (self.scroll..self.rows.len())
            .take(list.height as usize)
            .enumerate()
        {
            let row = &self.rows[index];
            let selected = index == self.cursor;
            let style = if selected {
                theme::selected()
            } else {
                Style::default()
            };
            let ticked = self.ticked[index];
            // A box that changed is worth seeing as a change, not just as a
            // state: it is what `⏎` is about to act on.
            let box_style = match (selected, ticked == row.watching) {
                (true, _) => style,
                (false, true) => theme::value(),
                (false, false) if ticked => theme::key(),
                (false, false) => theme::error(),
            };
            let when = row.updated.map(|u| short(self.now, u)).unwrap_or_default();
            frame.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled(" ", style),
                    Span::styled(if ticked { "[x]" } else { "[ ]" }, box_style),
                    Span::styled(" ", style),
                    Span::styled(
                        fit(&row.key, key_width),
                        if selected { style } else { theme::value() },
                    ),
                    Span::styled(" ", style),
                    Span::styled(
                        fit(&row.summary, summary_width),
                        if selected { style } else { theme::value() },
                    ),
                    Span::styled(" ", style),
                    Span::styled(fit(&when, 5), if selected { style } else { theme::dim() }),
                ])),
                Rect {
                    y: list.y + offset as u16,
                    height: 1,
                    ..list
                },
            );
        }
    }
}

fn hint() -> Line<'static> {
    let mut spans = Vec::new();
    for (index, (key, what)) in [
        ("space", "tick"),
        ("a", "all"),
        ("⏎", "send"),
        ("esc", "cancel"),
    ]
    .iter()
    .enumerate()
    {
        if index > 0 {
            spans.push(Span::styled("  ·  ", theme::dim()));
        }
        spans.push(Span::styled(*key, theme::key()));
        spans.push(Span::styled(format!(" {what}"), theme::dim()));
    }
    Line::from(spans)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::render_lines;

    fn rows() -> Vec<Row> {
        vec![
            Row {
                key: "PROJ-142".into(),
                summary: "Fix login flow".into(),
                updated: Some(Utc::now()),
                watching: true,
            },
            Row {
                key: "PROJ-143".into(),
                summary: "Rotate the signing key".into(),
                updated: Some(Utc::now()),
                watching: false,
            },
        ]
    }

    fn press(screen: &mut WatchScreen, code: KeyCode) -> Flow<Changes> {
        screen.handle(KeyEvent::new(code, KeyModifiers::NONE))
    }

    #[test]
    fn the_screen_starts_from_what_the_server_says_and_plans_nothing() {
        let screen = WatchScreen::new("PROJ", rows(), Utc::now());
        assert_eq!(screen.changes(), Changes::default());
        assert!(screen.changes().is_empty());
    }

    #[test]
    fn ticking_a_box_plans_a_watch_and_unticking_one_plans_a_stop() {
        let mut screen = WatchScreen::new("PROJ", rows(), Utc::now());
        press(&mut screen, KeyCode::Down);
        press(&mut screen, KeyCode::Char(' '));
        assert_eq!(screen.changes().watch, ["PROJ-143"]);
        assert!(screen.changes().unwatch.is_empty());

        press(&mut screen, KeyCode::Up);
        press(&mut screen, KeyCode::Char(' '));
        assert_eq!(screen.changes().unwatch, ["PROJ-142"]);

        // Back where it started, so there is nothing to send — a plan, not a
        // record of which keys were pressed.
        press(&mut screen, KeyCode::Char(' '));
        press(&mut screen, KeyCode::Down);
        press(&mut screen, KeyCode::Char(' '));
        assert!(screen.changes().is_empty());
    }

    /// The one key the screen exists for, and its way back.
    #[test]
    fn a_ticks_everything_and_ticks_it_back_off() {
        let mut screen = WatchScreen::new("PROJ", rows(), Utc::now());
        press(&mut screen, KeyCode::Char('a'));
        assert_eq!(screen.changes().watch, ["PROJ-143"]);
        assert!(
            screen.changes().unwatch.is_empty(),
            "an issue already watched is not sent again"
        );

        press(&mut screen, KeyCode::Char('a'));
        assert_eq!(screen.changes().unwatch, ["PROJ-142"]);
        assert!(screen.changes().watch.is_empty());
    }

    #[test]
    fn enter_hands_back_the_plan_and_esc_throws_it_away() {
        let mut screen = WatchScreen::new("PROJ", rows(), Utc::now());
        press(&mut screen, KeyCode::Char('a'));
        assert!(matches!(
            press(&mut screen, KeyCode::Enter),
            Flow::Done(changes) if changes.watch == ["PROJ-143"]
        ));
        assert!(matches!(press(&mut screen, KeyCode::Esc), Flow::Cancel));
    }

    #[test]
    fn the_screen_shows_the_boxes_the_counts_and_the_keys() {
        let mut screen = WatchScreen::new("PROJ", rows(), Utc::now());
        let lines = render_lines(&mut screen, 60, 8);
        let text = lines.join("\n");
        assert!(text.contains("Watching"), "{text}");
        assert!(text.contains("1 of 2 ticked"), "{text}");
        assert!(text.contains("[x] PROJ-142"), "{text}");
        assert!(text.contains("[ ] PROJ-143"), "{text}");
        assert!(text.contains("Fix login flow"), "{text}");
        assert!(text.contains("space tick"), "{text}");

        press(&mut screen, KeyCode::Char('a'));
        let text = render_lines(&mut screen, 60, 8).join("\n");
        assert!(
            text.contains("+1 to watch"),
            "the plan is said out loud: {text}"
        );
    }

    /// A list longer than the window still shows the row under the cursor.
    #[test]
    fn the_list_scrolls_to_keep_the_cursor_visible() {
        let many: Vec<Row> = (1..=40)
            .map(|n| Row {
                key: format!("PROJ-{n}"),
                summary: format!("Issue {n}"),
                updated: None,
                watching: false,
            })
            .collect();
        let mut screen = WatchScreen::new("PROJ", many, Utc::now());
        press(&mut screen, KeyCode::End);
        let text = render_lines(&mut screen, 60, 10).join("\n");
        assert!(text.contains("PROJ-40"), "{text}");
        assert!(
            !text.contains("PROJ-1 "),
            "the top has scrolled off: {text}"
        );
    }
}
