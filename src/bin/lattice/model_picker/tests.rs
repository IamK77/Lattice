use super::*;
use lattice::view::ModelRow;

fn two_models() -> ModelView {
    ModelView {
        rows: vec![
            ModelRow {
                id: "deepseek".into(),
                model: "deepseek-v4-flash".into(),
                endpoint: "api.deepseek.com".into(),
                dialect: "openai".into(),
                window: Some(1_000_000),
                rungs: vec!["high".into(), "max".into()],
                accepts_images: false,
                key_env: "DEEPSEEK_API_KEY".into(),
                key_present: true,
            },
            ModelRow {
                id: "sonnet".into(),
                model: "claude-sonnet-5".into(),
                endpoint: "api.anthropic.com".into(),
                dialect: "anthropic".into(),
                window: Some(200_000),
                rungs: ["low", "medium", "high", "xhigh", "max"]
                    .map(str::to_string)
                    .to_vec(),
                accepts_images: true,
                key_env: "ANTHROPIC_API_KEY".into(),
                key_present: false,
            },
        ],
        now: Some(0),
    }
}

fn text(models: &ModelView, cursor: usize) -> String {
    picker_lines(models, cursor)
        .iter()
        .map(Line::to_string)
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn the_picker_spells_out_what_switching_would_change() {
    let models = two_models();
    let on_current = text(&models, 0);
    assert!(
        on_current.contains("[deepseek]") && on_current.contains("running now"),
        "the cursor opens on what is running, and says so: {on_current}"
    );
    let on_other = text(&models, 1);
    for shown in [
        "api.deepseek.com → api.anthropic.com",
        "openai → anthropic",
        "1M → 200k",
    ] {
        assert!(
            on_other.contains(shown),
            "the change is spelled out ({shown}): {on_other}"
        );
    }
    assert!(
        on_other.contains("ANTHROPIC_API_KEY is NOT set"),
        "and the thing that decides whether it can answer at all: {on_other}"
    );
}

#[test]
fn the_picker_holds_its_columns_as_the_cursor_travels() {
    let models = two_models();
    let width = |cursor| {
        picker_lines(&models, cursor)[0]
            .spans
            .iter()
            .map(|span| span.content.chars().count())
            .sum::<usize>()
    };
    assert_eq!(width(0), width(1), "the row changed width with the cursor");
}

#[test]
fn a_picker_with_nothing_to_pick_says_where_models_come_from() {
    assert!(text(&ModelView::default(), 0).contains("models.json"));
}

#[test]
fn availability_and_selection_are_independent_and_unknown_is_not_zero() {
    let mut models = two_models();
    let rows = picker_lines(&models, usize::MAX);
    assert_eq!(rows, picker_lines(&models, 1));
    for (name, color, selected) in [("deepseek", FG, false), ("sonnet", DIM, true)] {
        let span = rows[0]
            .spans
            .iter()
            .find(|span| span.content == name)
            .unwrap();
        assert_eq!(span.style.fg, Some(color));
        assert_eq!(span.style.add_modifier.contains(Modifier::BOLD), selected);
    }
    models.rows[1].window = None;
    models.rows[1].key_env.clear();
    let display = text(&models, 1);
    assert!(display.contains("1M → unknown"), "{display}");
    assert!(display.contains("none needed"), "{display}");
    assert_eq!(window_text(Some(0)), "0");
    assert_eq!(window_text(None), "unknown");
}
