//! Real terminal entry points and real authority state, synchronized by events.
use super::*;
use lattice::components::{interface_permissions as ip, operation_policy as op};
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
    let mut script = Vec::new();
    for n in 1..=3 {
        script.push(json!({"status":"ok","toolCalls":[{"id":format!("run-{n}"),"tool":"Run","arguments":{"command":"printf permission-test"}}]}));
        script.push(json!({"status":"ok","text":"done"}));
    }
    let cfg = PresetConfig {
        adapter: "scripted".into(),
        model: "scripted".into(),
        base_url: String::new(),
        key_env: String::new(),
        workspace: None,
        context_window: 64_000,
        usage_input_field: "input_tokens".into(),
        profile: None,
        catalog_problems: vec![],
        system: "Permission test".into(),
        thinking: None,
        scripted: Some(json!({"script":script})),
        overlay: None,
        assembly: None,
    };
    let path = std::env::current_dir().unwrap().join("permissions.ledger");
    let session = Session::spawn("ui", move |tx| session_build::build(tx, &cfg, path)).unwrap();
    let live = Live(Some(session));
    let mut ui = Ui::replayed(&[]);
    until(&mut ui, &live, |e| {
        e.event_type == ip::STATE && e.payload["action"] == "open"
    });
    (ui, live)
}
fn until(
    ui: &mut Ui,
    live: &Live,
    matches: impl Fn(&lattice::EventEnvelope) -> bool,
) -> lattice::EventEnvelope {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let render = live
            .session()
            .next_render_timeout(deadline.saturating_duration_since(Instant::now()))
            .expect("authority did not reach the expected event");
        let found = match &render {
            RenderEvent::Appended(event) if matches(event) => Some(event.as_ref().clone()),
            _ => None,
        };
        fold_render(ui, render).expect("rendered authority event must be readable");
        if let Some(event) = found {
            return event;
        }
    }
}
fn press(ui: &mut Ui, live: &Live, key: KeyCode) {
    assert!(!on_key(
        ui,
        Some(live.session()),
        KeyEvent::from(key),
        &Hit::default()
    ));
}
fn command(ui: &mut Ui, live: &Live, text: &str) {
    run_slash(ui, text, Some(live.session()));
}
fn finish(ui: &mut Ui, live: &Live) {
    until(ui, live, |e| {
        e.event_type == lattice::core_events::TURN_COMPLETED
    });
}

#[test]
fn permission_commands_and_modal_toggle_change_only_the_live_interface() {
    test_support::isolated(|| {
        let (mut ui, live) = fixture();
        assert_eq!(live.session().permission_enabled().unwrap(), Some(false));
        command(&mut ui, &live, "/permission on");
        until(&mut ui, &live, |e| {
            e.event_type == ip::STATE
                && e.payload["action"] == "set"
                && e.payload["accepted"] == true
        });
        assert_eq!(live.session().permission_enabled().unwrap(), Some(true));
        command(&mut ui, &live, "/permission off");
        until(&mut ui, &live, |e| {
            e.event_type == ip::STATE && e.payload["action"] == "set"
        });
        assert_eq!(live.session().permission_enabled().unwrap(), Some(false));
        for character in "keep this draft".chars() {
            press(&mut ui, &live, KeyCode::Char(character));
        }
        assert_eq!(ui.input(), "keep this draft");
        live.session().send_text("ask for a command");
        let question = until(&mut ui, &live, |e| e.event_type == op::AUTH_REQUESTED);
        let draft = ui.input().to_string();
        let cursor = ui.cursor();
        press(&mut ui, &live, KeyCode::Char('i'));
        until(&mut ui, &live, |e| {
            e.event_type == ip::STATE && e.payload["action"] == "set"
        });
        assert_eq!(live.session().permission_enabled().unwrap(), Some(true));
        assert_eq!(ui.pending_auth(), Some(question.id.as_str()));
        assert_eq!(ui.input(), draft);
        assert_eq!(ui.cursor(), cursor);
        assert!(live.session().flow_grants().unwrap().grants.is_empty());
        press(&mut ui, &live, KeyCode::Up);
        press(&mut ui, &live, KeyCode::Enter);
        let answer = until(&mut ui, &live, |e| {
            e.event_type == lattice::core_events::EXTERNAL_INPUT
                && e.payload["request"] == question.id
        });
        assert_eq!(answer.payload["scope"], "once");
        assert_eq!(
            answer.payload["interface"].as_str(),
            live.session().interface_id()
        );
        finish(&mut ui, &live);
        assert!(live.session().flow_grants().unwrap().grants.is_empty());
    });
}

