//! The book-time screen.
//!
//! `jired logwork` is offline like the rest of the working copy: the entry is
//! staged in the file and `push` sends it. So this form never talks to a
//! server — the issues it offers are the ones already pulled, and the durations
//! and start times it offers are the ones people actually type.
//!
//! The two fields that matter are checked while they are being typed rather
//! than after the screen closes. A duration Jira would refuse and a timestamp
//! that is not one are both mistakes worth catching with the cursor still on
//! the field that caused them.

use crate::tui::field::Field;
use crate::tui::form::{self, Card};
use crate::tui::picker::{self, Item, Picker};
use crate::tui::{theme, Flow, Screen};
use chrono::{DateTime, Duration, Utc};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::text::{Line, Span};
use ratatui::Frame;

pub use crate::tui::catalog::KnownIssue;

/// Durations offered in the Time field's list.
///
/// Deliberately short: it is a shortcut for the common bookings, not a menu
/// anyone has to read to the end of. Anything else is typed.
const COMMON: &[(&str, &str)] = &[
    ("15m", "a quarter of an hour"),
    ("30m", "half an hour"),
    ("1h", "an hour"),
    ("2h", "a couple of hours"),
    ("4h", "half a day"),
    ("1d", "a day"),
];

/// What the command line already supplied; the form starts from these.
#[derive(Debug, Clone, Default)]
pub struct Seed {
    pub issue: Option<String>,
    pub time: Option<String>,
    /// Already normalized to the form the file carries.
    pub started: Option<String>,
    pub note: Option<String>,
}

/// A finished form: exactly what `logwork` needs to stage the entry.
#[derive(Debug, Clone, PartialEq)]
pub struct Booking {
    pub issue: String,
    pub time: String,
    pub started: String,
    pub note: String,
}

// Field order is the tab order, and the order they are drawn in.
const ISSUE: usize = 0;
const TIME: usize = 1;
const STARTED: usize = 2;
const NOTE: usize = 3;

enum Overlay {
    Pick {
        field: usize,
        picker: Picker,
    },
    Help,
    /// Esc with something typed; confirm before throwing it away.
    Discard,
}

pub struct LogworkScreen {
    fields: Vec<Field>,
    focus: usize,
    overlay: Option<Overlay>,
    /// A validation failure, cleared as soon as anything is typed.
    error: Option<String>,
    /// Right-aligned in the border: the workspace's project.
    project: String,
    /// When the screen opened, which is what the start-time presets count back
    /// from and what an empty Started field means.
    now: DateTime<Utc>,
    body_scroll: usize,
}

impl LogworkScreen {
    pub fn new(project: &str, issues: &[KnownIssue], now: DateTime<Utc>, seed: &Seed) -> Self {
        let issue_items: Vec<Item> = issues
            .iter()
            .map(|i| {
                let aside: Vec<&str> = [i.status.as_str(), i.issue_type.as_str()]
                    .into_iter()
                    .filter(|s| !s.is_empty())
                    .collect();
                Item::new(&i.key).with_detail(if aside.is_empty() {
                    i.summary.clone()
                } else {
                    format!("{}  ·  {}", i.summary, aside.join("  ·  "))
                })
            })
            .collect();

        let mut fields = vec![
            Field::line(ISSUE_LABEL, "which issue the time went to")
                .required()
                .with_items(issue_items)
                .suggesting()
                .with_hint("type any part of the key or the summary"),
            Field::line(TIME_LABEL, "2h 30m")
                .required()
                .with_items(
                    COMMON
                        .iter()
                        .map(|(value, what)| Item::new(*value).with_detail(*what))
                        .collect(),
                )
                .with_hint("weeks, days, hours and minutes — `2h 30m`, `1d`, `45m`"),
            Field::line(STARTED_LABEL, "now")
                .with_items(start_presets(now))
                .with_hint("when the work started, not when it is booked"),
            Field::body(NOTE_LABEL, "what the time went on", 4),
        ];

        fields[ISSUE].set(seed.issue.as_deref());
        fields[TIME].set(seed.time.as_deref());
        fields[NOTE].set(seed.note.as_deref());
        // Prefilled rather than left empty, so what will be recorded is on
        // screen: "now" is a value, and one worth being able to correct.
        fields[STARTED].set(Some(
            seed.started
                .clone()
                .unwrap_or_else(|| crate::api::model::format_local_iso(&now))
                .as_str(),
        ));

        Self {
            // Start on the first thing still missing: with the issue already
            // named on the command line, the duration is what is being asked
            // for and the first keystroke should land there.
            focus: if seed.issue.is_some() { TIME } else { ISSUE },
            fields,
            overlay: None,
            error: None,
            project: project.to_string(),
            now,
            body_scroll: 0,
        }
    }

