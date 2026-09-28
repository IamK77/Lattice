//! Entry-level baselines: real keys/commands, real session messages, private HOME.
//! These stay outside the operation owner so routing remains part of the check.

use super::*;
use lattice::components::{context_gate, silent_ui};
use ratatui::crossterm::event::KeyEvent;
use std::time::Instant;

struct Live(Option<Session>);

impl Live {
    fn session(&self) -> &Session {
        self.0.as_ref().unwrap()
    }
}

impl Drop for Live {
    fn drop(&mut self) {
        if let Some(session) = self.0.take() {
            session.shutdown();
        }
    }
}

fn fixture() -> (Ui, Live) {
    let cfg = PresetConfig {
        adapter: "scripted".into(),
        model: "scripted".into(),
        base_url: String::new(),
        key_env: String::new(),
        workspace: None,
        context_window: 64_000,
        usage_input_field: "input_tokens".into(),
        profile: None,
        catalog_problems: Vec::new(),
        system: "Test model.".into(),
        thinking: None,
        scripted: Some(json!({"script": []})),
        overlay: None,
        assembly: None,
    };
    let running = lattice::models::Entry {
        id: "scripted".into(),
        adapter: cfg.adapter.clone(),
        model: cfg.model.clone(),
        base_url: cfg.base_url.clone(),
        key_env: cfg.key_env.clone(),
        profile: None,
    };
    let mut ui = Ui::replayed(&[]);
    ui.domain.model = model_state::ModelState::new(
        model_catalog::load(&running),
        running,
        EffortView {
            rungs: vec!["high".into(), "max".into()],
            now: Some("high".into()),
        },
        Some(64_000),
    );
    let path = std::env::current_dir().unwrap().join("actions.ledger");
    let session = Session::spawn("ui", move |tx| session_build::build(tx, &cfg, path)).unwrap();
    (ui, Live(Some(session)))
}

fn press(ui: &mut Ui, live: &Live, code: KeyCode) {
    assert!(!on_key(
        ui,
        Some(live.session()),
        KeyEvent::from(code),
        &Hit::default(),
    ));
}

/// A queued catalog note is a causal barrier behind the operations under test.
/// No sleep, quiet-period guess, or model call is needed to establish completion.
fn through_barrier(live: &Live) -> Vec<RenderEvent> {
    let session = live.session();
    session.note_catalog_change(lattice::CatalogNote {
        action: "barrier".into(),
        id: "test-barrier".into(),
        model: String::new(),
        adapter: String::new(),
        endpoint: String::new(),
        key_env: String::new(),
    });
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut events = Vec::new();
    loop {
        let event = session
            .next_render_timeout(deadline.saturating_duration_since(Instant::now()))
            .expect("queued operations must reach the barrier");
        let done = matches!(&event, RenderEvent::Appended(event)
            if event.event_type == silent_ui::CATALOG_CHANGED
                && event.payload["action"] == "barrier");
        events.push(event);
        if done {
            return events;
        }
    }
}

#[test]
fn unchanged_controls_acknowledge_without_sending_live_requests() {
    test_support::isolated(|| {
        let (mut ui, live) = fixture();
        run_slash(&mut ui, "/effort", Some(live.session()));
        press(&mut ui, &live, KeyCode::Enter);
        assert_eq!(ui.flash.as_deref(), Some("effort unchanged — still high"));
        assert!(ui.controls.dial().is_none());
        ui.controls
            .seed_picker(ui.domain.model.catalog().now.unwrap());
        press(&mut ui, &live, KeyCode::Enter);
        assert_eq!(
            ui.flash.as_deref(),
            Some("model unchanged — still scripted")
        );
        assert!(ui.controls.picker().is_none());
        assert!(!ui.domain.turns.busy());
        for render in through_barrier(&live) {
            match render {
                RenderEvent::Appended(event) => assert_ne!(
                    event.payload["channel"],
                    context_gate::EFFORT_CHANNEL,
                    "unchanged dial must not issue another request"
                ),
                RenderEvent::Notice { source, .. } => assert!(
                    source != "preferences" && source != "model",
                    "unchanged controls must not reach the session: {source}"
                ),
                _ => {}
            }
        }
    });
}

