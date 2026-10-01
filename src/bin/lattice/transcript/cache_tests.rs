use crate::terminal_host::*;
use ratatui::{backend::TestBackend, crossterm::event::KeyEvent};

fn assert_matches_uncached(ui: &mut Ui, terminal: &mut Terminal<TestBackend>) {
    draw_ui(terminal, ui).unwrap();
    let area = terminal.backend().buffer().area;
    let mut reference = Terminal::new(TestBackend::new(area.width, area.height)).unwrap();
    draw(&mut reference, ui).unwrap();
    assert_eq!(terminal.backend().buffer(), reference.backend().buffer());
}

#[test]
fn viewport_reuses_large_work_groups_during_typing_animation_and_streaming() {
    let mut ui = Ui::replayed(&[]);
    ui.push_local_card(Entry::User("⠋ this is literal user text".into()));
    for n in 0..512 {
        ui.push_local_card(Entry::Tool(view::ToolCard {
            call: Some(format!("tool-{n}")),
            name: "Run".into(),
            args: json!({"command":format!("printf step-{n}")}),
            status: view::ToolStatus::Ok,
            output: vec!["output".repeat(100); 6],
            changed: None,
            edit_diff: None,
        }));
    }
    ui.push_local_card(Entry::Tool(view::ToolCard {
        call: Some("pending".into()),
        name: "Run".into(),
        args: json!({"command":"⠋ do not animate this argument"}),
        status: view::ToolStatus::Running,
        output: vec![],
        changed: None,
        edit_diff: None,
    }));
    ui.domain.turns.optimistic_activity();
    let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
    let start = std::time::Instant::now();
    draw_ui(&mut terminal, &mut ui).unwrap();
    let cold = start.elapsed();
    let builds = ui.transcript_cache.borrow().builds;
    let groups = ui.transcript_cache.borrow().group_builds();
    let start = std::time::Instant::now();
    for _ in 0..20 {
        on_key(
            &mut ui,
            None,
            KeyEvent::from(KeyCode::Char('a')),
            &Hit::default(),
        );
        ui.tick += 1;
        draw_ui(&mut terminal, &mut ui).unwrap();
    }
    eprintln!(
        "viewport cache: cold={cold:?}, 20 typing/animation frames={:?}",
        start.elapsed()
    );
    assert_eq!(
        ui.transcript_cache.borrow().builds,
        builds,
        "typing and animation must not rebuild history"
    );
    assert_matches_uncached(&mut ui, &mut terminal);
    for _ in 0..3 {
        fold_render(
            &mut ui,
            RenderEvent::Notice {
                source: "model".into(),
                payload: json!({"phase":"reasoning","chunk":"thinking "}),
            },
        )
        .unwrap();
        ui.tick += 1;
        assert_matches_uncached(&mut ui, &mut terminal);
    }
    assert_eq!(
        ui.transcript_cache.borrow().group_builds(),
        groups,
        "streaming must not lay out unchanged committed work again"
    );
    // Invalidate in place: completion changes no entry count.
    let completed = lattice::EventEnvelope {
        v: 1,
        id: "completed".into(),
        seq: 1,
        stream: "test".into(),
        time: "2026-09-30T00:00:00Z".into(),
        event_type: core_events::TOOL_EXEC_COMPLETED.into(),
        source: "test".into(),
        causes: vec![],
        origin: None,
        reason: None,
        payload: json!({"call":"pending","tool":"Run","status":"ok","result":{"stdout":"done"}}),
    };
    ui.absorb(&completed, ui.tick);
    assert_matches_uncached(&mut ui, &mut terminal);
    assert!(ui.transcript_cache.borrow().group_builds() > groups);
    ui.browsing.toggle("tool-0".into());
    assert_matches_uncached(&mut ui, &mut terminal);
    for width in [8, 42, 110] {
        terminal.backend_mut().resize(width, 30);
        terminal
            .resize(ratatui::layout::Rect::new(0, 0, width, 30))
            .unwrap();
        assert_matches_uncached(&mut ui, &mut terminal);
    }
    ui.clear_cards();
    assert_matches_uncached(&mut ui, &mut terminal);
}
