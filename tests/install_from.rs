//! install_component_from: the agent-facing "install a ready-made component
//! from a source" path. A local package (component.json + the files its
//! entry runs) is fetched, held to the component canon, exam-graded, wired
//! like its peers, hot-installed and persisted through the overlay. A
//! rejected source leaves no trace.
#![cfg(unix)]

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use lattice::components::{minimal_loop, scripted_model, silent_ui, workshop_sink};
use lattice::core_events as ce;
use lattice::workshop::{fetch_and_install, pending_fetch_installs, BuildOutcome};
use lattice::{
    overlay, AssemblyManifest, ComponentInstance, ComponentManifest, EventDraft, Factory, Kernel,
    KernelOptions, Wire,
};

const GREETER_PY: &str = r#"#!/usr/bin/env python3
import json, sys
for line in sys.stdin:
    msg = json.loads(line)
    if "hello" in msg:
        continue
    if "stop" in msg:
        break
    if "deliver" not in msg:
        continue
    event = msg["deliver"]["event"]
    p = event["payload"]
    if p.get("tool") == "greet":
        name = (p.get("arguments") or {}).get("name", "?")
        out = {"call": p.get("call"), "status": "ok", "result": f"hello {name}"}
    else:
        out = {"call": p.get("call"), "status": "error", "error": {
            "code": "tool.unknown", "message": "unknown tool", "blame": "request"}}
    print(json.dumps({"emit": {"port": "outcome", "type": "core.tool.exec_completed",
                               "causes": [event["id"]], "payload": out}}), flush=True)
    print(json.dumps({"processed": True}), flush=True)
"#;

fn write_package(dir: &Path) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(dir.join("greeter.py"), GREETER_PY).unwrap();
    std::fs::write(
        dir.join("component.json"),
        serde_json::to_string_pretty(&json!({
            "name": "greeter",
            "version": "0.1.0",
            "runtime": "process",
            "entry": "python3 {dir}/greeter.py",
            "inputs": [{"name": "execute", "events": ["core.tool.exec_started"]}],
            "outputs": [{"name": "outcome", "events": ["core.tool.exec_completed"]}],
            "implements": ["tool-provider"],
            "tools": [{
                "name": "greet",
                "description": "greet someone",
                "parameters": {"type": "object",
                               "properties": {"name": {"type": "string"}},
                               "required": ["name"]},
                "effects": {"reversible": true},
            }],
        }))
        .unwrap(),
    )
    .unwrap();
}

fn start(script: Value, displayed: Arc<Mutex<Vec<String>>>) -> Kernel {
    let registry: HashMap<String, ComponentManifest> = [
        (silent_ui::NAME.to_string(), silent_ui::manifest()),
        (minimal_loop::NAME.to_string(), minimal_loop::manifest()),
        (scripted_model::NAME.to_string(), scripted_model::manifest()),
        (workshop_sink::NAME.to_string(), workshop_sink::manifest()),
    ]
    .into();
    let mut factories: HashMap<String, Factory> = HashMap::new();
    factories.insert(
        silent_ui::NAME.to_string(),
        Box::new(move |_| Box::new(silent_ui::SilentUi::new(Arc::clone(&displayed)))),
    );
    factories.insert(
        minimal_loop::NAME.to_string(),
        Box::new(|c| Box::new(minimal_loop::MinimalLoop::from_config(c))),
    );
    factories.insert(
        scripted_model::NAME.to_string(),
        Box::new(|c| Box::new(scripted_model::ScriptedModel::from_config(c))),
    );
    factories.insert(
        workshop_sink::NAME.to_string(),
        Box::new(|_| Box::new(workshop_sink::WorkshopSink)),
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
                    config: Some(script),
                },
            ),
            (
                "workshop".to_string(),
                ComponentInstance {
                    component: workshop_sink::NAME.to_string(),
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
            Wire::new("loop.run", "workshop.execute"),
            Wire::new("workshop.outcome", "loop.tools"),
            Wire::new("loop.out", "ui.display"),
        ],
    };
    Kernel::start(
        &assembly,
        &registry,
        &mut factories,
        KernelOptions::default(),
    )
    .unwrap()
}

