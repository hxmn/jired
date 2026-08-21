//! One place for every colour and border a screen uses.
//!
//! Only the sixteen ANSI colours appear here, and never a background: the
//! terminal's own palette is the user's choice of theme, and a screen that
//! paints over it looks wrong in half the terminals it runs in.

use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::BorderType;

/// Border style for panels that frame the whole screen.
pub const BORDER: BorderType = BorderType::Rounded;

/// The screen title.
pub fn title() -> Style {
    Style::default()
        .fg(Color::Cyan)
        .add_modifier(Modifier::BOLD)
}

/// A field label the user is not currently editing.
pub fn label() -> Style {
    Style::default().fg(Color::DarkGray)
}

/// The label of the focused field.
pub fn label_focused() -> Style {
    Style::default()
        .fg(Color::Cyan)
        .add_modifier(Modifier::BOLD)
}

/// Text the user typed.
pub fn value() -> Style {
    Style::default()
}

/// Placeholder text standing in for a value that has not been given.
pub fn placeholder() -> Style {
    Style::default().fg(Color::DarkGray)
}

/// Secondary information: hints, counts, the filename preview.
pub fn dim() -> Style {
    Style::default().fg(Color::DarkGray)
}

/// A key name inside a hint line.
pub fn key() -> Style {
    Style::default().fg(Color::Cyan)
}

/// Something the user must fix before the screen can finish.
pub fn error() -> Style {
    Style::default().fg(Color::Red)
}

/// The selected row of a list.
pub fn selected() -> Style {
    Style::default()
        .fg(Color::Black)
        .bg(Color::Cyan)
        .add_modifier(Modifier::BOLD)
}

/// The frame around the focused field.
pub fn focus_frame() -> Style {
    Style::default().fg(Color::Cyan)
}

/// The frame around everything else.
pub fn idle_frame() -> Style {
    Style::default().fg(Color::DarkGray)
}
