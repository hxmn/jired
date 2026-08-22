//! Choosing one issue out of the workspace [M4.7].
//!
//! The question `edit`, `comment` and `open` all ask when nothing was named,
//! answered by one screen so that the answer is given the same way each time.
//! It is a table rather than a list: a key and a summary do not answer "which
//! of these was I in yesterday" or "which of these is still mine", and those
//! are the questions someone who cannot remember the key is actually asking.
//!
//! The filter box is a subsequence match over the key and the summary — `pl142`
//! finds `PLAT-142`, `sso ro` finds the SSO rollout epic — and deliberately not
//! over the other columns: typing `aug` should find an issue that says "August",
//! not every issue touched last month. Narrowing by status is a separate thing
//! with a separate key, because it is a different question.
//!
//! Sort column, direction and status filter are all remembered in the config
//! file, so the list opens the way it was left. That is the whole point of
//! making them adjustable: a preference re-entered every time is not one.

use crate::tui::catalog::KnownIssue;
use crate::tui::picker::{Item, Outcome, Picker};
use crate::tui::when::short;
use crate::tui::{form, theme, Flow, Screen};
use chrono::{DateTime, Utc};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;
use std::collections::HashMap;
use unicode_width::UnicodeWidthStr;

/// A column of the table, which is also something the list can be sorted by.
///
/// The order here is the order they are drawn in and the order `^s` walks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Column {
    Key,
    Summary,
    Status,
    Priority,
    Assignee,
    Reporter,
    Updated,
    Created,
    Viewed,
}

pub const COLUMNS: &[Column] = &[
    Column::Key,
    Column::Summary,
    Column::Status,
    Column::Priority,
    Column::Assignee,
    Column::Reporter,
    Column::Updated,
    Column::Created,
    Column::Viewed,
];

impl Column {
    /// How the column is spelled in `ui.issues.sort`.
    pub fn name(self) -> &'static str {
        match self {
            Column::Key => "key",
            Column::Summary => "summary",
            Column::Status => "status",
            Column::Priority => "priority",
            Column::Assignee => "assignee",
            Column::Reporter => "reporter",
            Column::Updated => "updated",
            Column::Created => "created",
            Column::Viewed => "viewed",
        }
    }

    fn heading(self) -> &'static str {
        match self {
            Column::Key => "Key",
            Column::Summary => "Summary",
            Column::Status => "Status",
            Column::Priority => "Pri",
            Column::Assignee => "Assignee",
            Column::Reporter => "Reporter",
            Column::Updated => "Updated",
            Column::Created => "Created",
            Column::Viewed => "Viewed",
        }
    }

    /// Columns the reader keeps even on a narrow terminal.
    ///
    /// Without the key the answer cannot be acted on, and without the summary
    /// the key cannot be recognised; every other column is context.
    fn is_essential(self) -> bool {
        matches!(self, Column::Key | Column::Summary)
    }

    /// Cells the column reserves, before the summary takes what is left.
    fn width(self) -> u16 {
        match self {
            Column::Key => 11,
            // Whatever the row has left; never below MIN_SUMMARY.
            Column::Summary => 0,
            Column::Status => 13,
            Column::Priority => 7,
            Column::Assignee | Column::Reporter => 14,
            // Wide enough for the heading and its sort arrow — `Updated↓` is
            // eight cells — which is more than `short` ever needs for a value.
            Column::Updated | Column::Created | Column::Viewed => 8,
        }
    }

    fn cell(self, issue: &KnownIssue, now: DateTime<Utc>) -> String {
        let when = |t: Option<DateTime<Utc>>| t.map(|t| short(now, t)).unwrap_or_default();
        match self {
            Column::Key => issue.key.clone(),
            Column::Summary => issue.summary.clone(),
            Column::Status => issue.status.clone(),
            Column::Priority => issue.priority.clone(),
            Column::Assignee => issue.assignee.clone(),
            Column::Reporter => issue.reporter.clone(),
            Column::Updated => when(issue.updated),
            Column::Created => when(issue.created),
            Column::Viewed => when(issue.viewed),
        }
    }

    fn next(self) -> Column {
        let at = COLUMNS.iter().position(|c| *c == self).unwrap_or(0);
        COLUMNS[(at + 1) % COLUMNS.len()]
    }

    /// Which way the column is read when nobody has said.
    ///
    /// Newest first for the three clocks, and first-to-last for everything
    /// else — including priority, whose ascending order is Highest downwards.
    /// "Sort by when" and "sort by name" mean opposite directions to a reader,
    /// and making them ask for it would be pedantry.
    fn descending_by_default(self) -> bool {
        matches!(self, Column::Updated | Column::Created | Column::Viewed)
    }

    fn parse(name: &str) -> Option<Column> {
        COLUMNS
            .iter()
            .copied()
            .find(|c| c.name().eq_ignore_ascii_case(name.trim()))
    }
}

