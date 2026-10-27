//! The time report as a screen: people down the side, days across the top.
//!
//! A viewer rather than a form — nothing here changes anything, so every key is
//! either movement or a way of asking the same question differently. Two of
//! those are the reason it is a screen and not a printed table: the window can
//! be widened and walked backwards (`+`/`-`, `<`/`>`) without re-running the
//! command, and the issue level opens under whichever person is worth asking
//! about (`i`, or `→` for one of them) instead of being decided in advance and
//! burying the totals.
//!
//! The report is re-aggregated on every one of those keystrokes. It costs one
//! pass over documents already in memory, which is cheaper than any of the ways
//! of keeping a pre-computed grid in step with a moving window.

use crate::report::{
    column_head, format_cell, format_hm, heat, is_weekend, pad, Person, Source, TimeReport,
    MAX_DAYS,
};
use crate::tui::{form, theme, Flow, Screen};
use chrono::NaiveDate;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::{Constraint, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph};
use ratatui::Frame;

/// Widest the card gets. Wider than a form's, because every extra column is
/// another day of the window that fits. The lower bound is what the title —
/// the project, the window, the dates — needs to stay readable.
const MAX_WIDTH: u16 = 120;
const MIN_WIDTH: u16 = 64;

/// A day column with a figure in it, and one with only a shade.
const CELL: usize = 6;
const HEAT_CELL: usize = 2;

/// The total column, and the name column's bounds.
const TOTAL: usize = 9;
const NAME_MIN: usize = 12;
const NAME_MAX: usize = 26;

/// Rows the card spends on something other than people: the top border, the
/// column heading, the totals, the key hints, the bottom border.
const CHROME: u16 = 5;

/// One line of the list: a person, or an issue under the person above them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Row {
    Person(usize),
    Issue(usize, usize),
}

/// How much of a day column there is room to say.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Cells {
    /// The figure itself: `2h30`.
    Numeric,
    /// A shade, for a window too wide to write the figures in.
    Heat,
    /// Not even that; only the totals column is drawn.
    None,
}

pub struct ReportScreen<'a> {
    source: &'a Source<'a>,
    report: TimeReport,
    days: u32,
    /// How many days back the window's last column sits from today.
    back: u32,
    /// People whose issues are open, by name — the window moving underneath
    /// them must not shuffle whose breakdown is showing.
    expanded: Vec<String>,
    selected: usize,
    scroll: usize,
    help: bool,
}

impl<'a> ReportScreen<'a> {
    pub fn new(source: &'a Source<'a>, days: u32, show_issues: bool) -> Self {
        let report = source.report(days, 0);
        let expanded = match show_issues {
            true => report.people.iter().map(|p| p.name.clone()).collect(),
            false => Vec::new(),
        };
        Self {
            source,
            report,
            days,
            back: 0,
            expanded,
            selected: 0,
            scroll: 0,
            help: false,
        }
    }

    /// Re-aggregate after the window moved, keeping the cursor on the row it
    /// was on where that row still exists.
    fn reload(&mut self) {
        let was = self.rows().get(self.selected).map(|row| match row {
            Row::Person(p) | Row::Issue(p, _) => self.report.people[*p].name.clone(),
        });
        self.report = self.source.report(self.days, self.back);
        self.selected = was
            .and_then(|name| {
                self.rows().iter().position(
                    |row| matches!(row, Row::Person(p) if self.report.people[*p].name == name),
                )
            })
            .unwrap_or(0);
        self.scroll = 0;
    }

    fn rows(&self) -> Vec<Row> {
        let mut rows = Vec::with_capacity(self.report.people.len());
        for (p, person) in self.report.people.iter().enumerate() {
            rows.push(Row::Person(p));
            if self.is_open(person) {
                rows.extend((0..person.issues.len()).map(|i| Row::Issue(p, i)));
            }
        }
        rows
    }

    fn is_open(&self, person: &Person) -> bool {
        self.expanded.iter().any(|name| name == &person.name)
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
        let Some(Row::Person(p)) = self.rows().get(self.selected).copied() else {
            return;
        };
        let person = &self.report.people[p];
        if !person.issues.is_empty() && !self.is_open(person) {
            self.expanded.push(person.name.clone());
        }
    }

