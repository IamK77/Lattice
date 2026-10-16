//! Real loop, standard permission chain and providers; only the model is a fixture.
#![cfg(unix)]
use lattice::background_aliases::{operations, Kind, Projection};
use lattice::components::scripted_model;
use lattice::preset::{self, PresetConfig};
use lattice::{
    core_events as ce, Component, Ctx, EventDraft, EventEnvelope, Kernel, KernelOptions,
};
use serde_json::json;

struct ReferenceModel {
    phase: usize,
    path: String,
}
impl Component for ReferenceModel {
    fn handle(&mut self, port: &str, event: &EventEnvelope, ctx: &mut Ctx) {
        if port != "request" {
            return;
        }
        let payload = match self.phase {
            0 => json!({"status":"ok","toolCalls":[
                {"id":"start-timer","tool":"Schedule","arguments":{"delay_ms":60000,"interval_ms":60000,"max_fires":1}},
                {"id":"start-watch","tool":"Watch","arguments":{"path":self.path,"debounce_ms":0}}
            ]}),
            1 | 2 => {
                let projection = Projection::recover(ctx.log(), ctx.log().snapshot_end()).unwrap();
                let alias = |kind| {
                    let identity = projection
                        .identities()
                        .iter()
                        .find(|i| i.kind == kind)
                        .unwrap();
                    projection.alias(identity).unwrap().to_string()
                };
                let calls = if self.phase == 1 {
                    json!([
                        {"id":"stale-timer","tool":"Unschedule","arguments":{"timer":alias(Kind::Timer)}},
                        {"id":"stale-watch","tool":"Unwatch","arguments":{"watch":alias(Kind::Watch)}},
                        {"id":"stop-timer","tool":"Unschedule","arguments":{"timer":alias(Kind::Timer)}},
                        {"id":"stop-watch","tool":"UseDeferredTool","arguments":{"tool":"Unwatch","arguments":{"watch":alias(Kind::Watch)}}},
                        {"id":"bad-expert","tool":"CancelExpert","arguments":{"job":"00000000","reason":"fixture"}}
                    ])
                } else {
                    json!([
                        {"id":"bad-timer","tool":"Unschedule","arguments":{"timer":"00000000"}},
                        {"id":"wrong-kind","tool":"UseDeferredTool","arguments":{"tool":"Unwatch","arguments":{"watch":alias(Kind::Timer)}}}
                    ])
                };
                json!({"status":"ok","toolCalls":calls})
            }
            _ => json!({"status":"ok","text":"all operations settled"}),
        };
        self.phase += 1;
        ctx.emit(
            "result",
            EventDraft::new(ce::MODEL_CALL_COMPLETED, &[&event.id], payload),
        );
    }
}

/// A test-only forwarding boundary substitutes an obsolete creation witness.
/// It represents the final guard's input after authorization outlives a target;
/// the numeric ID, kind and stream remain correct. Providers are not mocked.
struct StaleWitness;
impl Component for StaleWitness {
    fn handle(&mut self, _: &str, event: &EventEnvelope, ctx: &mut Ctx) {
        // Observers see all requests, but this fixture is inserted only before
        // Timer/Watch. Other providers retain their original standard wires.
        // Forwarding their requests here would create a dead-end copy whose
        // interruption races the real provider's answer.
        if !matches!(
            event.payload["tool"].as_str(),
            Some("Schedule" | "Unschedule" | "Watch" | "Unwatch")
        ) {
            return;
        }
        let mut payload = event.payload.clone();
        if matches!(
            payload["call"].as_str(),
            Some("stale-timer" | "stale-watch")
        ) {
            let mut reference = operations::reference(&payload).unwrap().unwrap();
            reference.identity.created_by = "retired-creation".into();
            payload[operations::REFERENCE] = json!(reference);
        }
        ctx.emit(
            "approved",
            EventDraft::new(ce::TOOL_EXEC_STARTED, &[&event.id], payload),
        );
    }
}

#[test]
fn aliases_cross_the_existing_permission_chain_and_all_rejections_settle_once() {
    round_trip(false);
}

#[test]
fn process_providers_receive_guarded_numbers_without_parent_history_access() {
    round_trip(true);
}

