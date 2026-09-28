//! Entry-level ordering and side-effect baselines for attachment extraction.
use super::*;

fn fill_placeholders(ui: &mut Ui) -> usize {
    (0..1024)
        .find(|id| !ui.draft.edit().attach("occupied", *id))
        .expect("bounded editor")
}

#[test]
fn attachment_capacity_failure_keeps_the_written_file_and_registered_reference() {
    let docs = tempfile::tempdir().unwrap();
    let mut ui = Ui::replayed(&[]);
    ui.documents = Some(docs.path().to_path_buf());
    let count = fill_placeholders(&mut ui);
    attach_bytes(
        &mut ui,
        b"stored before capacity check",
        "png",
        "image/png",
        "overflow",
    );
    assert_eq!(
        ui.flash.as_deref(),
        Some("too many attachments on one line")
    );
    assert_eq!(ui.draft.editor().images().len(), count);
    assert_eq!(ui.draft.references().len(), 1);
    let file = ui.draft.references()[0]["file"].as_str().unwrap();
    assert_eq!(
        std::fs::read(docs.path().join(file)).unwrap(),
        b"stored before capacity check"
    );
}

#[test]
fn full_file_chip_capacity_falls_back_to_the_original_quoted_paste() {
    let home = tempfile::tempdir().unwrap();
    let path = home.path().join("a document.txt");
    std::fs::write(&path, b"not an image").unwrap();
    let pasted = format!("  '{}'  ", path.display());
    let mut ui = Ui::replayed(&[]);
    fill_placeholders(&mut ui);
    let before = ui.draft.editor().expanded().into_owned();
    absorb_paste(&mut ui, &pasted);
    assert_eq!(ui.draft.editor().expanded(), format!("{before}{pasted}"));
    assert!(ui.draft.references().is_empty());
}

#[cfg(target_os = "macos")]
#[test]
fn clipboard_output_and_modal_key_routing_use_only_a_private_fake_command() {
    test_support::isolated(|| {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let command = root.path().join("osascript");
        let output = root.path().join("output");
        let calls = root.path().join("calls");
        std::fs::write(&command, "#!/bin/sh\nprintf '%s\\n' called >> \"$CLIPBOARD_TEST_CALLS\"\n/bin/cat \"$CLIPBOARD_TEST_OUTPUT\"\nexit \"$CLIPBOARD_TEST_STATUS\"\n").unwrap();
        std::fs::set_permissions(&command, std::fs::Permissions::from_mode(0o700)).unwrap();
        // This closure runs in the isolated test child, never the parent suite.
        std::env::set_var("PATH", root.path());
        std::env::set_var("CLIPBOARD_TEST_CALLS", &calls);
        std::env::set_var("CLIPBOARD_TEST_OUTPUT", &output);
        for (text, status, accepted) in [
            ("not an image", "0", false),
            ("«data PNGf1234»", "0", false),
            ("«data PNGf89504e470d0a1a0»", "0", false),
            ("«data PNGf89504e470d0a1a0a»", "1", false),
            ("«data PNGf89504e470d0a1a0a»", "0", true),
            ("«data PNGf89504e470d0a1a0agg trailing»", "0", true),
        ] {
            std::fs::write(&output, text).unwrap();
            std::env::set_var("CLIPBOARD_TEST_STATUS", status);
            let docs = tempfile::tempdir().unwrap();
            let mut ui = Ui::replayed(&[]);
            ui.documents = Some(docs.path().to_path_buf());
            attach_from_clipboard(&mut ui);
            if accepted {
                assert_eq!(ui.draft.references().len(), 1);
                let reference = &ui.draft.references()[0];
                assert_eq!(reference["name"], "clipboard 8 B");
                assert_eq!(
                    std::fs::read(docs.path().join(reference["file"].as_str().unwrap())).unwrap(),
                    b"\x89PNG\r\n\x1a\n"
                );
                assert!(ui.flash.is_none());
            } else {
                assert!(ui.draft.references().is_empty());
                assert_eq!(ui.flash.as_deref(), Some("no picture on the clipboard"));
            }
        }
        let before = std::fs::read_to_string(&calls).unwrap();
        for mode in 0..5 {
            let mut ui = Ui::replayed(&[]);
            match mode {
                0 => ui.controls.open_form(),
                1 => ui.panel.show(AT_MODELS),
                2 => ui.controls.open_dial(&EffortView::default()),
                3 => ui.controls.seed_picker(0),
                _ => ui.controls.ask_delete("model".into()),
            }
            on_key(
                &mut ui,
                None,
                ratatui::crossterm::event::KeyEvent::new(KeyCode::Char('v'), KeyModifiers::CONTROL),
                &Hit::default(),
            );
            assert!(ui.draft.references().is_empty());
            assert_eq!(std::fs::read_to_string(&calls).unwrap(), before);
        }
        let mut ui = Ui::replayed(&[]);
        on_key(
            &mut ui,
            None,
            ratatui::crossterm::event::KeyEvent::new(KeyCode::Char('v'), KeyModifiers::CONTROL),
            &Hit::default(),
        );
        assert_eq!(
            std::fs::read_to_string(&calls).unwrap(),
            format!("{before}called\n")
        );
    });
}

