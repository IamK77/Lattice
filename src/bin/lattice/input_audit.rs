//! Regressions retained from the 2026-09-14 input audit.

use super::*;

#[test]
fn audit_input_literal_private_character_is_not_a_fold_reference() {
    let mut editor = Editor::new();
    let body = "x".repeat(801);
    editor.paste(&body);
    editor.paste("\u{e000}");
    assert_eq!(editor.expanded(), format!("{body}\u{e000}"));
}

#[test]
fn audit_input_deleted_attachment_cannot_be_resurrected_by_paste() {
    let mut editor = Editor::new();
    editor.attach("audit.png", 7);
    editor.backspace();
    assert!(editor.images().is_empty());
    editor.paste("\u{e000}");
    assert!(editor.images().is_empty(), "Pasted text reattached image 7");
}

#[test]
fn audit_input_form_paste_does_not_enter_hidden_chat_buffer() {
    let mut ui = Ui::replayed(&[]);
    ui.controls.open_form();
    absorb_paste(&mut ui, "audit-form-value-not-a-real-secret");
    assert!(
        ui.draft.editor().text().is_empty(),
        "Form paste reached the chat editor"
    );
    assert_eq!(
        ui.controls.form().unwrap().values[0],
        "audit-form-value-not-a-real-secret"
    );
    absorb_paste(&mut ui, "bad\r\nvalue");
    assert_eq!(
        ui.controls.form().unwrap().values[0],
        "audit-form-value-not-a-real-secret"
    );
    assert!(ui.controls.form().unwrap().problem.is_some());
}

#[test]
fn every_modal_view_blocks_hidden_chat_paste() {
    for mode in 0..4 {
        let mut ui = Ui::replayed(&[]);
        match mode {
            0 => ui.panel.show(AT_MODELS),
            1 => ui.controls.open_dial(&EffortView::default()),
            2 => ui.controls.seed_picker(0),
            _ => ui.controls.ask_delete("not-a-real-model".into()),
        }
        absorb_paste(&mut ui, "not chat input");
        assert!(ui.draft.editor().is_empty());
    }
}

fn press(ui: &mut Ui, code: KeyCode, modifiers: KeyModifiers) {
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(40, 24)).unwrap();
    let hit = draw(&mut terminal, ui).unwrap();
    on_key(
        ui,
        None,
        ratatui::crossterm::event::KeyEvent::new(code, modifiers),
        &hit,
    );
}

#[test]
fn audit_input_option_enter_then_up_returns_to_first_line() {
    let mut ui = Ui::replayed(&[]);
    ui.draft.edit().insert_str("first");
    press(&mut ui, KeyCode::Enter, KeyModifiers::ALT);
    assert_eq!(ui.draft.editor().text(), "first\n");
    press(&mut ui, KeyCode::Up, KeyModifiers::NONE);
    press(&mut ui, KeyCode::Char('X'), KeyModifiers::NONE);
    assert_eq!(ui.draft.editor().text(), "Xfirst\n");
}

#[test]
fn audit_input_up_after_multiline_paste_keeps_the_current_draft() {
    let mut ui = Ui::replayed(&[]);
    ui.draft.edit().insert_str("older message");
    ui.draft.edit().submit();
    absorb_paste(&mut ui, "first\nsecond");
    press(&mut ui, KeyCode::Up, KeyModifiers::NONE);
    assert_eq!(ui.draft.editor().text(), "first\nsecond");
}

#[test]
fn arrows_follow_soft_wrapped_rows_at_the_drawn_width() {
    let mut ui = Ui::replayed(&[]);
    ui.draft.edit().insert_str(&"x".repeat(40));
    press(&mut ui, KeyCode::Up, KeyModifiers::NONE);
    assert_eq!(ui.draft.editor().cursor(), 4);
    press(&mut ui, KeyCode::Down, KeyModifiers::NONE);
    assert_eq!(ui.draft.editor().cursor(), 40);
}

#[test]
fn audit_paste_line_endings_preserve_buffer_contents() {
    for text in [
        "A_FIRST\nB_SECOND",
        "A_FIRST\r\nB_SECOND",
        "A_FIRST\rB_SECOND",
        "A_FIRST\n\tB_SECOND",
        "A_FIRST\nB_SECOND\n\nC_THIRD\nD_LAST",
    ] {
        let mut ui = Ui::replayed(&[]);
        absorb_paste(&mut ui, text);
        assert_eq!(
            ui.draft.editor().expanded(),
            text,
            "Paste altered buffer contents"
        );
        let _ = render_input(&ui);
        assert_eq!(
            ui.draft.editor().expanded(),
            text,
            "Drawing altered buffer contents"
        );
        assert_eq!(
            ui.draft.edit().submit(),
            text,
            "Editor submission altered contents"
        );
    }
}

#[test]
fn audit_paste_carriage_return_lines_render_like_line_feed() {
    let mut expected = Ui::replayed(&[]);
    absorb_paste(&mut expected, "A_FIRST\nB_SECOND");
    for text in ["A_FIRST\rB_SECOND", "A_FIRST\r\nB_SECOND"] {
        let mut actual = Ui::replayed(&[]);
        absorb_paste(&mut actual, text);
        assert_eq!(render_input(&actual), render_input(&expected));
    }
}

#[test]
fn pasted_controls_cannot_move_the_real_terminal_cursor() {
    let mut ui = Ui::replayed(&[]);
    absorb_paste(&mut ui, "first\r\n\tsecond\x1b[2J");
    let buffer = input_buffer(&ui, 40, 24);
    let mut bytes = Vec::new();
    let mut backend = ratatui::backend::CrosstermBackend::new(&mut bytes);
    ratatui::backend::Backend::draw(
        &mut backend,
        buffer
            .content
            .iter()
            .enumerate()
            .map(|(i, cell)| ((i % 40) as u16, (i / 40) as u16, cell)),
    )
    .unwrap();
    assert!(!bytes.contains(&b'\r'));
    assert!(!bytes.contains(&b'\t'));
    assert!(!bytes.windows(4).any(|s| s == b"\x1b[2J"));
}

fn input_screen(text: &str) -> String {
    let mut ui = Ui::replayed(&[]);
    ui.draft.edit().insert_str(text);
    render_input(&ui).join("\n")
}

fn input_buffer(ui: &Ui, width: u16, height: u16) -> ratatui::buffer::Buffer {
    let backend = ratatui::backend::TestBackend::new(width, height);
    let mut terminal = ratatui::Terminal::new(backend).unwrap();
    draw(&mut terminal, ui).unwrap();
    terminal.backend().buffer().clone()
}

fn render_input(ui: &Ui) -> Vec<String> {
    input_buffer(ui, 40, 24)
        .content
        .chunks(40)
        .map(|row| row.iter().map(|cell| cell.symbol()).collect())
        .collect()
}

#[test]
fn audit_input_long_line_keeps_typed_tail_visible() {
    assert!(input_screen(&format!("{}TAIL", "x".repeat(90))).contains("TAIL"));
}

#[test]
fn audit_input_more_than_six_lines_keeps_typed_tail_visible() {
    assert!(input_screen("a\nb\nc\nd\ne\nf\ng\nTAIL").contains("TAIL"));
}

#[test]
fn a_tiny_terminal_and_huge_input_do_not_overflow_cursor_coordinates() {
    let mut ui = Ui::replayed(&[]);
    ui.draft.edit().insert_str(&"x".repeat(70_000));
    for (width, height) in [(1, 1), (3, 3), (40, 5), (80, 24)] {
        let _ = input_buffer(&ui, width, height);
    }
}
