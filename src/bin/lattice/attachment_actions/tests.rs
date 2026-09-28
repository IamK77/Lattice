use super::*;

#[test]
fn a_clipboard_picture_is_named_by_something_about_itself() {
    let png = |w: u32, h: u32| {
        let mut v = b"\x89PNG\r\n\x1a\n".to_vec();
        v.extend_from_slice(&13u32.to_be_bytes());
        v.extend_from_slice(b"IHDR");
        v.extend_from_slice(&w.to_be_bytes());
        v.extend_from_slice(&h.to_be_bytes());
        v.extend_from_slice(&[8, 2, 0, 0, 0]);
        v
    };
    assert_eq!(png_dimensions(&png(1200, 800)), Some((1200, 800)));
    assert_eq!(clipboard_label(&png(1200, 800)), "clipboard 1200\u{d7}800");
    assert_eq!(png_dimensions(b"not a png at all........"), None);
    assert_eq!(png_dimensions(&png(1200, 800)[..20]), None);
    assert_eq!(png_dimensions(&png(0, 800)), None, "a zero size is no size");
    assert_eq!(clipboard_label(&vec![0u8; 2048]), "clipboard 2 KB");
}

#[test]
fn a_dragged_image_path_is_recognised_and_other_pastes_are_not() {
    let dir = tempfile::tempdir().unwrap();
    let png = dir.path().join("a shot.png");
    std::fs::write(&png, [0x89, b'P', b'N', b'G']).unwrap();
    let shown = png.display().to_string();
    for form in [
        shown.clone(),
        format!("  {shown}  "),
        format!("'{shown}'"),
        format!("\"{shown}\""),
        shown.replace(' ', "\\ "),
    ] {
        let (at, media) =
            dragged_image(&form).unwrap_or_else(|| panic!("{form:?} is a dragged picture"));
        assert_eq!(at, png);
        assert_eq!(media, "image/png");
    }
    let text = dir.path().join("notes.txt");
    std::fs::write(&text, "hi").unwrap();
    for not in [
        text.display().to_string(),
        dir.path().join("gone.png").display().to_string(),
        format!("look at {shown}"),
        format!("{shown}\nand more"),
        "just typing".to_string(),
    ] {
        assert!(
            dragged_image(&not).is_none(),
            "{not:?} is not an attachment"
        );
    }
}

#[test]
fn only_known_image_kinds_are_offered() {
    assert_eq!(image_media_type("a.png"), Some("image/png"));
    assert_eq!(image_media_type("a.JPG"), Some("image/jpeg"));
    assert_eq!(image_media_type("a.jpeg"), Some("image/jpeg"));
    assert_eq!(image_media_type("a.webp"), Some("image/webp"));
    for no in ["a.mov", "a.pdf", "a.txt", "a.png.zip", "png", "a."] {
        assert_eq!(image_media_type(no), None, "{no}");
    }
}

#[test]
fn file_labels_preserve_paths_and_reject_missing_files_and_directories() {
    let dir = tempfile::tempdir().unwrap();
    let at = dir.path().join("notes.md");
    std::fs::write(&at, "x".repeat(2048)).unwrap();
    let path = at.display().to_string();
    assert_eq!(
        dragged_file(&path),
        Some((path.clone(), "notes.md (2 KB)".into()))
    );
    assert!(dragged_file(&dir.path().join("no-such-file").display().to_string()).is_none());
    assert!(dragged_file(&dir.path().display().to_string()).is_none());
}