#[test]
fn settings_keep_local_effort_and_event_confirmed_model_boundaries() {
    test_support::isolated(|| {
        let (mut ui, live) = fixture();
        run_slash(&mut ui, "/thinking false", None);
        assert_eq!(ui.flash.as_deref(), Some("no session to set the effort on"));
        assert_eq!(ui.domain.model.effort().now.as_deref(), Some("high"));
        run_slash(&mut ui, "/effort max", Some(live.session()));
        assert_eq!(ui.domain.model.effort().now.as_deref(), Some("max"));
        run_slash(&mut ui, "/thinking false", Some(live.session()));
        assert_eq!(ui.domain.model.effort().now.as_deref(), Some("off"));
        assert!(ui.controls.dial().is_none());
        run_slash(&mut ui, "/effort invalid", Some(live.session()));
        assert!(ui.controls.dial().is_some());
        press(&mut ui, &live, KeyCode::Esc);
        let before = ui.domain.model.catalog().now;
        run_slash(&mut ui, "/model missing", Some(live.session()));
        assert!(ui.domain.turns.busy(), "activity starts at the request");
        assert_eq!(ui.domain.model.catalog().now, before);
        assert_eq!(ui.domain.model.running().id, "scripted");
        // No receipt has been folded into the UI before the local assertions.
        let events = through_barrier(&live);
        let values: Vec<_> = events
            .iter()
            .filter_map(|render| match render {
                RenderEvent::Appended(event)
                    if event.event_type == core_events::EXTERNAL_INPUT
                        && event.payload["channel"] == context_gate::EFFORT_CHANNEL =>
                {
                    Some(event.payload["value"].clone())
                }
                _ => None,
            })
            .collect();
        assert_eq!(values, [json!("max"), json!(false)]);
        assert!(events.iter().any(|render| matches!(render,
            RenderEvent::Notice { source, payload }
                if source == "model" && payload["note"].as_str().unwrap().contains("scripted"))));
        for event in events {
            fold_render(&mut ui, event).unwrap();
        }
        assert_eq!(ui.domain.model.catalog().now, before);
        assert_eq!(ui.domain.model.running().id, "scripted");
        assert!(
            ui.entries.is_empty(),
            "command receipts are not conversation entries"
        );
    });
}

fn fill_form(ui: &mut Ui) {
    for value in [
        "spare",
        "openai",
        "spare-model",
        "https://fixture.invalid/v1",
        "not-a-real-key-model-actions",
        "",
    ] {
        assert!(ui.controls.paste_form(value));
        ui.controls.next_field();
    }
}

