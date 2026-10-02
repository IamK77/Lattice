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
        permission_actions::observe(ui, &render, live.session());
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
        press(&mut ui, &live, KeyCode::Up);
        assert_eq!(
            ui.authorization_prompt().unwrap().unwrap().selected,
            view::AuthorizationChoice::Once
        );
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
fn permission_is_one_red_live_badge_without_success_notices() {
    test_support::isolated(|| {
        let (mut ui, live) = fixture();
        for enabled in [true, false, true] {
            press(&mut ui, &live, KeyCode::BackTab);
            assert!(
                ui.flash.is_none(),
                "successful requests must not produce a receipt"
            );
            until(&mut ui, &live, |e| {
                e.event_type == ip::STATE && e.payload["action"] == "set"
            });
            assert_eq!(ui.interface_permission, enabled);
            let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(100, 24)).unwrap();
            draw_ui(&mut terminal, &mut ui).unwrap();
            let buffer = terminal.backend().buffer();
            let text: String = buffer.content.iter().map(|cell| cell.symbol()).collect();
            assert_eq!(text.matches("permission").count(), usize::from(enabled));
            assert!(!text.contains("requested"));
            if enabled {
                for x in 89..99 {
                    assert_eq!(buffer[(x, 23)].fg, ratatui::style::Color::Red);
                }
            }
        }
        let reader = live.session().log_reader();
        let mut reopened = Ui::replayed(&[]);
        reopened
            .replay_prefix(&reader, reader.snapshot_end())
            .unwrap();
        assert!(
            !reopened.interface_permission,
            "historical states cannot light a live indicator"
        );
        let mut unavailable = Ui::replayed(&[]);
        on_key(
            &mut unavailable,
            None,
            KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT),
            &Hit::default(),
        );
        assert!(unavailable
            .flash
            .as_deref()
            .unwrap()
            .contains("No live session"));
    });
}

#[test]
fn authorization_panel_selection_saves_a_flow_grant_and_disappears() {
    test_support::isolated(|| {
        let (mut ui, live) = fixture();
        ui.draft.edit().insert_str("unsent draft");
        live.session().send_text("first command");
        let question = until(&mut ui, &live, |e| e.event_type == op::AUTH_REQUESTED);
        let mut term = Terminal::new(ratatui::backend::TestBackend::new(100, 30)).unwrap();
        draw(&mut term, &ui).unwrap();
        let shown: String = term
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(
            shown.contains("Allow for this conversation (survives reopening)"),
            "{shown}"
        );
        assert!(!shown.contains("Trust permanently"));
        assert!(shown.contains("> Refuse request"));
        press(&mut ui, &live, KeyCode::Up);
        assert_eq!(
            ui.authorization_prompt().unwrap().unwrap().selected,
            view::AuthorizationChoice::Flow
        );
        press(&mut ui, &live, KeyCode::Enter);
        assert!(ui.authorization_prompt().unwrap().is_none());
        assert_eq!(ui.input(), "unsent draft");
        let answer = until(&mut ui, &live, |e| {
            e.event_type == lattice::core_events::EXTERNAL_INPUT
                && e.payload["request"] == question.id
        });
        assert_eq!(answer.payload["scope"], "flow");
        finish(&mut ui, &live);
        assert_eq!(live.session().flow_grants().unwrap().grants.len(), 1);
        draw(&mut term, &ui).unwrap();
        let shown: String = term
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(!shown.contains("Approve Run operation"), "{shown}");
        assert!(!shown.contains("Authorization:"), "{shown}");
        assert!(shown.contains("unsent draft"));
        assert!(
            live.session()
                .log_reader()
                .get(&question.id)
                .unwrap()
                .is_some(),
            "the audit record is retained"
        );
        let reader = live.session().log_reader();
        let through = reader.snapshot_end();
        for _ in 0..2 {
            let mut reopened = Ui::replayed(&[]);
            reopened.replay_prefix(&reader, through).unwrap();
            assert!(reopened.authorization_prompt().unwrap().is_none());
            draw_ui(&mut term, &mut reopened).unwrap();
            let shown: String = term
                .backend()
                .buffer()
                .content
                .iter()
                .map(|cell| cell.symbol())
                .collect();
            assert!(!shown.contains("Approve Run operation"), "{shown}");
            assert!(!shown.contains("Authorization:"), "{shown}");
        }
        live.session().send_text("same command");
        finish(&mut ui, &live);
        assert!(ui.pending_auth().is_none());
    });
}

