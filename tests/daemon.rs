//! The daemon over a real Unix socket: two clients attach to the same stream
//! and both receive the broadcast — a terminal and a phone watching one
//! conversation. Drives it with a tiny Rust client (no graphics), so it runs
//! in CI.
#![cfg(unix)]

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::json;

use lattice::components::{minimal_loop, scripted_model, silent_ui};
use lattice::core_events as ce;
use lattice::daemon::{ClientMessage, Daemon, ServerMessage};
use lattice::{
    AssemblyManifest, ComponentInstance, ComponentManifest, Factory, StreamHost, StreamTemplate,
    Wire,
};

fn chat_template() -> StreamTemplate {
    let registry: HashMap<String, ComponentManifest> = [
        (silent_ui::NAME.to_string(), silent_ui::manifest()),
        (minimal_loop::NAME.to_string(), minimal_loop::manifest()),
        (scripted_model::NAME.to_string(), scripted_model::manifest()),
    ]
    .into();
    let mut factories: HashMap<String, Factory> = HashMap::new();
    factories.insert(
        silent_ui::NAME.to_string(),
        Box::new(|_| Box::new(silent_ui::SilentUi::new(Arc::new(Mutex::new(Vec::new()))))),
    );
    factories.insert(
        minimal_loop::NAME.to_string(),
        Box::new(|c| Box::new(minimal_loop::MinimalLoop::from_config(c))),
    );
    factories.insert(
        scripted_model::NAME.to_string(),
        Box::new(|c| Box::new(scripted_model::ScriptedModel::from_config(c))),
    );
    let assembly = AssemblyManifest {
        instances: [
            (
                "ui".to_string(),
                ComponentInstance {
                    component: silent_ui::NAME.to_string(),
                    requires: Vec::new(),
                    config: None,
                },
            ),
            (
                "loop".to_string(),
                ComponentInstance {
                    component: minimal_loop::NAME.to_string(),
                    requires: Vec::new(),
                    config: None,
                },
            ),
            (
                "model".to_string(),
                ComponentInstance {
                    component: scripted_model::NAME.to_string(),
                    requires: Vec::new(),
                    config: Some(
                        json!({"script": [{"status": "ok", "text": "hello from the daemon"}]}),
                    ),
                },
            ),
        ]
        .into(),
        wires: vec![
            Wire::new("ui.user", "loop.input"),
            Wire::new("loop.ask", "model.request"),
            Wire::new("model.result", "loop.model"),
            Wire::new("loop.out", "ui.display"),
        ],
    };
    StreamTemplate {
        registry,
        factories,
        assembly,
    }
}

/// A minimal blocking client: send a message, read server messages.
struct Client {
    write: UnixStream,
    read: BufReader<UnixStream>,
}
impl Client {
    fn connect(path: &std::path::Path) -> Self {
        // The daemon binds its socket on a background thread: poll for it
        // with a deadline instead of sleeping a fixed amount and hoping
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let write = loop {
            match UnixStream::connect(path) {
                Ok(stream) => break stream,
                Err(err) if std::time::Instant::now() >= deadline => {
                    panic!("daemon socket never came up: {err}")
                }
                Err(_) => std::thread::sleep(Duration::from_millis(10)),
            }
        };
        // Failure detector, not synchronization: a daemon that stops
        // answering fails the test in 10s instead of hanging it forever
        write
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let read = BufReader::new(write.try_clone().unwrap());
        Self { write, read }
    }
    fn send(&mut self, message: &ClientMessage) {
        let line = serde_json::to_string(message).unwrap();
        writeln!(self.write, "{line}").unwrap();
    }
    /// Read server messages until one satisfies `done`, returning all read.
    fn read_until(&mut self, mut done: impl FnMut(&ServerMessage) -> bool) -> Vec<ServerMessage> {
        let mut out = Vec::new();
        loop {
            let mut line = String::new();
            if self.read.read_line(&mut line).unwrap() == 0 {
                break; // socket closed
            }
            if line.trim().is_empty() {
                continue;
            }
            let msg: ServerMessage = serde_json::from_str(line.trim()).unwrap();
            let stop = done(&msg);
            out.push(msg);
            if stop {
                break;
            }
        }
        out
    }
}

fn with_permissions(mut template: StreamTemplate) -> StreamTemplate {
    use lattice::components::{interface_permissions as ip, operation_policy as op};
    template.registry.insert(ip::NAME.into(), ip::manifest());
    template.registry.insert(op::NAME.into(), op::manifest());
    template.factories.insert(
        ip::NAME.into(),
        Box::new(|c| Box::new(ip::InterfacePermissions::from_config(c))),
    );
    template.factories.insert(
        op::NAME.into(),
        Box::new(|c| Box::new(op::OperationPolicy::from_config(c))),
    );
    for (id, component) in [("permissions", ip::NAME), ("operations", op::NAME)] {
        template.assembly.instances.insert(
            id.into(),
            ComponentInstance {
                component: component.into(),
                requires: vec![],
                config: None,
            },
        );
    }
    template.assembly.wires.extend([
        Wire::new("ui.answer", "permissions.control"),
        Wire::new("ui.answer", "operations.answer"),
    ]);
    template
}

fn attach_permissions(
    client: &mut Client,
    stream: &str,
) -> lattice::daemon::protocol::AuthorizationAttachment {
    client.send(&ClientMessage::Attach {
        stream: stream.into(),
        template: None,
        derive_from: None,
        capabilities: vec![
            "authorize".into(),
            "history-pages".into(),
            lattice::daemon::protocol::PERMISSIONS_CAPABILITY.into(),
        ],
    });
    let messages = client.read_until(|m| {
        matches!(
            m,
            ServerMessage::AttachedV2 { .. } | ServerMessage::Error { .. }
        )
    });
    match messages.into_iter().last().unwrap() {
        ServerMessage::AttachedV2 { authorization, .. } => authorization,
        other => panic!("expected negotiated attachment, got {other:?}"),
    }
}

fn permission_event(message: &ServerMessage, id: &str, action: &str) -> bool {
    matches!(message, ServerMessage::Appended { event, .. }
        if event.event_type == lattice::components::interface_permissions::STATE
        && event.payload["interface"] == id && event.payload["action"] == action)
}

