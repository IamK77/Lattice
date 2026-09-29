use super::*;
use lattice::components::{scripted_model, silent_ui};
use lattice::EventDraft;

fn config(root: &std::path::Path) -> PresetConfig {
    PresetConfig {
        adapter: "scripted".into(),
        model: "fixture".into(),
        base_url: String::new(),
        key_env: String::new(),
        workspace: Some(root.display().to_string()),
        context_window: 64_000,
        usage_input_field: "input_tokens".into(),
        profile: None,
        catalog_problems: Vec::new(),
        system: "Offline startup fixture.".into(),
        thinking: None,
        scripted: Some(json!({"script": [
            {"status":"ok", "text":"first answer"},
            {"status":"ok", "text":"second answer"}
        ]})),
        overlay: None,
        assembly: None,
    }
}

fn with_observer(root: &std::path::Path) -> PresetConfig {
    let path = root.join("overlay.json");
    std::fs::write(
        &path,
        serde_json::to_vec(&json!({
            "instances": {"observer": {"component":silent_ui::NAME}},
            "wires": [{"from":"loop.out", "to":"observer.display"}]
        }))
        .unwrap(),
    )
    .unwrap();
    let mut cfg = config(root);
    cfg.overlay = Some(path);
    cfg
}

#[test]
fn preview_keeps_order_tools_wires_and_overlay_provenance() {
    crate::terminal_host::test_support::isolated(|| {
        let root = tempfile::tempdir().unwrap();
        let cfg = with_observer(root.path());
        let rows = preview(&cfg);
        assert!(rows.windows(2).all(|pair| pair[0].0 < pair[1].0));
        let fs = rows.iter().find(|row| row.0 == "fs").unwrap();
        assert_eq!(fs.2, "in-process");
        assert_eq!(fs.1, "fs-reader");
        assert_eq!(fs.3, "Read Ls");
        let writer = rows.iter().find(|row| row.0 == "fs-write").unwrap();
        assert_eq!(writer.1, "fs-writer");
        assert_eq!(writer.2, "in-process");
        assert_eq!(writer.3, "Write Edit");
        assert!(!writer.4);
        assert_eq!(
            writer.5,
            [
                "trust.forward → fs-write.execute",
                "fs-write.outcome → loop.tools"
            ]
        );
        assert!(!fs.4);
        assert_eq!(
            fs.5,
            ["trust.forward → fs.execute", "fs.outcome → loop.tools"]
        );
        let observer = rows.iter().find(|row| row.0 == "observer").unwrap();
        assert_eq!(observer.1, silent_ui::NAME);
        assert_eq!(observer.2, "in-process");
        assert!(observer.3.is_empty());
        assert!(observer.4);
        assert_eq!(observer.5, ["loop.out → observer.display"]);
        assert!(rows
            .iter()
            .filter(|row| row.4)
            .all(|row| row.0 == "observer"));
        let plain = preview(&config(root.path()));
        assert!(plain.iter().all(|row| !row.4));
        assert!(plain.iter().all(|row| row.0 != "observer"));
    });
}

#[test]
fn preview_failure_is_empty_but_kernel_build_reports_preset_inspection() {
    crate::terminal_host::test_support::isolated(|| {
        let root = tempfile::tempdir().unwrap();
        let mut cfg = config(root.path());
        cfg.assembly = Some(root.path().join("missing-assembly.json"));
        assert!(preview(&cfg).is_empty());
        let ledger = root.path().join("unopened.ledger");
        let (tx, _) = std::sync::mpsc::channel();
        match build(tx, &cfg, ledger.clone()) {
            Err(KernelError::Inspection(issues)) => {
                assert_eq!(issues.len(), 1);
                assert_eq!(issues[0].location, "preset");
                assert!(!issues[0].problem.is_empty());
            }
            Err(other) => panic!("unexpected build failure: {other}"),
            Ok(kernel) => {
                kernel.shutdown();
                panic!("an invalid assembly must not start");
            }
        }
        assert!(
            !ledger.exists(),
            "assembly failure precedes ledger creation"
        );
    });
}

