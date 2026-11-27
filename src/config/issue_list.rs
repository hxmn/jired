//! How the issue picker is ordered and narrowed, as `ui.issues.sort` and
//! `ui.issues.filter` spell it.
//!
//! The values and their spellings, without the screen: configuration checks a
//! value the moment it is set, and doing that must not need the terminal UI,
//! which draws these but has no say in how they are written down.

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

    /// Which way the column is read when nobody has said.
    ///
    /// Newest first for the three clocks, and first-to-last for everything
    /// else — including priority, whose ascending order is Highest downwards.
    /// "Sort by when" and "sort by name" mean opposite directions to a reader,
    /// and making them ask for it would be pedantry.
    pub fn descending_by_default(self) -> bool {
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
pub const CATEGORIES: &[(&str, &str)] = &[
    ("new", "To Do"),
    ("indeterminate", "In Progress"),
    ("done", "Done"),
];