#[test]
fn negotiated_controls_reject_missing_services_and_unnegotiated_bindings() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("lattice.sock");
    let daemon = Daemon::serve(&socket, || {
        StreamHost::new(
            [
                ("plain".into(), chat_template()),
                ("permissions".into(), with_permissions(chat_template())),
            ]
            .into(),
        )
    })
    .unwrap();
    let mut client = Client::connect(&socket);
    client.send(&ClientMessage::Attach {
        stream: "plain".into(),
        template: Some("plain".into()),
        derive_from: None,
        capabilities: vec![lattice::daemon::protocol::PERMISSIONS_CAPABILITY.into()],
    });
    let attached = client.read_until(|m| matches!(m, ServerMessage::AttachedV2 { .. }));
    let ServerMessage::AttachedV2 { authorization, .. } = attached.last().unwrap() else {
        unreachable!()
    };
    assert!(authorization.interface.is_none());
    assert!(authorization.operation_service.is_none());
    client.send(&ClientMessage::SetPermission {
        stream: "plain".into(),
        attachment: authorization.attachment.clone(),
        enabled: true,
    });
    client.read_until(|m| matches!(m, ServerMessage::Error { message, .. } if message.contains("does not support")));
    client.send(&ClientMessage::RevokeGrant {
        stream: "plain".into(),
        attachment: authorization.attachment.clone(),
        grant: "missing".into(),
    });
    client.read_until(|m| matches!(m, ServerMessage::Error { message, .. } if message.contains("does not support")));
    client.send(&ClientMessage::Attach {
        stream: "legacy".into(),
        template: Some("permissions".into()),
        derive_from: None,
        capabilities: vec!["authorize".into()],
    });
    client.read_until(|m| matches!(m, ServerMessage::Attached { .. }));
    let opened = client.read_until(|m| matches!(m, ServerMessage::Appended { event, .. }
        if event.event_type == lattice::components::interface_permissions::STATE && event.payload["action"] == "open"));
    let ServerMessage::Appended { event, .. } = opened.last().unwrap() else {
        unreachable!()
    };
    // Tokens are not secrets. Even knowing the real token does not negotiate
    // an old attachment into accepting the new controls.
    let token = event.payload["interface"].as_str().unwrap().to_string();
    client.send(&ClientMessage::SetPermission {
        stream: "legacy".into(),
        attachment: token,
        enabled: true,
    });
    client.read_until(
        |m| matches!(m, ServerMessage::Error { message, .. } if message.contains("unnegotiated")),
    );
    daemon.stop();
}

struct RecordedRun;
impl lattice::Component for RecordedRun {
    fn handle(&mut self, _: &str, event: &lattice::EventEnvelope, ctx: &mut lattice::Ctx) {
        ctx.emit("outcome", lattice::EventDraft::new(ce::TOOL_EXEC_COMPLETED, &[&event.id],
            json!({"call":event.payload["call"],"tool":"Run","status":"ok","result":{"stdout":"recorded, not executed"}})));
    }
}

#[test]
fn negotiated_history_keeps_old_question_details_and_controls_real_flow_grants() {
    use lattice::components::{operation_policy as op, shell_tools};
    use lattice::daemon::protocol::{ApprovalScope, PERMISSIONS_CAPABILITY};
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("lattice.sock");
    let daemon = Daemon::serve(&socket, || {
        let mut template = with_permissions(chat_template());
        template.assembly.instances.get_mut("operations").unwrap().config = Some(json!({"stance":"ask"}));
        template.assembly.instances.get_mut("model").unwrap().config = Some(json!({"script":[
            {"status":"ok","toolCalls":[{"id":"push","tool":"Run","arguments":{"command":"git push origin main"}}]},
            {"status":"ok","text":"done"}
        ]}));
        template.registry.insert(shell_tools::NAME.into(), shell_tools::manifest());
        template.factories.insert(shell_tools::NAME.into(), Box::new(|_| Box::new(RecordedRun)));
        template.assembly.instances.insert("shell".into(), ComponentInstance { component:shell_tools::NAME.into(), requires:vec![],config:None });
        template.assembly.wires.extend([
            Wire::new("loop.run", "operations.review"), Wire::new("operations.forward", "shell.execute"),
            Wire::new("shell.outcome", "loop.tools"), Wire::new("operations.verdict", "loop.tools"),
        ]);
        StreamHost::new([("chat".into(),template)].into())
    }).unwrap();
    let mut a = Client::connect(&socket);
    let first = attach_permissions(&mut a, "main");
    a.send(&ClientMessage::SendText {
        stream: "main".into(),
        text: "push".into(),
    });
    let opened = a.read_until(|m| matches!(m, ServerMessage::Appended { event, .. } if event.event_type == op::AUTH_REQUESTED));
    let ServerMessage::Appended {
        event: question, ..
    } = opened.last().unwrap()
    else {
        panic!("missing question")
    };
    // Grow a full newer page without consuming another work input.
    for _ in 0..70 {
        a.send(&ClientMessage::SetPermission {
            stream: "main".into(),
            attachment: first.attachment.clone(),
            enabled: false,
        });
        a.read_until(|m| permission_event(m, first.interface.as_deref().unwrap(), "set"));
    }
    let mut b = Client::connect(&socket);
    b.send(&ClientMessage::Attach {
        stream: "main".into(),
        template: None,
        derive_from: None,
        capabilities: vec![
            "authorize".into(),
            "history-pages".into(),
            PERMISSIONS_CAPABILITY.into(),
        ],
    });
    let attached = b.read_until(|m| {
        matches!(
            m,
            ServerMessage::AttachedV2 { .. } | ServerMessage::Error { .. }
        )
    });
    let ServerMessage::AttachedV2 {
        authorization,
        replay,
        ..
    } = attached.last().unwrap()
    else {
        panic!("missing v2 attachment: {attached:?}")
    };
    assert!(!replay.iter().any(|event| event.id == question.id));
    assert_eq!(authorization.pending_authorizations.len(), 1);
    let restored = &authorization.pending_authorizations[0];
    assert_eq!(restored.id, question.id);
    assert_eq!(restored.event_type, op::AUTH_REQUESTED);
    assert_eq!(
        restored.payload["grants"][0]["prefix"],
        json!(["git", "push", "origin"])
    );
    b.send(&ClientMessage::AuthorizeOperation {
        stream: "main".into(),
        attachment: first.attachment,
        request: question.id.clone(),
        approve: true,
        scope: ApprovalScope::Flow,
    });
    b.read_until(
        |m| matches!(m, ServerMessage::Error { message, .. } if message.contains("attachment")),
    );
    b.send(&ClientMessage::AuthorizeOperation {
        stream: "main".into(),
        attachment: authorization.attachment.clone(),
        request: question.id.clone(),
        approve: true,
        scope: ApprovalScope::Flow,
    });
    let granted = b.read_until(|m| matches!(m, ServerMessage::Appended { event, .. } if event.event_type == op::STATE && event.payload["grants"].as_object().is_some_and(|grants| !grants.is_empty())));
    let ServerMessage::Appended { event, .. } = granted.last().unwrap() else {
        panic!("missing grants")
    };
    let grant = event.payload["grants"]
        .as_object()
        .unwrap()
        .keys()
        .next()
        .unwrap()
        .clone();
    let completed = b.read_until(|m| matches!(m, ServerMessage::Appended { event, .. } if event.event_type == ce::TOOL_EXEC_COMPLETED));
    assert!(
        matches!(completed.last().unwrap(), ServerMessage::Appended { event, .. } if event.payload["status"] == "ok")
    );
    b.send(&ClientMessage::RevokeGrant {
        stream: "main".into(),
        attachment: authorization.attachment.clone(),
        grant,
    });
    b.read_until(|m| matches!(m, ServerMessage::Appended { event, .. } if event.event_type == op::STATE && event.payload["grants"] == json!({})));
    daemon.stop();
}

