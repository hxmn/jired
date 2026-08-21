//! The card a form screen draws itself into [M4.6].
//!
//! `create` and `edit` are the same shape: a bordered card, a column of
//! labelled fields, a status line, key hints, and a list, a help card or a
//! confirmation floating over all of it. Drawing that once here leaves each
//! screen holding only what makes it different — which fields it has, which
//! keys it answers to, and what it produces.
//!
//! Nothing here keeps state between frames. The scroll offset is derived from
//! which field has focus, so a screen cannot drift out of sync with its own
//! rendering.

use crate::tui::field::{Field, Value};
use crate::tui::picker::Picker;
use crate::tui::theme;
use ratatui::layout::{Constraint, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::Frame;

/// Width of the label gutter, including the focus marker.
const GUTTER: u16 = 13;

/// Widest the card gets; a form stretched across a 200-column terminal is
/// harder to read, not easier.
const MAX_WIDTH: u16 = 78;

/// The rule, the status line and the key hints.
const FOOTER: u16 = 3;

/// Top and bottom of the card.
const BORDER: u16 = 2;

/// Everything a screen wants drawn besides the fields themselves.
pub struct Card<'a> {
    pub title: &'a str,
    /// Right-aligned in the top border: the project, or the issue's key.
    pub subtitle: String,
    /// Field indices that get a blank row above them.
    pub gaps: &'a [usize],
    /// The field drawn as a paragraph under its own label, if any.
    pub body: Option<usize>,
    pub status: Line<'static>,
    pub keys: Vec<(&'static str, &'static str)>,
}

/// Draw the card and its fields, returning the area it took.
///
/// `focus` is `None` while an overlay owns the keyboard, which is what removes
/// the marker and the cursor from underneath it.
pub fn render(
    frame: &mut Frame,
    card: &Card<'_>,
    fields: &mut [Field],
    focus: Option<usize>,
    body_scroll: &mut usize,
) -> Rect {
    let area = card_area(frame.area(), needed_height(fields, card));
    let block = Block::bordered()
        .border_type(theme::BORDER)
        .border_style(theme::idle_frame())
        .title_top(Line::from(vec![
            Span::raw(" "),
            Span::styled(card.title.to_string(), theme::title()),
            Span::raw(" "),
        ]))
        .title_top(
            Line::from(vec![
                Span::raw(" "),
                Span::styled(card.subtitle.clone(), theme::dim()),
                Span::raw(" "),
            ])
            .right_aligned(),
        );
    let inner = block.inner(area);
    frame.render_widget(Clear, area);
    frame.render_widget(block, area);

    let footer_height = FOOTER.min(inner.height);
    let field_area = Rect {
        height: inner.height - footer_height,
        ..inner
    };
    let footer_area = Rect {
        y: inner.y + inner.height - footer_height,
        height: footer_height,
        ..inner
    };
    render_fields(frame, card, fields, focus, body_scroll, field_area);
    render_footer(frame, card, footer_area);
    area
}

/// Rows the card wants: the fields, the gaps between them, the body's own
/// label, the footer and the border.
fn needed_height(fields: &[Field], card: &Card<'_>) -> u16 {
    let rows: u16 = fields.iter().map(|f| f.height).sum();
    rows + card.gaps.len() as u16 + u16::from(card.body.is_some()) + FOOTER + BORDER
}

/// Where each field sits, in rows from the top of the field area.
fn plan(fields: &[Field], card: &Card<'_>) -> (Vec<(usize, u16, u16)>, u16) {
    let mut rows = Vec::with_capacity(fields.len());
    let mut y = 0;
    for (index, field) in fields.iter().enumerate() {
        if card.gaps.contains(&index) {
            y += 1;
        }
        // A body carries its label on the row above its text.
        let height = field.height + u16::from(card.body == Some(index));
        rows.push((index, y, height));
        y += height;
    }
    (rows, y)
}

/// Scroll far enough to keep the focused field on screen, and no further.
///
/// Derived rather than remembered: a form with more fields than rows is common
/// on an 80×24 terminal, and a stored offset would be one more thing to keep
/// consistent with the focus it exists to follow.
fn scroll_for(rows: &[(usize, u16, u16)], total: u16, focus: Option<usize>, height: u16) -> u16 {
    if total <= height {
        return 0;
    }
    let Some((_, top, field_height)) = focus.and_then(|f| rows.iter().copied().find(|r| r.0 == f))
    else {
        return 0;
    };
    let bottom = top + field_height;
    if bottom <= height {
        return 0;
    }
    (bottom - height).min(total - height)
}