    fn close(&mut self) {
        let Some(row) = self.rows().get(self.selected).copied() else {
            return;
        };
        let p = match row {
            Row::Person(p) => p,
            // Closing from inside moves back out to the person, which is where
            // the cursor would otherwise be left dangling.
            Row::Issue(p, _) => p,
        };
        let name = self.report.people[p].name.clone();
        self.expanded.retain(|open| open != &name);
        self.selected = self
            .rows()
            .iter()
            .position(|row| *row == Row::Person(p))
            .unwrap_or(0);
    }

    /// Open the issue level under everybody, or shut it everywhere.
    fn toggle_all(&mut self) {
        if self.expanded.is_empty() {
            self.expanded = self.report.people.iter().map(|p| p.name.clone()).collect();
        } else {
            self.expanded.clear();
            self.selected = self.selected.min(self.rows().len().saturating_sub(1));
        }
    }

    /// Widen or narrow the window, keeping its last day where it is.
    fn resize(&mut self, delta: i64) {
        let days = (i64::from(self.days) + delta).clamp(1, i64::from(MAX_DAYS)) as u32;
        if days != self.days {
            self.days = days;
            self.reload();
        }
    }

    /// Walk the window a whole width back in time, or forward again: from this
    /// week to the one before it, which is the step a timesheet is read in.
    fn shift(&mut self, windows_back: i64) {
        let back = (i64::from(self.back) + windows_back * i64::from(self.days))
            .clamp(0, i64::from(MAX_DAYS)) as u32;
        if back != self.back {
            self.back = back;
            self.reload();
        }
    }

    /// Open on a window that ends `back` days before today: where `--since`
    /// and `--until` put it, rather than on this week.
    pub fn go_back(&mut self, back: u32) {
        if back != self.back {
            self.back = back.min(MAX_DAYS);
            self.reload();
        }
    }

    fn today(&mut self) {
        if self.back != 0 {
            self.back = 0;
            self.reload();
        }
    }

    fn scroll_to_fit(&mut self, height: usize) {
        let height = height.max(1);
        if self.selected < self.scroll {
            self.scroll = self.selected;
        } else if self.selected >= self.scroll + height {
            self.scroll = self.selected + 1 - height;
        }
    }

    /// How the window is drawn in the top-right corner.
    fn subtitle(&self) -> String {
        let when = match self.back {
            0 if self.days == 1 => "today".to_string(),
            0 => format!("last {} days", self.days),
            back if back == self.days => format!("{} days before that", self.days),
            _ => format!("{} days, {} days back", self.days, self.back),
        };
        match &self.report.project {
            Some(project) => format!("{project} · {when} · {}", self.report.span()),
            None => format!("{when} · {}", self.report.span()),
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

    /// Which columns fit, and how much of each can be said.
    ///
    /// A window wider than the terminal keeps its most recent days rather than
    /// its oldest: the totals still cover everything asked for, and the note in
    /// the hint line says how many columns are not being shown.
    fn columns(&self, width: usize, name_width: usize) -> (Cells, usize) {
        let room = width.saturating_sub(2 + name_width + TOTAL);
        let days = self.days as usize;
        if room / CELL >= days {
            return (Cells::Numeric, days);
        }
        if room / HEAT_CELL >= days {
            return (Cells::Heat, days);
        }
        match room / HEAT_CELL {
            0 => (Cells::None, 0),
            fits => (Cells::Heat, fits),
        }
    }

    /// The first column follows the longest label it has to hold — and an open
    /// person's issues are labels in it too, which is why opening one widens it.
    fn name_width(&self, width: usize) -> usize {
        let longest = self
            .report
            .people
            .iter()
            .map(|person| {
                let issues = match self.is_open(person) {
                    false => 0,
                    true => person
                        .issues
                        .iter()
                        .map(|i| i.key.chars().count() + 1 + i.summary.chars().count() + 2)
                        .max()
                        .unwrap_or(0),
                };
                person.name.chars().count().max(issues)
            })
            .max()
            .unwrap_or(0);
        longest
            .clamp(NAME_MIN, NAME_MAX)
            .min(width.saturating_sub(TOTAL + 4).max(4))
    }

    fn keys(&self) -> Vec<(&'static str, &'static str)> {
        vec![
            ("↑↓", "move"),
            ("→", "issues"),
            ("i", "all"),
            ("±", "days"),
            ("<>", "window"),
            ("?", "keys"),
            ("q", "quit"),
        ]
    }

    const HELP: &'static [(&'static str, &'static str)] = &[
        ("↑ ↓", "move between rows"),
        ("→ ⏎", "open a person into their issues"),
        ("←", "close them again"),
        ("i", "open or shut the issue level for everybody"),
        ("+ -", "one day more or less in the window"),
        ("< >", "the window before this one, or after it"),
        ("t", "back to a window ending today"),
        ("q esc", "leave"),
    ];
}

impl Screen for ReportScreen<'_> {
    /// Nothing: the report is read, not answered.
    type Output = ();