#[test]
fn permission_off_and_disconnect_reach_the_authority_before_a_busy_model_finishes() {
    for ending in ["detach", "eof"] {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("lattice.sock");
        let daemon = Daemon::serve(&socket, || {
            StreamHost::new([("chat".into(), with_permissions(blocking_template()))].into())
        })
        .unwrap();
        let mut owner = Client::connect(&socket);
        let binding = attach_permissions(&mut owner, "main");
        let id = binding.interface.as_deref().unwrap();
        let mut observer = Client::connect(&socket);
        attach_permissions(&mut observer, "main");
        owner.send(&ClientMessage::SetPermission {
            stream: "main".into(),
            attachment: binding.attachment.clone(),
            enabled: true,
        });
        owner.read_until(|m| permission_event(m, id, "set"));
        owner.send(&ClientMessage::SendText {
            stream: "main".into(),
            text: "park".into(),
        });
        owner.read_until(is_parked_notice);
        owner.send(&ClientMessage::SetPermission {
            stream: "main".into(),
            attachment: binding.attachment.clone(),
            enabled: false,
        });
        let off = owner.read_until(|m| permission_event(m, id, "set"));
        assert!(!off.iter().any(|m| matches!(m, ServerMessage::Appended { event, .. } if event.event_type == ce::MODEL_CALL_COMPLETED)));
        let ServerMessage::Appended { event, .. } = off.last().unwrap() else {
            panic!("missing permission state")
        };
        assert_eq!(event.payload["interfaces"][id]["enabled"], false);
        owner.send(&ClientMessage::SetPermission {
            stream: "main".into(),
            attachment: binding.attachment,
            enabled: true,
        });
        owner.read_until(|m| permission_event(m, id, "set"));
        if ending == "detach" {
            owner.send(&ClientMessage::Detach {
                stream: "main".into(),
            });
        } else {
            drop(owner);
        }
        let closed = observer.read_until(|m| permission_event(m, id, "close"));
        assert!(!closed.iter().any(|m| matches!(m, ServerMessage::Appended { event, .. } if event.event_type == ce::MODEL_CALL_COMPLETED)), "{ending} waited behind the model");
        observer.send(&ClientMessage::Interrupt {
            stream: "main".into(),
        });
        let completed = observer.read_until(|m| matches!(m, ServerMessage::Appended { event, .. } if event.event_type == ce::MODEL_CALL_COMPLETED));
        assert!(cancelled_completion(&completed));
        daemon.stop();
    }
}

#[test]
fn permissions_belong_to_attachment_lifetimes_not_observers_or_reconnections() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("lattice.sock");
    let daemon = Daemon::serve(&socket, || {
        StreamHost::new([("chat".into(), with_permissions(chat_template()))].into())
    })
    .unwrap();
    let mut a = Client::connect(&socket);
    let mut b = Client::connect(&socket);
    let first = attach_permissions(&mut a, "shared");
    let observer = attach_permissions(&mut b, "shared");
    assert_ne!(first.interface, observer.interface);
    let id = first.interface.as_deref().unwrap();
    a.send(&ClientMessage::SetPermission {
        stream: "shared".into(),
        attachment: first.attachment.clone(),
        enabled: true,
    });
    let state = b.read_until(|m| permission_event(m, id, "set"));
    let ServerMessage::Appended { event, .. } = state.last().unwrap() else {
        panic!("missing permission state")
    };
    assert_eq!(event.payload["interfaces"][id]["enabled"], true);
    assert_ne!(
        event.payload["interfaces"][observer.interface.as_deref().unwrap()]["enabled"],
        true
    );
    b.send(&ClientMessage::SendText {
        stream: "shared".into(),
        text: "from the observer".into(),
    });
    let inputs = b.read_until(
        |m| matches!(m, ServerMessage::Appended {event,..} if event.event_type == ce::USER_MESSAGE),
    );
    let ServerMessage::Appended { event, .. } = inputs.last().unwrap() else {
        panic!("missing input")
    };
    assert_eq!(
        event.payload["interface"],
        observer.interface.as_deref().unwrap()
    );
    a.send(&ClientMessage::Detach {
        stream: "shared".into(),
    });
    b.read_until(|m| permission_event(m, id, "close"));
    a.send(&ClientMessage::SetPermission {
        stream: "shared".into(),
        attachment: first.attachment.clone(),
        enabled: true,
    });
    let errors = a.read_until(|m| matches!(m, ServerMessage::Error { .. }));
    assert!(matches!(errors.last(), Some(ServerMessage::Error { .. })));
    let next = attach_permissions(&mut a, "shared");
    assert_ne!(next.attachment, first.attachment);
    a.send(&ClientMessage::SetPermission {
        stream: "shared".into(),
        attachment: next.attachment,
        enabled: true,
    });
    b.read_until(|m| permission_event(m, next.interface.as_deref().unwrap(), "set"));
    drop(a);
    let closed = b.read_until(|m| permission_event(m, next.interface.as_deref().unwrap(), "close"));
    let ServerMessage::Appended { event, .. } = closed.last().unwrap() else {
        panic!("missing close")
    };
    assert_eq!(
        event.payload["interfaces"][next.interface.as_deref().unwrap()]["enabled"],
        false
    );
    daemon.stop();
}

#[test]
fn two_clients_watch_one_conversation() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("lattice.sock");
    let daemon = Daemon::serve(&socket, || {
        StreamHost::new([("chat".to_string(), chat_template())].into())
    })
    .unwrap();

    // Alice attaches (opening the stream), Bob attaches to the same one
    let mut alice = Client::connect(&socket);
    alice.send(&ClientMessage::Attach {
        stream: "main".into(),
        template: None,
        derive_from: None,
        capabilities: vec![],
    });
    let attached = alice.read_until(|m| matches!(m, ServerMessage::Attached { .. }));
    assert!(matches!(
        attached.last().unwrap(),
        ServerMessage::Attached { .. }
    ));

    let mut bob = Client::connect(&socket);
    bob.send(&ClientMessage::Attach {
        stream: "main".into(),
        template: None,
        derive_from: None,
        capabilities: vec![],
    });
    bob.read_until(|m| matches!(m, ServerMessage::Attached { .. }));

    // Alice sends a line; BOTH clients must see the conversation and the reply
    alice.send(&ClientMessage::SendText {
        stream: "main".into(),
        text: "hi".into(),
    });

    let alice_saw = alice.read_until(|m| matches!(m, ServerMessage::Quiescent { .. }));
    let bob_saw = bob.read_until(|m| matches!(m, ServerMessage::Quiescent { .. }));

    let reply = |msgs: &[ServerMessage]| -> Option<String> {
        msgs.iter().find_map(|m| match m {
            ServerMessage::Appended { event, .. }
                if event.event_type == lattice::core_events::OUTPUT_REPLY =>
            {
                event.payload["text"].as_str().map(str::to_string)
            }
            _ => None,
        })
    };
    assert_eq!(reply(&alice_saw).as_deref(), Some("hello from the daemon"));
    assert_eq!(reply(&bob_saw).as_deref(), Some("hello from the daemon"));

    // Bob's user message was Alice's — the ledger is shared, broadcast to both
    let user_lines = |msgs: &[ServerMessage]| {
        msgs.iter()
            .filter(|m| {
                matches!(m, ServerMessage::Appended { event, .. }
                    if event.event_type == lattice::core_events::USER_MESSAGE)
            })
            .count()
    };
    assert_eq!(user_lines(&bob_saw), 1);

    daemon.stop();
}