fn rejecting_ui() -> Ui {
    let mut ui = Ui::replayed(&[]);
    *ui.domain.model.fixture_catalog() = ModelView {
        rows: vec![ModelRow {
            id: "text-only".into(),
            accepts_images: false,
            ..ModelRow::default()
        }],
        now: Some(0),
    };
    ui
}

#[test]
fn attachment_failures_keep_read_capability_and_storage_order() {
    let home = tempfile::tempdir().unwrap();
    let missing = home.path().join("missing.png");
    let mut ui = rejecting_ui();
    attach_image(&mut ui, &missing, "image/png");
    assert!(ui.flash.as_deref().unwrap().starts_with("cannot read "));
    assert!(ui.draft.references().is_empty());

    std::fs::write(&missing, b"image bytes").unwrap();
    attach_image(&mut ui, &missing, "image/png");
    assert!(ui
        .flash
        .as_deref()
        .unwrap()
        .contains("text-only cannot read pictures"));
    assert!(ui.draft.references().is_empty());

    ui.domain.model.fixture_catalog().now = None;
    attach_image(&mut ui, &missing, "image/png");
    assert_eq!(
        ui.flash.as_deref(),
        Some("this conversation has no ledger, so it cannot hold a picture")
    );

    let docs = home.path().join("documents");
    std::fs::create_dir(&docs).unwrap();
    ui.documents = Some(docs.clone());
    ui.flash = Some("keep the previous notice".into());
    attach_image(&mut ui, &missing, "image/png");
    assert_eq!(
        ui.draft.references().len(),
        1,
        "an unknown current model is not a refusal"
    );
    assert_eq!(ui.flash.as_deref(), Some("keep the previous notice"));
    assert_eq!(std::fs::read_dir(&docs).unwrap().count(), 1);
}

#[test]
fn attachment_storage_failure_does_not_modify_the_draft() {
    let home = tempfile::tempdir().unwrap();
    let not_a_directory = home.path().join("file");
    std::fs::write(&not_a_directory, b"untouched").unwrap();
    let mut ui = Ui::replayed(&[]);
    ui.documents = Some(not_a_directory.clone());
    ui.draft.edit().insert_str("keep my draft");
    attach_bytes(&mut ui, b"picture", "png", "image/png", "test");
    assert!(ui.flash.is_some());
    assert_eq!(ui.draft.editor().expanded(), "keep my draft");
    assert!(ui.draft.references().is_empty());
    assert_eq!(std::fs::read(not_a_directory).unwrap(), b"untouched");
}

#[test]
fn pasted_image_refusal_is_not_silently_converted_to_a_text_path() {
    let home = tempfile::tempdir().unwrap();
    let image = home.path().join("a shot.PNG");
    std::fs::write(&image, b"picture").unwrap();
    let docs = home.path().join("documents");
    std::fs::create_dir(&docs).unwrap();
    let mut ui = rejecting_ui();
    ui.documents = Some(docs.clone());
    ui.draft.edit().insert_str("keep my draft");
    absorb_paste(&mut ui, &image.to_string_lossy());
    assert!(ui
        .flash
        .as_deref()
        .unwrap()
        .contains("cannot read pictures"));
    assert_eq!(ui.draft.editor().expanded(), "keep my draft");
    assert!(ui.draft.references().is_empty());
    assert_eq!(std::fs::read_dir(docs).unwrap().count(), 0);
}

#[test]
fn modal_paste_routes_before_any_attachment_write() {
    let home = tempfile::tempdir().unwrap();
    let image = home.path().join("shot.png");
    std::fs::write(&image, b"picture").unwrap();
    let docs = home.path().join("documents");
    std::fs::create_dir(&docs).unwrap();
    for mode in 0..5 {
        let mut ui = Ui::replayed(&[]);
        ui.documents = Some(docs.clone());
        ui.flash = Some("unchanged".into());
        match mode {
            0 => ui.controls.open_form(),
            1 => ui.panel.show(AT_MODELS),
            2 => ui.controls.open_dial(&EffortView::default()),
            3 => ui.controls.seed_picker(0),
            _ => ui.controls.ask_delete("model".into()),
        }
        absorb_paste(&mut ui, &image.to_string_lossy());
        assert!(ui.draft.editor().is_empty());
        assert!(ui.draft.references().is_empty());
        assert_eq!(std::fs::read_dir(&docs).unwrap().count(), 0);
        assert_eq!(ui.flash.as_deref(), Some("unchanged"));
        if mode == 0 {
            assert_eq!(
                ui.controls.form().unwrap().values[0],
                image.to_string_lossy()
            );
        }
    }
}
