use super::*;
use crate::components::workshop_sink::{self, WorkshopSink};
use crate::{
    AssemblyManifest, ComponentInstance, EventDraft, EventLog, Factory, KernelOptions, PortDecl,
    Wire,
};
use std::collections::HashMap;

fn start(path: &Path) -> Kernel {
    let mut driver = workshop_sink::manifest();
    driver.name = "driver".into();
    driver.inputs.clear();
    driver.tools.clear();
    driver.implements.clear();
    driver.outputs = vec![
        PortDecl::new("direct", &[ce::TOOL_EXEC_STARTED]),
        PortDecl::new("review", &[ce::TOOL_EXEC_STARTED]),
    ];
    let mut gate = driver.clone();
    gate.name = "gate".into();
    gate.inputs = vec![PortDecl::new("review", &[ce::TOOL_EXEC_STARTED])];
    gate.outputs = vec![PortDecl::new("forward", &[ce::TOOL_EXEC_STARTED])];
    let registry = [driver, gate, workshop_sink::manifest()]
        .into_iter()
        .map(|m| (m.name.clone(), m))
        .collect();
    let mut factories: HashMap<String, Factory> = ["driver", "gate", workshop_sink::NAME]
        .into_iter()
        .map(|name| {
            (
                name.into(),
                Box::new(|_: Option<&Value>| Box::new(WorkshopSink) as Box<dyn crate::Component>)
                    as Factory,
            )
        })
        .collect();
    let assembly = AssemblyManifest {
        instances: ["driver", "gate", workshop_sink::NAME]
            .into_iter()
            .map(|name| {
                (
                    name.into(),
                    ComponentInstance {
                        component: name.into(),
                        config: None,
                        requires: Vec::new(),
                    },
                )
            })
            .collect(),
        wires: vec![
            Wire::new("driver.direct", "workshop.execute"),
            Wire::new("driver.review", "gate.review"),
            Wire::new("gate.forward", "workshop.execute"),
        ],
    };
    Kernel::start(
        &assembly,
        &registry,
        &mut factories,
        KernelOptions {
            stream: Some("history-test".into()),
            log_file: Some(path.into()),
            ..KernelOptions::default()
        },
    )
    .unwrap()
}

fn reads(kernel: &Kernel) -> u64 {
    let cache = kernel.log().reader().memory_stats().unwrap().cache.unwrap();
    cache.hits + cache.decodes
}

fn request(kernel: &mut Kernel, port: &str, call: &str) -> EventEnvelope {
    kernel.injector("driver").emit(
        port,
        EventDraft::new(
            ce::TOOL_EXEC_STARTED,
            &[],
            json!({
                "call": call, "tool": INSTALL_TOOL, "arguments": {"reason": "fixture"}
            }),
        ),
    );
    kernel.run_until_quiescent().unwrap();
    kernel
        .log()
        .reader()
        .scan_back_types(&[ce::TOOL_EXEC_STARTED], |event, _| {
            Ok((event.payload["call"] == call).then(|| event.clone()))
        })
        .unwrap()
        .unwrap()
}

#[test]
fn pending_checks_read_only_unsettled_requests_that_reached_the_sink() {
    for extension in ["jsonl", "ledger"] {
        assert_pending_checks(extension);
    }
}

fn assert_pending_checks(extension: &str) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(format!("ledger.{extension}"));
    let mut log =
        EventLog::open(ce::core_event_decls(), "history-test", Some(path.clone())).unwrap();
    for _ in 0..256 {
        log.append(
            EventDraft::new(
                ce::USER_MESSAGE,
                &[],
                json!({"text": "unrelated".repeat(128)}),
            ),
            "driver",
        )
        .unwrap();
    }
    let old = log
        .append(
            EventDraft::new(
                ce::TOOL_EXEC_STARTED,
                &[],
                json!({
                    "call": "old", "tool": INSTALL_TOOL, "arguments": {}
                }),
            ),
            "driver",
        )
        .unwrap();
    log.append(
        EventDraft::new(
            ce::TOOL_EXEC_COMPLETED,
            &[&old.id],
            json!({
                "call": "old", "status": "ok", "result": {}
            }),
        ),
        "workshop",
    )
    .unwrap();
    drop(log);
    let mut kernel = start(&path);
    let before = reads(&kernel);
    assert!(pending_installs(&kernel).unwrap().is_empty());
    assert!(pending_fetch_installs(&kernel).unwrap().is_empty());
    assert!(pending_removals(&kernel).unwrap().is_empty());
    assert_eq!(
        reads(&kernel),
        before,
        "quiet checks must not load historical bodies"
    );

    let blocked = request(&mut kernel, "review", "blocked");
    assert!(!kernel.log().reader().has_outcome(&blocked.id).unwrap());
    let before = reads(&kernel);
    assert!(pending_installs(&kernel).unwrap().is_empty());
    assert_eq!(
        reads(&kernel),
        before,
        "an unapproved request needs no body read"
    );

    kernel.injector("gate").emit(
        "forward",
        EventDraft::new(
            ce::TOOL_EXEC_STARTED,
            &[&blocked.id],
            blocked.payload.clone(),
        ),
    );
    kernel.run_until_quiescent().unwrap();
    let before = reads(&kernel);
    let pending = pending_installs(&kernel).unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].call, "blocked");
    assert_ne!(
        pending[0].cause, blocked.id,
        "answer the forwarded request, not its unapproved copy"
    );
    assert_eq!(reads(&kernel) - before, 1);
    kernel.injector("workshop").emit(
        "outcome",
        EventDraft::new(
            ce::TOOL_EXEC_COMPLETED,
            &[&pending[0].cause],
            json!({
                "call": "blocked", "status": "ok", "result": {}
            }),
        ),
    );
    kernel.run_until_quiescent().unwrap();
    let before = reads(&kernel);
    assert!(pending_installs(&kernel).unwrap().is_empty());
    assert_eq!(
        reads(&kernel),
        before,
        "completed forwarded chains need no body reads"
    );

    // A new request may reuse a provider's call label. Only its own causal
    // chain can settle it; an older answer with the same label cannot.
    let reused = request(&mut kernel, "direct", "blocked");
    let pending = pending_installs(&kernel).unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].cause, reused.id);

    request(&mut kernel, "review", "never-approved");
    kernel.shutdown();
    let kernel = start(&path);
    let before = reads(&kernel);
    assert!(
        pending_installs(&kernel).unwrap().is_empty(),
        "restart is not approval"
    );
    assert_eq!(
        reads(&kernel),
        before,
        "interrupted chains need no body reads"
    );
    kernel.shutdown();
}