#[test]
fn history_pages_are_bounded_contiguous_and_do_not_include_later_appends() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("lattice.sock");
    let daemon = Daemon::serve(&socket, || {
        StreamHost::new([("chat".to_string(), chat_template())].into())
    })
    .unwrap();
    let mut client = Client::connect(&socket);
    let attach = ClientMessage::Attach {
        stream: "main".into(),
        template: None,
        derive_from: None,
        capabilities: vec!["history-pages".into()],
    };
    client.send(&attach);
    client.read_until(|m| matches!(m, ServerMessage::Attached { .. }));
    for _ in 0..80 {
        client.send(&ClientMessage::SendText {
            stream: "main".into(),
            text: "hello".into(),
        });
        client.read_until(|m| matches!(m, ServerMessage::Quiescent { .. }));
    }
    client.send(&attach);
    let got = client.read_until(|m| matches!(m, ServerMessage::Attached { .. }));
    let ServerMessage::Attached {
        replay,
        history: Some(history),
        ..
    } = got.last().unwrap()
    else {
        panic!("paged attach required")
    };
    assert_eq!(replay.len(), 128);
    assert!(!history.state.busy);
    let through = history.through;
    let mut expected = replay.first().unwrap().seq;
    let first_cursor = history.older.clone().unwrap();
    let mut cursor = Some(first_cursor.clone());
    client.send(&ClientMessage::SendText {
        stream: "main".into(),
        text: "after boundary".into(),
    });
    client.read_until(|m| matches!(m, ServerMessage::Quiescent { .. }));
    while let Some(request) = cursor {
        client.send(&ClientMessage::History {
            stream: "main".into(),
            cursor: request.clone(),
        });
        let got = client.read_until(|m| matches!(m, ServerMessage::HistoryPage { .. }));
        let ServerMessage::HistoryPage {
            replay,
            older,
            cursor: echoed,
            ..
        } = got.last().unwrap()
        else {
            unreachable!()
        };
        assert_eq!(echoed, &request);
        assert!(replay.len() <= 128);
        assert_eq!(replay.last().unwrap().seq + 1, expected);
        for pair in replay.windows(2) {
            assert_eq!(pair[0].seq + 1, pair[1].seq);
        }
        assert!(replay.iter().all(|event| event.seq <= through));
        expected = replay.first().unwrap().seq;
        cursor = older.clone();
    }
    assert_eq!(expected, 1);
    client.send(&ClientMessage::History {
        stream: "main".into(),
        cursor: first_cursor,
    });
    let got = client.read_until(|m| matches!(m, ServerMessage::HistoryError { .. }));
    assert!(
        matches!(got.last(), Some(ServerMessage::HistoryError { message, .. }) if message.contains("stale"))
    );
    let mut legacy = Client::connect(&socket);
    legacy.send(&ClientMessage::Attach {
        stream: "main".into(),
        template: None,
        derive_from: None,
        capabilities: vec![],
    });
    let got = legacy.read_until(|m| matches!(m, ServerMessage::Error { .. }));
    assert!(
        matches!(got.last(), Some(ServerMessage::Error { message, .. }) if message.contains("history-pages"))
    );
    daemon.stop();
}

#[test]
fn a_late_client_gets_the_backlog_on_attach() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("lattice.sock");
    let daemon = Daemon::serve(&socket, || {
        StreamHost::new([("chat".to_string(), chat_template())].into())
    })
    .unwrap();

    let mut alice = Client::connect(&socket);
    alice.send(&ClientMessage::Attach {
        stream: "main".into(),
        template: None,
        derive_from: None,
        capabilities: vec![],
    });
    alice.read_until(|m| matches!(m, ServerMessage::Attached { .. }));
    alice.send(&ClientMessage::SendText {
        stream: "main".into(),
        text: "earlier".into(),
    });
    alice.read_until(|m| matches!(m, ServerMessage::Quiescent { .. }));

    // A client connecting AFTER the conversation gets the whole ledger replayed
    let mut latecomer = Client::connect(&socket);
    latecomer.send(&ClientMessage::Attach {
        stream: "main".into(),
        template: None,
        derive_from: None,
        capabilities: vec![],
    });
    let got = latecomer.read_until(|m| matches!(m, ServerMessage::Attached { .. }));
    let ServerMessage::Attached { replay, .. } = got.last().unwrap() else {
        panic!("expected Attached");
    };
    assert!(replay.iter().any(|e| e.payload["text"] == "earlier"));
    assert!(!replay.is_empty(), "the backlog was replayed on attach");

    daemon.stop();
}

#[test]
fn a_sidechannel_opens_derived_from_its_parent() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("lattice.sock");
    let daemon = Daemon::serve(&socket, || {
        StreamHost::new([("chat".to_string(), chat_template())].into())
    })
    .unwrap();

    // Open the main conversation and give it some history
    let mut main = Client::connect(&socket);
    main.send(&ClientMessage::Attach {
        stream: "main".into(),
        template: None,
        derive_from: None,
        capabilities: vec![],
    });
    main.read_until(|m| matches!(m, ServerMessage::Attached { .. }));
    main.send(&ClientMessage::SendText {
        stream: "main".into(),
        text: "in the main chat".into(),
    });
    main.read_until(|m| matches!(m, ServerMessage::Quiescent { .. }));

    // /btw: open a sidechannel derived from the main stream. It attaches as its
    // own (empty) stream, distinct from main — and, being derived, observes the
    // parent read-only at the core (proven at the StreamHost level in
    // tests/multi_stream.rs).
    let mut btw = Client::connect(&socket);
    btw.send(&ClientMessage::Attach {
        stream: "btw".into(),
        template: None,
        derive_from: Some("main".into()),
        capabilities: vec![],
    });
    let got = btw.read_until(|m| matches!(m, ServerMessage::Attached { .. }));
    let ServerMessage::Attached { stream, replay, .. } = got.last().unwrap() else {
        panic!("expected Attached");
    };
    assert_eq!(stream, "btw");
    // The replay is the ledger, and every ledger opens with its own
    // `core.stream.opened` — so "empty" now means "nothing was said yet".
    assert_eq!(replay.len(), 1, "a fresh sidechannel: {replay:?}");
    assert_eq!(replay[0].event_type, ce::STREAM_OPENED);

    // The sidechannel is its own conversation — a message there does not touch main
    btw.send(&ClientMessage::SendText {
        stream: "btw".into(),
        text: "quick aside".into(),
    });
    btw.read_until(|m| matches!(m, ServerMessage::Quiescent { .. }));

    daemon.stop();
}

