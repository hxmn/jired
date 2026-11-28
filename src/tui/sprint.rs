//! The new-sprint screen.
//!
//! Unlike the rest of the working copy, a sprint has no file to stage it in:
//! `jired sprint create` is a write that goes straight to the server. So this
//! form is filled in from what the server just said — the project's boards and
//! the sprints already on them — and everything it produces is checked here,
//! with the cursor still on the field that caused the complaint, rather than
//! after the screen has closed and taken the half-filled form with it.
//!
//! The name is prefilled with the one the board is due: a board whose last
//! sprint is `Sprint 42` gets `Sprint 43` offered, and picking a different
//! board renumbers it — until someone types over it, after which it is theirs.

use crate::tui::controller::{FormController, Step};
use crate::tui::field::{Field, Value};
use crate::tui::form::{self, Card};
use crate::tui::picker::Item;
use crate::tui::when::span;
use crate::tui::{theme, Flow, Screen};
use chrono::{DateTime, Datelike, Duration, Local, Utc};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::text::{Line, Span};
use ratatui::Frame;

/// A board the sprint can be created on.
#[derive(Debug, Clone, Default)]
pub struct KnownBoard {
    pub id: String,
    pub name: String,
    /// `scrum` or `kanban`; a kanban board has no sprints to speak of.
    pub board_type: String,
}

/// A sprint one of those boards already has.
///
/// What the next name is derived from, and what says a name is taken.
#[derive(Debug, Clone, Default)]
pub struct BoardSprint {
    pub board_id: String,
    pub name: String,
    pub state: crate::api::SprintState,
}

/// What the command line already supplied; the form starts from these.
#[derive(Debug, Clone, Default)]
pub struct Seed {
    pub name: Option<String>,
    /// A board *name*: the command resolves whatever `--board` said — an id or
    /// a name — before the screen opens, so the form only ever shows names.
    pub board: Option<String>,
    /// Already normalized to a timestamp that names its zone.
    pub start: Option<String>,
    pub end: Option<String>,
    pub goal: Option<String>,
}

/// A finished form: exactly what `sprint create` needs to make the call.
#[derive(Debug, Clone, PartialEq)]
pub struct NewSprint {
    pub name: String,
    pub board: String,
    pub start: Option<String>,
    pub end: Option<String>,
    pub goal: String,
}

// Field order is the tab order, and the order they are drawn in.
const NAME: usize = 0;
const BOARD: usize = 1;
const START: usize = 2;
const END: usize = 3;
const GOAL: usize = 4;

pub struct SprintScreen {
    form: FormController,
    boards: Vec<KnownBoard>,
    sprints: Vec<BoardSprint>,
    /// Right-aligned in the border: the workspace's project.
    project: String,
    /// The name the form put in the Name field, so a board change can renumber
    /// it and typing over it can stop that happening.
    offered_name: String,
    /// When the screen opened, which is what the date presets count from.
    now: DateTime<Utc>,
}