/// Which column the list is ordered by, and which way.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sort {
    pub column: Column,
    pub descending: bool,
}

impl Default for Sort {
    /// Most recently touched first — the order the list had before it could be
    /// ordered at all, and still the right answer to "which was I just in".
    fn default() -> Self {
        Self {
            column: Column::Updated,
            descending: true,
        }
    }
}

impl Sort {
    /// Read `column:asc` or `column:desc`; a bare column means descending for
    /// the three clocks and ascending for everything else, which is what
    /// "sort by name" and "sort by when" respectively mean to a reader.
    pub fn parse(spec: &str) -> Option<Sort> {
        let spec = spec.trim();
        if spec.is_empty() {
            return Some(Sort::default());
        }
        let (name, direction) = match spec.split_once(':') {
            Some((n, d)) => (n, Some(d.trim())),
            None => (spec, None),
        };
        let column = Column::parse(name)?;
        let descending = match direction {
            None => column.descending_by_default(),
            Some(d) if d.eq_ignore_ascii_case("desc") => true,
            Some(d) if d.eq_ignore_ascii_case("asc") => false,
            Some(_) => return None,
        };
        Some(Sort { column, descending })
    }

    /// The arrow drawn beside the heading of the column in force.
    fn arrow(&self) -> &'static str {
        if self.descending {
            "↓"
        } else {
            "↑"
        }
    }
}

impl std::fmt::Display for Sort {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}:{}",
            self.column.name(),
            if self.descending { "desc" } else { "asc" }
        )
    }
}

/// What the list is allowed to show.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Scope {
    #[default]
    All,
    /// One of Jira's three buckets: `new`, `indeterminate`, `done`. The bucket
    /// is what survives a workflow whose statuses are named something local.
    Category(String),
    /// One status, by the name the instance spells it with.
    Status(String),
}

impl Scope {
    pub fn parse(spec: &str) -> Option<Scope> {
        let spec = spec.trim();
        if spec.is_empty() || spec.eq_ignore_ascii_case("all") {
            return Some(Scope::All);
        }
        match spec.split_once(':') {
            Some((kind, value)) if kind.eq_ignore_ascii_case("category") => {
                let value = value.trim();
                CATEGORIES
                    .iter()
                    .find(|(key, _)| key.eq_ignore_ascii_case(value))
                    .map(|(key, _)| Scope::Category((*key).to_string()))
            }
            Some((kind, value)) if kind.eq_ignore_ascii_case("status") => {
                let value = value.trim();
                (!value.is_empty()).then(|| Scope::Status(value.to_string()))
            }
            _ => None,
        }
    }

    fn matches(&self, issue: &KnownIssue) -> bool {
        match self {
            Scope::All => true,
            Scope::Category(c) => issue.status_category.eq_ignore_ascii_case(c),
            Scope::Status(s) => issue.status.eq_ignore_ascii_case(s),
        }
    }

    /// How it reads on the status line and in the chooser.
    fn label(&self) -> String {
        match self {
            Scope::All => "any status".to_string(),
            Scope::Category(c) => format!("{} (category)", category_name(c)),
            Scope::Status(s) => s.clone(),
        }
    }
}

impl std::fmt::Display for Scope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Scope::All => write!(f, "all"),
            Scope::Category(c) => write!(f, "category:{c}"),
            Scope::Status(s) => write!(f, "status:{s}"),
        }
    }
}

/// Jira's status categories, in workflow order, with the names it shows.
const CATEGORIES: &[(&str, &str)] = &[
    ("new", "To Do"),
    ("indeterminate", "In Progress"),
    ("done", "Done"),
];

fn category_name(key: &str) -> &str {
    CATEGORIES
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(key))
        .map(|(_, name)| *name)
        .unwrap_or(key)
}

/// Where an unknown category sorts: after the three real ones.
fn category_rank(key: &str) -> usize {
    CATEGORIES
        .iter()
        .position(|(k, _)| k.eq_ignore_ascii_case(key))
        .unwrap_or(CATEGORIES.len())
}

/// Jira's stock priorities, most urgent first. An instance with its own
/// vocabulary falls through to the name, which at least sorts stably.
const PRIORITIES: &[&str] = &["Highest", "High", "Medium", "Low", "Lowest"];

fn priority_rank(name: &str) -> usize {
    PRIORITIES
        .iter()
        .position(|p| p.eq_ignore_ascii_case(name))
        .unwrap_or(PRIORITIES.len())
}

/// The narrowest the summary is allowed to get before a column is dropped
/// instead. Below this it stops being a summary and starts being a hint, and a
/// column of context is worth less than the words that say what the issue is.
const MIN_SUMMARY: u16 = 20;

/// Written where a cell has no value, so an empty column reads as "nothing
/// here" rather than as a rendering fault.
const NOTHING: &str = "—";