fn render_fields(
    frame: &mut Frame,
    card: &Card<'_>,
    fields: &mut [Field],
    focus: Option<usize>,
    body_scroll: &mut usize,
    area: Rect,
) {
    let (rows, total) = plan(fields, card);
    let scroll = scroll_for(&rows, total, focus, area.height);

    for (index, top, height) in rows {
        // Anything above the scroll or past the bottom edge is not drawn; a
        // field straddling the edge is clipped rather than dropped.
        if top < scroll || top >= scroll + area.height {
            continue;
        }
        let y = area.y + top - scroll;
        let visible = height.min(area.y + area.height - y);
        if visible == 0 {
            continue;
        }
        let row = Rect {
            x: area.x,
            y,
            width: area.width,
            height: visible,
        };
        render_field(
            frame,
            card,
            fields,
            index,
            focus == Some(index),
            body_scroll,
            row,
        );
    }
}

fn render_field(
    frame: &mut Frame,
    card: &Card<'_>,
    fields: &mut [Field],
    index: usize,
    focused: bool,
    body_scroll: &mut usize,
    area: Rect,
) {
    // Read out of the field before anything borrows it mutably.
    let label = fields[index].label;
    let placeholder = fields[index].placeholder;
    let detail = fields[index].detail.clone();
    let marker = Span::styled(
        if focused { "▌" } else { " " },
        if focused {
            theme::focus_frame()
        } else {
            theme::dim()
        },
    );
    let label_style = if focused {
        theme::label_focused()
    } else {
        theme::label()
    };
    let heading = Line::from(vec![
        marker,
        Span::raw(" "),
        Span::styled(label, label_style),
        Span::styled(if fields[index].changed { " •" } else { "" }, theme::key()),
    ]);

    // The body's label sits on its own row with the text beneath it;
    // everything else shares one row with its label.
    if card.body == Some(index) {
        frame.render_widget(Paragraph::new(heading), Rect { height: 1, ..area });
        let body = Rect {
            x: area.x + 2,
            y: area.y + 1,
            width: area.width.saturating_sub(3),
            height: area.height.saturating_sub(1),
        };
        render_body(frame, fields, index, focused, body_scroll, body);
        return;
    }

    let value_x = area.x + GUTTER;
    let value_width = area.width.saturating_sub(GUTTER + 1);
    frame.render_widget(Paragraph::new(heading), area);
    let value_area = Rect {
        x: value_x,
        width: value_width,
        ..area
    };

    match &mut fields[index].value {
        Value::Choice(choice) => {
            let text = choice.selected().unwrap_or("—").to_string();
            frame.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled("‹ ", theme::dim()),
                    Span::styled(
                        text,
                        if choice.selected().is_some() {
                            theme::value()
                        } else {
                            theme::placeholder()
                        },
                    ),
                    Span::styled(" ›", theme::dim()),
                ])),
                value_area,
            );
        }
        Value::Line(input) | Value::Body(input) => {
            let (text, col) = input.line_view(value_width as usize);
            let span = if text.is_empty() {
                Span::styled(placeholder, theme::placeholder())
            } else {
                Span::styled(text.to_string(), theme::value())
            };
            frame.render_widget(Paragraph::new(Line::from(span)), value_area);
            if focused {
                frame.set_cursor_position((value_x + col as u16, area.y));
            }
        }
    }

    // A field that points at something else shows what: the check the user
    // actually wants is "did I pick the right issue?".
    if let Some(detail) = detail {
        let used = fields[index].text().chars().count() as u16 + 2;
        if value_width > used + 4 {
            frame.render_widget(
                Paragraph::new(Span::styled(detail, theme::dim())),
                Rect {
                    x: value_x + used,
                    width: value_width - used,
                    ..area
                },
            );
        }
    }
}

