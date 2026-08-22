//! The time report as a screen: one row per person, opening into issues.
//!
//! A viewer rather than a form — nothing here changes anything, so every key is
//! either movement or a way out. The second level is the whole reason it is a
//! screen instead of a table: `--show-issues` on a printed report either buries
//! the totals under fifty issue lines or is not passed at all, whereas here the
//! breakdown is one keystroke under whichever person is worth asking about.

use crate::report::{bar, fit, format_hm, pad, Person, TimeReport};
use crate::tui::{form, theme, Flow, Screen};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::{Constraint, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph};
use ratatui::Frame;

/// Widest the card gets; a table stretched across a 200-column terminal puts
/// the totals an eye movement away from the names they belong to.
const MAX_WIDTH: u16 = 96;

/// Columns held by the total and the share.
const TOTAL: usize = 9;
const SHARE: usize = 5;

/// How wide the bar is allowed to get, and the narrowest that still says
/// anything; below it the bar is dropped rather than squeezing out the names.
const BAR_MAX: usize = 30;
const BAR_MIN: usize = 6;

/// The name column: wide enough to read, narrow enough that the total stays
/// beside the name rather than an eye movement away from it.
const NAME_MIN: usize = 10;
const NAME_MAX: usize = 28;

/// One line of the list: a person, or an issue under the person above them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Row {
    Person(usize),
    Issue(usize, usize),
}

pub struct ReportScreen<'a> {
    report: &'a TimeReport,
    /// Whether the issue level exists at all; without it the screen is a flat
    /// list and says so in its hints.
    show_issues: bool,
    expanded: Vec<bool>,
    selected: usize,
    scroll: usize,
}

impl<'a> ReportScreen<'a> {
    pub fn new(report: &'a TimeReport, show_issues: bool) -> Self {
        Self {
            report,
            show_issues,
            // Collapsed to start with: the answer to "who spent what" is the
            // top level, and opening every person would bury it.
            expanded: vec![false; report.people.len()],
            selected: 0,
            scroll: 0,
        }
    }

    fn rows(&self) -> Vec<Row> {
        let mut rows = Vec::with_capacity(self.report.people.len());
        for (p, person) in self.report.people.iter().enumerate() {
            rows.push(Row::Person(p));
            if self.show_issues && self.expanded[p] {
                rows.extend((0..person.issues.len()).map(|i| Row::Issue(p, i)));
            }
        }
        rows
    }

    fn move_by(&mut self, delta: isize) {
        let last = self.rows().len().saturating_sub(1) as isize;
        if last < 0 {
            return;
        }
        let next = self.selected as isize + delta;
        // Wrap on single steps, clamp on jumps: paging past the end should
        // land on the end rather than at the top again.
        self.selected = if delta.abs() == 1 {
            next.rem_euclid(last + 1) as usize
        } else {
            next.clamp(0, last) as usize
        };
    }

    fn open(&mut self) {
        if !self.show_issues {
            return;
        }
        if let Some(Row::Person(p)) = self.rows().get(self.selected) {
            if !self.report.people[*p].issues.is_empty() {
                self.expanded[*p] = true;
            }
        }
    }

    fn close(&mut self) {
        let Some(row) = self.rows().get(self.selected).copied() else {
            return;
        };
        match row {
            Row::Person(p) => self.expanded[p] = false,
            // Closing from inside moves back out to the person, which is where
            // the cursor would otherwise be left dangling.
            Row::Issue(p, _) => {
                self.expanded[p] = false;
                self.selected = self
                    .rows()
                    .iter()
                    .position(|r| *r == Row::Person(p))
                    .unwrap_or(0);
            }
        }
    }

    /// Open everything, or close it if anything is already open.
    fn toggle_all(&mut self) {
        if !self.show_issues {
            return;
        }
        let opening = self.expanded.iter().all(|e| !e);
        for (p, expanded) in self.expanded.iter_mut().enumerate() {
            *expanded = opening && !self.report.people[p].issues.is_empty();
        }
        self.selected = self.selected.min(self.rows().len().saturating_sub(1));
    }

    fn scroll_to_fit(&mut self, height: usize) {
        let height = height.max(1);
        if self.selected < self.scroll {
            self.scroll = self.selected;
        } else if self.selected >= self.scroll + height {
            self.scroll = self.selected + 1 - height;
        }
    }

    fn subtitle(&self) -> String {
        let days = match self.report.days {
            1 => "today".to_string(),
            days => format!("last {days} days"),
        };
        match &self.report.project {
            Some(project) => format!("{project} · {days} · {}", self.report.span()),
            None => format!("{days} · {}", self.report.span()),
        }
    }