pub struct PickScreen {
    picker: Picker,
    /// Every issue offered, before the scope narrows or the sort orders them.
    issues: Vec<KnownIssue>,
    sort: Sort,
    scope: Scope,
    /// The status chooser, while it is open.
    chooser: Option<Picker>,
    /// Fixed for the life of the screen, so the ages in the table do not
    /// disagree with each other between one redraw and the next.
    now: DateTime<Utc>,
}

impl PickScreen {
    /// A screen listing `issues`, ordered and narrowed as `sort` and `scope`
    /// say. The order they arrive in does not matter; the sort decides.
    pub fn new(
        title: impl Into<String>,
        issues: &[KnownIssue],
        sort: Sort,
        scope: Scope,
        now: DateTime<Utc>,
    ) -> Self {
        let mut screen = Self {
            // Not clearable: this list exists to answer "which issue", and
            // "none" is what Esc says.
            picker: Picker::new(title, Vec::new(), false),
            issues: issues.to_vec(),
            sort,
            scope,
            chooser: None,
            now,
        };
        screen.rebuild();
        screen
    }

    /// The sort in force, for the caller to persist. Read after the screen
    /// closes, cancelled or not: changing the order and then deciding against
    /// an issue is still a decision about the order.
    pub fn sort(&self) -> Sort {
        self.sort
    }

    pub fn scope(&self) -> Scope {
        self.scope.clone()
    }

    /// Whether the workspace had anything to offer at all.
    ///
    /// Deliberately not "does the list have rows": a scope that matches nothing
    /// is a filter to change, which the screen says for itself, and not the
    /// caller's "this workspace has no issues" error.
    pub fn is_empty(&self) -> bool {
        self.issues.is_empty()
    }

    /// Re-order and re-narrow the rows under whatever is typed in the filter.
    fn rebuild(&mut self) {
        let mut shown: Vec<&KnownIssue> = self
            .issues
            .iter()
            .filter(|issue| self.scope.matches(issue))
            .collect();
        let sort = self.sort;
        shown.sort_by(|a, b| compare(a, b, sort));
        let now = self.now;
        let items = shown
            .into_iter()
            .map(|issue| {
                // The key and the summary are what the filter reads; the cells
                // are what the table draws.
                Item::new(&issue.key)
                    .with_detail(issue.summary.clone())
                    .with_cells(COLUMNS.iter().map(|c| c.cell(issue, now)).collect())
            })
            .collect();
        self.picker.set_items(items);
    }

    fn open_chooser(&mut self) {
        let mut items = vec![Item::new("all")
            .with_detail("every issue in this list")
            .with_cells(vec!["any status".to_string()])];

        // Only the categories something actually carries. Offering a bucket
        // that would empty the list is a promise the snapshots cannot keep —
        // older ones, and locally created issues, have no category at all.
        for (key, name) in CATEGORIES {
            if self
                .issues
                .iter()
                .any(|i| i.status_category.eq_ignore_ascii_case(key))
            {
                items.push(
                    Item::new(format!("category:{key}"))
                        .with_detail(format!("{name} — every status Jira files under it"))
                        .with_cells(vec![format!("{name} (category)")]),
                );
            }
        }

        let mut counts: HashMap<&str, usize> = HashMap::new();
        for issue in &self.issues {
            if !issue.status.is_empty() {
                *counts.entry(issue.status.as_str()).or_default() += 1;
            }
        }
        let mut statuses: Vec<(&str, usize)> = counts.into_iter().collect();
        // Commonest first, ties alphabetical, so the list is stable between runs.
        statuses.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
        for (status, count) in statuses {
            items.push(
                Item::new(format!("status:{status}"))
                    .with_detail(format!("{count} issue(s)"))
                    .with_cells(vec![status.to_string()]),
            );
        }

        let current = self.scope.to_string();
        let mut chooser = Picker::new("Show which issues?", items, false);
        chooser.select(&current);
        self.chooser = Some(chooser);
    }

    fn handle_chooser(&mut self, key: KeyEvent, mut chooser: Picker) -> Flow<String> {
        match chooser.handle(key) {
            Outcome::Continue => self.chooser = Some(chooser),
            Outcome::Cancel | Outcome::Chose(None) | Outcome::ChoseMany(_) => self.chooser = None,
            Outcome::Chose(Some(value)) => {
                self.chooser = None;
                // A filter typed into the chooser and confirmed without a match
                // arrives here verbatim; an unreadable one leaves the scope be
                // rather than emptying the list on a typo.
                if let Some(scope) = Scope::parse(&value) {
                    self.scope = scope;
                    self.rebuild();
                }
            }
        }
        Flow::Continue
    }
}

