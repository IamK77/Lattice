//! Submit through the real key handler and inspect actual session input events.
use super::*;
use lattice::components::silent_ui;
use lattice::contracts::{document::DocRef, event::StreamRef};
use std::sync::mpsc::{channel, Receiver};

#[test]
fn submission_exit_candidate_and_quit_alias_stay_local_and_preserve_image_references() {
    for text in ["/exit", "/ex", "/quit"] {
        let mut ui = Ui::replayed(&[]);
        attach(&mut ui, "retained.png");
        ui.draft.edit().set(text);
        assert!(enter(&mut ui, None));
        assert!(ui.draft.editor().is_empty());
        assert_eq!(ui.draft.references().len(), 1);
        assert_eq!(ui.entry_count(), 0);
        assert!(!ui.busy());
    }
}

struct Live(Option<Session>);
impl Drop for Live {
    fn drop(&mut self) {
        if let Some(session) = self.0.take() {
            session.shutdown();
        }
    }
}

fn session() -> (Live, Receiver<EventEnvelope>) {
    let (sent, received) = channel();
    let session = Session::spawn("ui", move |_| {
        let registry = [(silent_ui::NAME.into(), silent_ui::manifest())].into();
        let mut factories: std::collections::HashMap<String, lattice::Factory> = [(
            silent_ui::NAME.into(),
            Box::new(|_: Option<&Value>| -> Box<dyn lattice::Component> {
                Box::new(silent_ui::SilentUi::new(Default::default()))
            }) as lattice::Factory,
        )]
        .into();
        let assembly = lattice::AssemblyManifest {
            instances: [(
                "ui".into(),
                lattice::ComponentInstance {
                    component: silent_ui::NAME.into(),
                    requires: vec![],
                    config: None,
                },
            )]
            .into(),
            wires: vec![],
        };
        let mut kernel =
            lattice::Kernel::start(&assembly, &registry, &mut factories, Default::default())?;
        kernel.subscribe_log(move |event| {
            let _ = sent.send(event.clone());
        });
        Ok(kernel)
    })
    .unwrap();
    (Live(Some(session)), received)
}

fn attach(ui: &mut Ui, file: &str) {
    assert!(ui.draft.attach(
        DocRef {
            file: file.into(),
            bytes: 4,
            lines: None,
            preview: None,
        },
        "image/png",
        file
    ));
}

fn enter(ui: &mut Ui, session: Option<&Session>) -> bool {
    on_key(
        ui,
        session,
        ratatui::crossterm::event::KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &Hit::default(),
    )
}

fn next_user(received: &Receiver<EventEnvelope>) -> EventEnvelope {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        let event = received.recv_timeout(remaining).unwrap();
        if event.event_type == core_events::USER_MESSAGE {
            return event;
        }
    }
}

#[test]
fn submission_sends_only_live_images_in_editor_order_and_consumes_origin_once() {
    test_support::isolated(|| {
        let (live, received) = session();
        let mut ui = Ui::replayed(&[]);
        ui.initial_origin = Some(StreamRef {
            stream: "parent".into(),
            event: "parent-event".into(),
        });
        attach(&mut ui, "first.png");
        attach(&mut ui, "deleted.png");
        ui.draft.edit().home();
        attach(&mut ui, "front.png");
        ui.draft.edit().end();
        ui.draft.edit().backspace();
        assert!(!enter(&mut ui, live.0.as_ref()));
        assert!(ui.initial_origin.is_none());
        assert!(ui.draft.references().is_empty());
        assert!(ui.draft.editor().is_empty());
        assert!(ui.busy());
        assert_eq!(
            ui.entry_count(),
            0,
            "submission does not echo before ledger delivery"
        );
        let event = next_user(&received);
        assert_eq!(event.payload["text"], "");
        let images = event.payload["images"].as_array().unwrap();
        assert_eq!(
            images
                .iter()
                .map(|image| image["file"].as_str().unwrap())
                .collect::<Vec<_>>(),
            vec!["front.png", "first.png"]
        );
        let origin = event.origin.unwrap();
        assert_eq!(origin.stream, "parent");
        assert_eq!(origin.event, "parent-event");
        ui.draft.edit().set("  ordinary message  ");
        assert!(!enter(&mut ui, live.0.as_ref()));
        let event = next_user(&received);
        assert_eq!(event.payload["text"], "ordinary message");
        assert!(event.origin.is_none());
    });
}

