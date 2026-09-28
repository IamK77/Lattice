use super::*;

#[test]
fn key_paths_that_do_not_send_an_ordinary_message_keep_image_references() {
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    for case in 0..8 {
        let mut ui = crate::terminal_host::Ui::replayed(&[]);
        ui.domain.skills = vec![("research".into(), "research".into())];
        ui.draft.attach(stored("kept.png"), "image/png", "kept");
        let (key, modifiers) = match case {
            0 => (KeyCode::Char('c'), KeyModifiers::CONTROL),
            n => {
                if n != 6 {
                    ui.draft.edit().set(match n {
                        1 => "/he",
                        2 => "/res",
                        3 => "/clear",
                        4 => "",
                        5 => "question",
                        7 => "/research argument",
                        _ => unreachable!(),
                    });
                }
                (KeyCode::Enter, KeyModifiers::NONE)
            }
        };
        crate::terminal_host::on_key(
            &mut ui,
            None,
            KeyEvent::new(key, modifiers),
            &crate::terminal_host::Hit::default(),
        );
        assert_eq!(
            ui.draft.references().len(),
            1,
            "path {case} cleared references without a send"
        );
        assert!(
            ui.draft.editor().is_empty(),
            "path {case} did not clear submitted input"
        );
    }
}

fn stored(file: &str) -> DocRef {
    DocRef {
        file: file.into(),
        bytes: 10,
        lines: None,
        preview: None,
    }
}

#[test]
fn submission_captures_only_present_images_in_editor_order_before_clearing() {
    let mut draft = Draft::new();
    assert!(draft.attach(stored("aaaa.png"), "image/png", "first"));
    assert!(draft.attach(stored("bbbb.png"), "image/png", "second"));
    draft.edit().home();
    assert!(draft.attach(stored("cccc.png"), "image/png", "third"));
    draft.edit().end();
    draft.edit().backspace();
    let (text, kept) = draft.submit();
    assert!(text.is_empty());
    assert_eq!(kept, vec![2, 0]);
    assert!(draft.editor().is_empty());
    assert_eq!(
        draft.references().len(),
        3,
        "submission alone is not a send"
    );
    let images = draft.take_images(&kept);
    assert_eq!(
        images
            .iter()
            .map(|v| v["file"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["cccc.png", "aaaa.png"]
    );
    assert!(draft.references().is_empty());
}

#[test]
fn clearing_text_and_candidate_cursor_does_not_clear_stored_references() {
    let mut draft = Draft::new();
    draft.attach(stored("aaaa.png"), "image/png", "photo");
    draft.next_hint(3);
    draft.edit().clear();
    draft.reset_selection();
    assert_eq!(draft.selected(), 0);
    assert_eq!(draft.references().len(), 1);
    draft.attach(stored("bbbb.png"), "image/png", "photo");
    assert_eq!(draft.references()[1]["name"], "photo ·bbbb");
    draft.attach(stored("aaaa.png"), "image/png", "photo");
    // The earlier different file is stored with its suffixed label, so the
    // first content can continue to use the original label.
    assert_eq!(draft.references()[2]["name"], "photo");
    draft.previous_hint();
    assert_eq!(draft.selected(), 0);
    for _ in 0..5 {
        draft.next_hint(3);
    }
    assert_eq!(draft.selected(), 2);
    draft.previous_hint();
    assert_eq!(draft.selected(), 1);
}

#[test]
fn failed_placeholder_insertion_retains_the_reference_but_does_not_send_it() {
    let mut draft = Draft::new();
    // Ask the editor where its actual bound is, without copying its constant.
    let bound = (0..1024)
        .find(|id| {
            !draft.attach(
                stored(&format!("{id}.png")),
                "image/png",
                &format!("image {id}"),
            )
        })
        .expect("bounded editor");
    assert_eq!(draft.references().len(), bound + 1);
    assert_eq!(draft.editor().images().len(), bound);
    draft.edit().backspace();
    assert!(draft.attach(stored("replacement.png"), "image/png", "replacement"));
    let (_, kept) = draft.submit();
    assert_eq!(kept.last(), Some(&(bound + 1)));
    let images = draft.take_images(&kept);
    assert_eq!(images.len(), bound);
    assert!(images.iter().all(|v| v["file"] != format!("{bound}.png")));
    assert_eq!(images.last().unwrap()["file"], "replacement.png");
}