/// Order two issues by `sort`, with everything the column has no value for
/// last — in both directions, because a row with nothing in the column being
/// read is not the answer to "show me the oldest" either.
fn compare(a: &KnownIssue, b: &KnownIssue, sort: Sort) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let missing = |i: &KnownIssue| match sort.column {
        Column::Key => i.key.is_empty(),
        Column::Summary => i.summary.is_empty(),
        Column::Status => i.status.is_empty(),
        Column::Priority => i.priority.is_empty(),
        Column::Assignee => i.assignee.is_empty(),
        Column::Reporter => i.reporter.is_empty(),
        Column::Updated => i.updated.is_none(),
        Column::Created => i.created.is_none(),
        Column::Viewed => i.viewed.is_none(),
    };
    let by_value = match sort.column {
        Column::Key => natural_key(&a.key).cmp(&natural_key(&b.key)),
        Column::Summary => a.summary.to_lowercase().cmp(&b.summary.to_lowercase()),
        // Statuses read in workflow order rather than alphabetically: "Done"
        // before "In Progress" is an alphabet, not a workflow.
        Column::Status => category_rank(&a.status_category)
            .cmp(&category_rank(&b.status_category))
            .then_with(|| a.status.to_lowercase().cmp(&b.status.to_lowercase())),
        // Ascending priority means most urgent first, which is what someone
        // asking to sort by priority wants to see at the top.
        Column::Priority => priority_rank(&a.priority)
            .cmp(&priority_rank(&b.priority))
            .then_with(|| a.priority.to_lowercase().cmp(&b.priority.to_lowercase())),
        Column::Assignee => a.assignee.to_lowercase().cmp(&b.assignee.to_lowercase()),
        Column::Reporter => a.reporter.to_lowercase().cmp(&b.reporter.to_lowercase()),
        Column::Updated => a.updated.cmp(&b.updated),
        Column::Created => a.created.cmp(&b.created),
        Column::Viewed => a.viewed.cmp(&b.viewed),
    };
    let by_value = if sort.descending {
        by_value.reverse()
    } else {
        by_value
    };
    missing(a)
        .cmp(&missing(b))
        .then(by_value)
        // Two rows equal on the column still need one order rather than
        // whichever the sort happened to leave them in.
        .then_with(|| match natural_key(&a.key).cmp(&natural_key(&b.key)) {
            Ordering::Equal => a.key.cmp(&b.key),
            other => other,
        })
}

/// `PROJ-9` before `PROJ-10`: an issue key is a name and a number, and sorting
/// the number as text puts the hundreds among the tens.
fn natural_key(key: &str) -> (String, u64) {
    match key.rsplit_once('-') {
        Some((project, number)) => (project.to_lowercase(), number.parse().unwrap_or(u64::MAX)),
        None => (key.to_lowercase(), 0),
    }
}

/// Which columns fit in `width`, and how wide each is drawn.
///
/// Columns are dropped from the right, because they are ordered by how much
/// they tell the reader. The two essential ones and whichever column the list
/// is sorted by always survive: sorting by a column that is not on screen
/// would leave the order looking arbitrary.
fn layout(width: u16, sort: Sort, key_width: u16) -> Vec<(Column, u16)> {
    let mut shown: Vec<Column> = COLUMNS.to_vec();
    let fixed = |c: Column| {
        if c == Column::Key {
            key_width
        } else {
            c.width()
        }
    };
    // One space between columns.
    let needed = |shown: &[Column]| -> u16 {
        shown.iter().map(|c| fixed(*c)).sum::<u16>()
            + MIN_SUMMARY
            + shown.len().saturating_sub(1) as u16
    };
    while needed(&shown) > width && shown.len() > 2 {
        let Some(at) = shown
            .iter()
            .rposition(|c| !c.is_essential() && *c != sort.column)
        else {
            break;
        };
        shown.remove(at);
    }

    let gaps = shown.len().saturating_sub(1) as u16;
    let others: u16 = shown.iter().map(|c| fixed(*c)).sum();
    // Whatever is left over is the summary's; on a terminal too narrow for even
    // the essentials it shrinks rather than pushing the key off the edge.
    let summary = width.saturating_sub(others + gaps).max(1);
    shown
        .into_iter()
        .map(|c| {
            let w = if c == Column::Summary {
                summary
            } else {
                fixed(c)
            };
            (c, w)
        })
        .collect()
}

/// Pad or truncate `text` to exactly `width` terminal cells.
fn fit(text: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    let shown = if text.is_empty() { NOTHING } else { text };
    if shown.width() <= width {
        return format!("{shown}{}", " ".repeat(width - shown.width()));
    }
    // Room for the ellipsis that says the value goes on.
    let mut out = String::new();
    let mut used = 0;
    for c in shown.chars() {
        let w = c.to_string().width();
        if used + w > width.saturating_sub(1) {
            break;
        }
        out.push(c);
        used += w;
    }
    out.push('…');
    used += 1;
    format!("{out}{}", " ".repeat(width.saturating_sub(used)))
}