#[test]
fn submission_prefix_waits_for_completion_and_preserves_arguments_folds_and_images() {
    test_support::isolated(|| {
        let (live, received) = session();
        let mut ui = Ui::replayed(&[]);
        ui.domain.skills = vec![("research".into(), "test skill".into())];
        ui.draft.edit().set("/res \"a b\"  ");
        ui.draft
            .edit()
            .paste("one\ntwo\nthree\nfour\nfive\nsix\nseven\neight");
        attach(&mut ui, "skill.png");
        let original = ui.draft.editor().expanded().to_string();
        let raw = ui.draft.editor().text().to_string();
        assert!(!enter(&mut ui, live.0.as_ref()));
        assert_eq!(ui.draft.editor().text(), raw);
        assert_eq!(ui.draft.references().len(), 1);
        assert!(!ui.busy());
        on_key(
            &mut ui,
            live.0.as_ref(),
            ratatui::crossterm::event::KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE),
            &Hit::default(),
        );
        assert_eq!(
            ui.draft.editor().expanded(),
            original.replacen("/res", "/research", 1)
        );
        assert!(!enter(&mut ui, live.0.as_ref()));
        let event = next_user(&received);
        assert_eq!(
            event.payload["text"],
            original.replacen("/res", "/research", 1)
        );
        assert_eq!(event.payload["images"].as_array().unwrap().len(), 1);
        assert!(ui.draft.references().is_empty());
        assert!(ui.busy());
    });
}

#[test]
fn submission_exact_skills_precede_candidates_and_preserve_history() {
    test_support::isolated(|| {
        let (live, received) = session();
        for (input, expected, candidate, folded) in [
            (
                "/res preserved arguments",
                "/res preserved arguments",
                false,
                false,
            ),
            ("/research", "/research", false, false),
            (
                "  /research argument  ",
                "/research argument  ",
                false,
                false,
            ),
            (
                "/res\na\nb\nc\nd\ne\nf\ng",
                "/res\na\nb\nc\nd\ne\nf\ng",
                false,
                true,
            ),
        ] {
            let mut ui = Ui::replayed(&[]);
            ui.domain.skills = vec![
                ("research".into(), String::new()),
                ("research-more".into(), String::new()),
                ("res".into(), String::new()),
            ];
            ui.draft.edit().set("previous history");
            ui.draft.submit();
            if folded {
                ui.draft.edit().paste(input);
            } else {
                ui.draft.edit().set(input);
            }
            for _ in 0..10 {
                ui.draft.next_hint(100);
            }
            let selected = ui.draft.selected();
            ui.initial_origin = Some(StreamRef {
                stream: "parent".into(),
                event: "origin".into(),
            });
            assert!(!enter(&mut ui, live.0.as_ref()));
            let event = next_user(&received);
            assert_eq!(event.payload["text"], expected, "{input:?}");
            assert_eq!(event.origin.unwrap().event, "origin");
            assert!(ui.initial_origin.is_none());
            assert_eq!(ui.draft.selected(), if candidate { 0 } else { selected });
            ui.draft.edit().history_prev();
            assert_eq!(
                ui.draft.editor().expanded(),
                if candidate { "previous history" } else { input }
            );
        }
    });
}

#[test]
fn submission_without_a_session_preserves_origin_and_registered_images() {
    for text in [
        "",
        "ordinary message",
        "/research argument",
        "/res",
        "/help",
        "/he",
    ] {
        let mut ui = Ui::replayed(&[]);
        ui.domain.skills = vec![("research".into(), "test skill".into())];
        ui.initial_origin = Some(StreamRef {
            stream: "parent".into(),
            event: "parent-event".into(),
        });
        attach(&mut ui, "retained.png");
        ui.draft.edit().set(text);
        assert!(!enter(&mut ui, None));
        assert_eq!(ui.draft.references().len(), 1, "{text}");
        assert!(ui.initial_origin.is_some(), "{text}");
        assert_eq!(ui.draft.editor().is_empty(), text != "/res", "{text}");
        assert_eq!(ui.entry_count(), 0, "{text}");
        assert_eq!(
            ui.busy(),
            !matches!(text, "" | "/help" | "/he" | "/res"),
            "{text}"
        );
    }
}
