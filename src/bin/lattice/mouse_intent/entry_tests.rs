use super::*;
use ratatui::{crossterm::event::MouseEvent, layout::Rect};

fn event(kind: MouseEventKind) -> MouseEvent {
    MouseEvent {
        kind,
        column: 2,
        row: 2,
        modifiers: KeyModifiers::CONTROL,
    }
}
fn targets() -> Hit {
    Hit {
        area: Rect::new(1, 1, 8, 4),
        owner: vec![Some("card".into()); 30],
        ..Hit::default()
    }
}

#[test]
fn mouse_wheel_routes_to_the_visible_panel_even_outside_its_area() {
    let mut ui = Ui::replayed(&[]);
    let hit = targets();
    ui.browsing.set_offset(9);
    ui.panel.show(AT_CONTEXT);
    ui.panel.scroll_down(8);
    ui.panel.select_row(2);
    ui.panel.toggle_details();
    let mut wheel = event(MouseEventKind::ScrollUp);
    wheel.column = 100;
    wheel.row = 100;
    on_mouse(&mut ui, wheel, &hit);
    assert_eq!(ui.panel.scroll_offset(), 5);
    assert_eq!(ui.browsing.offset(), 9);
    wheel.kind = MouseEventKind::ScrollDown;
    on_mouse(&mut ui, wheel, &hit);
    assert_eq!(ui.panel.scroll_offset(), 8);
    assert_eq!(ui.panel.selected_row(), 2);
    assert!(ui.panel.details_expanded());
    ui.panel.dismiss_preserving_details();
    on_mouse(&mut ui, event(MouseEventKind::ScrollUp), &hit);
    assert_eq!(ui.browsing.offset(), 12);
    on_mouse(&mut ui, event(MouseEventKind::ScrollDown), &hit);
    assert_eq!(ui.browsing.offset(), 9);
    ui.browsing.set_offset(0);
    on_mouse(&mut ui, event(MouseEventKind::ScrollDown), &hit);
    assert_eq!(ui.browsing.offset(), 0);
}

#[test]
fn mouse_overlaps_execute_only_status_then_jump_then_link_then_card() {
    let rect = Rect::new(1, 1, 8, 4);
    for layer in 0..4 {
        let mut ui = Ui::replayed(&[]);
        ui.flash = Some("old notice".into());
        ui.browsing.set_offset(9);
        let mut hit = targets();
        if layer == 0 {
            hit.status.push((rect, Opens::Context));
        }
        if layer <= 1 {
            hit.jump = Some(rect);
        }
        if layer <= 2 {
            hit.links.push((rect, "file:///not-opened".into()));
        }
        on_mouse(
            &mut ui,
            event(MouseEventKind::Down(MouseButton::Left)),
            &hit,
        );
        assert_eq!(
            ui.panel.active(),
            if layer == 0 { Some(AT_CONTEXT) } else { None }
        );
        assert_eq!(ui.browsing.offset(), if layer == 1 { 0 } else { 9 });
        assert_eq!(ui.browsing.is_expanded("card"), layer == 3);
        assert_eq!(
            ui.flash.as_deref(),
            Some(if layer == 2 {
                "Only HTTP(S) links without credentials can be opened"
            } else {
                "old notice"
            })
        );
    }
}

#[test]
fn mouse_ignored_events_and_blank_clicks_keep_the_notice_and_draft() {
    for kind in [
        MouseEventKind::Down(MouseButton::Right),
        MouseEventKind::Up(MouseButton::Left),
        MouseEventKind::Drag(MouseButton::Left),
        MouseEventKind::Moved,
        MouseEventKind::Down(MouseButton::Left),
    ] {
        let mut ui = Ui::replayed(&[]);
        ui.flash = Some("old notice".into());
        ui.draft.edit().set("draft");
        ui.browsing.set_offset(9);
        on_mouse(&mut ui, event(kind), &Hit::default());
        assert_eq!(ui.flash.as_deref(), Some("old notice"));
        assert_eq!(ui.draft.editor().text(), "draft");
        assert_eq!(ui.browsing.offset(), 9);
        assert_eq!(ui.panel.active(), None);
    }
}