#[test]
fn a_repeated_attach_does_not_double_the_broadcast() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("lattice.sock");
    let daemon = Daemon::serve(&socket, || {
        StreamHost::new([("chat".to_string(), chat_template())].into())
    })
    .unwrap();

    // Attach twice from the same connection (a frontend re-running `/new main`
    // does exactly this). The second attach must replace the subscription,
    // not add a second copy of it.
    let mut client = Client::connect(&socket);
    for _ in 0..2 {
        client.send(&ClientMessage::Attach {
            stream: "main".into(),
            template: None,
            derive_from: None,
            capabilities: vec![],
        });
        client.read_until(|m| matches!(m, ServerMessage::Attached { .. }));
    }

    client.send(&ClientMessage::SendText {
        stream: "main".into(),
        text: "once please".into(),
    });
    let turn = client.read_until(|m| matches!(m, ServerMessage::Quiescent { .. }));
    let user_lines = turn
        .iter()
        .filter(|m| {
            matches!(m, ServerMessage::Appended { event, .. }
                if event.event_type == "core.input.user_message")
        })
        .count();
    assert_eq!(
        user_lines, 1,
        "each event must reach this client exactly once"
    );

    daemon.stop();
}

/// A model that parks until its work is cancelled — the probe for "an
/// interrupt reaches a running turn". It notifies once parked so tests
/// synchronize on causality instead of clocks, and carries a safety valve so
/// a broken interrupt path fails the test instead of hanging it.
struct BlockingModel;
impl lattice::Component for BlockingModel {
    fn handle(&mut self, port: &str, event: &lattice::EventEnvelope, ctx: &mut lattice::Ctx) {
        if port != "request" {
            return;
        }
        ctx.notify(json!({"parked": true}));
        let safety = std::time::Instant::now() + Duration::from_secs(10);
        while !ctx.cancelled() && std::time::Instant::now() < safety {
            std::thread::sleep(Duration::from_millis(5));
        }
        let payload = if ctx.cancelled() {
            json!({"status": "cancelled"})
        } else {
            json!({"status": "error", "error": {
                "code": "test.never_cancelled",
                "message": "the interrupt never arrived within the safety valve",
                "blame": "environment",
            }})
        };
        ctx.emit(
            "result",
            lattice::EventDraft::new(ce::MODEL_CALL_COMPLETED, &[&event.id], payload),
        );
    }
}

/// Like `chat_template`, but the brain is a BlockingModel and the interrupt
/// wire is connected — the assembly shape a real frontend uses.
fn blocking_template() -> StreamTemplate {
    let mut manifest = scripted_model::manifest();
    manifest.name = "blocking-model".to_string(); // same shape, incl. control
    let registry: HashMap<String, ComponentManifest> = [
        (silent_ui::NAME.to_string(), silent_ui::manifest()),
        (minimal_loop::NAME.to_string(), minimal_loop::manifest()),
        ("blocking-model".to_string(), manifest),
    ]
    .into();
    let mut factories: HashMap<String, Factory> = HashMap::new();
    factories.insert(
        silent_ui::NAME.to_string(),
        Box::new(|_| Box::new(silent_ui::SilentUi::new(Arc::new(Mutex::new(Vec::new()))))),
    );
    factories.insert(
        minimal_loop::NAME.to_string(),
        Box::new(|c| Box::new(minimal_loop::MinimalLoop::from_config(c))),
    );
    factories.insert(
        "blocking-model".to_string(),
        Box::new(|_| Box::new(BlockingModel)),
    );
    let assembly = AssemblyManifest {
        instances: [
            (
                "ui".to_string(),
                ComponentInstance {
                    component: silent_ui::NAME.to_string(),
                    requires: Vec::new(),
                    config: None,
                },
            ),
            (
                "loop".to_string(),
                ComponentInstance {
                    component: minimal_loop::NAME.to_string(),
                    requires: Vec::new(),
                    config: None,
                },
            ),
            (
                "model".to_string(),
                ComponentInstance {
                    component: "blocking-model".to_string(),
                    requires: Vec::new(),
                    config: None,
                },
            ),
        ]
        .into(),
        wires: vec![
            Wire::new("ui.user", "loop.input"),
            Wire::new("loop.ask", "model.request"),
            Wire::new("model.result", "loop.model"),
            Wire::new("loop.out", "ui.display"),
            Wire::new("ui.interrupt", "model.control"),
        ],
    };
    StreamTemplate {
        registry,
        factories,
        assembly,
    }
}

fn is_parked_notice(message: &ServerMessage) -> bool {
    matches!(message, ServerMessage::Notice { payload, .. } if payload["parked"] == true)
}

fn cancelled_completion(messages: &[ServerMessage]) -> bool {
    messages.iter().any(|m| {
        matches!(m, ServerMessage::Appended { event, .. }
            if event.event_type == ce::MODEL_CALL_COMPLETED
                && event.payload["status"] == "cancelled")
    })
}

#[test]
fn an_interrupt_cancels_a_running_turn_and_the_stream_lives_on() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("lattice.sock");
    let daemon = Daemon::serve(&socket, || {
        StreamHost::new([("blocking".to_string(), blocking_template())].into())
    })
    .unwrap();

    let mut client = Client::connect(&socket);
    client.send(&ClientMessage::Attach {
        stream: "main".into(),
        template: Some("blocking".into()),
        derive_from: None,
        capabilities: vec![],
    });
    client.read_until(|m| matches!(m, ServerMessage::Attached { .. }));

    // Twice over — an interrupted stream must remain a usable stream
    for round in 1..=2 {
        client.send(&ClientMessage::SendText {
            stream: "main".into(),
            text: format!("round {round}"),
        });
        // Causal sync: the model says it is parked before we interrupt
        client.read_until(is_parked_notice);
        client.send(&ClientMessage::Interrupt {
            stream: "main".into(),
        });
        let turn = client.read_until(|m| matches!(m, ServerMessage::Quiescent { .. }));
        assert!(
            cancelled_completion(&turn),
            "round {round}: expected a cancelled completion, got {turn:?}"
        );
    }

    daemon.stop();
}

