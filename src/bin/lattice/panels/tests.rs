use super::*;
use lattice::view::{Entry, Live, ModelForm, ModelRow, ModelView, View};
use std::borrow::Cow;

#[derive(Default)]
struct Fixture {
    models: ModelView,
    form: Option<ModelForm>,
    parts: Vec<lattice::Assembled>,
    live: Vec<Live>,
    selected: usize,
    open: bool,
    deleting: Option<String>,
}

impl View for Fixture {
    fn title(&self) -> &str {
        "fixture"
    }
    fn entries(&self) -> &[Entry] {
        &[]
    }
    fn streaming(&self) -> &str {
        ""
    }
    fn input(&self) -> Cow<'_, str> {
        Cow::Borrowed("")
    }
    fn busy(&self) -> bool {
        false
    }
    fn tick(&self) -> usize {
        0
    }
    fn models(&self) -> ModelView {
        self.models.clone()
    }
    fn model_form(&self) -> Option<ModelForm> {
        self.form.clone()
    }
    fn components(&self) -> Vec<lattice::Assembled> {
        self.parts.clone()
    }
    fn background(&self) -> &[Live] {
        &self.live
    }
    fn panel_sel(&self) -> usize {
        self.selected
    }
    fn panel_open(&self) -> bool {
        self.open
    }
    fn confirm_delete(&self) -> Option<String> {
        self.deleting.clone()
    }
}

#[test]
fn config_uses_effective_model_but_current_file_sources() {
    use crate::terminal_host::panel_sources::ConfigSources;
    let view = Fixture {
        models: ModelView {
            rows: vec![row("effective", false)],
            now: Some(0),
        },
        ..Default::default()
    };
    let mut sources = ConfigSources {
        preferences: serde_json::json!({"model": "different-on-disk"}),
        prompt_overrides: 0,
        prompt_sections: 7,
        overlay_present: false,
        grants: 1234,
        ledgers: 2,
    };
    let rows = config::rows(&view, &sources);
    let value = |key: &str| rows.iter().find(|(k, _)| k == key).unwrap().1.as_str();
    assert!(value("model").starts_with("effective"));
    assert!(value("model").ends_with("preferences.json"));
    assert_eq!(
        value("api key"),
        format!("{:<28}models.json", "MISSING (TEST_KEY_NAME)")
    );
    assert!(value("prompt sections").ends_with("shipped, none overridden"));
    assert!(value("trust grants").starts_with("1,234"));
    assert!(value("ledgers").starts_with("2 kept"));
    sources.preferences = serde_json::json!({});
    sources.prompt_overrides = 3;
    sources.overlay_present = true;
    let next = config::rows(&view, &sources);
    assert!(next
        .iter()
        .any(|(k, v)| k == "model" && v.ends_with("this launch")));
    assert!(next
        .iter()
        .any(|(k, v)| k == "prompt sections" && v.ends_with("3 from ~/.lattice/prompts")));
    assert!(next
        .iter()
        .any(|(k, v)| k == "assembly overlay" && v.starts_with("present")));
}

fn row(id: &str, key_present: bool) -> ModelRow {
    ModelRow {
        id: id.into(),
        model: "model".into(),
        endpoint: "endpoint".into(),
        dialect: "openai".into(),
        window: Some(200_000),
        rungs: vec!["high".into()],
        accepts_images: true,
        key_env: "TEST_KEY_NAME".into(),
        key_present,
    }
}

#[test]
fn model_form_masks_the_key_and_takes_precedence_over_the_catalog() {
    let mut form = ModelForm {
        at: 4,
        ..Default::default()
    };
    form.values[4] = "never-display-this-key".into();
    let view = Fixture {
        form: Some(form),
        models: ModelView {
            rows: vec![row("catalog-model", true)],
            now: Some(0),
        },
        ..Default::default()
    };
    let rows = models::rows(&view, 80, "unused-path");
    let text = format!("{rows:?}");
    assert!(!text.contains("never-display-this-key"));
    assert!(!text.contains("catalog-model"));
    assert!(text.contains(&"•".repeat(22)));
    assert!(text.contains("fill ONE"));
}