#[test]
fn grants_panel_lists_confirms_and_waits_for_authoritative_revocation() {
    test_support::isolated(|| {
        let (mut ui, live) = fixture();
        ui.draft.edit().insert_str("keep this draft");
        command(&mut ui, &live, "/grants");
        assert_eq!(ui.panel.active(), Some(panels::AT_GRANTS));
        assert!(ui.flash.is_none());
        assert!(ui.grants.display().state.grants.is_empty());
        assert!(ui.grants.display().problem.is_none());
        press(&mut ui, &live, KeyCode::Esc);
        live.session().send_text("first command");
        until(&mut ui, &live, |e| e.event_type == op::AUTH_REQUESTED);
        press(&mut ui, &live, KeyCode::Char('f'));
        finish(&mut ui, &live);
        command(&mut ui, &live, "/grants");
        let id = ui.grants.display().selected.clone().unwrap();
        let mut term = Terminal::new(ratatui::backend::TestBackend::new(100, 35)).unwrap();
        draw_ui(&mut term, &mut ui).unwrap();
        let shown: String = term
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(shown.contains("Conversation Grants"), "{shown}");
        assert!(shown.contains("survives reopening"), "{shown}");
        assert!(shown.contains("printf"), "{shown}");
        press(&mut ui, &live, KeyCode::Char('x'));
        absorb_paste(&mut ui, "must not become input");
        assert_eq!(ui.input(), "keep this draft");
        assert_eq!(ui.panel.active(), Some(panels::AT_GRANTS));
        press(&mut ui, &live, KeyCode::Char('d'));
        assert_eq!(ui.grants.display().confirming.as_deref(), Some(id.as_str()));
        on_key(
            &mut ui,
            Some(live.session()),
            KeyEvent::new(KeyCode::Enter, KeyModifiers::CONTROL),
            &Hit::default(),
        );
        assert_eq!(ui.grants.display().confirming.as_deref(), Some(id.as_str()));
        press(&mut ui, &live, KeyCode::Esc);
        assert!(ui.grants.display().confirming.is_none());
        assert_eq!(live.session().flow_grants().unwrap().grants.len(), 1);
        press(&mut ui, &live, KeyCode::Char('d'));
        press(&mut ui, &live, KeyCode::Enter);
        assert_eq!(ui.grants.display().pending.as_deref(), Some(id.as_str()));
        assert!(
            ui.grants.display().state.grants.contains_key(&id),
            "sending is not removal"
        );
        until(&mut ui, &live, |e| {
            e.event_type == op::STATE && e.payload["grants"] == json!({})
        });
        assert!(ui.grants.display().pending.is_none());
        assert!(ui.grants.display().state.grants.is_empty());
        assert_eq!(ui.panel.active(), Some(panels::AT_GRANTS));
        press(&mut ui, &live, KeyCode::Esc);
        assert!(ui.panel.active().is_none());
        assert_eq!(ui.input(), "keep this draft");
        live.session().send_text("ask again");
        until(&mut ui, &live, |e| e.event_type == op::AUTH_REQUESTED);
        press(&mut ui, &live, KeyCode::Esc);
        finish(&mut ui, &live);
    });
}

#[test]
fn grants_panel_without_a_live_session_shows_an_error_not_an_empty_success() {
    let mut ui = Ui::replayed(&[]);
    run_slash(&mut ui, "/grants", None);
    assert_eq!(ui.panel.active(), Some(panels::AT_GRANTS));
    assert!(ui
        .grants
        .display()
        .problem
        .as_deref()
        .unwrap()
        .contains("No live session"));
    assert!(ui.flash.is_none());
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
        assert_eq!(ui.panel.active(), Some(panels::AT_GRANTS));
        assert!(ui.flash.is_none());
        assert!(ui.grants.display().state.grants.contains_key(id));
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