#[test]
fn an_interrupt_on_an_idle_stream_is_recorded_not_fatal() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("lattice.sock");
    let daemon = Daemon::serve(&socket, || {
        StreamHost::new([("chat".to_string(), chat_template())].into())
    })
    .unwrap();

    let mut client = Client::connect(&socket);
    client.send(&ClientMessage::Attach {
        stream: "main".into(),
        template: None,
        derive_from: None,
        capabilities: vec![],
    });
    client.read_until(|m| matches!(m, ServerMessage::Attached { .. }));

    // Interrupt with no turn running: the injection fires a wake, so the
    // interrupted event is recorded (audit-first) and the wake-run ends in a
    // quiescent — one injection path for everything. No ghost reply appears.
    client.send(&ClientMessage::Interrupt {
        stream: "main".into(),
    });
    let seen = client.read_until(|m| matches!(m, ServerMessage::Quiescent { .. }));
    assert!(
        seen.iter()
            .any(|m| matches!(m, ServerMessage::Appended { event, .. }
            if event.event_type == ce::INTERRUPTED)),
        "the idle interrupt must be recorded"
    );
    assert!(
        !seen
            .iter()
            .any(|m| matches!(m, ServerMessage::Appended { event, .. }
            if event.event_type == ce::OUTPUT_REPLY)),
        "an idle interrupt must not conjure a reply"
    );

    // The stream still works — read to the reply itself (past its quiescent)
    client.send(&ClientMessage::SendText {
        stream: "main".into(),
        text: "still alive?".into(),
    });
    let turn = client.read_until(|m| {
        matches!(m, ServerMessage::Appended { event, .. }
        if event.event_type == ce::OUTPUT_REPLY)
    });
    assert!(turn
        .iter()
        .any(|m| matches!(m, ServerMessage::Appended { event, .. }
        if event.event_type == ce::OUTPUT_REPLY)));

    daemon.stop();
}

#[test]
fn a_parked_turn_in_one_stream_does_not_block_another_stream() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("lattice.sock");
    let daemon = Daemon::serve(&socket, || {
        StreamHost::new(
            [
                ("chat".to_string(), chat_template()),
                ("blocking".to_string(), blocking_template()),
            ]
            .into(),
        )
    })
    .unwrap();

    // Park a turn in one stream (causally confirmed, no clocks)…
    let mut left = Client::connect(&socket);
    left.send(&ClientMessage::Attach {
        stream: "left".into(),
        template: Some("blocking".into()),
        derive_from: None,
        capabilities: vec![],
    });
    left.read_until(|m| matches!(m, ServerMessage::Attached { .. }));
    left.send(&ClientMessage::SendText {
        stream: "left".into(),
        text: "park here".into(),
    });
    left.read_until(is_parked_notice);

    // …and, while it is parked, run a WHOLE turn in another stream. If
    // streams shared a queue this read would hit the failure-detector
    // timeout, because the parked turn never finishes on its own.
    let mut right = Client::connect(&socket);
    right.send(&ClientMessage::Attach {
        stream: "right".into(),
        template: None,
        derive_from: None,
        capabilities: vec![],
    });
    right.read_until(|m| matches!(m, ServerMessage::Attached { .. }));
    right.send(&ClientMessage::SendText {
        stream: "right".into(),
        text: "quick one".into(),
    });
    right.read_until(|m| matches!(m, ServerMessage::Quiescent { .. }));

    // Release the parked stream and see it finish as cancelled
    left.send(&ClientMessage::Interrupt {
        stream: "left".into(),
    });
    let turn = left.read_until(|m| matches!(m, ServerMessage::Quiescent { .. }));
    assert!(cancelled_completion(&turn));

    daemon.stop();
}

#[test]
fn a_detached_client_stops_receiving_broadcasts() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("lattice.sock");
    let daemon = Daemon::serve(&socket, || {
        StreamHost::new([("chat".to_string(), chat_template())].into())
    })
    .unwrap();

    // Attach, detach, then drive a turn FROM THE SAME CONNECTION. The
    // per-stream driver processes one connection's commands in arrival
    // order, so the unsubscribe is guaranteed to land before the turn —
    // silence below is proof of the detach, not of a race won.
    let mut client = Client::connect(&socket);
    client.send(&ClientMessage::Attach {
        stream: "main".into(),
        template: None,
        derive_from: None,
        capabilities: vec![],
    });
    client.read_until(|m| matches!(m, ServerMessage::Attached { .. }));
    client.send(&ClientMessage::Detach {
        stream: "main".into(),
    });
    client.send(&ClientMessage::SendText {
        stream: "main".into(),
        text: "into the silence".into(),
    });

    // Negative-space probe: a bounded window in which nothing may arrive
    client
        .write
        .set_read_timeout(Some(Duration::from_millis(300)))
        .unwrap();
    let mut line = String::new();
    let got = client.read.read_line(&mut line);
    assert!(
        got.is_err() || got.unwrap() == 0,
        "a detached client still received: {line}"
    );

    // The turn DID run — re-attaching replays it. This distinguishes "the
    // detach worked" from "the daemon did nothing at all".
    client
        .write
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    client.send(&ClientMessage::Attach {
        stream: "main".into(),
        template: None,
        derive_from: None,
        capabilities: vec![],
    });
    let got = client.read_until(|m| matches!(m, ServerMessage::Attached { .. }));
    let ServerMessage::Attached { replay, .. } = got.last().unwrap() else {
        panic!("expected Attached");
    };
    assert!(replay
        .iter()
        .any(|e| e.payload["text"] == "into the silence"));

    daemon.stop();
}

#[test]
fn a_daemon_restart_does_not_break_the_conversation() {
    let dir = tempfile::tempdir().unwrap();
    let ledger_dir = dir.path().join("streams");
    let make_host = {
        let ledger_dir = ledger_dir.clone();
        move || {
            let ledger_dir = ledger_dir.clone();
            StreamHost::new([("chat".to_string(), chat_template())].into()).with_ledger_path(
                move |stream| {
                    std::fs::create_dir_all(&ledger_dir).ok();
                    Some(ledger_dir.join(format!("{stream}.jsonl")))
                },
            )
        }
    };

    // Life one: a whole turn, then the daemon goes away entirely
    let socket_one = dir.path().join("one.sock");
    let daemon = Daemon::serve(&socket_one, {
        let make_host = make_host.clone();
        move || make_host()
    })
    .unwrap();
    let mut client = Client::connect(&socket_one);
    client.send(&ClientMessage::Attach {
        stream: "main".into(),
        template: None,
        derive_from: None,
        capabilities: vec![],
    });
    client.read_until(|m| matches!(m, ServerMessage::Attached { .. }));
    client.send(&ClientMessage::SendText {
        stream: "main".into(),
        text: "before the restart".into(),
    });
    let turn_one = client.read_until(|m| matches!(m, ServerMessage::Quiescent { .. }));
    let user_one = turn_one
        .iter()
        .find_map(|m| match m {
            ServerMessage::Appended { event, .. } if event.event_type == ce::USER_MESSAGE => {
                Some(event.id.clone())
            }
            _ => None,
        })
        .expect("the user line was recorded");
    drop(client);
    daemon.stop();

    // Life two: a NEW daemon process-equivalent on the same ledgers
    let socket_two = dir.path().join("two.sock");
    let daemon = Daemon::serve(&socket_two, make_host).unwrap();
    let mut client = Client::connect(&socket_two);
    client.send(&ClientMessage::Attach {
        stream: "main".into(),
        template: None,
        derive_from: None,
        capabilities: vec![],
    });
    let got = client.read_until(|m| matches!(m, ServerMessage::Attached { .. }));
    let ServerMessage::Attached { replay, .. } = got.last().unwrap() else {
        panic!("expected Attached");
    };
    // The history is intact across the restart…
    assert!(replay.iter().any(|e| e.id == user_one));

    // …and so is the conversation's MEMORY: the next turn's model call
    // carries life one's events as material
    client.send(&ClientMessage::SendText {
        stream: "main".into(),
        text: "after the restart".into(),
    });
    let turn_two = client.read_until(|m| matches!(m, ServerMessage::Quiescent { .. }));
    let ask = turn_two
        .iter()
        .find_map(|m| match m {
            ServerMessage::Appended { event, .. } if event.event_type == ce::MODEL_CALL_STARTED => {
                Some(event.payload.clone())
            }
            _ => None,
        })
        .expect("the new turn asked the model");
    let parts: Vec<&str> = ask["input"]["parts"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|p| p["event"].as_str())
        .collect();
    assert!(
        parts.contains(&user_one.as_str()),
        "the restarted daemon must remember life one: {parts:?}"
    );

    daemon.stop();
}

