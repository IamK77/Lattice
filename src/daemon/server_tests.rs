use super::*;
use crate::components::silent_ui;

fn host() -> StreamHost {
    let mut factories: HashMap<String, crate::Factory> = HashMap::new();
    factories.insert(
        silent_ui::NAME.into(),
        Box::new(|_| Box::new(silent_ui::SilentUi::new(Default::default()))),
    );
    StreamHost::new(
        [(
            "blank".into(),
            crate::StreamTemplate {
                registry: [(silent_ui::NAME.into(), silent_ui::manifest())].into(),
                factories,
                assembly: serde_json::from_value(
                    json!({"instances":{"ui":{"component":silent_ui::NAME}},"wires":[]}),
                )
                .unwrap(),
            },
        )]
        .into(),
    )
}
fn through(
    rx: &mpsc::Receiver<ServerMessage>,
    matches: impl Fn(&ServerMessage) -> bool,
) -> ServerMessage {
    loop {
        let message = rx
            .recv_timeout(std::time::Duration::from_secs(3))
            .expect("accepted command must reach the observer");
        assert!(
            !matches!(&message, ServerMessage::Error { .. }),
            "unexpected daemon error: {message:?}"
        );
        if matches(&message) {
            return message;
        }
    }
}

#[test]
fn eof_ends_permission_but_does_not_discard_already_accepted_text() {
    let (tx, rx) = mpsc::channel();
    let worker = std::thread::spawn(move || core_loop(host(), rx, Default::default()));
    let alive = Arc::new(AtomicBool::new(true));
    let (owner_tx, owner_rx) = mpsc::channel();
    let (observer_tx, observer_rx) = mpsc::channel();
    for (id, outbox, receiver, connection) in [
        (1, owner_tx.clone(), &owner_rx, alive.clone()),
        (
            2,
            observer_tx,
            &observer_rx,
            Arc::new(AtomicBool::new(true)),
        ),
    ] {
        tx.send(CoreMessage::Client(Box::new(CoreCommand {
            client_id: id,
            alive: connection,
            outbox,
            message: ClientMessage::Attach {
                stream: "main".into(),
                template: Some("blank".into()),
                derive_from: None,
                capabilities: vec![],
            },
        })))
        .unwrap();
        through(receiver, |m| matches!(m, ServerMessage::Attached { .. }));
    }
    // The reader has already parsed this frame; EOF can set alive=false
    // before the core consumes its queued command. No scheduling guess.
    alive.store(false, Ordering::Release);
    tx.send(CoreMessage::Client(Box::new(CoreCommand {
        client_id: 1,
        alive,
        outbox: owner_tx,
        message: ClientMessage::SendText {
            stream: "main".into(),
            text: "accepted before EOF".into(),
        },
    })))
    .unwrap();
    tx.send(CoreMessage::Disconnected(1)).unwrap();
    let message = through(&observer_rx, |m| {
        matches!(m, ServerMessage::Appended { event, .. }
        if event.event_type == ce::USER_MESSAGE && event.payload["text"] == "accepted before EOF")
    });
    let ServerMessage::Appended { event, .. } = message else {
        unreachable!()
    };
    assert!(
        event.payload["interface"].is_null(),
        "a departed writer cannot lend permission"
    );
    tx.send(CoreMessage::Shutdown).unwrap();
    worker.join().unwrap();
}