    fn footer(&self) -> String {
        let people = self.report.people.len();
        let mut text = format!(
            "{people} {} · {}",
            if people == 1 { "person" } else { "people" },
            format_hm(self.report.seconds)
        );
        if self.report.staged_seconds > 0 {
            text.push_str(&format!(
                " · {} staged",
                format_hm(self.report.staged_seconds)
            ));
        }
        if self.report.skipped > 0 {
            text.push_str(&format!(" · {} unreadable", self.report.skipped));
        }
        text
    }

    fn keys(&self) -> Vec<(&'static str, &'static str)> {
        if self.show_issues {
            vec![
                ("↑↓", "move"),
                ("→", "issues"),
                ("←", "close"),
                ("a", "all"),
                ("q", "quit"),
            ]
        } else {
            vec![("↑↓", "move"), ("q", "quit"), ("--show-issues", "by issue")]
        }
    }
}

impl Screen for ReportScreen<'_> {
    /// Nothing: the report is read, not answered.
    type Output = ();

    fn handle(&mut self, key: KeyEvent) -> Flow<()> {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return Flow::Cancel;
        }
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => return Flow::Done(()),
            KeyCode::Down => self.move_by(1),
            KeyCode::Up => self.move_by(-1),
            KeyCode::PageDown => self.move_by(10),
            KeyCode::PageUp => self.move_by(-10),
            KeyCode::Home => self.selected = 0,
            KeyCode::End => self.selected = self.rows().len().saturating_sub(1),
            KeyCode::Right | KeyCode::Enter | KeyCode::Char(' ') => self.open(),
            KeyCode::Left => self.close(),
            KeyCode::Char('a') => self.toggle_all(),
            _ => {}
        }
        Flow::Continue
    }

    fn render(&mut self, frame: &mut Frame) {
        let rows = self.rows();
        // Border, the hint line, and a row each; capped by what the terminal
        // has, so a two-person report is a two-person card. An empty one still
        // needs the two lines that explain the emptiness.
        let wanted = if rows.is_empty() {
            5
        } else {
            rows.len() as u16 + 3
        };
        let area = frame.area().centered(
            Constraint::Length(frame.area().width.min(MAX_WIDTH)),
            Constraint::Length(frame.area().height.min(wanted)),
        );
        let block = Block::bordered()
            .border_type(theme::BORDER)
            .border_style(theme::idle_frame())
            .title_top(Line::from(vec![
                Span::raw(" "),
                Span::styled("Time booked", theme::title()),
                Span::raw(" "),
            ]))
            .title_top(
                Line::from(vec![
                    Span::raw(" "),
                    Span::styled(self.subtitle(), theme::dim()),
                    Span::raw(" "),
                ])
                .right_aligned(),
            )
            .title_bottom(
                Line::from(vec![
                    Span::raw(" "),
                    Span::styled(self.footer(), theme::dim()),
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

        let hints = Rect {
            y: inner.y + inner.height - 1,
            height: 1,
            ..inner
        };
        let list = Rect {
            height: inner.height - 1,
            ..inner
        };

        if rows.is_empty() {
            frame.render_widget(
                Paragraph::new(vec![
                    Line::from(Span::styled(
                        " nobody booked any time in this window",
                        theme::value(),
                    )),
                    Line::from(Span::styled(
                        " `jired pull` brings down what the server has",
                        theme::dim(),
                    )),
                ]),
                list,
            );
            frame.render_widget(
                Paragraph::new(form::key_hints(&[("q", "quit")])).centered(),
                hints,
            );
            return;
        }

        self.scroll_to_fit(list.height as usize);
        let width = list.width as usize;
        // The name column follows the longest name, so the totals sit beside
        // the names; whatever is left over goes to the bar, which is the one
        // thing that reads better the longer it is.
        let name_width = self
            .report
            .people
            .iter()
            .map(|p| p.name.chars().count())
            .max()
            .unwrap_or(0)
            .clamp(NAME_MIN, NAME_MAX);
        let bar_cells = match width.saturating_sub(2 + name_width + TOTAL + 2 + SHARE) {
            room if room >= BAR_MIN => room.min(BAR_MAX),
            _ => 0,
        };
        // One key column for the whole screen, so opening a second person does
        // not shift the summaries of the first.
        let key_width = self
            .report
            .people
            .iter()
            .flat_map(|p| p.issues.iter())
            .map(|i| i.key.chars().count())
            .max()
            .unwrap_or(0)
            .clamp(4, 16);

        for (index, row) in rows
            .iter()
            .enumerate()
            .skip(self.scroll)
            .take(list.height as usize)
        {
            let selected = index == self.selected;
            let area = Rect {
                y: list.y + (index - self.scroll) as u16,
                height: 1,
                ..list
            };
            let line = match row {
                Row::Person(p) => self.person_line(
                    &self.report.people[*p],
                    self.expanded[*p],
                    name_width,
                    bar_cells,
                    selected,
                ),
                Row::Issue(p, i) => {
                    let issue = &self.report.people[*p].issues[*i];
                    issue_line(
                        &issue.key,
                        &issue.summary,
                        issue.seconds,
                        key_width,
                        // The same right edge the rows above end at: an issue
                        // total belongs under its person's, not past it.
                        (2 + name_width + TOTAL + 2 + bar_cells + SHARE).min(width),
                        selected,
                    )
                }
            };
            frame.render_widget(Paragraph::new(line), area);
        }

        frame.render_widget(
            Paragraph::new(form::key_hints(&self.keys())).centered(),
            hints,
        );
    }
}

impl ReportScreen<'_> {
    fn person_line(
        &self,
        person: &Person,
        expanded: bool,
        name_width: usize,
        bar_cells: usize,
        selected: bool,
    ) -> Line<'static> {
        let style = if selected {
            theme::selected()
        } else {
            Style::default()
        };
        let dim = if selected { style } else { theme::dim() };
        let marker = match (self.show_issues && !person.issues.is_empty(), expanded) {
            (false, _) => "  ",
            (true, false) => "▸ ",
            (true, true) => "▾ ",
        };
        let share = self.report.share(person);
        let mut spans = vec![
            Span::styled(marker, if selected { style } else { theme::key() }),
            Span::styled(pad(&person.name, name_width), style),
            Span::styled(format!("{:>TOTAL$}", format_hm(person.seconds)), style),
            Span::raw(" "),
        ];
        if bar_cells > 0 {
            spans.push(Span::styled(
                bar(
                    share,
                    self.report.fraction(person.staged_seconds),
                    bar_cells,
                ),
                dim,
            ));
        }
        spans.push(Span::styled(format!("{:>4.0}%", share * 100.0), style));
        Line::from(spans)
    }
}