impl SprintScreen {
    pub fn new(
        project: &str,
        boards: &[KnownBoard],
        sprints: &[BoardSprint],
        now: DateTime<Utc>,
        seed: &Seed,
    ) -> Self {
        // A project with no boards cannot be corrected by a list, so that case
        // gets a field the board can simply be typed into.
        let board = if boards.is_empty() {
            Field::line(BOARD_LABEL, "the board the sprint belongs to").required()
        } else {
            Field::choice(
                BOARD_LABEL,
                boards.iter().map(|b| b.name.clone()).collect(),
                false,
            )
        };

        let mut fields = vec![
            Field::line(NAME_LABEL, "what the team will call it").required(),
            board.with_hint("the sprint is created on this board"),
            Field::line(START_LABEL, "not set")
                .with_items(start_presets(now))
                .with_hint("a date, or a date and a time — read in your own timezone"),
            Field::line(END_LABEL, "not set")
                .with_hint("when the sprint closes; Jira wants both dates or neither"),
            Field::body(GOAL_LABEL, "what the sprint is for", 3),
        ];

        fields[NAME].set(seed.name.as_deref());
        fields[START].set(seed.start.as_deref());
        fields[END].set(seed.end.as_deref());
        fields[GOAL].set(seed.goal.as_deref());
        // A choice only takes a seed when there is one: setting `None` would
        // clear the first board, which is the sensible default it starts on.
        if let Some(b) = &seed.board {
            fields[BOARD].set(Some(b));
        }

        let mut form = FormController::new(fields, Some(GOAL));
        // Start on the first thing still missing: with the name already on the
        // command line, the board is what is being asked for.
        form.focus = if seed.name.is_some() { BOARD } else { NAME };
        let mut screen = Self {
            form,
            boards: boards.to_vec(),
            sprints: sprints.to_vec(),
            project: project.to_string(),
            offered_name: String::new(),
            now,
        };
        // Prefilled rather than left empty, so the name that will be created is
        // on screen: for the usual sprint after the last one it is also right,
        // and a value nobody had to type is still a value worth correcting.
        if seed.name.is_none() {
            screen.offer_name();
        }
        screen
    }

    fn new_sprint(&self) -> NewSprint {
        NewSprint {
            name: self.form.fields[NAME].text().trim().to_string(),
            board: self.form.fields[BOARD].text().trim().to_string(),
            start: self.form.fields[START].filled().map(str::to_string),
            end: self.form.fields[END].filled().map(str::to_string),
            goal: self.form.fields[GOAL].text().trim().to_string(),
        }
    }

    /// The board the form is pointing at, when it names one this project has.
    fn board(&self) -> Option<&KnownBoard> {
        let name = self.form.fields[BOARD].filled()?;
        self.boards.iter().find(|b| b.name == name)
    }

    /// Every sprint already on the board the form is pointing at.
    fn on_board(&self) -> Vec<&BoardSprint> {
        let Some(board) = self.board() else {
            return Vec::new();
        };
        self.sprints
            .iter()
            .filter(|s| s.board_id == board.id)
            .collect()
    }

    /// Put the name this board is due in the Name field, while it is still the
    /// one the form offered rather than one somebody typed.
    fn offer_name(&mut self) {
        if self.form.fields[NAME].text() != self.offered_name {
            return;
        }
        let names: Vec<&str> = self.on_board().iter().map(|s| s.name.as_str()).collect();
        let offered = next_name(&names).unwrap_or_default();
        self.form.fields[NAME].set(Some(&offered));
        self.offered_name = offered;
    }

    /// Refuse to finish while something is missing or unreadable, and put the
    /// cursor on the field that has to be fixed.
    ///
    /// Every complaint here is one the server would otherwise make after the
    /// screen had already closed.
    fn submit(&mut self) -> Flow<NewSprint> {
        for (index, what) in [(NAME, "a name"), (BOARD, "a board")] {
            if self.form.fields[index].filled().is_none() {
                return self.refuse(index, format!("{what} is required"));
            }
        }

        // A date with no zone means the reader's own — the one every other
        // screen prints — and only that case is rewritten, with the offset
        // spelled out so what goes to the server names one instant rather than
        // one per machine.
        for index in [START, END] {
            let Some(typed) = self.form.fields[index].filled().map(str::to_string) else {
                continue;
            };
            match crate::api::model::parse_local_timestamp(&typed) {
                None => return self.refuse(index, format!("`{typed}` is not a date")),
                Some(t) if !crate::api::model::names_a_zone(&typed) => {
                    let spelled_out = crate::api::model::format_local_iso(&t);
                    self.form.fields[index].set(Some(&spelled_out));
                }
                Some(_) => {}
            }
        }

        match (self.at(START), self.at(END)) {
            // Jira takes both dates or neither: an end on its own is silently
            // dropped, which is worse than being asked for the other half.
            (None, Some(_)) => {
                return self.refuse(START, "an end date needs a start date".to_string())
            }
            (Some(start), Some(end)) if end <= start => {
                return self.refuse(END, "the sprint ends before it starts".to_string())
            }
            _ => {}
        }

        Flow::Done(self.new_sprint())
    }