/// The host loop: run to quiescence, service fetch-installs, run again —
/// what the chat example does.
fn drive_to_completion(
    kernel: &mut Kernel,
    components_dir: &Path,
    overlay_path: Option<&Path>,
) -> Vec<String> {
    let mut outcomes = Vec::new();
    loop {
        kernel.run_until_quiescent().unwrap();
        let pending = pending_fetch_installs(kernel).unwrap();
        if pending.is_empty() {
            break;
        }
        for req in pending {
            let outcome = fetch_and_install(
                kernel,
                &req,
                components_dir,
                "loop",
                overlay_path,
                |_, _| true,
            )
            .unwrap();
            let payload = match outcome {
                BuildOutcome::Installed(msg) => {
                    outcomes.push(format!("installed: {msg}"));
                    json!({"call": req.call, "status": "ok", "result": msg})
                }
                BuildOutcome::Rejected(why) => {
                    outcomes.push(format!("rejected: {why}"));
                    json!({"call": req.call, "status": "error",
                           "error": {"code": "workshop.rejected", "message": why,
                                     "blame": "request"}})
                }
            };
            kernel.injector("workshop").emit(
                "outcome",
                EventDraft::new(ce::TOOL_EXEC_COMPLETED, &[&req.cause], payload),
            );
        }
    }
    outcomes
}

#[test]
fn a_packaged_component_installs_serves_and_persists() {
    let package = tempfile::tempdir().unwrap();
    write_package(&package.path().join("greeter"));
    let home = tempfile::tempdir().unwrap();
    let components_dir = home.path().join("components");
    let overlay_path = home.path().join("assembly.json");

    let displayed: Arc<Mutex<Vec<String>>> = Arc::default();
    let script = json!({"script": [
        {"status": "ok", "toolCalls": [
            {"id": "t1", "tool": "InstallComponentFrom", "arguments": {
                "source": package.path().join("greeter").display().to_string(),
                "reason": "the user wants greetings",
            }}]},
        {"status": "ok", "toolCalls": [
            {"id": "t2", "tool": "greet", "arguments": {"name": "lattice"}}]},
        {"status": "ok", "text": "done"},
    ]});
    let mut kernel = start(script, Arc::clone(&displayed));
    kernel.injector("ui").emit(
        "user",
        EventDraft::new(ce::USER_MESSAGE, &[], json!({"text": "install and greet"})),
    );
    let outcomes = drive_to_completion(&mut kernel, &components_dir, Some(&overlay_path));
    assert_eq!(outcomes.len(), 1);
    assert!(outcomes[0].starts_with("installed:"), "{outcomes:?}");

    // The fetched component answered its tool in the same conversation
    let greeted = kernel.log().replay(1).unwrap().into_iter().any(|e| {
        e.event_type == ce::TOOL_EXEC_COMPLETED
            && e.source == "greeter"
            && e.payload["result"] == "hello lattice"
    });
    assert!(greeted, "the installed component served its tool");
    kernel.shutdown();

    // Landed under its own roof, entry resolved, and persisted: a fresh
    // baseline plus the overlay brings it back
    assert!(components_dir.join("greeter/greeter.py").is_file());
    let mut registry: HashMap<String, ComponentManifest> = HashMap::new();
    let mut assembly = AssemblyManifest::default();
    let report = overlay::apply(&mut registry, &mut assembly, &overlay_path).unwrap();
    assert_eq!(report.instances, 1);
    let entry = &registry["greeter"].entry;
    assert!(
        entry.contains(components_dir.to_str().unwrap()),
        "the {{dir}} placeholder resolved to the installed location: {entry}"
    );
}

