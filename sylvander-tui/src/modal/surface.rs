//! Shared geometry for temporary interaction surfaces.
//!
//! Decision surfaces are part of the terminal flow: they rise from the bottom,
//! replace the Composer, and leave the one-row status line visible. They never
//! float in the middle of the transcript.

use ratatui::{
    Frame,
    layout::Rect,
    text::Line,
    widgets::{Block, Clear, Paragraph},
};

use crate::theme;

const STATUS_ROWS: u16 = 1;
const DOCK_RULE_ROWS: u16 = 2;
const CONTENT_GUTTER: u16 = 2;
const MAX_CONTENT_WIDTH: u16 = 110;

pub(crate) struct ReviewAreas {
    pub header: Rect,
    pub body: Rect,
    pub footer: Rect,
}

pub(crate) struct FocusPickerAreas {
    pub results: Rect,
    pub query: Rect,
}

/// Paint a bottom-anchored Decision Dock and return its readable body.
///
/// `requested_body_rows` excludes the top and bottom rules. The body is reduced
/// when the viewport is short; callers should render with wrapping and clipping.
pub(crate) fn decision_dock(frame: &mut Frame, parent: Rect, requested_body_rows: u16) -> Rect {
    let available = parent.height.saturating_sub(STATUS_ROWS);
    let dock_height = requested_body_rows
        .saturating_add(DOCK_RULE_ROWS)
        .min(available);
    let dock = Rect {
        x: parent.x,
        y: parent.y + available.saturating_sub(dock_height),
        width: parent.width,
        height: dock_height,
    };

    frame.render_widget(Clear, dock);
    frame.render_widget(Block::default().style(theme::text_on_canvas()), dock);

    if dock.height > 0 {
        frame.render_widget(
            Paragraph::new(Line::from("─".repeat(dock.width as usize)).style(theme::rule())),
            Rect { height: 1, ..dock },
        );
    }
    if dock.height > 1 {
        frame.render_widget(
            Paragraph::new(Line::from("─".repeat(dock.width as usize)).style(theme::rule())),
            Rect {
                y: dock.y + dock.height - 1,
                height: 1,
                ..dock
            },
        );
    }

    let horizontal_gutter = CONTENT_GUTTER.min(dock.width / 2);
    Rect {
        x: dock.x + horizontal_gutter,
        y: dock.y.saturating_add(1),
        width: dock
            .width
            .saturating_sub(horizontal_gutter.saturating_mul(2))
            .min(MAX_CONTENT_WIDTH),
        height: dock.height.saturating_sub(DOCK_RULE_ROWS),
    }
}

/// Give long-form material the transcript viewport while keeping status visible.
pub(crate) fn review_view(frame: &mut Frame, parent: Rect, footer_rows: u16) -> ReviewAreas {
    let view = Rect {
        height: parent.height.saturating_sub(STATUS_ROWS),
        ..parent
    };
    frame.render_widget(Clear, view);
    frame.render_widget(Block::default().style(theme::text_on_canvas()), view);

    let gutter = CONTENT_GUTTER.min(view.width / 2);
    let content_x = view.x + gutter;
    let content_width = view
        .width
        .saturating_sub(gutter.saturating_mul(2))
        .min(MAX_CONTENT_WIDTH);
    let footer_rows = footer_rows.min(view.height.saturating_sub(3));
    let footer_y = view.y + view.height.saturating_sub(footer_rows);
    for y in [view.y.saturating_add(1), footer_y.saturating_sub(1)] {
        if y < view.y + view.height {
            frame.render_widget(
                Paragraph::new(Line::from("─".repeat(view.width as usize)).style(theme::rule())),
                Rect {
                    x: view.x,
                    y,
                    width: view.width,
                    height: 1,
                },
            );
        }
    }

    ReviewAreas {
        header: Rect {
            x: content_x,
            y: view.y,
            width: content_width,
            height: 1,
        },
        body: Rect {
            x: content_x,
            y: view.y.saturating_add(2),
            width: content_width,
            height: footer_y.saturating_sub(view.y.saturating_add(3)),
        },
        footer: Rect {
            x: view.x,
            y: footer_y,
            width: view.width,
            height: footer_rows,
        },
    }
}

/// Raise a searchable selector from the Composer without covering context.
pub(crate) fn focus_picker(
    frame: &mut Frame,
    parent: Rect,
    requested_result_rows: u16,
) -> FocusPickerAreas {
    let available = parent.height.saturating_sub(STATUS_ROWS);
    let height = requested_result_rows.saturating_add(4).min(available);
    let picker = Rect {
        x: parent.x,
        y: parent.y + available.saturating_sub(height),
        width: parent.width,
        height,
    };
    frame.render_widget(Clear, picker);
    frame.render_widget(Block::default().style(theme::text_on_canvas()), picker);

    let result_rows = height.saturating_sub(4);
    let middle_y = picker.y.saturating_add(1).saturating_add(result_rows);
    for y in [picker.y, middle_y, picker.y + height.saturating_sub(1)] {
        frame.render_widget(
            Paragraph::new(Line::from("─".repeat(picker.width as usize)).style(theme::rule())),
            Rect {
                x: picker.x,
                y,
                width: picker.width,
                height: 1,
            },
        );
    }

    let gutter = CONTENT_GUTTER.min(picker.width / 2);
    FocusPickerAreas {
        results: Rect {
            x: picker.x + gutter,
            y: picker.y.saturating_add(1),
            width: picker
                .width
                .saturating_sub(gutter.saturating_mul(2))
                .min(MAX_CONTENT_WIDTH),
            height: result_rows,
        },
        query: Rect {
            x: picker.x,
            y: middle_y.saturating_add(1),
            width: picker.width,
            height: 1,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend};

    #[test]
    fn dock_leaves_status_row_and_stays_left_anchored() {
        let backend = TestBackend::new(240, 40);
        let mut terminal = Terminal::new(backend).expect("terminal");
        let mut body = Rect::default();
        terminal
            .draw(|frame| body = decision_dock(frame, frame.area(), 8))
            .expect("draw");
        assert_eq!(body.x, 2);
        assert_eq!(body.width, 110);
        assert_eq!(body.y, 30);
        assert_eq!(body.height, 8);
    }

    #[test]
    fn picker_uses_composer_row_and_preserves_status() {
        let backend = TestBackend::new(120, 30);
        let mut terminal = Terminal::new(backend).expect("terminal");
        let mut query = Rect::default();
        terminal
            .draw(|frame| query = focus_picker(frame, frame.area(), 8).query)
            .expect("draw");
        assert_eq!(query.x, 0);
        assert_eq!(query.y, 27);
        assert_eq!(query.width, 120);
    }

    #[test]
    fn review_owns_viewport_but_not_status() {
        let backend = TestBackend::new(120, 30);
        let mut terminal = Terminal::new(backend).expect("terminal");
        let mut footer = Rect::default();
        terminal
            .draw(|frame| footer = review_view(frame, frame.area(), 2).footer)
            .expect("draw");
        assert_eq!(footer.y, 27);
        assert_eq!(footer.height, 2);
        assert_eq!(footer.width, 120);
    }
}