impl Screen for PickScreen {
    /// The key of the chosen issue.
    type Output = String;

    fn handle(&mut self, key: KeyEvent) -> Flow<String> {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return Flow::Cancel;
        }
        if let Some(chooser) = self.chooser.take() {
            return self.handle_chooser(key, chooser);
        }
        // The table's own keys are all control chords, because every plain
        // keystroke belongs to the filter box.
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            match key.code {
                KeyCode::Char('s') => {
                    // A new column arrives pointing the way that column is
                    // usually read, rather than carrying the last one's
                    // direction: switching to priority to find the urgent work
                    // and getting the trivia at the top would be a trap.
                    self.sort.column = self.sort.column.next();
                    self.sort.descending = self.sort.column.descending_by_default();
                    self.rebuild();
                    return Flow::Continue;
                }
                KeyCode::Char('r') => {
                    self.sort.descending = !self.sort.descending;
                    self.rebuild();
                    return Flow::Continue;
                }
                KeyCode::Char('f') => {
                    self.open_chooser();
                    return Flow::Continue;
                }
                _ => {}
            }
        }
        match self.picker.handle(key) {
            Outcome::Continue => Flow::Continue,
            // One issue is being chosen, so the many-value answer cannot arrive.
            Outcome::Cancel | Outcome::Chose(None) | Outcome::ChoseMany(_) => Flow::Cancel,
            // A filter that matched nothing is taken at face value by the
            // picker; the caller decides whether such a key exists.
            Outcome::Chose(Some(value)) => Flow::Done(value),
        }
    }

    fn render(&mut self, frame: &mut Frame) {
        let area = frame.area();
        if area.height == 0 || area.width == 0 {
            return;
        }
        let mut y = area.y;
        let row = |y: u16| Rect {
            y,
            height: 1,
            ..area
        };

        // Title on the left, how much of the list survived the filters on the
        // right: the two halves of "where am I".
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::raw(" "),
                Span::styled(self.picker.title.clone(), theme::title()),
            ])),
            row(y),
        );
        frame.render_widget(
            Paragraph::new(Span::styled(
                format!("{} of {} ", self.picker.matched(), self.issues.len()),
                theme::dim(),
            ))
            .right_aligned(),
            row(y),
        );
        y += 1;

        if y < area.y + area.height {
            let (text, col) = self.picker.filter().line_view(area.width as usize - 3);
            frame.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::raw(" "),
                    Span::styled("/ ", theme::key()),
                    if text.is_empty() {
                        Span::styled("type to filter", theme::placeholder())
                    } else {
                        Span::styled(text.to_string(), theme::value())
                    },
                ])),
                row(y),
            );
            frame.set_cursor_position((area.x + 3 + col as u16, y));
            y += 1;
        }

        // The hint owns the last row; the list gets what is between.
        let bottom = area.y + area.height;
        let hint_y = (bottom > y).then(|| bottom - 1);
        let key_width = self
            .picker
            .label_width()
            .clamp(6, Column::Key.width() as usize) as u16;
        let columns = layout(area.width.saturating_sub(1), self.sort, key_width);

        let mut list_top = y;
        if hint_y.is_some_and(|h| h > y + 1) {
            frame.render_widget(Paragraph::new(headings(&columns, self.sort)), row(y));
            list_top = y + 1;
        }

        if let Some(hint_y) = hint_y {
            frame.render_widget(Paragraph::new(hint(&self.scope)).centered(), row(hint_y));
        }

        let list = Rect {
            y: list_top,
            height: hint_y.unwrap_or(bottom).saturating_sub(list_top),
            ..area
        };
        if list.height == 0 {
            return;
        }
        self.picker.scroll_to_fit(list.height as usize);
        if self.picker.is_empty() {
            let why = match (&self.scope, self.picker.filter().is_empty()) {
                (Scope::All, _) => "nothing matches what you typed".to_string(),
                (scope, true) => format!("no issue is {} — ^f to widen it", scope.label()),
                (scope, false) => {
                    format!("nothing matches, within {} — ^f to widen it", scope.label())
                }
            };
            frame.render_widget(
                Paragraph::new(Span::styled(format!(" {why}"), theme::dim())),
                list,
            );
            return;
        }

        for (offset, (item, selected)) in
            self.picker.visible(list.height as usize).iter().enumerate()
        {
            let Some(item) = item else { continue };
            let style = if *selected {
                theme::selected()
            } else {
                Style::default()
            };
            let mut spans = vec![Span::styled(" ", style)];
            for (index, (column, width)) in columns.iter().enumerate() {
                if index > 0 {
                    spans.push(Span::styled(" ", style));
                }
                let at = COLUMNS.iter().position(|c| c == column).unwrap_or(0);
                let cell = fit(
                    item.cells.get(at).map(String::as_str).unwrap_or_default(),
                    *width as usize,
                );
                // The key and the summary carry the row; the rest is context,
                // and dimming it is what stops nine columns reading as noise.
                let plain = if *column == Column::Key || *column == Column::Summary {
                    theme::value()
                } else {
                    theme::dim()
                };
                spans.push(Span::styled(cell, if *selected { style } else { plain }));
            }
            frame.render_widget(
                Paragraph::new(Line::from(spans)),
                Rect {
                    y: list.y + offset as u16,
                    height: 1,
                    ..list
                },
            );
        }

        if let Some(chooser) = &mut self.chooser {
            form::render_picker(frame, area, chooser);
        }
    }
}