    fn handle(&mut self, key: KeyEvent) -> Flow<()> {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return Flow::Cancel;
        }
        // The key card is a reading of the same screen; any key puts it away.
        if self.help {
            self.help = false;
            return Flow::Continue;
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
            KeyCode::Char('i') => self.toggle_all(),
            KeyCode::Char('+' | '=') => self.resize(1),
            KeyCode::Char('-' | '_') => self.resize(-1),
            KeyCode::Char(',' | '<') => self.shift(1),
            KeyCode::Char('.' | '>') => self.shift(-1),
            KeyCode::Char('t') => self.today(),
            KeyCode::Char('?') => self.help = true,
            _ => {}
        }
        Flow::Continue
    }

    fn render(&mut self, frame: &mut Frame) {
        let rows = self.rows();
        // Chrome, and a row each; capped by what the terminal has, so a
        // two-person report is a two-person card. An empty one still needs the
        // two lines that explain the emptiness.
        let wanted = match rows.is_empty() {
            true => 5,
            false => rows.len() as u16 + CHROME,
        };
        // The card is as wide as the grid it holds and no wider — a table
        // stretched across a 200-column terminal puts the totals an eye
        // movement away from the names — but never so narrow that the window
        // it is showing cannot be read off the title.
        let available = frame.area().width.min(MAX_WIDTH);
        // Borders, the marker, the columns, and one space so the totals do not
        // sit against the frame.
        let wants = 5 + self.name_width(available as usize) + self.days as usize * CELL + TOTAL;
        let area = frame.area().centered(
            Constraint::Length((wants as u16).clamp(available.min(MIN_WIDTH), available)),
            Constraint::Length(frame.area().height.min(wanted)),
        );
        // Worked out before the frame is drawn, because how many columns fit
        // is something the frame itself has to admit to.
        let width = area.width.saturating_sub(2) as usize;
        let name_width = self.name_width(width);
        let (cells, shown) = self.columns(width, name_width);
        let hidden = self.days as usize - shown;

        let mut block = Block::bordered()
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
        // A grid missing columns says how many, rather than letting the totals
        // look like the sum of what is on screen.
        if hidden > 0 {
            block = block.title_bottom(Line::from(vec![
                Span::raw(" "),
                Span::styled(
                    format!("{hidden} earlier day{} not drawn", plural_s(hidden)),
                    theme::placeholder(),
                ),
                Span::raw(" "),
            ]));
        }
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
        let body = Rect {
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
                        " `<` and `>` walk it back and forward; `jired pull` refreshes it",
                        theme::dim(),
                    )),
                ]),
                body,
            );
            frame.render_widget(
                Paragraph::new(form::key_hints(&[
                    ("<>", "window"),
                    ("±", "days"),
                    ("q", "quit"),
                ]))
                .centered(),
                hints,
            );
            return;
        }

        // The last columns, not the first: what happened this week is what a
        // window too wide to draw in full is still being asked about.
        let from = self.report.dates.len().saturating_sub(shown);
        let layout = Layout {
            name_width,
            cells,
            from,
            peak: self.report.peak(),
        };

        frame.render_widget(
            Paragraph::new(self.head_line(&layout)),
            Rect { height: 1, ..body },
        );

        let list = Rect {
            y: body.y + 1,
            // The totals row sits below the list rather than in it, so it is
            // still there after scrolling past the last person.
            height: body.height.saturating_sub(2),
            ..body
        };
        self.scroll_to_fit(list.height as usize);
        for (index, row) in rows
            .iter()
            .enumerate()
            .skip(self.scroll)
            .take(list.height as usize)
        {
            let area = Rect {
                y: list.y + (index - self.scroll) as u16,
                height: 1,
                ..list
            };
            let selected = index == self.selected;
            let line = match row {
                Row::Person(p) => self.person_line(&self.report.people[*p], &layout, selected),
                Row::Issue(p, i) => {
                    self.issue_line(&self.report.people[*p].issues[*i], &layout, selected)
                }
            };
            frame.render_widget(Paragraph::new(line), area);
        }

        if body.height >= 2 {
            frame.render_widget(
                Paragraph::new(self.totals_line(&layout)),
                Rect {
                    y: body.y + body.height - 1,
                    height: 1,
                    ..body
                },
            );
        }

        frame.render_widget(
            Paragraph::new(form::key_hints(&self.keys())).centered(),
            hints,
        );

        // Over the whole terminal rather than over the card: the card is only
        // as tall as the report, and a key card cut off at the fifth key is
        // worse than none.
        if self.help {
            form::render_help(frame, frame.area(), Self::HELP);
        }
    }
}