#[test]
fn shift_tab_toggles_permission_with_a_draft_and_a_pending_question() {
    test_support::isolated(|| {
        let (mut ui, live) = fixture();
        for character in "keep this draft".chars() {
            press(&mut ui, &live, KeyCode::Char(character));
        }
        let cursor = ui.cursor();
        // Terminals report Shift+Tab either as BackTab or as shifted Tab.
        for (code, modifiers, enabled) in [
            (KeyCode::BackTab, KeyModifiers::SHIFT, true),
            (KeyCode::Tab, KeyModifiers::SHIFT, false),
            (KeyCode::BackTab, KeyModifiers::NONE, true),
            (KeyCode::BackTab, KeyModifiers::SHIFT, false),
        ] {
            assert!(!on_key(
                &mut ui,
                Some(live.session()),
                KeyEvent::new(code, modifiers),
                &Hit::default()
            ));
            until(&mut ui, &live, |e| {
                e.event_type == ip::STATE && e.payload["action"] == "set"
            });
            assert_eq!(live.session().permission_enabled().unwrap(), Some(enabled));
            assert_eq!(ui.input(), "keep this draft");
            assert_eq!(ui.cursor(), cursor);
            assert!(
                !ui.domain.turns.busy(),
                "toggling must not submit the draft"
            );
        }
        live.session().send_text("ask for a command");
        let question = until(&mut ui, &live, |e| e.event_type == op::AUTH_REQUESTED);
        on_key(
            &mut ui,
            Some(live.session()),
            KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT),
            &Hit::default(),
        );
        until(&mut ui, &live, |e| {
            e.event_type == ip::STATE && e.payload["action"] == "set"
        });
        assert_eq!(live.session().permission_enabled().unwrap(), Some(true));
        assert_eq!(ui.pending_auth(), Some(question.id.as_str()));
        assert_eq!(ui.input(), "keep this draft");
        assert_eq!(ui.cursor(), cursor);
        assert!(live.session().flow_grants().unwrap().grants.is_empty());
    });
}

#[test]
fn flow_key_and_grant_commands_manage_a_real_persistent_scope() {
    test_support::isolated(|| {
        let (mut ui, live) = fixture();
        live.session().send_text("first command");
        let question = until(&mut ui, &live, |e| e.event_type == op::AUTH_REQUESTED);
        press(&mut ui, &live, KeyCode::Char('p'));
        assert_eq!(ui.pending_auth(), Some(question.id.as_str()));
        assert!(ui.flash.as_deref().unwrap().contains("only for admission"));
        press(&mut ui, &live, KeyCode::Char('f'));
        until(&mut ui, &live, |e| {
            e.event_type == op::STATE
                && e.payload["grants"]
                    .as_object()
                    .is_some_and(|g| !g.is_empty())
        });
        let state = live.session().flow_grants().unwrap();
        let (id, grant) = state.grants.iter().next().unwrap();
        assert_eq!(grant.question, question.id);
        assert_eq!(grant.interface.as_deref(), live.session().interface_id());
        command(&mut ui, &live, "/grants");
        assert!(ui.flash.as_deref().unwrap().contains(id));
        finish(&mut ui, &live);
        live.session().send_text("same command");
        finish(&mut ui, &live);
        assert!(
            ui.pending_auth().is_none(),
            "the matching operation must use its flow grant"
        );
        command(&mut ui, &live, &format!("/revoke {id}"));
        until(&mut ui, &live, |e| {
            e.event_type == op::STATE && e.payload["grants"] == json!({})
        });
        assert!(live.session().flow_grants().unwrap().grants.is_empty());
        live.session().send_text("ask again");
        until(&mut ui, &live, |e| e.event_type == op::AUTH_REQUESTED);
        press(&mut ui, &live, KeyCode::Esc);
        finish(&mut ui, &live);
    });
}