/// The heading row, with the sorted column named and pointed.
fn headings(columns: &[(Column, u16)], sort: Sort) -> Line<'static> {
    let mut spans = vec![Span::raw(" ")];
    for (index, (column, width)) in columns.iter().enumerate() {
        if index > 0 {
            spans.push(Span::raw(" "));
        }
        let sorted = *column == sort.column;
        let text = if sorted {
            format!("{}{}", column.heading(), sort.arrow())
        } else {
            column.heading().to_string()
        };
        // A heading is never "empty", so pad it directly rather than through
        // `fit`, which would write a dash over a column with a blank name.
        let cell = if text.width() > *width as usize {
            fit(&text, *width as usize)
        } else {
            format!("{text}{}", " ".repeat(*width as usize - text.width()))
        };
        spans.push(Span::styled(
            cell,
            if sorted { theme::key() } else { theme::label() },
        ));
    }
    Line::from(spans)
}

fn hint(scope: &Scope) -> Line<'static> {
    let mut spans = Vec::new();
    // A narrowed list looks like a short one, so the reason it is short goes
    // first — before the keys, one of which is how to widen it again.
    if *scope != Scope::All {
        spans.push(Span::styled("showing ", theme::dim()));
        spans.push(Span::styled(scope.label(), theme::value()));
        spans.push(Span::styled("  ·  ", theme::dim()));
    }
    for (index, (key, what)) in [
        ("^s", "sort"),
        ("^r", "reverse"),
        ("^f", "status"),
        ("⏎", "open"),
        ("esc", "cancel"),
    ]
    .iter()
    .enumerate()
    {
        if index > 0 {
            spans.push(Span::styled("  ·  ", theme::dim()));
        }
        spans.push(Span::styled(*key, theme::key()));
        spans.push(Span::raw(" "));
        spans.push(Span::styled(*what, theme::dim()));
    }
    Line::from(spans)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::render_lines;

    fn at(text: &str) -> Option<DateTime<Utc>> {
        crate::api::model::parse_timestamp(text)
    }

    fn now() -> DateTime<Utc> {
        at("2026-08-21T13:00:00Z").expect("a fixed clock")
    }

    fn issues() -> Vec<KnownIssue> {
        vec![
            KnownIssue {
                key: "PROJ-142".into(),
                summary: "Fix login flow".into(),
                issue_type: "Story".into(),
                status: "In Progress".into(),
                status_category: "indeterminate".into(),
                priority: "High".into(),
                assignee: "Ilya A".into(),
                reporter: "Jane D".into(),
                created: at("2026-08-01T09:12:00Z"),
                updated: at("2026-08-16T12:03:00Z"),
                viewed: at("2026-08-20T09:00:00Z"),
            },
            KnownIssue {
                key: "PROJ-100".into(),
                summary: "SSO rollout".into(),
                issue_type: "Epic".into(),
                status: "To Do".into(),
                status_category: "new".into(),
                priority: "Highest".into(),
                reporter: "Ilya A".into(),
                created: at("2026-08-02T09:00:00Z"),
                updated: at("2026-08-19T08:00:00Z"),
                ..Default::default()
            },
            KnownIssue {
                key: "NEW-8bf7b6".into(),
                summary: "Забота о сотрудниках".into(),
                issue_type: "Task".into(),
                ..Default::default()
            },
        ]
    }

    fn screen() -> PickScreen {
        PickScreen::new(
            "Edit which issue?",
            &issues(),
            Sort::default(),
            Scope::All,
            now(),
        )
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
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

    /// The order the list had before it could be ordered: most recent first.
    #[test]
    fn the_default_sort_is_the_most_recently_updated() {
        let mut screen = screen();
        assert_eq!(chose(screen.handle(key(KeyCode::Enter))), "PROJ-100");
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

    /// The columns are data, not filter fodder: an age or an assignee that
    /// happens to contain the letters typed must not pull a row in.
    #[test]
    fn the_filter_reads_the_key_and_summary_only() {
        let mut screen = screen();
        typed(&mut screen, "jane");
        assert!(
            screen.picker.is_empty(),
            "the reporter column is not searched"
        );
    }

    #[test]
    fn control_s_walks_the_sort_through_the_columns_and_control_r_reverses() {
        let mut screen = screen();
        assert_eq!(screen.sort().column, Column::Updated);

        // Key ascending: the natural order of a key, not its alphabet.
        screen.handle(ctrl('s'));
        assert_eq!(screen.sort().column, Column::Created);
        screen.handle(ctrl('s'));
        screen.handle(ctrl('s'));
        assert_eq!(screen.sort().column, Column::Key, "the walk wraps round");
        assert!(
            !screen.sort().descending,
            "a new column points the way that column is read, not the last one's way"
        );
        assert_eq!(chose(screen.handle(key(KeyCode::Enter))), "NEW-8bf7b6");

        let mut reversed = screen_sorted(Sort {
            column: Column::Key,
            descending: false,
        });
        reversed.handle(ctrl('r'));
        assert!(reversed.sort().descending);
        assert_eq!(chose(reversed.handle(key(KeyCode::Enter))), "PROJ-142");
    }

    fn screen_sorted(sort: Sort) -> PickScreen {
        PickScreen::new("Pick", &issues(), sort, Scope::All, now())
    }

    /// Sorting by priority has to mean urgency, not the alphabet — otherwise
    /// "High" would come before "Highest" and "Low" before either.
    #[test]
    fn priority_sorts_by_urgency_and_the_unset_sink_to_the_bottom() {
        let mut screen = screen_sorted(Sort {
            column: Column::Priority,
            descending: false,
        });
        assert_eq!(order(&mut screen), ["PROJ-100", "PROJ-142", "NEW-8bf7b6"]);

        // Reversed, the ones with no priority at all still go last: a blank is
        // not the answer to "show me the least urgent" either.
        screen.handle(ctrl('r'));
        assert_eq!(order(&mut screen), ["PROJ-142", "PROJ-100", "NEW-8bf7b6"]);
    }

    #[test]
    fn statuses_sort_in_workflow_order_rather_than_alphabetically() {
        let mut screen = screen_sorted(Sort {
            column: Column::Status,
            descending: false,
        });
        // To Do (new) before In Progress (indeterminate), and the issue with no
        // status at all after both.
        assert_eq!(order(&mut screen), ["PROJ-100", "PROJ-142", "NEW-8bf7b6"]);
    }

    #[test]
    fn a_never_viewed_issue_sorts_last_whichever_way_the_column_points() {
        for descending in [true, false] {
            let mut screen = screen_sorted(Sort {
                column: Column::Viewed,
                descending,
            });
            let seen = order(&mut screen);
            assert_eq!(seen[0], "PROJ-142", "the only issue ever viewed");
            assert!(seen[1..].contains(&"PROJ-100".to_string()));
        }
    }

    /// The rows the picker would hand back, top to bottom.
    fn order(screen: &mut PickScreen) -> Vec<String> {
        screen
            .picker
            .visible(100)
            .into_iter()
            .filter_map(|(item, _)| item.map(|i| i.value.clone()))
            .collect()
    }

    #[test]
    fn control_f_narrows_the_list_to_a_status_category() {
        let mut screen = screen();
        screen.handle(ctrl('f'));
        assert!(screen.chooser.is_some(), "^f opens the status chooser");

        // "any status", then the two categories present, then the statuses.
        screen.handle(key(KeyCode::Down));
        screen.handle(key(KeyCode::Enter));
        assert_eq!(screen.scope(), Scope::Category("new".into()));
        assert_eq!(order(&mut screen), ["PROJ-100"]);

        // A narrowed list looks like a short one, so it has to say why.
        let lines = render_lines(&mut screen, 100, 12).join("\n");
        assert!(lines.contains("showing To Do (category)"), "{lines}");
        assert!(lines.contains("1 of 3"), "{lines}");

        // Re-opened, the chooser starts on the scope in force rather than at
        // the top, so pressing enter twice cannot silently widen the list.
        screen.handle(ctrl('f'));
        screen.handle(key(KeyCode::Enter));
        assert_eq!(screen.scope(), Scope::Category("new".into()));

        // And back out again, from the row above it.
        screen.handle(ctrl('f'));
        screen.handle(key(KeyCode::Up));
        screen.handle(key(KeyCode::Enter));
        assert_eq!(screen.scope(), Scope::All);
        assert_eq!(order(&mut screen).len(), 3);
    }

    #[test]
    fn a_scope_that_matches_nothing_says_so_instead_of_looking_empty() {
        let mut screen = PickScreen::new(
            "Pick",
            &issues(),
            Sort::default(),
            Scope::Status("Blocked".into()),
            now(),
        );
        assert!(!screen.is_empty(), "the workspace still has issues");
        let lines = render_lines(&mut screen, 100, 20).join("\n");
        assert!(lines.contains("no issue is Blocked"), "{lines}");
        assert!(lines.contains("^f"), "{lines}");
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
        assert_eq!(chose(stepped.handle(key(KeyCode::Enter))), "PROJ-142");
        // Up from the top wraps to the end.
        let mut wrapped = screen();
        wrapped.handle(key(KeyCode::Up));
        assert_eq!(chose(wrapped.handle(key(KeyCode::Enter))), "NEW-8bf7b6");
    }

    #[test]
    fn the_table_shows_every_column_a_wide_terminal_has_room_for() {
        let mut screen = screen();
        let lines = render_lines(&mut screen, 120, 20).join("\n");
        assert!(lines.contains("Edit which issue?"), "{lines}");
        assert!(lines.contains("3 of 3"), "the count is shown:\n{lines}");
        for heading in [
            "Key", "Summary", "Status", "Pri", "Assignee", "Reporter", "Created", "Viewed",
        ] {
            assert!(lines.contains(heading), "no {heading} column:\n{lines}");
        }
        assert!(lines.contains("Updated↓"), "the sort is pointed:\n{lines}");
        assert!(lines.contains("PROJ-142"), "{lines}");
        assert!(lines.contains("Fix login flow"), "{lines}");
        assert!(lines.contains("In Progress"), "{lines}");
        assert!(lines.contains("Ilya A"), "{lines}");
        // Ages, not timestamps: the column is six cells wide.
        assert!(lines.contains("5d"), "an age is rendered short:\n{lines}");
        assert!(lines.contains("esc cancel"), "{lines}");
    }

    /// A column nobody can read is worth less than the summary beside it.
    #[test]
    fn a_narrow_terminal_drops_columns_from_the_right() {
        let mut screen = screen();
        let lines = render_lines(&mut screen, 60, 20).join("\n");
        assert!(lines.contains("Key"), "{lines}");
        assert!(lines.contains("Summary"), "{lines}");
        assert!(
            !lines.contains("Reporter"),
            "the rightmost columns go first:\n{lines}"
        );
        assert!(
            lines.contains("Updated↓"),
            "but never the column in force:\n{lines}"
        );
    }

    #[test]
    fn the_sorted_column_survives_a_narrow_terminal() {
        let mut screen = screen_sorted(Sort {
            column: Column::Viewed,
            descending: true,
        });
        let lines = render_lines(&mut screen, 56, 20).join("\n");
        assert!(lines.contains("Viewed↓"), "{lines}");
    }

    #[test]
    fn a_sort_round_trips_through_its_written_form() {
        for spec in [
            "key:asc",
            "summary:desc",
            "status:asc",
            "priority:asc",
            "assignee:desc",
            "reporter:asc",
            "updated:desc",
            "created:asc",
            "viewed:desc",
        ] {
            let sort = Sort::parse(spec).unwrap_or_else(|| panic!("`{spec}` did not parse"));
            assert_eq!(sort.to_string(), spec);
        }
        // A bare column takes the direction that column is usually read in.
        assert!(Sort::parse("updated").unwrap().descending);
        assert!(!Sort::parse("key").unwrap().descending);
        assert_eq!(Sort::parse("").unwrap(), Sort::default());
        assert!(Sort::parse("nonesuch").is_none());
        assert!(Sort::parse("key:sideways").is_none());
    }

    #[test]
    fn a_scope_round_trips_through_its_written_form() {
        assert_eq!(Scope::parse("all"), Some(Scope::All));
        assert_eq!(Scope::parse(""), Some(Scope::All));
        assert_eq!(
            Scope::parse("category:done"),
            Some(Scope::Category("done".into()))
        );
        assert_eq!(
            Scope::parse("status:In Progress"),
            Some(Scope::Status("In Progress".into()))
        );
        assert_eq!(Scope::Category("new".into()).to_string(), "category:new");
        assert_eq!(
            Scope::Status("In Progress".into()).to_string(),
            "status:In Progress"
        );
        assert!(Scope::parse("category:nonesuch").is_none());
        assert!(Scope::parse("status:").is_none());
        assert!(Scope::parse("colour:blue").is_none());
    }

    /// Print the table for looking at:
    /// `cargo test --lib pick::tests::preview -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn preview() {
        for width in [80, 100, 120, 160] {
            println!("--- {width} columns ---");
            let mut s = screen();
            for l in render_lines(&mut s, width, 9) {
                println!("|{l}");
            }
        }
    }

    #[test]
    fn a_small_terminal_renders_without_panicking() {
        for (w, h) in [(20u16, 1u16), (20, 4), (40, 8), (80, 24), (200, 60)] {
            let mut screen = screen();
            typed(&mut screen, "zzz");
            render_lines(&mut screen, w, h);

            let mut chooser = screen_sorted(Sort::default());
            chooser.handle(ctrl('f'));
            render_lines(&mut chooser, w, h);
        }
    }
}