#[test]
fn model_markers_prioritize_selection_without_hiding_current_or_missing_keys() {
    let view = Fixture {
        models: ModelView {
            rows: vec![
                row("current", true),
                row("selected", false),
                row("missing", false),
            ],
            now: Some(0),
        },
        selected: 1,
        open: true,
        deleting: Some("selected".into()),
        ..Default::default()
    };
    let rows = models::rows(&view, 120, "unused");
    for key in ["• current", "▸ selected", "! missing"] {
        assert!(rows.iter().any(|(k, _)| k == key), "{rows:?}");
    }
    let text = format!("{rows:?}");
    assert!(text.contains("200,000 tokens") && text.contains("NOT SET"));
    assert!(text.contains("delete selected from the catalog?"));
    assert!(!text.contains("s switch"));
    assert_eq!(
        models::rows(&Fixture::default(), 80, "/explicit/catalog.json")[1].1,
        "  /explicit/catalog.json"
    );
}

#[test]
fn component_selection_and_removability_are_distinct_and_wiring_is_explicit() {
    let mut view = Fixture {
        parts: vec![
            (
                "base".into(),
                "builtin".into(),
                "in-process",
                "Read".into(),
                false,
                vec!["request -> owner".into()],
            ),
            (
                "added".into(),
                "installed".into(),
                "process",
                "Other".into(),
                true,
                vec![],
            ),
        ],
        open: true,
        ..Default::default()
    };
    let rows = components::rows(&view, 120);
    assert!(rows.iter().any(|(k, _)| k == "▸ base"));
    assert!(rows.iter().any(|(k, _)| k == "+ added"));
    assert!(rows.iter().any(|(_, v)| v.contains("request -> owner")));
    view.selected = usize::MAX;
    let rows = components::rows(&view, 120);
    assert!(rows.iter().any(|(k, _)| k == "▸ added"));
    assert!(rows.iter().any(|(_, v)| v.contains("on no wires")));
}

#[test]
fn background_groups_keep_running_work_separate_from_armed_arrangements() {
    let item = |key: &str, standing: bool, ledger: Option<String>, tokens| Live {
        kind: "test",
        key: key.into(),
        label: "label".into(),
        standing,
        fires: 3,
        since: 0,
        ledger,
        tools: 2,
        tokens,
        read_len: 0,
    };
    let view = Fixture {
        live: vec![
            item("timer", true, None, 0),
            item("expert", false, Some("unused".into()), 1_250_000),
            item("command", false, None, 0),
        ],
        ..Default::default()
    };
    let rows = background::rows(&view, 120);
    assert_eq!(
        rows.iter().map(|(key, _)| key.as_str()).collect::<Vec<_>>(),
        vec!["", "expert", "command", "", "", "timer"]
    );
    assert_eq!(rows[0].1, "running");
    assert_eq!(rows[1].1, "label · 2 tools · 1.2M tokens");
    assert_eq!(rows[2].1, "label");
    assert_eq!(rows[4].1, "armed");
    assert_eq!(rows[5].1, "label · fired 3");
}

#[test]
fn panel_tables_keep_whole_columns_and_the_existing_character_width_rule() {
    let cells = [("first", 8), ("second", 8), ("tail-value", 0)];
    assert_eq!(table_row(&cells, 7), "");
    assert_eq!(table_row(&cells, 8), "first");
    assert_eq!(table_row(&cells, 21), "first   second");
    assert_eq!(table_row(&cells, 22), "first   second  tail…");
    assert_eq!(key_column(["→", "ab"].into_iter()), 4);
    assert_eq!(thousands(268000), "268,000");
    assert_eq!(
        thousands_wide(u128::from(u64::MAX) + 1),
        "18,446,744,073,709,551,616"
    );
    assert_eq!(panel_tabs(usize::MAX), &[] as &[&str]);
}