    /// A date field as an instant, when it holds one.
    fn at(&self, index: usize) -> Option<DateTime<Utc>> {
        crate::api::model::parse_local_timestamp(self.form.fields[index].filled()?)
    }

    fn refuse(&mut self, field: usize, complaint: String) -> Flow<NewSprint> {
        self.form.focus = field;
        self.form.error = Some(complaint);
        Flow::Continue
    }

    /// Whether anything has been typed that would be lost by cancelling.
    ///
    /// The offered name does not count: nobody typed it.
    fn dirty(&self) -> bool {
        self.form.fields[NAME].text() != self.offered_name
            || [START, END, GOAL]
                .iter()
                .any(|&i| self.form.fields[i].filled().is_some())
    }

    fn dispatch(&mut self, key: KeyEvent) -> Flow<NewSprint> {
        // The end dates worth offering depend on the start, which may have
        // been typed since the screen opened, so they are built as the list
        // is asked for.
        let asks_for_list = key.code == KeyCode::F(2)
            || (key.code == KeyCode::Char('l') && key.modifiers.contains(KeyModifiers::CONTROL));
        if asks_for_list && self.form.overlay.is_none() && self.form.focus == END {
            let from = self.at(START).unwrap_or(self.now);
            self.form.fields[END].items = end_presets(from);
        }
        let dirty = self.dirty();
        match self.form.handle(key, dirty) {
            Step::Continue => Flow::Continue,
            Step::Cancel => Flow::Cancel,
            Step::Submit => self.submit(),
        }
    }
}

impl Screen for SprintScreen {
    type Output = NewSprint;

    fn handle(&mut self, key: KeyEvent) -> Flow<NewSprint> {
        let flow = self.dispatch(key);
        // A board change renumbers the offered name, so the name on screen is
        // always the one that board is due rather than the first board's.
        if matches!(flow, Flow::Continue) {
            self.offer_name();
        }
        flow
    }

    fn render(&mut self, frame: &mut Frame) {
        // Each field says what it points at, which is the check the user
        // actually wants: the right board, and the dates they meant.
        self.form.fields[NAME].detail = self.name_detail();
        self.form.fields[BOARD].detail = self.board_detail();
        self.form.fields[START].detail = self.date_detail(START);
        self.form.fields[END].detail = self.date_detail(END);

        let card = Card {
            title: "New sprint",
            subtitle: self.project.clone(),
            gaps: &[START, GOAL],
            body: Some(GOAL),
            status: self.status_line(),
            keys: self.key_hints(),
        };
        let focus = self.form.visible_focus();
        let area = form::render(
            frame,
            &card,
            &mut self.form.fields,
            focus,
            &mut self.form.body_scroll,
        );
        self.form
            .render_overlay(frame, area, HELP, "Discard this sprint?");
    }
}

const HELP: &[(&str, &str)] = &[
    ("tab / shift-tab", "move between fields"),
    ("↑ ↓", "move between fields"),
    ("← →", "change a choice"),
    ("^l or f2", "open the field as a list"),
    ("⏎", "create the sprint"),
    ("^s", "create from the goal"),
    ("^a ^e ^w ^u ^k", "edit the line like a shell does"),
    ("esc", "cancel"),
];

const NAME_LABEL: &str = "Name";
const BOARD_LABEL: &str = "Board";
const START_LABEL: &str = "Start";
const END_LABEL: &str = "End";
const GOAL_LABEL: &str = "Goal";

impl SprintScreen {
    /// Whether the board already has a sprint by this name.
    ///
    /// Not refused — Jira allows it — but worth saying, because jired resolves
    /// an issue's `sprint:` by name and two sprints called the same thing make
    /// that ambiguous.
    fn name_taken(&self) -> bool {
        let Some(name) = self.form.fields[NAME].filled() else {
            return false;
        };
        self.on_board()
            .iter()
            .any(|s| s.name.eq_ignore_ascii_case(name))
    }