fn render_body(
    frame: &mut Frame,
    fields: &mut [Field],
    index: usize,
    focused: bool,
    body_scroll: &mut usize,
    area: Rect,
) {
    let width = area.width.max(1) as usize;
    let height = area.height.max(1) as usize;
    let placeholder = fields[index].placeholder;
    let Some(input) = fields[index].input_mut() else {
        return;
    };
    input.set_width(width);
    let rows = input.rows();
    let (cursor_row, cursor_col) = input.cursor_at();

    if cursor_row < *body_scroll {
        *body_scroll = cursor_row;
    } else if cursor_row >= *body_scroll + height {
        *body_scroll = cursor_row + 1 - height;
    }

    let input = fields[index].input().expect("a body is a text input");
    let value = input.value();
    if value.is_empty() {
        frame.render_widget(
            Paragraph::new(Span::styled(placeholder, theme::placeholder())),
            Rect { height: 1, ..area },
        );
    }
    for (offset, row) in rows.iter().skip(*body_scroll).take(height).enumerate() {
        frame.render_widget(
            Paragraph::new(Span::styled(&value[row.start..row.end], theme::value())),
            Rect {
                y: area.y + offset as u16,
                height: 1,
                ..area
            },
        );
    }
    if focused {
        frame.set_cursor_position((
            area.x + cursor_col as u16,
            area.y + (cursor_row - *body_scroll) as u16,
        ));
    }
}

fn render_footer(frame: &mut Frame, card: &Card<'_>, area: Rect) {
    if area.height == 0 {
        return;
    }
    frame.render_widget(
        Block::new()
            .borders(Borders::TOP)
            .border_style(theme::idle_frame()),
        area,
    );
    if area.height < 2 {
        return;
    }
    frame.render_widget(
        Paragraph::new(card.status.clone()),
        Rect {
            x: area.x + 1,
            y: area.y + 1,
            width: area.width.saturating_sub(2),
            height: 1,
        },
    );
    if area.height < 3 {
        return;
    }
    frame.render_widget(
        Paragraph::new(key_hints(&card.keys)),
        Rect {
            x: area.x + 1,
            y: area.y + 2,
            width: area.width.saturating_sub(2),
            height: 1,
        },
    );
}

pub fn key_hints(keys: &[(&str, &str)]) -> Line<'static> {
    let mut spans = Vec::new();
    for (i, (key, what)) in keys.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled("  ·  ", theme::dim()));
        }
        spans.push(Span::styled(key.to_string(), theme::key()));
        spans.push(Span::raw(" "));
        spans.push(Span::styled(what.to_string(), theme::dim()));
    }
    Line::from(spans)
}

/// Centre the card, capping its size so it stays readable, and let it shrink
/// rather than overflow on a small terminal.
fn card_area(area: Rect, wanted_height: u16) -> Rect {
    let width = area.width.min(MAX_WIDTH);
    let height = area.height.min(wanted_height);
    area.centered(Constraint::Length(width), Constraint::Length(height))
}

/// Draw a list over the card.
pub fn render_picker(frame: &mut Frame, card: Rect, picker: &mut Picker) {
    // Sit above the footer rather than across it: the key hints are what tell
    // the user how to get out of the list they just opened.
    let over = Rect {
        height: card.height.saturating_sub(3).max(3),
        ..card
    };
    let area = over.centered(
        Constraint::Length(card.width.saturating_sub(6).max(20)),
        Constraint::Length(over.height.saturating_sub(2).max(3)),
    );
    let block = Block::bordered()
        .border_type(theme::BORDER)
        .border_style(theme::focus_frame())
        .title_top(Line::from(vec![
            Span::raw(" "),
            Span::styled(picker.title.clone(), theme::title()),
            Span::raw(" "),
        ]))
        .title_bottom(
            Line::from(vec![
                Span::raw(" "),
                Span::styled(
                    format!("{} of {}", picker.matched(), picker.total()),
                    theme::dim(),
                ),
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

    // The filter is the top row and always has the cursor: typing is the
    // primary way to drive a list.
    let (text, col) = picker
        .filter()
        .line_view(inner.width.saturating_sub(2) as usize);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("/ ", theme::key()),
            if text.is_empty() {
                Span::styled("type to filter", theme::placeholder())
            } else {
                Span::styled(text.to_string(), theme::value())
            },
        ])),
        Rect { height: 1, ..inner },
    );
    frame.set_cursor_position((inner.x + 2 + col as u16, inner.y));

    let list = Rect {
        y: inner.y + 1,
        height: inner.height - 1,
        ..inner
    };
    picker.scroll_to_fit(list.height as usize);
    if picker.is_empty() {
        frame.render_widget(
            Paragraph::new(Span::styled(
                "nothing matches — ⏎ uses what you typed",
                theme::dim(),
            )),
            list,
        );
        return;
    }
    for (offset, (item, selected)) in picker.visible(list.height as usize).iter().enumerate() {
        let row = Rect {
            y: list.y + offset as u16,
            height: 1,
            ..list
        };
        let style = if *selected {
            theme::selected()
        } else {
            Style::default()
        };
        let line = match item {
            Some(item) => Line::from(vec![
                Span::styled(format!(" {:<12}", item.label), style),
                Span::styled(
                    item.detail.clone(),
                    if *selected { style } else { theme::dim() },
                ),
            ]),
            None => Line::from(Span::styled(
                " (leave empty)",
                if *selected {
                    style
                } else {
                    theme::placeholder()
                },
            )),
        };
        frame.render_widget(Paragraph::new(line), row);
    }
}

