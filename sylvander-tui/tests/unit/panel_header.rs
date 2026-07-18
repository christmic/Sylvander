use super::*;

#[test]
fn mode_label_three_branches() {
    // mode_label derives a static string from `state.mode`. The
    // disconnected label is a fourth branch drawn directly by
    // `render()` when state.connected is false.
    let mut s = crate::app::AppState::new();
    s.apply(crate::event::DomainEvent::Connected);
    assert_eq!(mode_label(&s), "main");
    s.mode = crate::app::AppMode::ApprovalPending;
    assert_eq!(mode_label(&s), "approval");
    s.mode = crate::app::AppMode::AskPending;
    assert_eq!(mode_label(&s), "ask");
}

#[test]
fn header_renders_brand_when_connected() {
    let mut s = crate::app::AppState::new();
    s.apply(crate::event::DomainEvent::Connected);
    let mut t =
        ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 6)).expect("terminal");
    t.draw(|f| {
        super::HeaderPanel.render(f, ratatui::layout::Rect::new(0, 0, 120, 6), &s);
    })
    .unwrap();
    let buf = t.backend().buffer().clone();
    // Assert the compact Seed-Crab slash or the wordmark survives.
    let mut found_brand = false;
    for y in 0..6 {
        for x in 0..120 {
            if let Some(c) = buf.cell((x, y)) {
                let sym = c.symbol();
                if sym == "\u{25e6}" || sym == "S" || sym.contains("SYLVANDER") {
                    found_brand = true;
                    break;
                }
            }
        }
        if found_brand {
            break;
        }
    }
    assert!(
        found_brand,
        "expected brand mark or wordmark in connected header"
    );
}

#[test]
fn header_renders_no_crab_when_disconnected() {
    let s = crate::app::AppState::new();
    // No Connected event applied → state.connected stays false.
    let mut t =
        ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 6)).expect("terminal");
    t.draw(|f| {
        super::HeaderPanel.render(f, ratatui::layout::Rect::new(0, 0, 120, 6), &s);
    })
    .unwrap();
    let buf = t.backend().buffer().clone();
    for y in 0..6 {
        for x in 0..120 {
            if let Some(c) = buf.cell((x, y)) {
                assert_ne!(
                    c.symbol(),
                    "\u{25e6}",
                    "header should not render crab when disconnected"
                );
            }
        }
    }
}

#[test]
fn header_carries_hairline_rule_on_row_2() {
    let mut s = crate::app::AppState::new();
    s.apply(crate::event::DomainEvent::Connected);
    let mut t =
        ratatui::Terminal::new(ratatui::backend::TestBackend::new(40, 6)).expect("terminal");
    t.draw(|f| {
        super::HeaderPanel.render(f, ratatui::layout::Rect::new(0, 0, 40, 6), &s);
    })
    .unwrap();
    let buf = t.backend().buffer().clone();
    // Hairline is at row 2 (line 1 of inner header), per the
    // 02-tui-immersive.svg ground truth.
    let hairline_cell = buf.cell((0, 2)).expect("hairline cell");
    assert_eq!(
        hairline_cell.symbol(),
        "\u{2500}",
        "expected ─ (hairline) on row 2"
    );
    assert_eq!(
        hairline_cell.fg,
        crate::theme::RULE,
        "hairline must use the RULE color"
    );
}
