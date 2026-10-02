use super::*;

#[test]
fn slash_candidates_narrow_as_you_type() {
    let effort = EffortView::default();
    for text in ["hello", "/help", "/help me", "/zzz"] {
        assert!(slash_matches(text, &[], &effort).is_empty(), "{text}");
    }
    assert_eq!(slash_matches("/", &[], &effort).len(), SLASH.len());
    let he = slash_matches("/he", &[], &effort);
    assert_eq!(he.len(), 1);
    assert_eq!(he[0].name, "/help");
    assert!(!he[0].skill);
}

#[test]
fn installed_skills_join_the_slash_palette() {
    let skills = vec![(
        "research-notes".to_string(),
        "organize research notes".to_string(),
    )];
    let effort = EffortView::default();
    assert_eq!(slash_matches("/", &skills, &effort).len(), SLASH.len() + 1);
    let shared = slash_matches("/re", &skills, &effort);
    assert!(shared
        .iter()
        .any(|item| item.name == "/revoke" && !item.skill));
    assert!(shared
        .iter()
        .any(|item| item.name == "/research-notes" && item.skill));
    let re = slash_matches("/rese", &skills, &effort);
    assert_eq!(re.len(), 1);
    assert_eq!(re[0].name, "/research-notes");
    assert!(re[0].skill);
    assert!(slash_matches("/research-notes go", &skills, &effort).is_empty());
}

#[test]
fn actual_room_limits_rows_and_keeps_the_hidden_count_inside_the_last_row() {
    let hints = slash_matches("/", &[], &EffortView::default());
    assert!(lines(&hints, 0, 0, 80).is_empty());
    let short = lines(&hints, 0, 1, 36);
    assert_eq!(short.len(), 1);
    let text = short[0].to_string();
    assert!(text.contains("/help"));
    assert!(text.ends_with(&format!("+{}", hints.len() - 1)));
    assert!(wrap::str_cols(&text) <= 36, "{text}");
    assert!(wrap::str_cols(&short[0].spans[2].content) < wrap::str_cols(&hints[0].summary));
    let fitting = &hints[..3];
    let full = lines(fitting, 0, hints.len(), 200);
    assert_eq!(full.len(), fitting.len());
    assert!(full.iter().all(|line| line.spans.len() == 3));
}

#[test]
fn the_palette_never_shows_more_than_six_candidates() {
    let hints = slash_matches("/", &[], &EffortView::default());
    assert!(hints.len() > 6);
    for room in [0, 1, 5, 6, 7, 100, usize::MAX] {
        assert_eq!(lines(&hints, 0, room, 200).len(), room.min(6));
    }
}

#[test]
fn every_selection_stays_visible_in_a_contiguous_window() {
    let skills = vec![("tail-skill".into(), "last candidate".into())];
    let hints = slash_matches("/", &skills, &EffortView::default());
    assert_eq!(hints.len(), SLASH.len() + 1, "matching is not truncated");
    for room in [1, 2, 5, 6, 20] {
        for selected in (0..hints.len()).chain(std::iter::once(usize::MAX)) {
            let rows = lines(&hints, selected, room, 200);
            let selected = selected.min(hints.len() - 1);
            let marked: Vec<_> = rows
                .iter()
                .filter(|row| row.spans[0].content == "▸ ")
                .collect();
            assert_eq!(marked.len(), 1, "selection {selected}, room {room}");
            assert_eq!(marked[0].spans[1].content.trim(), hints[selected].name);
            let first = hints
                .iter()
                .position(|hint| hint.name == rows[0].spans[1].content.trim())
                .unwrap();
            for (offset, row) in rows.iter().enumerate() {
                assert_eq!(row.spans[1].content.trim(), hints[first + offset].name);
            }
            assert!(rows
                .last()
                .unwrap()
                .to_string()
                .ends_with(&format!("+{}", hints.len() - rows.len())));
        }
    }
    assert!(lines(&[], usize::MAX, 6, 200).is_empty());
}

#[test]
fn selection_brightens_both_name_and_summary_without_changing_the_shared_column() {
    let hints = slash_matches("/", &[], &EffortView::default());
    let rows = lines(&hints, 1, hints.len(), 200);
    for (i, row) in rows.iter().enumerate() {
        let fg = Some(if i == 1 { FG } else { DIM });
        assert_eq!(row.spans[1].style.fg, fg);
        assert_eq!(row.spans[2].style.fg, fg);
        assert_eq!(
            row.spans[1].style.add_modifier.contains(Modifier::BOLD),
            i == 1
        );
        assert_eq!(row.spans[1].content.chars().count(), slash_col());
        assert_eq!(row.spans[0].content, if i == 1 { "▸ " } else { "  " });
    }
}
