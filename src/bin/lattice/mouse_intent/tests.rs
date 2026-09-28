use super::*;
use ratatui::crossterm::event::KeyModifiers;

#[test]
fn geometry_uses_half_open_rectangles_offsets_and_first_matches() {
    let rect = Rect::new(2, 3, 4, 2);
    let owners = vec![
        Some("hidden".into()),
        Some("first".into()),
        Some("second".into()),
    ];
    let links = vec![
        (rect, "https://first.example/".into()),
        (rect, "https://second.example/".into()),
    ];
    let status = vec![(rect, Opens::Context), (rect, Opens::Config)];
    let mut targets = Targets {
        area: rect,
        offset: 1,
        owner: &owners,
        jump: None,
        status: &status,
        links: &links,
    };
    assert_eq!(targets.card_at(2, 3).as_deref(), Some("first"));
    assert_eq!(targets.card_at(5, 4).as_deref(), Some("second"));
    for (col, row) in [(1, 3), (6, 3), (2, 2), (2, 5)] {
        assert!(targets.card_at(col, row).is_none());
        assert!(targets.status_at(col, row).is_none());
        assert!(targets.url_at(col, row).is_none());
    }
    assert_eq!(targets.status_at(2, 3), Some(Opens::Context));
    assert_eq!(targets.url_at(2, 3), Some("https://first.example/"));
    targets.jump = Some(rect);
    assert!(targets.url_at(2, 3).is_none());
}

#[test]
fn links_precede_cards_even_with_a_panel_and_modified_click() {
    let rect = Rect::new(2, 3, 4, 2);
    let links = vec![(rect, "https://first.example/".into())];
    let owners = vec![Some("card".into())];
    let targets = Targets {
        area: rect,
        offset: 0,
        owner: &owners,
        jump: None,
        status: &[],
        links: &links,
    };
    let event = MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: 2,
        row: 3,
        modifiers: KeyModifiers::CONTROL,
    };
    assert_eq!(
        interpret(event, true, targets),
        Some(Intent::Link("https://first.example/".into()))
    );
}