fn round_trip(process: bool) {
    let dir = tempfile::tempdir().unwrap();
    let cfg = PresetConfig {
        adapter: "scripted".into(),
        model: "scripted".into(),
        base_url: String::new(),
        key_env: String::new(),
        workspace: Some(dir.path().display().to_string()),
        context_window: 64000,
        usage_input_field: "input_tokens".into(),
        profile: None,
        entry_id: None,
        catalog_problems: Vec::new(),
        system: "fixture".into(),
        scripted: Some(json!({"script":[]})),
        thinking: None,
        overlay: None,
        assembly: None,
    };
    let (mut registry, mut factories, mut assembly) = preset::standard(&cfg).unwrap();
    let fixture: lattice::ComponentManifest = serde_json::from_value(json!({
        "name":"stale-witness-fixture", "version":"1", "runtime":lattice::RuntimeKind::Inproc, "entry":"fixture",
        "inputs":[{"name":"review","events":[ce::TOOL_EXEC_STARTED]}],
        "outputs":[{"name":"approved","events":[ce::TOOL_EXEC_STARTED]}]
    })).unwrap();
    registry.insert(fixture.name.clone(), fixture);
    factories.insert(
        "stale-witness-fixture".into(),
        Box::new(|_| Box::new(StaleWitness)),
    );
    assembly.instances.insert(
        "stale-witness".into(),
        lattice::ComponentInstance::new("stale-witness-fixture", None),
    );
    let providers: Vec<_> = assembly
        .instances
        .iter()
        .filter(|(_, instance)| {
            matches!(
                instance.component.as_str(),
                lattice::components::timer_tools::NAME | lattice::components::fs_watch::NAME
            )
        })
        .map(|(name, _)| format!("{name}.execute"))
        .collect();
    let mut sources = std::collections::BTreeSet::new();
    for wire in &mut assembly.wires {
        if providers.contains(&wire.to) {
            sources.insert(wire.from.clone());
            wire.from = "stale-witness.approved".into();
        }
    }
    assert!(!sources.is_empty());
    assembly.wires.extend(
        sources
            .into_iter()
            .map(|source| lattice::Wire::new(&source, "stale-witness.review")),
    );
    if process {
        for name in [
            lattice::components::timer_tools::NAME,
            lattice::components::fs_watch::NAME,
        ] {
            let manifest = registry.get_mut(name).unwrap();
            manifest.runtime = lattice::RuntimeKind::Process;
            manifest.entry = format!("{} component {name}", env!("CARGO_BIN_EXE_lattice"));
            assert!(
                factories.remove(name).is_some(),
                "no in-process fallback may mask a broken bridge child"
            );
        }
    }
    let path = dir.path().display().to_string();
    factories.insert(
        scripted_model::NAME.into(),
        Box::new(move |_| {
            Box::new(ReferenceModel {
                phase: 0,
                path: path.clone(),
            })
        }),
    );
    let mut kernel = Kernel::start(
        &assembly,
        &registry,
        &mut factories,
        KernelOptions::default(),
    )
    .unwrap();
    kernel.injector("ui").emit(
        "user",
        EventDraft::new(
            ce::USER_MESSAGE,
            &[],
            json!({"text":"start and stop fixture subscriptions"}),
        ),
    );
    kernel.run_until_quiescent().unwrap();
    let events = kernel.log().replay(1).unwrap();
    assert!(
        events
            .iter()
            .any(|e| e.event_type == ce::OUTPUT_REPLY
                && e.payload["text"] == "all operations settled"),
        "the loop must finish mixed and all-invalid batches"
    );
    for (call, status) in [
        ("start-timer", "ok"),
        ("start-watch", "ok"),
        ("stale-timer", "error"),
        ("stale-watch", "error"),
        ("stop-timer", "ok"),
        ("stop-watch", "ok"),
        ("bad-expert", "error"),
        ("bad-timer", "error"),
        ("wrong-kind", "error"),
    ] {
        let results: Vec<_> = events
            .iter()
            .filter(|e| e.event_type == ce::TOOL_EXEC_COMPLETED && e.payload["call"] == call)
            .collect();
        assert_eq!(results.len(), 1, "one outcome for {call}");
        assert_eq!(
            results[0].payload["status"], status,
            "{}",
            results[0].payload
        );
        if status == "error" {
            assert_eq!(
                results[0].payload["error"]["code"],
                "background.invalid_reference"
            );
        }
    }
    for (call, tool, field) in [
        ("stop-timer", "Unschedule", "timer"),
        ("stop-watch", "Unwatch", "watch"),
    ] {
        let chain: Vec<_> = events
            .iter()
            .filter(|e| e.event_type == ce::TOOL_EXEC_STARTED && e.payload["call"] == call)
            .collect();
        assert!(
            chain.len() >= 2,
            "the standard gates must actually forward the request"
        );
        for request in chain {
            assert_eq!(request.payload["tool"], tool);
            assert_eq!(
                request.payload["arguments"][field], 1,
                "authorization and execution see the original number"
            );
            let reference = operations::reference(&request.payload).unwrap().unwrap();
            assert_eq!(reference.identity.stream, request.stream);
            assert!(
                !request.causes.contains(&reference.identity.created_by),
                "target identity is not a forwarding predecessor"
            );
        }
    }
    assert!(events
        .iter()
        .filter(|e| e.event_type == ce::TOOL_EXEC_STARTED && e.payload["call"] == "bad-timer")
        .all(|e| e.payload["arguments"]["timer"] == "00000000"));
    let fixture_requests: Vec<_> = events
        .iter()
        .filter(|e| e.source == "stale-witness" && e.event_type == ce::TOOL_EXEC_STARTED)
        .collect();
    assert!(
        fixture_requests.iter().all(|e| matches!(
            e.payload["tool"].as_str(),
            Some("Schedule" | "Unschedule" | "Watch" | "Unwatch")
        )),
        "the fixture must not forward requests outside its wired providers: {fixture_requests:?}"
    );
    let faults: Vec<_> = events
        .iter()
        .filter(|e| e.event_type == ce::INTERRUPTED || e.event_type == ce::ERROR)
        .collect();
    assert!(faults.is_empty(), "unexpected runtime faults: {faults:?}");
    kernel.shutdown();
}
