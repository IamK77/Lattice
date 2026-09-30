use super::*;
use ratatui::crossterm::event::KeyEvent;

#[test]
fn expert_completion_uses_exp_without_taking_the_existing_ex_exit_prefix() {
    for command in ["/exp", "/experts"] {
        let mut ui = Ui::replayed(&[]);
        ui.draft.edit().set(command);
        assert!(!on_key(
            &mut ui,
            None,
            KeyEvent::from(KeyCode::Enter),
            &Hit::default()
        ));
        assert_eq!(ui.panel.active(), Some(panels::AT_EXPERTS));
    }
}

#[test]
fn expert_panel_keyboard_and_multiline_render_preserve_the_conversation_draft() {
    let mut ui = Ui::replayed(&[]);
    ui.draft.edit().set("unsent conversation");
    run_slash(&mut ui, "/experts", None);
    assert_eq!(ui.panel.active(), Some(panels::AT_EXPERTS));
    assert!(!ui.busy());
    on_key(
        &mut ui,
        None,
        KeyEvent::from(KeyCode::Char('n')),
        &Hit::default(),
    );
    for _ in 0..5 {
        on_key(&mut ui, None, KeyEvent::from(KeyCode::Tab), &Hit::default());
    }
    ui.experts
        .paste("Check the boundary.\n保留第二行。\nDo not silently retry.");
    for width in [30, 60, 100] {
        let lines = panel_lines(panels::AT_EXPERTS, &ui, width);
        let text = lines
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("保留第二行。"));
        assert!(text.contains("F2"));
        for line in lines {
            assert!(lattice::wrap::str_cols(&line.to_string()) <= width);
        }
    }
    let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(100, 40)).unwrap();
    draw(&mut terminal, &ui).unwrap();
    let screen: String = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect();
    assert!(screen.contains("Experts"));
    // TestBackend retains a padding cell after each double-width character.
    assert!(screen.replace(' ', "").contains("保留第二行。"), "{screen}");
    on_key(&mut ui, None, KeyEvent::from(KeyCode::Esc), &Hit::default());
    assert_eq!(ui.panel.active(), Some(panels::AT_EXPERTS));
    on_key(&mut ui, None, KeyEvent::from(KeyCode::Esc), &Hit::default());
    assert!(!ui.panel.is_visible());
    assert_eq!(ui.draft.editor().text(), "unsent conversation");
}