/// On its first trigger, spawns a background thread that (after a beat)
/// injects a user message — a stand-in for a background task finishing, a
/// timer firing, a monitor tripping. Proves the daemon's push loop wakes a
/// turn from a background injection and broadcasts it to watching clients.
struct BackgroundWaker {
    fired: bool,
}
impl lattice::Component for BackgroundWaker {
    fn handle(&mut self, port: &str, _event: &lattice::EventEnvelope, ctx: &mut lattice::Ctx) {
        if port != "trigger" || self.fired {
            return;
        }
        self.fired = true;
        let injector = ctx.injector();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(80));
            injector.emit(
                "out",
                lattice::EventDraft::new(
                    ce::USER_MESSAGE,
                    &[],
                    json!({"text": "from the background"}),
                ),
            );
        });
    }
}

fn background_template() -> StreamTemplate {
    let waker = ComponentManifest {
        name: "bg-waker".to_string(),
        version: "0".to_string(),
        runtime: lattice::RuntimeKind::Inproc,
        entry: "builtin:bg-waker".to_string(),
        inputs: vec![lattice::PortDecl::new("trigger", &[ce::USER_MESSAGE])],
        outputs: vec![lattice::PortDecl::new("out", &[ce::USER_MESSAGE])],
        events: Vec::new(),
        default_wiring: Vec::new(),
        capabilities: None,
        implements: Vec::new(),
        tools: Vec::new(),
        prompt: None,
        handle_timeout_ms: None,
        concurrency: None,
    };
    let registry: HashMap<String, ComponentManifest> = [
        (silent_ui::NAME.to_string(), silent_ui::manifest()),
        ("bg-waker".to_string(), waker),
    ]
    .into();
    let mut factories: HashMap<String, Factory> = HashMap::new();
    factories.insert(
        silent_ui::NAME.to_string(),
        Box::new(|_| Box::new(silent_ui::SilentUi::new(Arc::new(Mutex::new(Vec::new()))))),
    );
    factories.insert(
        "bg-waker".to_string(),
        Box::new(|_| Box::new(BackgroundWaker { fired: false })),
    );
    let assembly = AssemblyManifest {
        instances: [
            (
                "ui".to_string(),
                ComponentInstance {
                    component: silent_ui::NAME.to_string(),
                    requires: Vec::new(),
                    config: None,
                },
            ),
            (
                "waker".to_string(),
                ComponentInstance {
                    component: "bg-waker".to_string(),
                    requires: Vec::new(),
                    config: None,
                },
            ),
        ]
        .into(),
        // The user's text triggers the waker; its background injection later
        // records on its own (unwired out — recorded, hence broadcast)
        wires: vec![Wire::new("ui.user", "waker.trigger")],
    };
    StreamTemplate {
        registry,
        factories,
        assembly,
    }
}

#[test]
fn a_background_injection_wakes_the_daemon_and_reaches_a_client() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("lattice.sock");
    let daemon = Daemon::serve(&socket, || {
        StreamHost::new([("bg".to_string(), background_template())].into())
    })
    .unwrap();

    let mut client = Client::connect(&socket);
    client.send(&ClientMessage::Attach {
        stream: "main".into(),
        template: Some("bg".into()),
        derive_from: None,
        capabilities: vec![],
    });
    client.read_until(|m| matches!(m, ServerMessage::Attached { .. }));

    // One message kicks the waker; its turn goes quiescent
    client.send(&ClientMessage::SendText {
        stream: "main".into(),
        text: "kick".into(),
    });
    client.read_until(|m| matches!(m, ServerMessage::Quiescent { .. }));

    // Now WITHOUT sending anything, the client must receive the background
    // injection — the daemon's push loop woke a turn on its own
    let woken = client.read_until(|m| {
        matches!(m, ServerMessage::Appended { event, .. }
            if event.event_type == ce::USER_MESSAGE
                && event.payload["text"] == "from the background")
    });
    assert!(
        woken
            .iter()
            .any(|m| matches!(m, ServerMessage::Appended { event, .. }
            if event.payload["text"] == "from the background")),
        "the background injection must reach the client with no further input"
    );

    daemon.stop();
}

/// The handshake's capability check. An assembly whose trust gate ASKS may
/// put authorization cards in front of a client; a client that declared no
/// "authorize" capability is told so at attach — out loud, on Attached —
/// while a capable client hears nothing. The attach itself always succeeds
/// (observers are legitimate).
#[test]
fn a_client_that_cannot_authorize_is_warned_at_the_handshake() {
    use lattice::components::trust_policy;

    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("lattice.sock");
    // The template is built inside the serve closure (factories are not
    // Send); unwired trust is enough for the handshake check — the warning
    // reads the instance's stance, not the wiring
    let daemon = Daemon::serve(&socket, move || {
        let mut template = chat_template();
        template
            .registry
            .insert(trust_policy::NAME.to_string(), trust_policy::manifest());
        template.factories.insert(
            trust_policy::NAME.to_string(),
            Box::new(|c| Box::new(trust_policy::TrustPolicy::from_config(c))),
        );
        template.assembly.instances.insert(
            "trust".to_string(),
            ComponentInstance {
                component: trust_policy::NAME.to_string(),
                requires: Vec::new(),
                config: Some(json!({"stance": "ask"})),
            },
        );
        StreamHost::new([("chat".to_string(), template)].into())
    })
    .unwrap();

    let mut mute = Client::connect(&socket);
    mute.send(&ClientMessage::Attach {
        stream: "main".into(),
        template: None,
        derive_from: None,
        capabilities: vec![],
    });
    let got = mute.read_until(|m| matches!(m, ServerMessage::Attached { .. }));
    let ServerMessage::Attached { warnings, .. } = got.last().unwrap() else {
        panic!("expected Attached");
    };
    assert_eq!(warnings.len(), 1, "the mute client must be warned");
    assert!(warnings[0].contains("authorize"), "{warnings:?}");

    let mut capable = Client::connect(&socket);
    capable.send(&ClientMessage::Attach {
        stream: "main".into(),
        template: None,
        derive_from: None,
        capabilities: vec!["authorize".into()],
    });
    let got = capable.read_until(|m| matches!(m, ServerMessage::Attached { .. }));
    let ServerMessage::Attached { warnings, .. } = got.last().unwrap() else {
        panic!("expected Attached");
    };
    assert!(warnings.is_empty(), "a capable client hears nothing");

    daemon.stop();
}