    fn name_detail(&self) -> Option<String> {
        self.name_taken()
            .then(|| "this board already has one by that name".to_string())
    }

    /// The board's id and kind, which is what tells two boards with similar
    /// names apart — and says when a sprint is being made on a kanban board.
    fn board_detail(&self) -> Option<String> {
        let board = self.board()?;
        let count = self.on_board().len();
        let mut detail = format!("board {}", board.id);
        if !board.board_type.is_empty() {
            detail.push_str(&format!("  ·  {}", board.board_type));
        }
        if count > 0 {
            detail.push_str(&format!("  ·  {count} sprints"));
        }
        Some(detail)
    }

    /// A date in the words that let it be checked: nobody reads `2026-09-07`
    /// for whether it is a Monday.
    fn date_detail(&self, index: usize) -> Option<String> {
        let typed = self.form.fields[index].filled()?;
        let Some(when) = crate::api::model::parse_local_timestamp(typed) else {
            return Some("not a date".to_string());
        };
        Some(
            when.with_timezone(&Local)
                .format("%A, %-d %B %Y")
                .to_string(),
        )
    }

    /// Whichever is most useful right now: a complaint, then what the form
    /// would create, then help for the field under the cursor.
    fn status_line(&self) -> Line<'static> {
        if let Some(error) = &self.form.error {
            return Line::from(Span::styled(format!("✗ {error}"), theme::error()));
        }
        if let (Some(name), Some(board)) = (
            self.form.fields[NAME].filled(),
            self.form.fields[BOARD].filled(),
        ) {
            let mut line = vec![
                Span::styled("creates ", theme::dim()),
                Span::styled(name.to_string(), theme::value()),
                Span::styled(" on ", theme::dim()),
                Span::styled(board.to_string(), theme::value()),
            ];
            if let (Some(start), Some(end)) = (self.at(START), self.at(END)) {
                if end > start {
                    line.push(Span::styled(" for ", theme::dim()));
                    line.push(Span::styled(span(start, end), theme::value()));
                }
            }
            return Line::from(line);
        }
        Line::from(Span::styled(
            self.form.fields[self.form.focus].hint.clone(),
            theme::dim(),
        ))
    }

    fn key_hints(&self) -> Vec<(&'static str, &'static str)> {
        let mut keys = vec![("tab", "field")];
        if self.form.focus == GOAL {
            keys.push(("^s", "create"));
        } else {
            keys.push(("⏎", "create"));
        }
        // The end dates are built when the list opens, so this field offers one
        // even before it has any items to show.
        if self.form.fields[self.form.focus].openable() || self.form.focus == END {
            keys.push(("^l", "list"));
        } else if matches!(self.form.fields[self.form.focus].value, Value::Choice(_)) {
            keys.push(("←→", "change"));
        }
        keys.push(("esc", "cancel"));
        keys.push(("f1", "keys"));
        keys
    }
}

/// The name a board's next sprint is due: the highest number it has already
/// used, plus one, in the same words and the same width.
///
/// `Sprint 42` becomes `Sprint 43` and `Sprint 09` becomes `Sprint 10`; a board
/// whose sprints are not numbered gets nothing, because guessing at a naming
/// scheme is worse than an empty field.
fn next_name(existing: &[&str]) -> Option<String> {
    let numbered = existing.iter().filter_map(|name| {
        let digits = name.len() - name.trim_end_matches(|c: char| c.is_ascii_digit()).len();
        if digits == 0 {
            return None;
        }
        let (prefix, number) = name.split_at(name.len() - digits);
        Some((number.parse::<u64>().ok()?, prefix.to_string(), digits))
    });
    let (highest, prefix, width) = numbered.max_by_key(|(n, _, _)| *n)?;
    // Padding is part of the name a team chose: `Sprint 09` is followed by
    // `Sprint 10`, not `Sprint 010`.
    Some(format!("{prefix}{:0width$}", highest + 1))
}