/// Draw the reference card of keys over the form.
pub fn render_help(frame: &mut Frame, card: Rect, rows: &[(&str, &str)]) {
    let area = card.centered(
        Constraint::Length(card.width.saturating_sub(8).max(24)),
        Constraint::Length(rows.len() as u16 + 2),
    );
    let block = Block::bordered()
        .border_type(theme::BORDER)
        .border_style(theme::focus_frame())
        .title_top(Line::from(vec![
            Span::raw(" "),
            Span::styled("Keys", theme::title()),
            Span::raw(" "),
        ]));
    let inner = block.inner(area);
    frame.render_widget(Clear, area);
    frame.render_widget(block, area);
    for (i, (key, what)) in rows.iter().enumerate() {
        if i as u16 >= inner.height {
            break;
        }
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(format!(" {key:<16}"), theme::key()),
                Span::styled(*what, theme::dim()),
            ])),
            Rect {
                y: inner.y + i as u16,
                height: 1,
                ..inner
            },
        );
    }
}

/// Ask before throwing work away.
pub fn render_confirm(frame: &mut Frame, card: Rect, question: &str) {
    let area = card.centered(
        Constraint::Length(40.min(card.width)),
        Constraint::Length(4),
    );
    let block = Block::bordered()
        .border_type(theme::BORDER)
        .border_style(theme::error());
    let inner = block.inner(area);
    frame.render_widget(Clear, area);
    frame.render_widget(block, area);
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled(format!(" {question}"), theme::value())),
            key_hints(&[("y", "discard"), ("any other key", "keep editing")]),
        ]),
        inner,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fields(count: usize) -> Vec<Field> {
        (0..count)
            .map(|_| Field::line("Field", "placeholder"))
            .collect()
    }

    fn card<'a>(gaps: &'a [usize], body: Option<usize>) -> Card<'a> {
        Card {
            title: "Form",
            subtitle: String::new(),
            gaps,
            body,
            status: Line::default(),
            keys: Vec::new(),
        }
    }

    #[test]
    fn a_form_that_fits_is_never_scrolled() {
        let fields = fields(4);
        let card = card(&[], None);
        let (rows, total) = plan(&fields, &card);
        assert_eq!(total, 4);
        assert_eq!(scroll_for(&rows, total, Some(3), 10), 0);
    }

    /// The case an 80×24 terminal hits with ten fields: the focused one has to
    /// be on screen, and scrolling stops at the last row rather than past it.
    #[test]
    fn scrolling_follows_the_focus_and_stops_at_the_end() {
        let fields = fields(12);
        let card = card(&[], None);
        let (rows, total) = plan(&fields, &card);
        assert_eq!(total, 12);

        assert_eq!(
            scroll_for(&rows, total, Some(0), 6),
            0,
            "the top needs none"
        );
        assert_eq!(scroll_for(&rows, total, Some(5), 6), 0, "still in view");
        assert_eq!(scroll_for(&rows, total, Some(6), 6), 1, "one row down");
        assert_eq!(
            scroll_for(&rows, total, Some(11), 6),
            6,
            "the last field sits on the last row"
        );
    }

    #[test]
    fn gaps_and_the_body_label_take_rows_of_their_own() {
        let mut fields = fields(3);
        fields[2] = Field::body("Description", "", 4);
        let card = card(&[1], Some(2));
        let (rows, total) = plan(&fields, &card);

        assert_eq!(rows[0], (0, 0, 1));
        assert_eq!(rows[1], (1, 2, 1), "a gap pushes the second field down");
        assert_eq!(rows[2], (2, 3, 5), "the body is its label plus its height");
        assert_eq!(total, 8);
        // The card asks for the fields, the gap, the body label, footer, border.
        assert_eq!(needed_height(&fields, &card), 6 + 1 + 1 + FOOTER + BORDER);
    }
}