    fn booking(&self) -> Booking {
        Booking {
            issue: self.fields[ISSUE].text().trim().to_string(),
            time: self.fields[TIME].text().trim().to_string(),
            started: self.fields[STARTED]
                .filled()
                .map(str::to_string)
                .unwrap_or_else(|| crate::api::model::format_local_iso(&self.now)),
            note: self.fields[NOTE].text().trim().to_string(),
        }
    }

    /// Refuse to finish while something is missing or unreadable, and put the
    /// cursor on the field that has to be fixed.
    ///
    /// Every complaint here is one the command would otherwise make after the
    /// screen had already closed, taking the half-filled form with it.
    fn submit(&mut self) -> Flow<Booking> {
        // Enter on a half-typed key takes the suggestion that is on screen
        // rather than refusing it: the completion was already being offered,
        // and refusing what the form just proposed is only annoying.
        self.fields[ISSUE].complete();

        for (index, what) in [(ISSUE, "an issue"), (TIME, "a length of time")] {
            if self.fields[index].filled().is_none() {
                return self.refuse(index, format!("{what} is required"));
            }
        }

        let time = self.fields[TIME].text().trim().to_string();
        if crate::doc::parse_duration(&time).is_none() {
            return self.refuse(TIME, format!("`{time}` is not a length of time"));
        }

        // A start time with no zone means the reader's own — the one the field
        // was showing them — and only that case is rewritten, with the offset
        // spelled out so the issue file names one instant rather than one per
        // machine. A time that already says which zone it is in is left in the
        // words it was typed in.
        if let Some(started) = self.fields[STARTED].filled() {
            let named_a_zone = crate::api::model::names_a_zone(started);
            match crate::api::model::parse_local_timestamp(started) {
                None => return self.refuse(STARTED, format!("`{started}` is not a timestamp")),
                Some(t) if !named_a_zone => {
                    let spelled_out = crate::api::model::format_local_iso(&t);
                    self.fields[STARTED].set(Some(&spelled_out));
                }
                Some(_) => {}
            }
        }

        // The workspace's own issues are the only ones with a file to stage
        // this in, so a key that names none of them is a typo, not a discovery.
        let issue = self.fields[ISSUE].text().trim().to_string();
        if !self.fields[ISSUE].items.is_empty() && self.known(&issue).is_none() {
            return self.refuse(ISSUE, format!("`{issue}` is not in this workspace"));
        }

        Flow::Done(self.booking())
    }

    fn refuse(&mut self, field: usize, complaint: String) -> Flow<Booking> {
        self.focus = field;
        self.error = Some(complaint);
        Flow::Continue
    }

    /// The issue item a key names, matched the way a key is typed.
    fn known(&self, key: &str) -> Option<&Item> {
        self.fields[ISSUE]
            .items
            .iter()
            .find(|i| i.value.eq_ignore_ascii_case(key))
    }

    /// Whether anything has been typed that would be lost by cancelling.
    ///
    /// The prefilled start time does not count: nobody typed it.
    fn dirty(&self) -> bool {
        [ISSUE, TIME, NOTE]
            .iter()
            .any(|&i| self.fields[i].filled().is_some())
    }