/// The PRODUCT path, not the example's. `Workshop::run` is what both hosts
/// call at their quiet point, so this drives it directly: the agent asks to
/// install, the host services it inside the same run, and the newcomer answers
/// its own tool in the same conversation — with no host loop written by hand.
#[test]
fn the_hosts_own_service_installs_and_the_newcomer_serves_in_the_same_run() {
    let package = tempfile::tempdir().unwrap();
    write_package(&package.path().join("greeter"));
    let home = tempfile::tempdir().unwrap();

    let displayed: Arc<Mutex<Vec<String>>> = Arc::default();
    let script = json!({"script": [
        {"status": "ok", "toolCalls": [
            {"id": "t1", "tool": "InstallComponentFrom", "arguments": {
                "source": package.path().join("greeter").display().to_string(),
                "reason": "the user wants greetings",
            }}]},
        {"status": "ok", "toolCalls": [
            {"id": "t2", "tool": "greet", "arguments": {"name": "lattice"}}]},
        {"status": "ok", "text": "done"},
    ]});
    let mut kernel = start(script, Arc::clone(&displayed));
    kernel.injector("ui").emit(
        "user",
        EventDraft::new(ce::USER_MESSAGE, &[], json!({"text": "install and greet"})),
    );

    let shop = lattice::workshop::Workshop {
        workshop_dir: home.path().join("workshop"),
        components_dir: home.path().join("components"),
        loop_instance: "loop".to_string(),
        overlay: Some(home.path().join("assembly.json")),
    };
    shop.run(&mut kernel)
        .expect("the serviced run must not fail");

    let events = kernel.log().replay(1).unwrap();
    kernel.shutdown();

    let installed = events.iter().any(|e| {
        e.event_type == ce::TOOL_EXEC_COMPLETED
            && e.payload["call"] == "t1"
            && e.payload["status"] == "ok"
    });
    assert!(installed, "the host answered the install request itself");
    let greeted = events.iter().any(|e| {
        e.event_type == ce::TOOL_EXEC_COMPLETED
            && e.source == "greeter"
            && e.payload["result"] == "hello lattice"
    });
    assert!(
        greeted,
        "the newcomer served its tool inside the SAME run — the agent never had \
         to be prompted again"
    );
    assert!(home.path().join("components/greeter/greeter.py").is_file());
}

#[test]
fn a_source_without_a_manifest_is_rejected_and_leaves_no_trace() {
    let package = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(package.path().join("junk")).unwrap();
    std::fs::write(package.path().join("junk/whatever.txt"), "not a component").unwrap();
    let home = tempfile::tempdir().unwrap();
    let components_dir = home.path().join("components");

    let displayed: Arc<Mutex<Vec<String>>> = Arc::default();
    let script = json!({"script": [
        {"status": "ok", "toolCalls": [
            {"id": "t1", "tool": "InstallComponentFrom", "arguments": {
                "source": package.path().join("junk").display().to_string(),
                "reason": "trying anyway",
            }}]},
        {"status": "ok", "text": "done"},
    ]});
    let mut kernel = start(script, Arc::clone(&displayed));
    kernel.injector("ui").emit(
        "user",
        EventDraft::new(ce::USER_MESSAGE, &[], json!({"text": "install it"})),
    );
    let outcomes = drive_to_completion(&mut kernel, &components_dir, None);
    kernel.shutdown();

    assert_eq!(outcomes.len(), 1);
    assert!(outcomes[0].starts_with("rejected:"), "{outcomes:?}");
    let residue: Vec<_> = std::fs::read_dir(&components_dir)
        .map(|d| d.filter_map(|e| e.ok()).collect())
        .unwrap_or_default();
    assert!(
        residue.is_empty(),
        "a rejected source must leave nothing behind: {residue:?}"
    );
}

// ── The gated path: install through a trust gate (the standard preset shape) ──

use lattice::components::trust_policy;

/// A gated assembly, exactly the preset's workshop line: the install request
/// passes the trust gate (stance ask) before reaching the workshop, and a
/// human proxy answers the authorization card. This is the shape that used to
/// loop: the service loop saw the request while it was still at the gate,
/// serviced it, and its completion was rejected (the workshop never witnessed
/// the pre-gate start), so the install never cleared and re-downloaded forever.
fn start_gated(script: Value, grants: &Path) -> Kernel {
    start_gated_at(script, grants, None)
}