/// Stopping means the streams have wound down, not that the process is about
/// to take them with it.
///
/// `stop` used to detach: it set a flag, joined the accept thread and
/// returned, leaving every stream's kernel holding whatever it held. The
/// ledgers survived only because each event is fsynced as it is written —
/// none of the stop protocol (refuse new work, let what is in flight finish,
/// seal) happened at all. It also could not have waited even if it tried:
/// the core loop ended only when every client had disconnected, and a daemon
/// is stopped while people are still attached.
/// A component that leaves a mark when its thread ends — which is what
/// `Kernel::shutdown` makes happen, and what a detached stop never did.
struct Witness(std::path::PathBuf);
impl lattice::Component for Witness {
    fn handle(&mut self, _p: &str, _e: &lattice::EventEnvelope, _c: &mut lattice::Ctx) {}
}
impl Drop for Witness {
    fn drop(&mut self) {
        let _ = std::fs::write(&self.0, "wound down");
    }
}

#[test]
fn stopping_the_daemon_winds_its_streams_down_while_a_client_is_attached() {
    let dir = tempfile::tempdir().unwrap();
    let ledger_dir = dir.path().join("streams");
    let socket = dir.path().join("stop.sock");
    let mark = dir.path().join("wound-down.txt");
    let daemon = Daemon::serve(&socket, {
        let ledger_dir = ledger_dir.clone();
        let mark = mark.clone();
        move || {
            let mut template = chat_template();
            template.registry.insert(
                "witness".to_string(),
                ComponentManifest {
                    name: "witness".to_string(),
                    version: "0".to_string(),
                    runtime: lattice::RuntimeKind::Inproc,
                    entry: "builtin:witness".to_string(),
                    inputs: vec![lattice::PortDecl::new(
                        "input",
                        &[lattice::core_events::USER_MESSAGE],
                    )],
                    outputs: Vec::new(),
                    events: Vec::new(),
                    default_wiring: Vec::new(),
                    capabilities: None,
                    implements: Vec::new(),
                    tools: Vec::new(),
                    prompt: None,
                    handle_timeout_ms: None,
                    concurrency: None,
                },
            );
            template.factories.insert(
                "witness".to_string(),
                Box::new(move |_| Box::new(Witness(mark.clone()))),
            );
            template.assembly.instances.insert(
                "witness".to_string(),
                ComponentInstance {
                    component: "witness".to_string(),
                    requires: Vec::new(),
                    config: None,
                },
            );
            template
                .assembly
                .wires
                .push(lattice::Wire::new("ui.user", "witness.input"));
            StreamHost::new([("chat".to_string(), template)].into()).with_ledger_path(
                move |stream| {
                    std::fs::create_dir_all(&ledger_dir).ok();
                    Some(ledger_dir.join(format!("{stream}.jsonl")))
                },
            )
        }
    })
    .unwrap();

    let mut client = Client::connect(&socket);
    client.send(&ClientMessage::Attach {
        stream: "main".into(),
        template: None,
        derive_from: None,
        capabilities: vec![],
    });
    client.read_until(|m| matches!(m, ServerMessage::Attached { .. }));
    client.send(&ClientMessage::SendText {
        stream: "main".into(),
        text: "hello".into(),
    });
    client.read_until(|m| matches!(m, ServerMessage::Quiescent { .. }));

    // The client is still connected. Stopping must not need it to leave, and
    // must not return before the stream has actually stopped.
    let stopped = std::thread::spawn(move || daemon.stop());
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    while !stopped.is_finished() && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        stopped.is_finished(),
        "stop() must return rather than wait for every client to disconnect"
    );
    stopped.join().unwrap();
    assert!(
        !socket.exists(),
        "and the socket file goes with it: {}",
        socket.display()
    );
    // The point of stopping: by the time it returns, the stream really has
    // wound down. Detaching also returns, and also removes the socket.
    assert!(
        mark.exists(),
        "stop() returned before the stream's components were shut down"
    );
}

/// A sidechannel says where it came from.
///
/// `origin` is the one thing tying a derived stream to the conversation it
/// grew out of — the kernel deliberately does not validate it, so if nobody
/// writes it there is no link at all. Nothing on the daemon's path ever did:
/// `/btw` opened a stream that observed its parent and carried no record of
/// which parent, or of what was being talked about when it opened.
#[test]
fn a_derived_stream_records_where_it_came_from() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("btw.sock");
    let daemon = Daemon::serve(&socket, || {
        StreamHost::new([("chat".to_string(), chat_template())].into())
    })
    .unwrap();

    let mut client = Client::connect(&socket);
    client.send(&ClientMessage::Attach {
        stream: "main".into(),
        template: None,
        derive_from: None,
        capabilities: vec![],
    });
    client.read_until(|m| matches!(m, ServerMessage::Attached { .. }));
    client.send(&ClientMessage::SendText {
        stream: "main".into(),
        text: "the main thread".into(),
    });
    client.read_until(|m| matches!(m, ServerMessage::Quiescent { .. }));

    let mut aside = Client::connect(&socket);
    aside.send(&ClientMessage::Attach {
        stream: "side".into(),
        template: None,
        derive_from: Some("main".into()),
        capabilities: vec![],
    });
    aside.read_until(|m| matches!(m, ServerMessage::Attached { .. }));
    // The first thing said in the aside is its root event — the one place
    // provenance belongs.
    aside.send(&ClientMessage::SendText {
        stream: "side".into(),
        text: "what were we saying?".into(),
    });
    let got = aside.read_until(|m| matches!(m, ServerMessage::Quiescent { .. }));

    let rooted = got.iter().any(|m| match m {
        ServerMessage::Appended { event, .. } => {
            event.origin.as_ref().map(|o| o.stream.as_str()) == Some("main")
        }
        _ => false,
    });
    assert!(
        rooted,
        "the sidechannel's ledger must point back at the parent: {got:?}"
    );

    daemon.stop();
}
