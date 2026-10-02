use super::*;
use crate::components::silent_ui;

fn kernel() -> Kernel {
    let registry = [
        (silent_ui::NAME.into(), silent_ui::manifest()),
        (ip::NAME.into(), ip::manifest()),
    ]
    .into();
    let mut factories: HashMap<String, crate::Factory> = HashMap::new();
    factories.insert(
        silent_ui::NAME.into(),
        Box::new(|_| Box::new(silent_ui::SilentUi::new(Default::default()))),
    );
    factories.insert(
        ip::NAME.into(),
        Box::new(|c| Box::new(ip::InterfacePermissions::from_config(c))),
    );
    let assembly = serde_json::from_value(json!({
        "instances": {"ui":{"component":silent_ui::NAME}, "permissions":{"component":ip::NAME}},
        "wires":[{"from":"ui.answer","to":"permissions.control"}]
    }))
    .unwrap();
    Kernel::start(
        &assembly,
        &registry,
        &mut factories,
        crate::KernelOptions::default(),
    )
    .unwrap()
}

fn page(reader: &LogReader) -> PreparedAttachment {
    PreparedAttachment {
        stream: reader.stream().into(),
        replay: vec![],
        warnings: vec![],
        history: None,
        through: reader.snapshot_end(),
        pending_auth: vec![],
    }
}

#[test]
fn delayed_subscription_cannot_resurrect_a_detached_dead_or_replaced_binding() {
    for ending in ["detach", "dead", "replace"] {
        let mut kernel = kernel();
        let bindings = Bindings::new(&kernel);
        let (tx, rx) = std::sync::mpsc::channel();
        let alive = Arc::new(AtomicBool::new(true));
        let capabilities = vec![PERMISSIONS_CAPABILITY.into()];
        let token = bindings.prepare(1, tx.clone(), &capabilities, alive.clone());
        match ending {
            "detach" => bindings.detach(1),
            "dead" => alive.store(false, Ordering::Release),
            "replace" => {
                bindings.prepare(1, tx.clone(), &capabilities, alive.clone());
            }
            _ => unreachable!(),
        }
        bindings
            .attach(1, &token, page(&kernel.log().reader()))
            .unwrap();
        assert!(rx.try_recv().is_err(), "{ending} emitted a stale handshake");
        assert!(bindings.source(1).is_none());
        assert!(bindings
            .control(
                1,
                &token,
                json!({"channel":ip::CHANNEL,"action":"set","enabled":true}),
                true
            )
            .is_err());
        kernel.run_until_quiescent().unwrap();
        assert!(
            !kernel
                .log()
                .replay(1)
                .unwrap()
                .iter()
                .any(|e| e.event_type == ip::STATE && e.payload["action"] == "open"),
            "{ending} resurrected permission"
        );

        let fresh = bindings.prepare(1, tx, &capabilities, Arc::new(AtomicBool::new(true)));
        assert_ne!(fresh, token);
        bindings
            .attach(1, &fresh, page(&kernel.log().reader()))
            .unwrap();
        assert!(
            matches!(rx.recv().unwrap(), ServerMessage::AttachedV2 { authorization, .. } if authorization.attachment == fresh)
        );
        kernel.run_until_quiescent().unwrap();
        let state = ip::read_state(&kernel.log().reader(), "permissions")
            .unwrap()
            .unwrap();
        assert!(state.interfaces[&fresh].open);
        assert!(!state.permits(&fresh));
        assert!(!state.interfaces.contains_key(&token));
    }
}