fn plural_s(n: usize) -> &'static str {
    if n == 1 {
        ""
    } else {
        "s"
    }
}

/// What every row of one frame agrees about: where the columns are and how
/// they are drawn.
struct Layout {
    name_width: usize,
    cells: Cells,
    /// First column drawn, when the window is wider than the terminal.
    from: usize,
    /// The busiest cell, which the heat shading is scaled against.
    peak: u64,
}

impl ReportScreen<'_> {
    fn head_line(&self, layout: &Layout) -> Line<'static> {
        let mut spans = vec![Span::styled(
            format!("  {}", pad("", layout.name_width)),
            theme::dim(),
        )];
        for date in self.report.dates.iter().skip(layout.from) {
            spans.push(Span::styled(
                self.head_cell(date, layout.cells),
                match is_weekend(date) {
                    // A weekend column is still a column; it is just not one
                    // most people are expected to have filled in.
                    true => theme::placeholder(),
                    false => theme::label(),
                },
            ));
        }
        spans.push(Span::styled(format!("{:>TOTAL$}", "total"), theme::label()));
        Line::from(spans)
    }

    fn head_cell(&self, date: &NaiveDate, cells: Cells) -> String {
        match cells {
            Cells::Numeric => format!("{:>CELL$}", column_head(date)),
            // One letter is all a heat column has room for; the range in the
            // title is what says which days these are.
            Cells::Heat => format!("{:>HEAT_CELL$}", &column_head(date)[..1]),
            Cells::None => String::new(),
        }
    }

    fn person_line(&self, person: &Person, layout: &Layout, selected: bool) -> Line<'static> {
        let style = match selected {
            true => theme::selected(),
            false => Style::default(),
        };
        let marker = match (person.issues.is_empty(), self.is_open(person)) {
            (true, _) => "  ",
            (false, false) => "▸ ",
            (false, true) => "▾ ",
        };
        let mut spans = vec![
            Span::styled(marker, if selected { style } else { theme::key() }),
            Span::styled(pad(&person.name, layout.name_width), style),
        ];
        self.push_cells(&mut spans, &person.by_day, layout, style, selected);
        spans.push(Span::styled(
            format!("{:>TOTAL$}", format_hm(person.seconds)),
            style,
        ));
        Line::from(spans)
    }

    fn issue_line(
        &self,
        issue: &crate::report::IssueTime,
        layout: &Layout,
        selected: bool,
    ) -> Line<'static> {
        let style = match selected {
            true => theme::selected(),
            false => Style::default(),
        };
        let dim = if selected { style } else { theme::dim() };
        let mut spans = vec![
            Span::styled("    ", style),
            Span::styled(
                pad(
                    &format!("{} {}", issue.key, issue.summary),
                    layout.name_width.saturating_sub(2),
                ),
                dim,
            ),
        ];
        self.push_cells(&mut spans, &issue.by_day, layout, dim, selected);
        spans.push(Span::styled(
            format!("{:>TOTAL$}", format_hm(issue.seconds)),
            dim,
        ));
        Line::from(spans)
    }

    fn totals_line(&self, layout: &Layout) -> Line<'static> {
        let mut spans = vec![Span::styled(
            format!("  {}", pad("all", layout.name_width)),
            theme::label(),
        )];
        self.push_cells(
            &mut spans,
            &self.report.by_day,
            layout,
            theme::label(),
            false,
        );
        spans.push(Span::styled(
            format!("{:>TOTAL$}", format_hm(self.report.seconds)),
            theme::label(),
        ));
        Line::from(spans)
    }

    /// The day columns of one row, drawn the way the width allows.
    fn push_cells(
        &self,
        spans: &mut Vec<Span<'static>>,
        by_day: &[u64],
        layout: &Layout,
        style: Style,
        selected: bool,
    ) {
        for (day, seconds) in by_day.iter().enumerate().skip(layout.from) {
            let empty = *seconds == 0;
            let text = match layout.cells {
                Cells::Numeric => format!("{:>CELL$}", format_cell(*seconds)),
                Cells::Heat => format!("{:>HEAT_CELL$}", heat(*seconds, layout.peak)),
                Cells::None => break,
            };
            // An empty day is a dot, and a dot should not shout; the weekend it
            // may fall on is dimmer still.
            let style = match (selected, empty, self.report.dates.get(day)) {
                (true, _, _) => style,
                (false, true, _) => theme::placeholder(),
                (false, false, Some(date)) if is_weekend(date) => theme::dim(),
                _ => style,
            };
            spans.push(Span::styled(text, style));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doc::{IssueDoc, WorklogEntry, WorklogId};
    use crate::tui::render_lines;

    fn issue(key: &str, summary: &str, worklogs: Vec<WorklogEntry>) -> IssueDoc {
        let mut doc = IssueDoc::parse(&format!(
            "---\nkey: {key}\nid: '1'\nsummary: {summary}\njired: {{}}\n---\n\n# {summary}\n"
        ))
        .expect("a parseable issue");
        doc.worklogs = worklogs;
        doc
    }

    fn booked(author: &str, time: &str, started: &str) -> WorklogEntry {
        WorklogEntry {
            id: WorklogId::Existing(format!("{author}-{started}")),
            time_spent: time.into(),
            author: author.into(),
            started: started.into(),
            body: String::new(),
        }
    }

    fn now() -> chrono::DateTime<chrono::Utc> {
        crate::api::model::parse_timestamp("2026-08-22T10:00:00Z").unwrap()
    }

    fn issues() -> Vec<IssueDoc> {
        vec![
            issue(
                "PROJ-142",
                "Fix login flow",
                vec![
                    booked("Jane D", "2h 30m", "2026-08-20T09:00:00Z"),
                    booked("Ilya A", "1h", "2026-08-21T08:00:00Z"),
                    // A fortnight back, for the window to be walked onto.
                    booked("Old Hand", "8h", "2026-08-10T09:00:00Z"),
                ],
            ),
            issue(
                "PROJ-150",
                "Rotate the certificate",
                vec![booked("Ilya A", "4h", "2026-08-22T08:00:00Z")],
            ),
        ]
    }

    fn source(issues: &[IssueDoc]) -> Source<'_> {
        Source {
            issues,
            me: "Ilya A".into(),
            lookup: crate::report::Lookup::default(),
            rollup: false,
            now: now(),
            project: Some("PROJ".into()),
            last_fetch: None,
            last_fetch_age_seconds: None,
        }
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn typed(screen: &mut ReportScreen, keys: &str) {
        for c in keys.chars() {
            screen.handle(key(KeyCode::Char(c)));
        }
    }

    #[test]
    fn the_grid_is_people_down_the_side_and_days_across_the_top() {
        let issues = issues();
        let source = source(&issues);
        let mut screen = ReportScreen::new(&source, 7, false);
        let lines = render_lines(&mut screen, 100, 12).join("\n");

        assert!(lines.contains("PROJ · last 7 days"), "{lines}");
        // A column heading each, weekday and day of the month.
        assert!(lines.contains("Su16"), "{lines}");
        assert!(lines.contains("Sa22"), "{lines}");
        // A row per person, the figure under the day it was booked on.
        assert!(lines.contains("Ilya A"), "{lines}");
        assert!(lines.contains("2h30"), "{lines}");
        assert!(lines.contains("5h"), "{lines}");
        // Column totals, always in view.
        assert!(lines.contains("all"), "{lines}");
        assert!(lines.contains("7h 30m"), "{lines}");
    }

    #[test]
    fn i_opens_the_issue_level_and_shuts_it_again() {
        let issues = issues();
        let source = source(&issues);
        let mut screen = ReportScreen::new(&source, 7, false);
        assert!(!render_lines(&mut screen, 100, 14)
            .join("\n")
            .contains("PROJ-150"));

        typed(&mut screen, "i");
        let open = render_lines(&mut screen, 100, 14).join("\n");
        assert!(open.contains("PROJ-150 Rotate"), "{open}");
        assert!(open.contains("PROJ-142 Fix login"), "{open}");

        typed(&mut screen, "i");
        assert!(!render_lines(&mut screen, 100, 14)
            .join("\n")
            .contains("PROJ-150"));
    }

    /// `--show-issues` is the same state, decided before the screen opens.
    #[test]
    fn the_flag_opens_the_issue_level_from_the_start() {
        let issues = issues();
        let source = source(&issues);
        let mut screen = ReportScreen::new(&source, 7, true);
        let lines = render_lines(&mut screen, 100, 14).join("\n");
        assert!(lines.contains("PROJ-150 Rotate"), "{lines}");
    }

    #[test]
    fn plus_and_minus_change_how_many_days_are_shown() {
        let issues = issues();
        let source = source(&issues);
        let mut screen = ReportScreen::new(&source, 7, false);

        typed(&mut screen, "+++");
        assert_eq!(screen.days, 10);
        assert_eq!(screen.report.dates.len(), 10);
        assert_eq!(screen.report.span(), "2026-08-13 → 2026-08-22");

        typed(&mut screen, "---------");
        assert_eq!(screen.days, 1, "a window cannot shrink past one day");
        assert_eq!(screen.report.span(), "2026-08-22");
        let lines = render_lines(&mut screen, 100, 10).join("\n");
        assert!(lines.contains("today"), "{lines}");
    }

    /// The window walks a whole width at a time, which is what "the week
    /// before" means, and `t` comes back.
    #[test]
    fn the_window_walks_backwards_and_forwards() {
        let issues = issues();
        let source = source(&issues);
        let mut screen = ReportScreen::new(&source, 7, false);
        assert!(screen.report.people.iter().any(|p| p.name == "Jane D"));

        typed(&mut screen, "<");
        assert_eq!(screen.back, 7);
        assert_eq!(screen.report.span(), "2026-08-09 → 2026-08-15");
        let earlier = render_lines(&mut screen, 100, 10).join("\n");
        assert!(earlier.contains("Old Hand"), "{earlier}");
        assert!(!earlier.contains("Jane D"), "{earlier}");

        typed(&mut screen, ">");
        assert_eq!(screen.back, 0);
        assert!(screen.report.people.iter().any(|p| p.name == "Jane D"));

        // Forward from today stays on today; there is nothing after it.
        typed(&mut screen, ">>");
        assert_eq!(screen.back, 0);

        typed(&mut screen, "<<t");
        assert_eq!(screen.back, 0, "t comes back to today");
    }

    /// A window too wide for a figure per day still says something.
    #[test]
    fn a_wide_window_falls_back_to_a_heat_map() {
        let issues = issues();
        let source = source(&issues);
        let mut screen = ReportScreen::new(&source, 30, false);
        let lines = render_lines(&mut screen, 100, 12).join("\n");
        assert!(lines.contains("last 30 days"), "{lines}");
        assert!(lines.contains('█'), "the busiest day is full:\n{lines}");
        assert!(!lines.contains("2h30"), "no room for figures:\n{lines}");
        // The totals are still figures, whatever the columns do.
        assert!(lines.contains("8h"), "{lines}");
    }

    /// Columns that cannot be drawn are counted out loud, never dropped in
    /// silence: the totals cover days the grid does not show.
    #[test]
    fn a_window_wider_than_the_terminal_says_what_it_left_out() {
        let issues = issues();
        let source = source(&issues);
        let mut screen = ReportScreen::new(&source, 200, false);
        let lines = render_lines(&mut screen, 80, 12).join("\n");
        assert!(lines.contains("earlier days not drawn"), "{lines}");
        // What is not drawn is still counted: the totals cover the whole window.
        assert!(lines.contains("15h 30m"), "{lines}");
    }

    #[test]
    fn a_person_opens_into_the_issues_they_booked_against() {
        let issues = issues();
        let source = source(&issues);
        let mut screen = ReportScreen::new(&source, 7, false);
        screen.handle(key(KeyCode::Right));
        let lines = render_lines(&mut screen, 100, 14).join("\n");
        assert!(lines.contains("PROJ-150"), "{lines}");

        // Biggest first, as everywhere else in the report.
        let rows: Vec<&str> = lines.lines().collect();
        let at_150 = rows.iter().position(|l| l.contains("PROJ-150")).unwrap();
        let at_142 = rows.iter().position(|l| l.contains("PROJ-142")).unwrap();
        assert!(at_150 < at_142, "{lines}");

        screen.handle(key(KeyCode::Left));
        assert!(!render_lines(&mut screen, 100, 14)
            .join("\n")
            .contains("PROJ-150"));
    }

    #[test]
    fn closing_from_inside_a_person_lands_back_on_them() {
        let issues = issues();
        let source = source(&issues);
        let mut screen = ReportScreen::new(&source, 7, false);
        screen.handle(key(KeyCode::Right));
        screen.handle(key(KeyCode::Down));
        assert_eq!(screen.rows()[screen.selected], Row::Issue(0, 0));

        screen.handle(key(KeyCode::Left));
        assert_eq!(screen.rows()[screen.selected], Row::Person(0));
    }

    /// The cursor is on a person, not on a row number: moving the window must
    /// not leave it on somebody else.
    #[test]
    fn the_cursor_stays_with_the_person_it_was_on() {
        let issues = issues();
        let source = source(&issues);
        let mut screen = ReportScreen::new(&source, 7, false);
        screen.handle(key(KeyCode::Down));
        let before = screen.report.people[1].name.clone();
        assert_eq!(before, "Jane D");

        // Widening keeps both people, so the cursor keeps its person.
        typed(&mut screen, "+");
        let Row::Person(p) = screen.rows()[screen.selected] else {
            panic!("the cursor left the person level");
        };
        assert_eq!(screen.report.people[p].name, before);

        // Walking onto a window they booked nothing in puts it back at the top.
        typed(&mut screen, "<");
        assert_eq!(screen.selected, 0);
    }

    #[test]
    fn the_key_card_lists_every_key_and_any_key_puts_it_away() {
        let issues = issues();
        let source = source(&issues);
        let mut screen = ReportScreen::new(&source, 7, false);
        typed(&mut screen, "?");
        let help = render_lines(&mut screen, 100, 16).join("\n");
        assert!(help.contains("Keys"), "{help}");
        assert!(help.contains("the window before this one"), "{help}");

        typed(&mut screen, "x");
        assert!(!render_lines(&mut screen, 100, 16)
            .join("\n")
            .contains("the window before this one"));
    }

    #[test]
    fn q_and_escape_close_the_report() {
        let issues = issues();
        let source = source(&issues);
        let mut screen = ReportScreen::new(&source, 7, false);
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
    fn an_empty_report_says_so_rather_than_drawing_an_empty_grid() {
        let issues = vec![issue("PROJ-142", "Fix login flow", vec![])];
        let source = source(&issues);
        let mut screen = ReportScreen::new(&source, 7, false);
        let lines = render_lines(&mut screen, 100, 12).join("\n");
        assert!(lines.contains("nobody booked any time"), "{lines}");
        assert!(lines.contains("jired pull"), "{lines}");
    }

    /// Print the report for looking at:
    /// `cargo test --lib tui::report::tests::preview -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn preview() {
        let issues = issues();
        let source = source(&issues);
        let mut screen = ReportScreen::new(&source, 7, false);
        typed(&mut screen, "i");
        for l in render_lines(&mut screen, 100, 16) {
            println!("|{l}");
        }
    }

    #[test]
    fn a_small_terminal_renders_without_panicking() {
        for (w, h) in [(20u16, 4u16), (40, 8), (80, 24), (200, 60)] {
            let issues = issues();
            let source = source(&issues);
            let mut screen = ReportScreen::new(&source, 7, true);
            typed(&mut screen, "+++++++++<>i?");
            render_lines(&mut screen, w, h);
        }
    }
}