#[test]
fn subscription_orders_and_filters_input_observations_and_survives_reopen() {
    crate::terminal_host::test_support::isolated(|| {
        let root = tempfile::tempdir().unwrap();
        let mut cfg = with_observer(root.path());
        cfg.key_env = "LATTICE_TEST_MODEL_KEY".into();
        let secret = "fixture-secret-that-is-not-a-real-key";
        std::env::set_var(&cfg.key_env, secret);
        let ledger = root.path().join("stream.ledger");
        let (tx, rx) = std::sync::mpsc::channel();
        let mut kernel = build(tx, &cfg, ledger.clone()).unwrap();
        let stream = kernel.log().stream().to_string();
        let opening = kernel
            .log()
            .reader()
            .scan_back(|event, _| {
                Ok((event.event_type == core_events::STREAM_OPENED).then(|| event.clone()))
            })
            .unwrap()
            .unwrap();
        assert_eq!(opening.payload["host"], "tui");
        assert_eq!(opening.payload["adapter"], "scripted");
        assert_eq!(opening.payload["model"], "fixture");
        assert_eq!(
            opening.payload["workspace"],
            cfg.workspace.as_deref().unwrap()
        );
        assert_eq!(
            opening.payload["cwd"],
            std::env::current_dir().unwrap().display().to_string()
        );
        assert_ne!(opening.payload["cwd"], opening.payload["workspace"]);

        // All dispatch is drained causally, not by waiting for a wall-clock delay.
        kernel.injector("ui").emit(
            "user",
            EventDraft::new(core_events::USER_MESSAGE, &[], json!({"text":secret})),
        );
        kernel.run_until_quiescent().unwrap();
        let first = kernel
            .log()
            .reader()
            .scan_back(|event, _| {
                Ok(
                    (event.event_type == core_events::USER_MESSAGE && event.source == "ui")
                        .then(|| event.clone()),
                )
            })
            .unwrap()
            .unwrap();
        assert!(!first.payload.to_string().contains(secret));
        let through = kernel.log().reader().snapshot_end();

        kernel.injector("ui").emit(
            "user",
            EventDraft::new(
                core_events::USER_MESSAGE,
                &[&first.id],
                json!({"text":"caused input"}),
            ),
        );
        kernel.injector("observer").emit(
            "user",
            EventDraft::new(
                core_events::USER_MESSAGE,
                &[],
                json!({"text":"another frontend"}),
            ),
        );
        kernel.injector("ui").emit(
            "answer",
            EventDraft::new(
                core_events::EXTERNAL_INPUT,
                &[],
                json!({"channel":"fixture"}),
            ),
        );
        kernel.run_until_quiescent().unwrap();
        let rendered: Vec<_> = rx.try_iter().collect();
        let observations: Vec<_> = rendered
            .iter()
            .filter_map(|event| match event {
                RenderEvent::InputObserved { event, pid, .. } => Some((event, pid)),
                _ => None,
            })
            .collect();
        assert_eq!(observations, [(&first.id, &std::process::id())]);
        let at = rendered
            .iter()
            .position(|event| {
                matches!(
                    event, RenderEvent::InputObserved {event, ..} if event == &first.id
                )
            })
            .unwrap();
        assert!(matches!(&rendered[at + 1], RenderEvent::Appended(event) if event.id == first.id));
        let appended: Vec<_> = rendered
            .iter()
            .filter_map(|event| match event {
                RenderEvent::Appended(event) => Some(event.as_ref()),
                _ => None,
            })
            .collect();
        assert!(appended.iter().any(|event| event.seq <= through));
        assert!(appended
            .iter()
            .any(|event| event.seq > through && event.source == "observer"));
        assert!(appended.iter().any(|event| {
            event.source == "ui" && event.causes.as_slice() == std::slice::from_ref(&first.id)
        }));
        assert!(appended
            .iter()
            .any(|event| event.event_type == core_events::EXTERNAL_INPUT));
        assert!(appended
            .iter()
            .all(|event| event.event_type != core_events::ERROR));

        // The terminal host must own factories, not only the opening adapter.
        kernel
            .replace(
                preset::MAIN_MODEL,
                scripted_model::NAME,
                Some(json!({"script":[]})),
                "exercise the host's retained factories",
                &[],
            )
            .unwrap();
        kernel.shutdown();
        let (tx, _) = std::sync::mpsc::channel();
        let reopened = build(tx, &cfg, ledger).unwrap();
        assert_eq!(reopened.log().stream(), stream);
        let resumed = reopened
            .log()
            .reader()
            .scan_back(|event, _| {
                Ok((event.event_type == core_events::STREAM_RESUMED).then(|| event.clone()))
            })
            .unwrap()
            .unwrap();
        assert!(resumed.payload["fromSeq"].as_u64().unwrap() >= through);
        reopened.shutdown();
    });
}