/// The same, on a named ledger so a second start REOPENS the first one's
/// history — which is when the question "has this request been dealt with?"
/// gets its hardest test.
fn start_gated_at(script: Value, grants: &Path, log: Option<&Path>) -> Kernel {
    let registry: HashMap<String, ComponentManifest> = [
        (silent_ui::NAME.to_string(), silent_ui::manifest()),
        (minimal_loop::NAME.to_string(), minimal_loop::manifest()),
        (scripted_model::NAME.to_string(), scripted_model::manifest()),
        (trust_policy::NAME.to_string(), trust_policy::manifest()),
        (workshop_sink::NAME.to_string(), workshop_sink::manifest()),
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
    factories.insert(
        trust_policy::NAME.to_string(),
        Box::new(|c| Box::new(trust_policy::TrustPolicy::from_config(c))),
    );
    factories.insert(
        workshop_sink::NAME.to_string(),
        Box::new(|_| Box::new(workshop_sink::WorkshopSink)),
    );
    let inst = |c: &str, cfg: Option<Value>| ComponentInstance {
        component: c.to_string(),
        config: cfg,
        requires: Vec::new(),
    };
    let assembly = AssemblyManifest {
        instances: [
            ("ui".to_string(), inst(silent_ui::NAME, None)),
            ("loop".to_string(), inst(minimal_loop::NAME, None)),
            (
                "model".to_string(),
                inst(scripted_model::NAME, Some(script)),
            ),
            (
                "trust".to_string(),
                inst(
                    trust_policy::NAME,
                    Some(json!({"stance": "ask", "grants": grants.display().to_string()})),
                ),
            ),
            ("workshop".to_string(), inst(workshop_sink::NAME, None)),
        ]
        .into(),
        wires: vec![
            Wire::new("ui.user", "loop.input"),
            Wire::new("loop.ask", "model.request"),
            Wire::new("model.result", "loop.model"),
            Wire::new("loop.run", "trust.review"),
            Wire::new("trust.verdict", "loop.tools"),
            Wire::new("ui.answer", "trust.answer"),
            Wire::new("trust.forward", "workshop.execute"),
            Wire::new("workshop.outcome", "loop.tools"),
            Wire::new("loop.out", "ui.display"),
        ],
    };
    let mut f = factories;
    let options = KernelOptions {
        stream: log.map(|_| "st_gated".to_string()),
        log_file: log.map(Path::to_path_buf),
        ..KernelOptions::default()
    };
    Kernel::start(&assembly, &registry, &mut f, options).unwrap()
}

/// The one authorization request currently unanswered, if any. A decision
/// names the REVIEWED call (its cause), not the request event, so correlate
/// the request's reviewed-call id against the decisions' causes.
fn open_auth(kernel: &Kernel) -> Option<String> {
    let events = kernel.log().replay(1).unwrap();
    let decided: std::collections::HashSet<&str> = events
        .iter()
        .filter(|e| e.event_type == trust_policy::DECISION)
        .flat_map(|e| e.causes.iter().map(String::as_str))
        .collect();
    events
        .iter()
        .filter(|e| e.event_type == trust_policy::AUTH_REQUESTED)
        .find(|e| {
            let reviewed = e.payload["request"].as_str().unwrap_or("");
            !decided.contains(reviewed)
        })
        .map(|e| e.id.clone())
}

/// A card nobody ever answered does not become a yes by restarting.
///
/// The person is asked whether to admit new code and does not decide — they
/// want to look at that URL first, and close the terminal. Reopening the
/// conversation must find the same undecided request, not an installed
/// component: consent that was never given cannot be inferred from a process
/// ending.
///
/// It could be, twice over. "Has this been dealt with?" counted only
/// completions, and a reopen settles what was in flight with an INTERRUPTED —
/// an ending, but not one that answer recognised. And "has it reached the
/// workshop?" asked the kernel who had witnessed it, which after a reopen is
/// everyone, because the durable past has to be citable by restored
/// components. Two half-right answers, and the install went through.
#[test]
fn an_unanswered_card_is_still_unanswered_after_a_restart() {
    let package = tempfile::tempdir().unwrap();
    write_package(&package.path().join("greeter"));
    let home = tempfile::tempdir().unwrap();
    let grants = home.path().join("trust.json");
    let ledger = home.path().join("ledger.jsonl");

    let script = json!({"script": [
        {"status": "ok", "toolCalls": [
            {"id": "t1", "tool": "InstallComponentFrom", "arguments": {
                "source": package.path().join("greeter").display().to_string(),
                "reason": "the user wants greetings",
            }}]},
        {"status": "ok", "text": "done"},
    ]});

    let mut kernel = start_gated_at(script.clone(), &grants, Some(&ledger));
    kernel.injector("ui").emit(
        "user",
        EventDraft::new(ce::USER_MESSAGE, &[], json!({"text": "install"})),
    );
    kernel.run_until_quiescent().unwrap();
    assert!(open_auth(&kernel).is_some(), "the gate is asking");
    assert!(
        pending_fetch_installs(&kernel).unwrap().is_empty(),
        "precondition: not serviceable while the card is open"
    );
    kernel.shutdown();

    // The restart. Nobody answered; nobody has answered yet.
    let reopened = start_gated_at(script, &grants, Some(&ledger));
    let events = reopened.log().replay(1).unwrap();
    assert!(
        events
            .iter()
            .any(|e| e.event_type == ce::INTERRUPTED && e.payload["by"] == "restart"),
        "precondition: the reopen did settle the in-flight request"
    );
    assert!(
        !events
            .iter()
            .any(|e| e.event_type == trust_policy::DECISION),
        "precondition: still nobody has decided"
    );
    assert!(
        pending_fetch_installs(&reopened).unwrap().is_empty(),
        "an install nobody approved must not become serviceable by restarting"
    );
    reopened.shutdown();
}

/// An approved install that was cut short is not silently redone.
///
/// The card was answered, the gate forwarded, and the process ended before
/// the workshop got to it. Reopening settles that request with an
/// INTERRUPTED, which is its ending — "recovery only ever speaks; it never
/// re-executes side effects". Reading only completions as endings left it
/// looking unanswered, so the next quiet moment fetched and installed it
/// again: a second clone, a second install, from a ledger that says plainly
/// it was already tried.
#[test]
fn an_install_cut_short_by_a_restart_is_not_fetched_again() {
    let package = tempfile::tempdir().unwrap();
    write_package(&package.path().join("greeter"));
    let home = tempfile::tempdir().unwrap();
    let grants = home.path().join("trust.json");
    let ledger = home.path().join("ledger.jsonl");

    let script = json!({"script": [
        {"status": "ok", "toolCalls": [
            {"id": "t1", "tool": "InstallComponentFrom", "arguments": {
                "source": package.path().join("greeter").display().to_string(),
                "reason": "the user wants greetings",
            }}]},
        {"status": "ok", "text": "done"},
    ]});

    let mut kernel = start_gated_at(script.clone(), &grants, Some(&ledger));
    kernel.injector("ui").emit(
        "user",
        EventDraft::new(ce::USER_MESSAGE, &[], json!({"text": "install"})),
    );
    kernel.run_until_quiescent().unwrap();
    let request = open_auth(&kernel).expect("the gate is asking");
    kernel.injector("ui").emit(
        "answer",
        EventDraft::new(
            ce::EXTERNAL_INPUT,
            &[],
            json!({"channel": trust_policy::AUTH_CHANNEL,
                   "request": request, "approve": true}),
        ),
    );
    kernel.run_until_quiescent().unwrap();
    // Approved and forwarded, and the host has not serviced it yet — the exact
    // moment the process is about to end.
    assert_eq!(
        pending_fetch_installs(&kernel).unwrap().len(),
        1,
        "precondition: approved, forwarded, and waiting for the host"
    );
    kernel.shutdown();

    let reopened = start_gated_at(script.clone(), &grants, Some(&ledger));
    assert!(
        reopened
            .log()
            .replay(1)
            .unwrap()
            .iter()
            .any(|e| e.event_type == ce::INTERRUPTED && e.payload["by"] == "restart"),
        "precondition: the reopen settled it"
    );
    assert!(
        pending_fetch_installs(&reopened).unwrap().is_empty(),
        "a settled install must not be fetched and installed a second time"
    );
    let interrupted = reopened
        .log()
        .replay(1)
        .unwrap()
        .iter()
        .filter(|event| event.event_type == ce::INTERRUPTED)
        .count();
    reopened.shutdown();
    let reopened = start_gated_at(script, &grants, Some(&ledger));
    assert!(pending_fetch_installs(&reopened).unwrap().is_empty());
    assert_eq!(
        reopened
            .log()
            .replay(1)
            .unwrap()
            .iter()
            .filter(|event| event.event_type == ce::INTERRUPTED)
            .count(),
        interrupted,
        "another restart must not settle the same chain again"
    );
    reopened.shutdown();
}

#[test]
fn a_gated_install_does_not_service_before_approval_and_installs_exactly_once() {
    let package = tempfile::tempdir().unwrap();
    write_package(&package.path().join("greeter"));
    let home = tempfile::tempdir().unwrap();
    let components_dir = home.path().join("components");
    let grants = home.path().join("trust.json");

    let script = json!({"script": [
        {"status": "ok", "toolCalls": [
            {"id": "t1", "tool": "InstallComponentFrom", "arguments": {
                "source": package.path().join("greeter").display().to_string(),
                "reason": "the user wants greetings",
            }}]},
        {"status": "ok", "toolCalls": [
            {"id": "t2", "tool": "greet", "arguments": {"name": "lattice"}}]},
        {"status": "ok", "text": "done"},
    ]});
    let mut kernel = start_gated(script, &grants);
    kernel.injector("ui").emit(
        "user",
        EventDraft::new(ce::USER_MESSAGE, &[], json!({"text": "install and greet"})),
    );
    kernel.run_until_quiescent().unwrap();

    // THE REGRESSION GUARD: while the request sits at the gate, the workshop
    // has not witnessed it, so it must NOT surface as pending — servicing it
    // now is exactly what looped before.
    assert!(
        pending_fetch_installs(&kernel).unwrap().is_empty(),
        "an install still at the gate must not be serviceable"
    );
    assert!(open_auth(&kernel).is_some(), "the gate is asking");

    // The host loop, mirroring what a frontend + service loop does: answer the
    // card when one is open, else service any (now-forwarded) install. Bounded
    // so the OLD infinite loop would blow the cap instead of hanging.
    let mut installs = 0;
    let mut guard = 0;
    loop {
        guard += 1;
        assert!(guard < 20, "converged far too slowly — the loop is back");
        if let Some(request) = open_auth(&kernel) {
            kernel.injector("ui").emit(
                "answer",
                EventDraft::new(
                    ce::EXTERNAL_INPUT,
                    &[],
                    json!({"channel": trust_policy::AUTH_CHANNEL,
                           "request": request, "approve": true}),
                ),
            );
            kernel.run_until_quiescent().unwrap();
            continue;
        }
        let pending = pending_fetch_installs(&kernel).unwrap();
        if pending.is_empty() {
            break;
        }
        for req in pending {
            let outcome =
                fetch_and_install(&mut kernel, &req, &components_dir, "loop", None, |_, _| {
                    true
                })
                .unwrap();
            let payload = match outcome {
                BuildOutcome::Installed(msg) => {
                    installs += 1;
                    json!({"call": req.call, "status": "ok", "result": msg})
                }
                BuildOutcome::Rejected(why) => {
                    json!({"call": req.call, "status": "error",
                           "error": {"code": "workshop.rejected", "message": why,
                                     "blame": "request"}})
                }
            };
            kernel.injector("workshop").emit(
                "outcome",
                EventDraft::new(ce::TOOL_EXEC_COMPLETED, &[&req.cause], payload),
            );
        }
        kernel.run_until_quiescent().unwrap();
    }

    assert_eq!(
        installs, 1,
        "the component installs exactly once, no re-downloads"
    );

    // And the whole point: the installed component actually served its tool
    let greeted = kernel.log().replay(1).unwrap().into_iter().any(|e| {
        e.event_type == ce::TOOL_EXEC_COMPLETED
            && e.source == "greeter"
            && e.payload["result"] == "hello lattice"
    });
    assert!(greeted, "the installed component served its tool");
    kernel.shutdown();
    assert!(components_dir.join("greeter/greeter.py").is_file());
}
