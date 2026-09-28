//! End-to-end desktop tool tests use interchangeable local backends, never a
//! model endpoint, macOS permissions, or a real desktop window.
use lattice::components::{
    desktop_driver::{Action, DesktopDriver, Failure, Frame, Target},
    desktop_tools, responses_media,
};
use lattice::core_events as ce;
use lattice::{
    AssemblyManifest, Component, ComponentInstance, Ctx, EventDraft, EventEnvelope, Factory,
    Kernel, KernelOptions, PortDecl, Wire,
};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio_util::sync::CancellationToken;

#[derive(Default)]
struct State {
    actions: Vec<Action>,
    observations: usize,
    closed: usize,
    fail_second: bool,
    cancel_first: bool,
}
struct FakeDesktop {
    state: Arc<Mutex<State>>,
    name: String,
}
fn picture() -> Vec<u8> {
    let mut bytes = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut bytes, 4, 4);
        encoder.set_color(png::ColorType::Rgb);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().unwrap();
        writer.write_image_data(&[32; 48]).unwrap();
    }
    bytes
}
impl DesktopDriver for FakeDesktop {
    fn targets(&mut self, _: &CancellationToken) -> Result<Vec<Target>, Failure> {
        Ok(vec![Target {
            id: "chosen".into(),
            application: self.name.clone(),
            title: "Owned test window".into(),
        }])
    }
    fn observe(&mut self, target: &str, _: &CancellationToken) -> Result<Frame, Failure> {
        if target != "chosen" {
            return Err(Failure::new("unknown target"));
        }
        self.state.lock().unwrap().observations += 1;
        Ok(Frame {
            width: 4,
            height: 4,
            png: picture(),
        })
    }
    fn act(
        &mut self,
        target: &str,
        action: &Action,
        cancel: &CancellationToken,
    ) -> Result<(), Failure> {
        if target != "chosen" {
            return Err(Failure::new("unknown target"));
        }
        action.validate_frame(4, 4)?;
        let mut state = self.state.lock().unwrap();
        state.actions.push(action.clone());
        if state.cancel_first && state.actions.len() == 1 {
            cancel.cancel();
        }
        if state.fail_second && state.actions.len() == 2 {
            return Err(Failure {
                message: "connection lost after mouse down".into(),
                interrupted: false,
                may_have_run: true,
            });
        }
        Ok(())
    }
    fn close(&mut self) {
        self.state.lock().unwrap().closed += 1;
    }
}
struct Client;
impl Component for Client {
    fn handle(&mut self, _: &str, _: &EventEnvelope, _: &mut Ctx) {}
}
fn kernel(path: Option<&std::path::Path>, state: Arc<Mutex<State>>, name: &str) -> Kernel {
    let mut client = desktop_tools::manifest();
    client.name = "client".into();
    client.entry = "builtin:client".into();
    client.tools.clear();
    client.implements.clear();
    client.inputs = vec![PortDecl::new(
        "done",
        &[ce::TOOL_EXEC_COMPLETED, ce::INTERRUPTED],
    )];
    client.outputs = vec![PortDecl::new("run", &[ce::TOOL_EXEC_STARTED])];
    let registry = [
        ("client".into(), client),
        (desktop_tools::NAME.into(), desktop_tools::manifest()),
    ]
    .into();
    let mut factories: HashMap<String, Factory> = HashMap::new();
    factories.insert("client".into(), Box::new(|_| Box::new(Client)));
    let name = name.to_owned();
    factories.insert(
        desktop_tools::NAME.into(),
        Box::new(move |_| {
            Box::new(desktop_tools::DesktopTools::with_driver(Box::new(
                FakeDesktop {
                    state: state.clone(),
                    name: name.clone(),
                },
            )))
        }),
    );
    Kernel::start(
        &AssemblyManifest {
            instances: [
                ("client".into(), ComponentInstance::new("client", None)),
                (
                    "desktop".into(),
                    ComponentInstance::new(desktop_tools::NAME, None),
                ),
            ]
            .into(),
            wires: vec![
                Wire::new("client.run", "desktop.execute"),
                Wire::new("desktop.outcome", "client.done"),
                Wire::new("desktop.interrupted", "client.done"),
            ],
        },
        &registry,
        &mut factories,
        KernelOptions {
            log_file: path.map(Into::into),
            stream: path.and_then(lattice::kernel::log::EventLog::stream_of),
            ..Default::default()
        },
    )
    .unwrap()
}
fn call(kernel: &mut Kernel, id: &str, args: Value) {
    kernel.injector("client").emit(
        "run",
        EventDraft::new(
            ce::TOOL_EXEC_STARTED,
            &[],
            json!({"call":id,"tool":"Desktop","arguments":args}),
        ),
    );
    kernel.run_until_quiescent().unwrap();
}
fn completed(kernel: &Kernel, id: &str) -> Value {
    kernel
        .log()
        .replay(1)
        .unwrap()
        .into_iter()
        .find(|e| e.event_type == ce::TOOL_EXEC_COMPLETED && e.payload["call"] == id)
        .expect("tool must answer without another approval loop")
        .payload
}
#[test]
fn normal_desktop_work_is_enabled_and_backend_independent_without_extra_approval() {
    for name in ["Native-like backend", "Alternative backend"] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ledger.jsonl");
        let state = Arc::new(Mutex::new(State::default()));
        let mut kernel = kernel(Some(&path), state.clone(), name);
        call(&mut kernel, "list", json!({"operation":"list"}));
        assert_eq!(
            completed(&kernel, "list")["result"]["targets"][0]["application"],
            name
        );
        call(
            &mut kernel,
            "observe",
            json!({"operation":"observe","target":"chosen"}),
        );
        let image = completed(&kernel, "observe");
        assert_eq!(image["result"]["width"], 4);
        call(
            &mut kernel,
            "act",
            json!({"operation":"act","target":"chosen","actions":[{"type":"click","x":1,"y":2},{"type":"type","text":"literal\ntext"}]}),
        );
        let result = completed(&kernel, "act");
        assert_eq!(result["result"]["completedActions"], 2);
        assert_eq!(state.lock().unwrap().actions.len(), 2);
        assert!(result["result"]["problem"].is_null());
        let docs = lattice::contracts::document::documents_dir(&path);
        let restored=responses_media::restore_input(vec![json!({"type":"function_call_output","call_id":"act","output":result["result"].to_string()})],Some(&docs)).unwrap();
        assert_eq!(restored[0]["output"][1]["type"], "input_image");
        let ledger = std::fs::read_to_string(&path).unwrap();
        assert!(
            !ledger.contains("iVBOR"),
            "pixels live beside the ledger, not inside its lines"
        );
        assert!(!kernel
            .log()
            .replay(1)
            .unwrap()
            .iter()
            .any(|e| e.event_type.contains("authorization_requested")));
        call(&mut kernel, "close", json!({"operation":"close"}));
        assert_eq!(completed(&kernel, "close")["result"]["closed"], true);
        assert!(state.lock().unwrap().closed > 0);
    }
}
#[test]
fn invalid_batches_are_rejected_before_the_first_action() {
    let dir = tempfile::tempdir().unwrap();
    let state = Arc::new(Mutex::new(State::default()));
    let mut kernel = kernel(
        Some(&dir.path().join("ledger.jsonl")),
        state.clone(),
        "Fake",
    );
    call(
        &mut kernel,
        "bad",
        json!({"operation":"act","target":"chosen","actions":[{"type":"click","x":1,"y":1},{"type":"key","key":"space","modifiers":["command"]}]}),
    );
    assert_eq!(completed(&kernel, "bad")["status"], "error");
    assert!(state.lock().unwrap().actions.is_empty());
}
#[test]
fn partial_failure_reports_uncertainty_and_never_retries_or_runs_the_tail() {
    let dir = tempfile::tempdir().unwrap();
    let state = Arc::new(Mutex::new(State {
        fail_second: true,
        ..Default::default()
    }));
    let mut kernel = kernel(
        Some(&dir.path().join("ledger.jsonl")),
        state.clone(),
        "Fake",
    );
    call(
        &mut kernel,
        "partial",
        json!({"operation":"act","target":"chosen","actions":[{"type":"type","text":"first"},{"type":"click","x":1,"y":1},{"type":"type","text":"must not run"}]}),
    );
    let result = completed(&kernel, "partial")["result"].clone();
    assert_eq!(result["completedActions"], 1);
    assert_eq!(result["attemptedActions"], 2);
    assert_eq!(result["uncertainAction"], 1);
    assert_eq!(result["problem"]["mayHaveRun"], true);
    assert_eq!(state.lock().unwrap().actions.len(), 2);
    assert_eq!(state.lock().unwrap().observations, 1);
}
#[test]
fn cancellation_keeps_partial_details_without_fabricating_a_completed_result() {
    let dir = tempfile::tempdir().unwrap();
    let state = Arc::new(Mutex::new(State {
        cancel_first: true,
        ..Default::default()
    }));
    let mut kernel = kernel(
        Some(&dir.path().join("ledger.jsonl")),
        state.clone(),
        "Fake",
    );
    call(
        &mut kernel,
        "cancelled",
        json!({"operation":"act","target":"chosen","actions":[{"type":"type","text":"first"},{"type":"type","text":"must not run"}]}),
    );
    let events = kernel.log().replay(1).unwrap();
    assert!(!events
        .iter()
        .any(|e| e.event_type == ce::TOOL_EXEC_COMPLETED));
    let interrupted = events
        .iter()
        .find(|e| e.event_type == ce::INTERRUPTED && e.payload.get("details").is_some())
        .unwrap();
    assert_eq!(interrupted.payload["details"]["completedActions"], 1);
    assert_eq!(state.lock().unwrap().actions.len(), 1);
    assert_eq!(state.lock().unwrap().observations, 0);
}
#[test]
fn a_nonpersistent_host_cannot_start_desktop_actions() {
    let state = Arc::new(Mutex::new(State::default()));
    let mut kernel = kernel(None, state.clone(), "Fake");
    call(
        &mut kernel,
        "no-ledger",
        json!({"operation":"act","target":"chosen","actions":[{"type":"type","text":"must not run"}]}),
    );
    assert_eq!(completed(&kernel, "no-ledger")["status"], "error");
    assert!(state.lock().unwrap().actions.is_empty());
}
#[test]
fn reopening_rebuilds_no_desktop_side_effects() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ledger.jsonl");
    let first = Arc::new(Mutex::new(State::default()));
    {
        let mut k = kernel(Some(&path), first.clone(), "First");
        call(
            &mut k,
            "act",
            json!({"operation":"act","target":"chosen","actions":[{"type":"type","text":"once"}]}),
        );
    }
    let second = Arc::new(Mutex::new(State::default()));
    let mut reopened = kernel(Some(&path), second.clone(), "Second");
    reopened.run_until_quiescent().unwrap();
    assert_eq!(first.lock().unwrap().actions.len(), 1);
    assert!(second.lock().unwrap().actions.is_empty());
    assert_eq!(second.lock().unwrap().observations, 0);
}