fn issue_line(
    key: &str,
    summary: &str,
    seconds: u64,
    key_width: usize,
    width: usize,
    selected: bool,
) -> Line<'static> {
    let style = if selected {
        theme::selected()
    } else {
        Style::default()
    };
    let summary_width = width.saturating_sub(4 + key_width + 2 + TOTAL).max(4);
    Line::from(vec![
        Span::styled("    ", style),
        Span::styled(
            pad(key, key_width),
            if selected { style } else { theme::key() },
        ),
        Span::raw("  "),
        Span::styled(
            pad(&fit(summary, summary_width), summary_width),
            if selected { style } else { theme::dim() },
        ),
        Span::styled(format!("{:>TOTAL$}", format_hm(seconds)), style),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::report::{IssueTime, TimeReport};
    use crate::tui::render_lines;

    fn person(name: &str, seconds: u64, issues: Vec<(&str, &str, u64)>) -> Person {
        Person {
            name: name.into(),
            seconds,
            staged_seconds: 0,
            entries: issues.len().max(1),
            issues: issues
                .into_iter()
                .map(|(key, summary, seconds)| IssueTime {
                    key: key.into(),
                    summary: summary.into(),
                    seconds,
                    staged_seconds: 0,
                    entries: 1,
                })
                .collect(),
        }
    }

    fn report() -> TimeReport {
        let people = vec![
            person(
                "Ilya A",
                5 * 3600,
                vec![
                    ("PROJ-150", "Rotate the certificate", 4 * 3600),
                    ("PROJ-142", "Fix login flow", 3600),
                ],
            ),
            person("Jane D", 9000, vec![("PROJ-142", "Fix login flow", 9000)]),
        ];
        TimeReport {
            days: 7,
            since: "2026-08-16T00:00:00Z".into(),
            until: "2026-08-22T10:00:00Z".into(),
            project: Some("PROJ".into()),
            last_fetch_at: None,
            last_fetch_age_seconds: None,
            last_fetch: None,
            seconds: people.iter().map(|p| p.seconds).sum(),
            staged_seconds: 0,
            entries: 3,
            skipped: 0,
            people,
        }
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn the_top_level_is_who_spent_what() {
        let report = report();
        let mut screen = ReportScreen::new(&report, true);
        let lines = render_lines(&mut screen, 80, 12).join("\n");

        assert!(lines.contains("Time booked"), "{lines}");
        assert!(lines.contains("PROJ · last 7 days"), "{lines}");
        assert!(lines.contains("Ilya A"), "{lines}");
        assert!(lines.contains("5h"), "{lines}");
        assert!(lines.contains("Jane D"), "{lines}");
        assert!(lines.contains("2h 30m"), "{lines}");
        assert!(lines.contains("2 people · 7h 30m"), "{lines}");
        // Collapsed to start with: the issues are one keystroke away, not in
        // the way of the totals.
        assert!(!lines.contains("Rotate the certificate"), "{lines}");
    }

    #[test]
    fn a_person_opens_into_the_issues_they_booked_against() {
        let report = report();
        let mut screen = ReportScreen::new(&report, true);
        screen.handle(key(KeyCode::Right));
        let lines = render_lines(&mut screen, 80, 12).join("\n");
        assert!(lines.contains("PROJ-150"), "{lines}");
        assert!(lines.contains("Rotate the certificate"), "{lines}");
        assert!(lines.contains("Fix login flow"), "{lines}");

        // Biggest first, as everywhere else in the report.
        let rows: Vec<&str> = lines.lines().collect();
        let at_150 = rows.iter().position(|l| l.contains("PROJ-150")).unwrap();
        let at_142 = rows.iter().position(|l| l.contains("PROJ-142")).unwrap();
        assert!(at_150 < at_142, "{lines}");

        screen.handle(key(KeyCode::Left));
        let closed = render_lines(&mut screen, 80, 12).join("\n");
        assert!(!closed.contains("PROJ-150"), "{closed}");
    }

    /// Without the flag the second level does not exist, and the hint says what
    /// would bring it into being.
    #[test]
    fn without_show_issues_the_screen_is_flat() {
        let report = report();
        let mut screen = ReportScreen::new(&report, false);
        screen.handle(key(KeyCode::Right));
        let lines = render_lines(&mut screen, 80, 12).join("\n");
        assert!(!lines.contains("PROJ-150"), "{lines}");
        assert!(lines.contains("--show-issues"), "{lines}");
    }

    #[test]
    fn closing_from_inside_a_person_lands_back_on_them() {
        let report = report();
        let mut screen = ReportScreen::new(&report, true);
        screen.handle(key(KeyCode::Right));
        screen.handle(key(KeyCode::Down));
        assert_eq!(screen.rows()[screen.selected], Row::Issue(0, 0));

        screen.handle(key(KeyCode::Left));
        assert_eq!(screen.rows()[screen.selected], Row::Person(0));
    }

    #[test]
    fn a_opens_every_person_and_closes_them_again() {
        let report = report();
        let mut screen = ReportScreen::new(&report, true);
        screen.handle(key(KeyCode::Char('a')));
        assert_eq!(screen.rows().len(), 2 + 3);
        screen.handle(key(KeyCode::Char('a')));
        assert_eq!(screen.rows().len(), 2);
    }

    #[test]
    fn the_selection_wraps_and_the_view_follows_it() {
        let report = report();
        let mut screen = ReportScreen::new(&report, true);
        screen.handle(key(KeyCode::Up));
        assert_eq!(screen.selected, 1, "up from the top wraps to the end");
        screen.scroll_to_fit(1);
        assert_eq!(screen.scroll, 1);
    }

    #[test]
    fn q_and_escape_close_the_report() {
        let report = report();
        let mut screen = ReportScreen::new(&report, true);
        assert!(matches!(
            screen.handle(key(KeyCode::Char('q'))),
            Flow::Done(())
        ));
        assert!(matches!(screen.handle(key(KeyCode::Esc)), Flow::Done(())));
        assert!(matches!(
            screen.handle(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            Flow::Cancel
        ));
    }

    #[test]
    fn an_empty_report_says_so_rather_than_drawing_an_empty_table() {
        let mut empty = report();
        empty.people.clear();
        empty.seconds = 0;
        empty.entries = 0;
        let mut screen = ReportScreen::new(&empty, true);
        let lines = render_lines(&mut screen, 80, 12).join("\n");
        assert!(lines.contains("nobody booked any time"), "{lines}");
        assert!(lines.contains("jired pull"), "{lines}");
    }

    /// Print the report for looking at:
    /// `cargo test --lib report::tests::preview -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn preview() {
        let report = report();
        let mut screen = ReportScreen::new(&report, true);
        screen.handle(key(KeyCode::Char('a')));
        for l in render_lines(&mut screen, 80, 16) {
            println!("|{l}");
        }
    }

    #[test]
    fn a_small_terminal_renders_without_panicking() {
        for (w, h) in [(20u16, 4u16), (40, 8), (80, 24), (200, 60)] {
            let report = report();
            let mut screen = ReportScreen::new(&report, true);
            screen.handle(key(KeyCode::Char('a')));
            render_lines(&mut screen, w, h);
        }
    }
}