#[test]
fn catalog_edits_keep_failure_state_and_audit_only_success_without_keys() {
    test_support::isolated(|| {
        // Construct the session BEFORE the literal key is introduced: its
        // startup redactor cannot accidentally make this assertion pass.
        let (mut ui, live) = fixture();
        run_slash(&mut ui, "/model", Some(live.session()));
        press(&mut ui, &live, KeyCode::Char('a'));
        fill_form(&mut ui);
        press(&mut ui, &live, KeyCode::Enter);
        assert!(ui.controls.form().is_none());
        assert_eq!(ui.flash.as_deref(), Some("added spare — s to switch to it"));
        let selected = ui.panel.selected_row();
        let row = ui.domain.model.catalog().rows[selected].clone();
        assert_eq!(row.id, "spare");
        assert_eq!(ui.domain.model.catalog().current().unwrap().id, "scripted");
        let path = lattice::models::path().unwrap();
        let original = std::fs::read_to_string(&path).unwrap();
        assert!(original.contains("not-a-real-key-model-actions"));

        press(&mut ui, &live, KeyCode::Char('a'));
        fill_form(&mut ui);
        press(&mut ui, &live, KeyCode::Enter);
        assert!(ui
            .controls
            .form()
            .unwrap()
            .problem
            .as_deref()
            .unwrap()
            .contains("already"));
        assert_eq!(ui.controls.form().unwrap().values[0], "spare");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
        press(&mut ui, &live, KeyCode::Esc);

        ui.panel.select_row(ui.domain.model.catalog().now.unwrap());
        press(&mut ui, &live, KeyCode::Char('d'));
        assert!(ui.controls.deletion().is_none());
        assert!(ui.flash.as_deref().unwrap().contains("switch away"));
        ui.panel.select_row(selected);
        press(&mut ui, &live, KeyCode::Char('d'));
        assert_eq!(ui.controls.deletion(), Some("spare"));
        std::fs::write(&path, "{ broken private fixture").unwrap();
        press(&mut ui, &live, KeyCode::Char('y'));
        assert!(
            ui.controls.deletion().is_none(),
            "failure consumes the confirmation"
        );
        assert!(ui.flash.as_deref().unwrap().contains("not valid JSON"));
        assert_eq!(ui.domain.model.catalog().rows[selected].id, "spare");
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "{ broken private fixture"
        );

        std::fs::write(&path, &original).unwrap();
        press(&mut ui, &live, KeyCode::Char('d'));
        press(&mut ui, &live, KeyCode::Char('y'));
        assert!(ui.controls.deletion().is_none());
        assert_eq!(ui.flash.as_deref(), Some("deleted spare"));
        assert!(ui
            .domain
            .model
            .catalog()
            .rows
            .iter()
            .all(|row| row.id != "spare"));
        assert!(ui.panel.selected_row() < ui.domain.model.catalog().rows.len());
        let written: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        assert!(written["models"].as_object().unwrap().is_empty());

        let notes: Vec<_> = through_barrier(&live)
            .into_iter()
            .filter_map(|render| match render {
                RenderEvent::Appended(event) if event.event_type == silent_ui::CATALOG_CHANGED => {
                    Some(event)
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            notes.len(),
            3,
            "only successful add/remove and the barrier are audited"
        );
        for (note, action) in notes[..2].iter().zip(["added", "removed"]) {
            assert_eq!(
                note.payload,
                json!({
                    "action": action, "id": row.id, "model": row.model,
                    "adapter": row.dialect, "endpoint": row.endpoint, "keyEnv": row.key_env,
                })
            );
            assert!(note.reason.is_some());
            assert!(!serde_json::to_string(note)
                .unwrap()
                .contains("not-a-real-key-model-actions"));
        }
    });
}

#[test]
fn changed_controls_panel_and_explicit_commands_share_requests_not_deduplication() {
    test_support::isolated(|| {
        let (mut ui, live) = fixture();
        run_slash(&mut ui, "/effort", Some(live.session()));
        press(&mut ui, &live, KeyCode::Right);
        press(&mut ui, &live, KeyCode::Enter);
        assert!(ui.controls.dial().is_none());
        assert_eq!(ui.domain.model.effort().now.as_deref(), Some("xhigh"));
        // Explicit commands still send their value even when it is current;
        // only confirming an unchanged control suppresses the request.
        run_slash(&mut ui, "/effort xhigh", Some(live.session()));
        ui.domain.model.fixture_catalog().rows.push(ModelRow {
            id: "missing".into(),
            model: "missing-model".into(),
            ..ModelRow::default()
        });
        ui.controls.seed_picker(1);
        press(&mut ui, &live, KeyCode::Enter);
        assert!(ui.controls.picker().is_none());
        assert!(ui.domain.turns.busy());
        assert_eq!(ui.domain.model.catalog().now, Some(0));
        run_slash(&mut ui, "/model", Some(live.session()));
        press(&mut ui, &live, KeyCode::Down);
        press(&mut ui, &live, KeyCode::Char('s'));
        assert!(!ui.panel.is_visible());
        assert_eq!(ui.domain.model.catalog().now, Some(0));
        run_slash(&mut ui, "/model scripted", Some(live.session()));

        // Invalid positions consume the picker but do not dismiss the panel.
        ui.controls.seed_picker(usize::MAX);
        press(&mut ui, &live, KeyCode::Enter);
        assert!(ui.controls.picker().is_none());
        assert!(ui.flash.is_none());
        run_slash(&mut ui, "/model", Some(live.session()));
        ui.panel.select_row(usize::MAX);
        press(&mut ui, &live, KeyCode::Char('s'));
        assert!(ui.panel.is_visible());
        let events = through_barrier(&live);
        let values: Vec<_> = events
            .iter()
            .filter_map(|render| match render {
                RenderEvent::Appended(event)
                    if event.event_type == core_events::EXTERNAL_INPUT
                        && event.payload["channel"] == context_gate::EFFORT_CHANNEL =>
                {
                    Some(event.payload["value"].clone())
                }
                _ => None,
            })
            .collect();
        assert_eq!(values, [json!("xhigh"), json!("xhigh")]);
        assert_eq!(
            events
                .iter()
                .filter(|render| matches!(render,
            RenderEvent::Notice { source, .. } if source == "model"))
                .count(),
            3
        );
    });
}