    fn move_focus(&mut self, forward: bool) {
        let len = self.fields.len();
        self.focus = if forward {
            (self.focus + 1) % len
        } else {
            (self.focus + len - 1) % len
        };
    }

    fn open_list(&mut self) {
        let field = &self.fields[self.focus];
        if !field.openable() {
            return;
        }
        let mut picker = Picker::new(field.label, field.items.clone(), field.optional);
        if let Some(current) = field.filled() {
            picker.select(current);
        }
        self.overlay = Some(Overlay::Pick {
            field: self.focus,
            picker,
        });
    }

    fn handle_overlay(&mut self, key: KeyEvent, overlay: Overlay) -> Flow<Booking> {
        match overlay {
            // Any key closes help; it is a reference card, not a mode.
            Overlay::Help => self.overlay = None,
            Overlay::Discard => match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') => return Flow::Cancel,
                _ => self.overlay = None,
            },
            Overlay::Pick { field, mut picker } => match picker.handle(key) {
                picker::Outcome::Continue => {
                    self.overlay = Some(Overlay::Pick { field, picker });
                }
                // No field here takes several values, so this cannot arrive.
                picker::Outcome::Cancel | picker::Outcome::ChoseMany(_) => self.overlay = None,
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

impl Screen for LogworkScreen {
    type Output = Booking;

    fn handle(&mut self, key: KeyEvent) -> Flow<Booking> {
        if let Some(overlay) = self.overlay.take() {
            return self.handle_overlay(key, overlay);
        }

        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            // Always a way out, even mid-edit: this is the key people reach for.
            KeyCode::Char('c') if ctrl => return Flow::Cancel,
            KeyCode::Char('s') if ctrl => return self.submit(),
            KeyCode::Char('l') if ctrl => {
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
            // Tab completes the issue before it moves on: on a field whose
            // values are keys nobody remembers in full, finishing what was
            // typed is what tab is for.
            KeyCode::Tab if self.fields[self.focus].complete() => {
                self.error = None;
                return Flow::Continue;
            }
            KeyCode::Tab => {
                self.move_focus(true);
                return Flow::Continue;
            }
            KeyCode::BackTab => {
                self.move_focus(false);
                return Flow::Continue;
            }
            // Enter finishes the form. The note claims it first, below, because
            // a paragraph needs a way to hold more than one line.
            KeyCode::Enter if self.focus != NOTE => return self.submit(),
            _ => {}
        }

        if self.fields[self.focus].handle(key) {
            // Any edit invalidates the complaint that produced the error.
            self.error = None;
            return Flow::Continue;
        }

        match key.code {
            KeyCode::Down => self.move_focus(true),
            KeyCode::Up => self.move_focus(false),
            _ => {}
        }
        Flow::Continue
    }

    fn render(&mut self, frame: &mut Frame) {
        // Each field says what it points at, which is the check the user
        // actually wants: the right issue, and the time they meant.
        self.fields[ISSUE].detail = self.issue_detail();
        self.fields[STARTED].detail = self.started_detail();

        let card = Card {
            title: "Log work",
            subtitle: self.project.clone(),
            gaps: &[STARTED, NOTE],
            body: Some(NOTE),
            status: self.status_line(),
            keys: self.key_hints(),
        };
        let focus = self.overlay.is_none().then_some(self.focus);
        let area = form::render(frame, &card, &mut self.fields, focus, &mut self.body_scroll);

        match &mut self.overlay {
            Some(Overlay::Pick { picker, .. }) => form::render_picker(frame, area, picker),
            Some(Overlay::Help) => form::render_help(frame, area, HELP),
            Some(Overlay::Discard) => form::render_confirm(frame, area, "Discard this entry?"),
            None => {}
        }
    }
}

const HELP: &[(&str, &str)] = &[
    ("tab", "take the suggestion, where there is one"),
    ("tab", "otherwise, move to the next field"),
    ("shift-tab / ↑ ↓", "move between fields"),
    ("^l or f2", "open the field as a list"),
    ("⏎", "book the time"),
    ("^s", "book from the note"),
    ("^a ^e ^w ^u ^k", "edit the line like a shell does"),
    ("esc", "cancel"),
];

const ISSUE_LABEL: &str = "Issue";
const TIME_LABEL: &str = "Time";
const STARTED_LABEL: &str = "Started";
const NOTE_LABEL: &str = "Note";

impl LogworkScreen {
    /// What sits dim after the issue: the summary of the issue it names, and
    /// before that, what tab would complete it to.
    ///
    /// Showing the pending completion is what makes tab's two jobs — finish
    /// this, then move on — visible rather than something to discover by having
    /// the next word land in the wrong field.
    fn issue_detail(&self) -> Option<String> {
        if let Some(item) = self.fields[ISSUE].suggestion() {
            return Some(format!("→ {}  {}   tab", item.label, item.detail));
        }
        let key = self.fields[ISSUE].filled()?;
        Some(match self.known(key) {
            Some(item) => item.detail.clone(),
            None => "not in this workspace".to_string(),
        })
    }

    /// The start time in words, because nobody reads an ISO timestamp for
    /// whether it says this morning or last Tuesday.
    fn started_detail(&self) -> Option<String> {
        let started = self.fields[STARTED].filled()?;
        match crate::api::model::parse_timestamp(started) {
            Some(when) => Some(ago(self.now, when)),
            None => Some("not a timestamp".to_string()),
        }
    }

    /// Whichever is most useful right now: a complaint, then what the form
    /// would book, then help for the field under the cursor.
    fn status_line(&self) -> Line<'static> {
        if let Some(error) = &self.error {
            return Line::from(Span::styled(format!("✗ {error}"), theme::error()));
        }
        if let (Some(issue), Some(time)) = (self.fields[ISSUE].filled(), self.fields[TIME].filled())
        {
            if crate::doc::parse_duration(time).is_some() {
                return Line::from(vec![
                    Span::styled("books ", theme::dim()),
                    Span::styled(time.to_string(), theme::value()),
                    Span::styled(" against ", theme::dim()),
                    Span::styled(issue.to_uppercase(), theme::value()),
                ]);
            }
        }
        Line::from(Span::styled(
            self.fields[self.focus].hint.clone(),
            theme::dim(),
        ))
    }

    fn key_hints(&self) -> Vec<(&'static str, &'static str)> {
        let mut keys = vec![("tab", "field")];
        if self.focus == NOTE {
            keys.push(("^s", "log"));
        } else {
            keys.push(("⏎", "log"));
        }
        if self.fields[self.focus].suggestion().is_some() {
            keys.push(("tab", "complete"));
        }
        if self.fields[self.focus].openable() {
            keys.push(("^l", "list"));
        }
        keys.push(("esc", "cancel"));
        keys.push(("f1", "keys"));
        keys
    }
}

/// The start times worth one keystroke, counted back from when the screen
/// opened. The value is what the file carries; the detail is what it means.
fn start_presets(now: DateTime<Utc>) -> Vec<Item> {
    let mut out =
        vec![Item::new(crate::api::model::format_local_iso(&now)).with_detail("just now")];
    for hours in [1, 2, 4, 8] {
        let when = now - Duration::hours(hours);
        out.push(Item::new(crate::api::model::format_local_iso(&when)).with_detail(ago(now, when)));
    }
    let yesterday = now - Duration::days(1);
    out.push(Item::new(crate::api::model::format_local_iso(&yesterday)).with_detail("yesterday"));
    out
}

/// How long ago `when` was, in the words someone would use for it.
fn ago(now: DateTime<Utc>, when: DateTime<Utc>) -> String {
    let minutes = (now - when).num_minutes();
    match minutes {
        // A start time in the future is a mistake worth naming rather than
        // describing as "in -3 hours".
        m if m < -1 => "in the future".to_string(),
        m if m < 5 => "just now".to_string(),
        m if m < 60 => format!("{m} minutes ago"),
        m if m < 120 => "an hour ago".to_string(),
        m if m < 60 * 24 => format!("{} hours ago", m / 60),
        m if m < 60 * 48 => "yesterday".to_string(),
        m => format!("{} days ago", m / (60 * 24)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::render_lines;

    fn now() -> DateTime<Utc> {
        crate::api::model::parse_timestamp("2026-08-21T13:00:00Z").expect("a fixed clock")
    }

    /// What the form writes for an instant, in whatever zone the test machine
    /// keeps. Spelling the expected value as a literal would only assert that
    /// the machine running the suite is on UTC.
    fn shown(dt: DateTime<Utc>) -> String {
        crate::api::model::format_local_iso(&dt)
    }

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
        ]
    }

    fn screen() -> LogworkScreen {
        LogworkScreen::new("PROJ", &issues(), now(), &Seed::default())
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    fn typed(screen: &mut LogworkScreen, text: &str) {
        for c in text.chars() {
            screen.handle(key(KeyCode::Char(c)));
        }
    }

    fn done(flow: Flow<Booking>) -> Booking {
        match flow {
            Flow::Done(booking) => booking,
            _ => panic!("the screen did not finish"),
        }
    }

    #[test]
    fn the_form_starts_on_the_issue_with_the_clock_already_filled_in() {
        let screen = screen();
        assert_eq!(screen.focus, ISSUE);
        assert_eq!(screen.fields[STARTED].text(), shown(now()));
    }

    /// With the issue already named, the duration is what is being asked for.
    #[test]
    fn naming_the_issue_up_front_puts_the_cursor_on_the_duration() {
        let seed = Seed {
            issue: Some("PROJ-142".into()),
            ..Seed::default()
        };
        let mut screen = LogworkScreen::new("PROJ", &issues(), now(), &seed);
        assert_eq!(screen.focus, TIME);

        typed(&mut screen, "2h 30m");
        let booking = done(screen.handle(key(KeyCode::Enter)));
        assert_eq!(booking.issue, "PROJ-142");
        assert_eq!(booking.time, "2h 30m");
        assert_eq!(booking.started, shown(now()));
        assert!(booking.note.is_empty());
    }

    #[test]
    fn a_whole_entry_can_be_typed_from_an_empty_form() {
        let mut screen = screen();
        typed(&mut screen, "PROJ-142");
        screen.handle(key(KeyCode::Tab));
        typed(&mut screen, "45m");
        screen.focus = NOTE;
        typed(&mut screen, "Traced the redirect loop.");

        // The note keeps Enter for itself, so ^s is the way out from it.
        assert!(matches!(screen.handle(key(KeyCode::Enter)), Flow::Continue));
        let booking = done(screen.handle(ctrl('s')));
        assert_eq!(booking.issue, "PROJ-142");
        assert_eq!(booking.time, "45m");
        assert_eq!(booking.note, "Traced the redirect loop.");
    }

    /// Tab on a half-typed key finishes it, which is the whole reason the field
    /// suggests: nobody types an issue key out in full.
    ///
    /// Tab doing two things is only usable because the pending completion is on
    /// screen before it is pressed — otherwise the next word typed lands in the
    /// field you thought you had left.
    #[test]
    fn tab_completes_the_issue_before_it_moves_on_and_says_so_first() {
        let mut screen = screen();
        typed(&mut screen, "142");
        assert_eq!(
            screen.issue_detail().as_deref(),
            Some("→ PROJ-142  Fix login flow  ·  In Progress  ·  Story   tab")
        );
        assert!(screen.key_hints().contains(&("tab", "complete")));

        screen.handle(key(KeyCode::Tab));
        assert_eq!(screen.fields[ISSUE].text(), "PROJ-142");
        assert_eq!(screen.focus, ISSUE, "completing is not moving on");

        // With nothing left to complete, tab is a tab again — and the hint that
        // said so is gone with it.
        assert!(!screen.key_hints().contains(&("tab", "complete")));
        screen.handle(key(KeyCode::Tab));
        assert_eq!(screen.focus, TIME);
    }

    /// Enter takes the suggestion that is on screen rather than refusing the
    /// key it was already offering to finish.
    #[test]
    fn submitting_a_half_typed_key_takes_the_completion() {
        let mut screen = screen();
        typed(&mut screen, "142");
        screen.fields[TIME].set(Some("1h"));
        let booking = done(screen.handle(key(KeyCode::Enter)));
        assert_eq!(booking.issue, "PROJ-142");
    }

    /// Every complaint the command could make after the screen closed is made
    /// here instead, with the cursor on the field that caused it.
    #[test]
    fn a_bad_value_is_refused_with_the_cursor_on_the_field_that_caused_it() {
        let mut screen = screen();

        assert!(matches!(screen.handle(key(KeyCode::Enter)), Flow::Continue));
        assert_eq!(screen.focus, ISSUE);
        assert_eq!(screen.error.as_deref(), Some("an issue is required"));

        typed(&mut screen, "PROJ-142");
        screen.handle(key(KeyCode::Enter));
        assert_eq!(screen.focus, TIME);
        assert_eq!(
            screen.error.as_deref(),
            Some("a length of time is required")
        );

        typed(&mut screen, "a while");
        screen.handle(key(KeyCode::Enter));
        assert_eq!(screen.focus, TIME);
        assert_eq!(
            screen.error.as_deref(),
            Some("`a while` is not a length of time")
        );

        // A bare number means minutes only because of an instance setting.
        screen.fields[TIME].set(Some("90"));
        screen.handle(key(KeyCode::Enter));
        assert_eq!(
            screen.error.as_deref(),
            Some("`90` is not a length of time")
        );

        screen.fields[TIME].set(Some("90m"));
        screen.fields[STARTED].set(Some("tuesday"));
        screen.handle(key(KeyCode::Enter));
        assert_eq!(screen.focus, STARTED);
        assert_eq!(
            screen.error.as_deref(),
            Some("`tuesday` is not a timestamp")
        );

        screen.fields[STARTED].set(Some("2026-08-21T09:00:00Z"));
        let booking = done(screen.handle(key(KeyCode::Enter)));
        assert_eq!(booking.started, "2026-08-21T09:00:00Z");
    }

    /// The file is where the entry is staged, so an issue with no file here is
    /// a typo rather than something to go and look for.
    #[test]
    fn an_issue_the_workspace_does_not_have_is_refused() {
        let mut screen = screen();
        typed(&mut screen, "PROJ-999");
        screen.fields[TIME].set(Some("1h"));
        screen.handle(key(KeyCode::Enter));
        assert_eq!(screen.focus, ISSUE);
        assert_eq!(
            screen.error.as_deref(),
            Some("`PROJ-999` is not in this workspace")
        );

        // Typing clears the complaint rather than leaving it up to argue with.
        typed(&mut screen, "x");
        assert_eq!(screen.error, None);
    }

    /// A workspace whose issues are unknown to the catalog must not be locked
    /// out of its own form.
    #[test]
    fn with_no_issues_to_check_against_any_key_is_taken_at_face_value() {
        let mut screen = LogworkScreen::new("PROJ", &[], now(), &Seed::default());
        typed(&mut screen, "PROJ-999");
        screen.fields[TIME].set(Some("1h"));
        let booking = done(screen.handle(key(KeyCode::Enter)));
        assert_eq!(booking.issue, "PROJ-999");
    }

    #[test]
    fn the_duration_list_offers_the_common_bookings() {
        let mut screen = screen();
        screen.focus = TIME;
        screen.handle(ctrl('l'));
        let lines = render_lines(&mut screen, 80, 24).join("\n");
        assert!(lines.contains("half an hour"), "{lines}");
        screen.handle(key(KeyCode::Enter));
        assert_eq!(screen.fields[TIME].text(), "15m");
    }

    #[test]
    fn the_start_time_list_counts_back_from_when_the_screen_opened() {
        let mut screen = screen();
        screen.focus = STARTED;
        screen.handle(ctrl('l'));
        screen.handle(key(KeyCode::Down));
        screen.handle(key(KeyCode::Enter));
        assert_eq!(
            screen.fields[STARTED].text(),
            shown(now() - Duration::hours(1))
        );
    }

    #[test]
    fn timestamps_are_described_in_words() {
        let now = now();
        assert_eq!(ago(now, now), "just now");
        assert_eq!(ago(now, now - Duration::minutes(20)), "20 minutes ago");
        assert_eq!(ago(now, now - Duration::hours(1)), "an hour ago");
        assert_eq!(ago(now, now - Duration::hours(5)), "5 hours ago");
        assert_eq!(ago(now, now - Duration::days(1)), "yesterday");
        assert_eq!(ago(now, now - Duration::days(3)), "3 days ago");
        assert_eq!(ago(now, now + Duration::hours(3)), "in the future");
    }

    #[test]
    fn escape_asks_before_throwing_away_typed_input() {
        let mut screen = screen();
        // Only the prefilled clock: nobody typed that, so escape means escape.
        assert!(matches!(screen.handle(key(KeyCode::Esc)), Flow::Cancel));

        typed(&mut screen, "PROJ-1");
        assert!(matches!(screen.handle(key(KeyCode::Esc)), Flow::Continue));
        assert!(matches!(screen.overlay, Some(Overlay::Discard)));
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
    fn the_screen_draws_its_fields_and_what_it_would_book() {
        let mut screen = screen();
        typed(&mut screen, "PROJ-142");
        screen.fields[TIME].set(Some("2h 30m"));
        let lines = render_lines(&mut screen, 80, 24).join("\n");

        assert!(lines.contains("Log work"), "{lines}");
        assert!(lines.contains("PROJ"), "the project is shown:\n{lines}");
        for label in ["Issue", "Time", "Started", "Note"] {
            assert!(lines.contains(label), "{label} is missing from\n{lines}");
        }
        // The issue field says what it points at, and the start time says what
        // it means — an ISO string on its own tells nobody anything.
        assert!(lines.contains("Fix login flow"), "{lines}");
        assert!(lines.contains("just now"), "{lines}");
        assert!(
            lines.contains("books 2h 30m against PROJ-142"),
            "the preview is missing from\n{lines}"
        );
    }

    #[test]
    fn the_issue_list_draws_over_the_form() {
        let mut screen = screen();
        screen.handle(ctrl('l'));
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
        assert!(lines.contains("book the time"), "{lines}");
        screen.handle(key(KeyCode::Char('q')));
        assert!(screen.overlay.is_none());
    }

    /// Print the screen, its lists and its help, for looking at:
    /// `cargo test --lib logwork::tests::preview -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn preview() {
        let mut s = screen();
        typed(&mut s, "PROJ-142");
        s.fields[TIME].set(Some("2h 30m"));
        s.fields[STARTED].set(Some("2026-08-21T09:00:00Z"));
        s.focus = NOTE;
        typed(
            &mut s,
            "Traced the redirect loop through the IdP assertion.",
        );
        s.focus = ISSUE;
        for l in render_lines(&mut s, 90, 24) {
            println!("|{l}");
        }
        println!();
        s.focus = STARTED;
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
            typed(&mut screen, "PROJ-142");
            render_lines(&mut screen, w, h);
            screen.handle(key(KeyCode::F(1)));
            render_lines(&mut screen, w, h);
            screen.handle(key(KeyCode::Esc));
            screen.focus = STARTED;
            screen.handle(ctrl('l'));
            render_lines(&mut screen, w, h);
        }
    }
}
