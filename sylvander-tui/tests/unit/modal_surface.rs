use super::*;
use ratatui::{Terminal, backend::TestBackend};

#[test]
fn dock_fills_assigned_row_and_stays_left_anchored() {
    let backend = TestBackend::new(240, 10);
    let mut terminal = Terminal::new(backend).expect("terminal");
    let mut body = Rect::default();
    terminal
        .draw(|frame| body = decision_dock(frame, frame.area(), 8))
        .expect("draw");
    assert_eq!(body.x, 2);
    assert_eq!(body.width, 110);
    assert_eq!(body.y, 0);
    assert_eq!(body.height, 8);
}

#[test]
fn picker_fills_its_assigned_below_composer_row() {
    let backend = TestBackend::new(120, 12);
    let mut terminal = Terminal::new(backend).expect("terminal");
    let mut query = Rect::default();
    terminal
        .draw(|frame| query = focus_picker(frame, frame.area(), 8).query)
        .expect("draw");
    assert_eq!(query.x, 0);
    assert_eq!(query.y, 9);
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