/// The start dates worth one keystroke, counted from when the screen opened.
fn start_presets(now: DateTime<Utc>) -> Vec<Item> {
    let mut out = vec![Item::new(crate::api::model::format_local_iso(&now)).with_detail("now")];
    for (days, what) in [(1, "tomorrow"), (7, "in a week")] {
        out.push(preset(at_nine(now + Duration::days(days)), what));
    }
    // The Monday after this one, which is when a sprint usually starts.
    let local = now.with_timezone(&Local);
    let ahead = 7 - local.weekday().num_days_from_monday() as i64;
    out.push(preset(at_nine(now + Duration::days(ahead)), "next Monday"));
    out
}

/// The end dates worth one keystroke, counted from whatever the start says.
fn end_presets(start: DateTime<Utc>) -> Vec<Item> {
    [1, 2, 3, 4]
        .into_iter()
        .map(|weeks| {
            let end = start + Duration::weeks(weeks);
            preset(end, span(start, end))
        })
        .collect()
}

fn preset(when: DateTime<Utc>, what: impl Into<String>) -> Item {
    Item::new(crate::api::model::format_local_iso(&when)).with_detail(what)
}

/// Nine in the morning, local time, on the day `when` falls on.
///
/// A sprint starts when the working day does, not at whatever minute past the
/// hour the form happened to be opened.
fn at_nine(when: DateTime<Utc>) -> DateTime<Utc> {
    let day = when.with_timezone(&Local).date_naive();
    day.and_hms_opt(9, 0, 0)
        .and_then(|naive| crate::api::model::parse_local_timestamp(&naive.to_string()))
        .unwrap_or(when)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::controller::keys::{ctrl, key, typed};
    #[allow(unused_imports)]
    use crate::tui::controller::Overlay;
    use crate::tui::render_lines;

    fn now() -> DateTime<Utc> {
        // A Friday, so "next Monday" is three days away.
        crate::api::model::parse_timestamp("2026-08-21T13:00:00Z").expect("a fixed clock")
    }

    /// What the form writes for an instant, in whatever zone the test machine
    /// keeps. Spelling the expected value as a literal would only assert that
    /// the machine running the suite is on UTC.
    fn shown(dt: DateTime<Utc>) -> String {
        crate::api::model::format_local_iso(&dt)
    }

    fn boards() -> Vec<KnownBoard> {
        vec![
            KnownBoard {
                id: "12".into(),
                name: "Platform Board".into(),
                board_type: "scrum".into(),
            },
            KnownBoard {
                id: "19".into(),
                name: "Support Board".into(),
                board_type: "scrum".into(),
            },
        ]
    }

    fn sprints() -> Vec<BoardSprint> {
        vec![
            BoardSprint {
                board_id: "12".into(),
                name: "Sprint 41".into(),
                state: "closed".into(),
            },
            BoardSprint {
                board_id: "12".into(),
                name: "Sprint 42".into(),
                state: "active".into(),
            },
            BoardSprint {
                board_id: "19".into(),
                name: "Support 7".into(),
                state: "active".into(),
            },
        ]
    }

    fn screen() -> SprintScreen {
        SprintScreen::new("PROJ", &boards(), &sprints(), now(), &Seed::default())
    }

    fn done(flow: Flow<NewSprint>) -> NewSprint {
        match flow {
            Flow::Done(sprint) => sprint,
            _ => panic!("the screen did not finish"),
        }
    }

    /// The usual sprint is the one after the last, so that is what is on screen
    /// before anybody types anything.
    #[test]
    fn the_form_opens_on_the_name_the_board_is_due() {
        let screen = screen();
        assert_eq!(screen.form.focus, NAME);
        assert_eq!(screen.form.fields[NAME].text(), "Sprint 43");
        assert_eq!(screen.form.fields[BOARD].text(), "Platform Board");
    }

    /// Which board it is decides what the next sprint is called, so changing
    /// the board renumbers the offer rather than leaving the first board's.
    #[test]
    fn changing_the_board_renumbers_the_offered_name() {
        let mut screen = screen();
        screen.form.focus = BOARD;
        screen.handle(key(KeyCode::Right));
        assert_eq!(screen.form.fields[BOARD].text(), "Support Board");
        assert_eq!(screen.form.fields[NAME].text(), "Support 8");

        // Once it has been typed over it is the user's, not the form's.
        screen.form.focus = NAME;
        screen.form.fields[NAME].set(Some("Hardening week"));
        screen.form.focus = BOARD;
        screen.handle(key(KeyCode::Left));
        assert_eq!(screen.form.fields[NAME].text(), "Hardening week");
    }

    #[test]
    fn a_whole_sprint_can_be_typed_from_an_empty_form() {
        let mut screen = screen();
        screen.form.fields[NAME].set(Some("Sprint 43"));
        screen.form.fields[START].set(Some("2026-09-01T09:00:00Z"));
        screen.form.fields[END].set(Some("2026-09-15T09:00:00Z"));
        screen.form.focus = GOAL;
        typed(&mut screen, "Ship SSO.");

        // The goal keeps Enter for itself, so ^s is the way out from it.
        assert!(matches!(screen.handle(key(KeyCode::Enter)), Flow::Continue));
        let sprint = done(screen.handle(ctrl('s')));
        assert_eq!(sprint.name, "Sprint 43");
        assert_eq!(sprint.board, "Platform Board");
        assert_eq!(sprint.start.as_deref(), Some("2026-09-01T09:00:00Z"));
        assert_eq!(sprint.end.as_deref(), Some("2026-09-15T09:00:00Z"));
        assert_eq!(sprint.goal, "Ship SSO.");
    }

    /// With the name already named on the command line, the board is what is
    /// being asked for and the first keystroke should land there.
    #[test]
    fn naming_the_sprint_up_front_puts_the_cursor_on_the_board() {
        let seed = Seed {
            name: Some("Hardening week".into()),
            ..Seed::default()
        };
        let mut screen = SprintScreen::new("PROJ", &boards(), &sprints(), now(), &seed);
        assert_eq!(screen.form.focus, BOARD);
        assert_eq!(screen.form.fields[NAME].text(), "Hardening week");

        let sprint = done(screen.handle(key(KeyCode::Enter)));
        assert_eq!(sprint.name, "Hardening week");
        assert_eq!(sprint.start, None);
        assert_eq!(sprint.end, None);
    }

    /// Every complaint the server could make after the screen closed is made
    /// here instead, with the cursor on the field that caused it.
    #[test]
    fn a_bad_value_is_refused_with_the_cursor_on_the_field_that_caused_it() {
        let mut screen = screen();

        screen.form.fields[NAME].set(None);
        assert!(matches!(screen.handle(key(KeyCode::Enter)), Flow::Continue));
        assert_eq!(screen.form.focus, NAME);
        assert_eq!(screen.form.error.as_deref(), Some("a name is required"));

        screen.form.fields[NAME].set(Some("Sprint 43"));
        screen.form.fields[END].set(Some("tuesday"));
        screen.handle(key(KeyCode::Enter));
        assert_eq!(screen.form.focus, END);
        assert_eq!(
            screen.form.error.as_deref(),
            Some("`tuesday` is not a date")
        );

        // Jira takes both dates or neither, so an end on its own is asked about
        // rather than sent and silently dropped.
        screen.form.fields[END].set(Some("2026-09-15"));
        screen.handle(key(KeyCode::Enter));
        assert_eq!(screen.form.focus, START);
        assert_eq!(
            screen.form.error.as_deref(),
            Some("an end date needs a start date")
        );

        screen.form.fields[START].set(Some("2026-09-22"));
        screen.handle(key(KeyCode::Enter));
        assert_eq!(screen.form.focus, END);
        assert_eq!(
            screen.form.error.as_deref(),
            Some("the sprint ends before it starts")
        );

        screen.form.fields[START].set(Some("2026-09-01"));
        let sprint = done(screen.handle(key(KeyCode::Enter)));
        // A date with no zone is read in the reader's own and written back with
        // the offset spelled out, so what goes to Jira names one instant.
        assert_eq!(
            sprint.start.as_deref(),
            Some(shown(crate::api::model::parse_local_timestamp("2026-09-01").unwrap()).as_str())
        );

        // Typing clears the complaint rather than leaving it up to argue with.
        screen.form.error = Some("stale".into());
        screen.form.focus = NAME;
        typed(&mut screen, "x");
        assert_eq!(screen.form.error, None);
    }

    /// Two sprints with the same name make an issue's `sprint:` ambiguous, so
    /// the form says so — but Jira allows it, so it does not refuse.
    #[test]
    fn a_name_the_board_already_uses_is_pointed_out_rather_than_refused() {
        let mut screen = screen();
        screen.form.fields[NAME].set(Some("Sprint 42"));
        assert_eq!(
            screen.name_detail().as_deref(),
            Some("this board already has one by that name")
        );
        assert_eq!(done(screen.handle(key(KeyCode::Enter))).name, "Sprint 42");

        screen.form.fields[NAME].set(Some("Support 7"));
        assert_eq!(screen.name_detail(), None, "that one is on the other board");
    }

    #[test]
    fn the_start_list_offers_the_days_a_sprint_starts_on() {
        let mut screen = screen();
        screen.form.focus = START;
        screen.handle(ctrl('l'));
        let lines = render_lines(&mut screen, 80, 24).join("\n");
        assert!(lines.contains("next Monday"), "{lines}");

        // A date is optional, so the list leads with the row that clears it.
        screen.handle(key(KeyCode::Down));
        screen.handle(key(KeyCode::Enter));
        assert_eq!(screen.form.fields[START].text(), shown(now()));
    }

    /// The end dates are the ones that make sense for the start on screen, so
    /// they are built when the list opens rather than when the screen did.
    #[test]
    fn the_end_list_counts_from_whatever_the_start_says() {
        let mut screen = screen();
        screen.form.fields[START].set(Some("2026-09-01T09:00:00Z"));
        screen.form.focus = END;
        screen.handle(ctrl('l'));
        let lines = render_lines(&mut screen, 80, 24).join("\n");
        assert!(lines.contains("two weeks"), "{lines}");

        // Past the clear row and the one-week row to the fortnight.
        for _ in 0..2 {
            screen.handle(key(KeyCode::Down));
        }
        screen.handle(key(KeyCode::Enter));
        let expected = crate::api::model::parse_timestamp("2026-09-01T09:00:00Z").unwrap()
            + Duration::weeks(2);
        assert_eq!(screen.form.fields[END].text(), shown(expected));
    }

    #[test]
    fn escape_asks_before_throwing_away_typed_input() {
        let mut screen = screen();
        // Only the offered name: nobody typed that, so escape means escape.
        assert!(matches!(screen.handle(key(KeyCode::Esc)), Flow::Cancel));

        typed(&mut screen, "x");
        assert!(matches!(screen.handle(key(KeyCode::Esc)), Flow::Continue));
        assert!(matches!(screen.form.overlay, Some(Overlay::Discard)));
        screen.handle(key(KeyCode::Char('n')));
        assert!(screen.form.overlay.is_none());

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
    fn the_screen_draws_its_fields_and_what_it_would_create() {
        let mut screen = screen();
        screen.form.fields[START].set(Some("2026-09-01T09:00:00Z"));
        screen.form.fields[END].set(Some("2026-09-15T09:00:00Z"));
        let lines = render_lines(&mut screen, 90, 24).join("\n");

        assert!(lines.contains("New sprint"), "{lines}");
        assert!(lines.contains("PROJ"), "the project is shown:\n{lines}");
        for label in ["Name", "Board", "Start", "End", "Goal"] {
            assert!(lines.contains(label), "{label} is missing from\n{lines}");
        }
        // The board says which one it is, and a date says what day it lands on.
        assert!(lines.contains("board 12"), "{lines}");
        assert!(lines.contains("September"), "{lines}");
        assert!(
            lines.contains("creates Sprint 43 on Platform Board for two weeks"),
            "the preview is missing from\n{lines}"
        );
    }

    /// A project whose boards are unknown must not be locked out of its own
    /// form: the board becomes a field to type into.
    #[test]
    fn with_no_boards_to_choose_from_the_board_is_typed() {
        let mut screen = SprintScreen::new("PROJ", &[], &[], now(), &Seed::default());
        assert_eq!(screen.form.fields[NAME].text(), "");
        screen.form.fields[NAME].set(Some("Sprint 1"));
        screen.form.fields[BOARD].set(Some("12"));
        let sprint = done(screen.handle(key(KeyCode::Enter)));
        assert_eq!(sprint.board, "12");
    }

    #[test]
    fn the_next_name_follows_the_one_the_board_last_used() {
        assert_eq!(
            next_name(&["Sprint 41", "Sprint 42"]).as_deref(),
            Some("Sprint 43")
        );
        // Padding is part of the name a team chose.
        assert_eq!(next_name(&["Sprint 09"]).as_deref(), Some("Sprint 10"));
        assert_eq!(next_name(&["2026 Q3.7"]).as_deref(), Some("2026 Q3.8"));
        // The highest, not the last seen: sprints come back in board order.
        assert_eq!(
            next_name(&["Sprint 9", "Sprint 10"]).as_deref(),
            Some("Sprint 11")
        );
        // Guessing at an unnumbered scheme is worse than an empty field.
        assert_eq!(next_name(&["Hardening week"]), None);
        assert_eq!(next_name(&[]), None);
    }

    #[test]
    fn help_lists_the_keys_and_any_key_closes_it() {
        let mut screen = screen();
        screen.handle(key(KeyCode::F(1)));
        let lines = render_lines(&mut screen, 80, 24).join("\n");
        assert!(lines.contains("Keys"), "{lines}");
        assert!(lines.contains("create the sprint"), "{lines}");
        screen.handle(key(KeyCode::Char('q')));
        assert!(screen.form.overlay.is_none());
    }

    /// Print the screen, its lists and its help, for looking at:
    /// `cargo test --lib sprint::tests::preview -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn preview() {
        let mut s = screen();
        s.form.fields[START].set(Some("2026-09-01T09:00:00Z"));
        s.form.fields[END].set(Some("2026-09-15T09:00:00Z"));
        s.form.focus = GOAL;
        typed(&mut s, "Get SSO in front of the pilot customers.");
        s.form.focus = NAME;
        for l in render_lines(&mut s, 90, 24) {
            println!("|{l}");
        }
        println!();
        s.form.focus = END;
        s.handle(ctrl('l'));
        for l in render_lines(&mut s, 90, 24) {
            println!("|{l}");
        }
        println!();
        s.handle(key(KeyCode::Esc));
        s.handle(key(KeyCode::F(1)));
        for l in render_lines(&mut s, 90, 24) {
            println!("|{l}");
        }
    }

    #[test]
    fn a_short_terminal_still_renders_without_panicking() {
        for (w, h) in [(20u16, 6u16), (40, 8), (80, 10), (200, 60)] {
            let mut screen = screen();
            render_lines(&mut screen, w, h);
            screen.handle(key(KeyCode::F(1)));
            render_lines(&mut screen, w, h);
            screen.handle(key(KeyCode::Esc));
            screen.form.focus = END;
            screen.handle(ctrl('l'));
            render_lines(&mut screen, w, h);
        }
    }
}
