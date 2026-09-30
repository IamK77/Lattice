//! Subagent: the model asks for a second agent, the host opens a stream for
//! it, and its answer comes back as a wake.
//!
//! The three things these tests hold down are the three decisions the design
//! turns on: the child is opened ISOLATED (it is handed no read handle on the
//! parent), a subagent cannot start another subagent, and a job that was still
//! running when the process died is settled rather than re-run.

#[path = "subagent/custom_definitions.rs"]
mod custom_definitions;
#[path = "subagent/outcomes.rs"]
mod outcomes;
#[path = "subagent/snapshot_probe.rs"]
mod snapshot_probe;

use std::collections::HashMap;

use serde_json::{json, Value};

use lattice::components::{minimal_loop, scripted_model, silent_ui, subagent};
use lattice::core_events as ce;
use lattice::subagent_host::SubagentHost;
use lattice::{
    AssemblyManifest, Component, ComponentInstance, ComponentManifest, Ctx, EventDraft,
    EventEnvelope, Factory, PortDecl, RuntimeKind, StreamHost, StreamTemplate, Wire,
};

/// What a stream this test opens can see of other streams. A component is the
/// only thing that can answer this, because being handed a foreign reader is
/// something only a component experiences.
const SAW: &str = "test.saw";

struct Probe;
impl Component for Probe {
    fn handle(&mut self, _port: &str, event: &EventEnvelope, ctx: &mut Ctx) {
        let seen = ctx.foreign_streams();
        ctx.emit(
            "out",
            EventDraft::new(SAW, &[&event.id], json!({"streams": seen})),
        );
    }
}

fn probe_manifest() -> ComponentManifest {
    ComponentManifest {
        name: "probe".to_string(),
        version: "0".to_string(),
        runtime: RuntimeKind::Inproc,
        entry: "builtin:probe".to_string(),
        inputs: vec![PortDecl::new("input", &[ce::USER_MESSAGE])],
        outputs: vec![PortDecl::new("out", &[SAW])],
        events: vec![lattice::EventTypeDecl::new(
            SAW,
            "which foreign streams this one sees",
        )],
        default_wiring: Vec::new(),
        capabilities: None,
        implements: Vec::new(),
        tools: Vec::new(),
        prompt: None,
        handle_timeout_ms: None,
        concurrency: None,
    }
}

fn instance(component: &str, config: Option<Value>) -> ComponentInstance {
    ComponentInstance {
        component: component.to_string(),
        requires: Vec::new(),
        config,
    }
}

/// A chat template with a subagent component on its tool wire. `script` is the
/// model's replies in order; `depth` is how deep this stream already is.
fn template(script: Value, depth: u64) -> StreamTemplate {
    let registry: HashMap<String, ComponentManifest> = [
        (silent_ui::NAME.to_string(), silent_ui::manifest()),
        (minimal_loop::NAME.to_string(), minimal_loop::manifest()),
        (scripted_model::NAME.to_string(), scripted_model::manifest()),
        (subagent::NAME.to_string(), subagent::manifest()),
        ("probe".to_string(), probe_manifest()),
    ]
    .into();
    let mut factories: HashMap<String, Factory> = HashMap::new();
    factories.insert(
        silent_ui::NAME.to_string(),
        Box::new(|_| {
            Box::new(silent_ui::SilentUi::new(std::sync::Arc::new(
                std::sync::Mutex::new(Vec::new()),
            )))
        }),
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
        subagent::NAME.to_string(),
        Box::new(|c| Box::new(subagent::Subagent::from_config(c))),
    );
    factories.insert("probe".to_string(), Box::new(|_| Box::new(Probe)));

    let assembly = AssemblyManifest {
        instances: [
            ("ui".to_string(), instance(silent_ui::NAME, None)),
            ("loop".to_string(), instance(minimal_loop::NAME, None)),
            (
                "model".to_string(),
                instance(scripted_model::NAME, Some(json!({"script": script}))),
            ),
            (
                "subagent".to_string(),
                instance(
                    subagent::NAME,
                    Some(json!({
                        "depth": depth,
                        // Who exists is the assembler's knowledge: a name here
                        // has to be a template the host will actually open.
                        "experts": [{"name": "explorer", "description": "reads things"}],
                    })),
                ),
            ),
            ("probe".to_string(), instance("probe", None)),
        ]
        .into(),
        wires: vec![
            Wire::new("ui.user", "loop.input"),
            Wire::new("ui.user", "probe.input"),
            Wire::new("loop.ask", "model.request"),
            Wire::new("model.result", "loop.model"),
            // No trust gate here: what is under test is the hand-off path,
            // and the gate has its own file.
            Wire::new("loop.run", "subagent.execute"),
            Wire::new("subagent.outcome", "loop.tools"),
            Wire::new("subagent.wake", "loop.input"),
            Wire::new("loop.out", "ui.display"),
        ],
    };
    StreamTemplate {
        registry,
        factories,
        assembly,
    }
}

/// Hand work to an expert in the background, then reply once its answer comes.
fn parent_script() -> Value {
    json!([
        {"status": "ok", "toolCalls": [
            {"id": "t1", "tool": "ask",
             "arguments": {"expert": "explorer", "prompt": "count the files"}}]},
        {"status": "ok", "text": "the expert said 42"},
    ])
}

/// A directory of this test's own, since ledgers are files now: an expert
/// answers once and is gone, and reading what it did means reading its record.
fn fresh_dir() -> std::path::PathBuf {
    static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("lattice-ask-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn host(dir: &std::path::Path, parent_script: Value, child_script: Value) -> StreamHost {
    let templates: HashMap<String, StreamTemplate> = [
        ("chat".to_string(), template(parent_script, 0)),
        // An expert IS a template registered under its name. This one differs
        // from the parent's in exactly one way: depth 1.
        ("explorer".to_string(), template(child_script, 1)),
    ]
    .into();
    let dir = dir.to_path_buf();
    StreamHost::new(templates)
        .with_ledger_path(move |stream| Some(dir.join(format!("{stream}.jsonl"))))
}

/// What an expert did, read back from its file — the host does not hold its
/// kernel, because an expert runs on a thread of its own.
fn from_ledger(dir: &std::path::Path, stream: &str) -> Vec<EventEnvelope> {
    let path = dir.join(format!("{stream}.jsonl"));
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    text.lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

/// Say one thing into a stream, then run host + subagent host until nothing moves.
///
/// The wait is causal, not timed: a subagent advances only when the host next
/// runs its streams, so "nothing moved this pass" is the real end condition.
/// Bounded so a design mistake fails the test instead of hanging it.
fn settle(host: &mut StreamHost, subagents: &mut SubagentHost, done: impl Fn(&StreamHost) -> bool) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    loop {
        host.run_all().unwrap();
        subagents.poll(host).unwrap();
        if done(host) {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "what was expected never arrived"
        );
        // The expert is on a thread of its own, so this cannot be made causal
        // the way the rest of the suite is — nothing in this process produces
        // the event being waited for. The condition is the real signal; this
        // only keeps the wait from spinning a core.
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

/// Has an event of this type reached the conversation?
fn reached(host: &StreamHost, event_type: &str) -> bool {
    events(host, "chat")
        .iter()
        .any(|e| e.event_type == event_type)
}

fn say(host: &StreamHost, stream: &str, text: &str) {
    host.injector(stream, "ui").unwrap().emit(
        "user",
        EventDraft::new(ce::USER_MESSAGE, &[], json!({"text": text})),
    );
}

fn events(host: &StreamHost, stream: &str) -> Vec<EventEnvelope> {
    host.kernel(stream)
        .map(|k| k.log().replay(1).unwrap())
        .unwrap_or_default()
}

#[test]
fn the_answer_from_a_subagent_comes_back_as_a_wake_pointing_at_its_stream() {
    let dir = fresh_dir();
    let mut host = host(
        &dir,
        parent_script(),
        json!([{"status": "ok", "text": "there are 42 files"}]),
    );
    let mut subagents = SubagentHost::new();
    host.open("chat", "chat").unwrap();
    say(&host, "chat", "how many files are there");
    settle(&mut host, &mut subagents, |h| reached(h, ce::WAKE));

    let parent = events(&host, "chat");

    // The call was answered at once with a job number — the model is not left
    // holding a tool call open for as long as the subagent runs.
    let ack = parent
        .iter()
        .find(|e| e.event_type == ce::TOOL_EXEC_COMPLETED)
        .expect("the Task call was answered");
    assert_eq!(ack.payload["status"], "ok");
    assert_eq!(ack.payload["result"]["job"], 1);

    // And the answer arrived separately, as a wake.
    let wake = parent
        .iter()
        .find(|e| e.event_type == ce::WAKE)
        .expect("the subagent's answer came back");
    assert_eq!(wake.payload["body"]["job"], 1);
    assert_eq!(wake.payload["body"]["text"], "there are 42 files");
    // Where to go and read what it actually did. An expert cannot be asked a
    // follow-up — that is what you do with a peer — so the record has to be
    // reachable, and reaching it is reading a file.
    let ledger = wake.payload["body"]["ledger"]
        .as_str()
        .expect("the answer says where the record is");
    assert!(
        std::path::Path::new(ledger).exists(),
        "and the record is really there: {ledger}"
    );

    // Cross-stream, the tie is `origin` and only `origin`: `causes` is checked
    // against what this stream's emitter witnessed, and nothing in another
    // stream was witnessed here.
    let origin = wake
        .origin
        .as_ref()
        .expect("the wake names where it came from");
    assert_eq!(origin.stream, "chat-sub-1");
    let started = parent
        .iter()
        .find(|e| e.event_type == ce::TOOL_EXEC_STARTED)
        .unwrap();
    assert!(
        wake.causes.contains(&started.id),
        "the wake is caused by the Task call, so it frees the round waiting on it"
    );

    // The stream is closed once answered; its ledger is not unwritten, it just
    // stops being run.
    assert!(
        !host.open_stream_ids().contains(&"chat-sub-1".to_string()),
        "the subagent stream was closed: {:?}",
        host.open_stream_ids()
    );
}

#[test]
fn a_subagent_is_handed_no_way_to_read_the_conversation_that_sent_it() {
    let dir = fresh_dir();
    let mut host = host(
        &dir,
        parent_script(),
        json!([{"status": "ok", "text": "there are 42 files"}]),
    );
    let mut subagents = SubagentHost::new();
    host.open("chat", "chat").unwrap();
    say(&host, "chat", "SECRET-PARENT-TEXT");

    // Wait for the answer, then read what the expert did from its FILE: it
    // ran on a thread of its own and is gone by now, and its record is the
    // only way back to the work — which is the point of keeping one.
    settle(&mut host, &mut subagents, |h| reached(h, ce::WAKE));

    let child = from_ledger(&dir, "chat-sub-1");
    assert!(!child.is_empty(), "the expert left a record");

    // The child's components are handed no foreign reader at all. This is the
    // whole difference between `open` and `open_derived`, and it is the reason
    // a subagent starts with a clean context rather than this one.
    let saw = child
        .iter()
        .find(|e| e.event_type == SAW)
        .expect("the probe reported what it could see");
    assert_eq!(
        saw.payload["streams"],
        json!([]),
        "a subagent must be handed no read handle on any other stream"
    );

    // And nothing of the parent's conversation is in its material.
    let text = serde_json::to_string(&child).unwrap();
    assert!(
        !text.contains("SECRET-PARENT-TEXT"),
        "the parent's words must not appear in the child's ledger"
    );
    // What it did get is the prompt, and only the prompt.
    let first = child
        .iter()
        .find(|e| e.event_type == ce::USER_MESSAGE)
        .expect("the child was told what to do");
    assert_eq!(first.payload["text"], "count the files");
    let origin = first
        .origin
        .as_ref()
        .expect("the child knows where it came from");
    assert_eq!(origin.stream, "chat");
}

#[test]
fn a_subagent_cannot_start_another_subagent() {
    // The child's model tries to hand its own work off again.
    let dir = fresh_dir();
    let mut host = host(
        &dir,
        parent_script(),
        json!([
            {"status": "ok", "toolCalls": [
                {"id": "n1", "tool": "ask",
                 "arguments": {"expert": "explorer", "prompt": "go deeper"}}]},
            {"status": "ok", "text": "fine, I did it myself"},
        ]),
    );
    let mut subagents = SubagentHost::new();
    host.open("chat", "chat").unwrap();
    say(&host, "chat", "how many files are there");

    settle(&mut host, &mut subagents, |h| reached(h, ce::WAKE));

    let child = from_ledger(&dir, "chat-sub-1");
    let refused = child
        .iter()
        .find(|e| e.event_type == ce::TOOL_EXEC_COMPLETED)
        .expect("the nested ask call was answered");
    assert_eq!(refused.payload["status"], "error");
    assert_eq!(refused.payload["error"]["code"], "ask.too_deep");
    // Refused, and therefore no stream was opened for it.
    assert!(
        !host
            .open_stream_ids()
            .contains(&"chat-sub-1-sub-1".to_string()),
        "no grandchild stream: {:?}",
        host.open_stream_ids()
    );

    // A refusal is data, not a dead end: the round goes on and the child still
    // answers its parent.
    settle(&mut host, &mut subagents, |h| reached(h, ce::WAKE));
    let wake = events(&host, "chat")
        .into_iter()
        .find(|e| e.event_type == ce::WAKE)
        .expect("the subagent still answered");
    assert_eq!(wake.payload["body"]["text"], "fine, I did it myself");
}

#[test]
fn a_job_still_running_at_a_restart_is_settled_and_not_re_run() {
    let dir = tempfile::tempdir().unwrap();
    let ledger = dir.path().join("chat.jsonl");

    // Life one: the Task call is made and acked, and then the process dies
    // before the subagent ever answers — the subagent host is never polled.
    {
        let path = ledger.clone();
        let mut host = host(
            &fresh_dir(),
            parent_script(),
            json!([{"status": "ok", "text": "unused"}]),
        )
        .with_ledger_path(move |_| Some(path.clone()));
        host.open("chat", "chat").unwrap();
        say(&host, "chat", "how many files are there");
        host.run_all().unwrap();
        let first = events(&host, "chat");
        assert!(
            first
                .iter()
                .any(|e| e.event_type == subagent::STREAM_REQUESTED),
            "a stream was asked for before the crash"
        );
        assert!(
            !first.iter().any(|e| e.event_type == ce::WAKE),
            "and nothing answered it"
        );
        host.close("chat");
    }

    // Life two: a fresh host on the same ledger. Its model answers in plain
    // text — a script that handed out another `Task` would start a SECOND job
    // (the scripted queue begins again on every life), and this test would
    // then be watching that one instead of the one it cares about.
    let mut host = reopened(ledger.clone());
    let mut subagents = SubagentHost::new();
    host.open("chat", "chat").unwrap();
    host.run_all().unwrap();

    let after = events(&host, "chat");
    let settled: Vec<_> = after.iter().filter(|e| e.event_type == ce::WAKE).collect();
    assert_eq!(settled.len(), 1, "settled exactly once");
    assert_eq!(settled[0].payload["body"]["interrupted"], "restart");
    assert_eq!(settled[0].payload["body"]["job"], 1);

    // Not re-run. A subagent writes files and reaches the network; replay
    // only looks, it does not do.
    subagents.poll(&mut host).unwrap();
    assert!(
        !host.open_stream_ids().contains(&"chat-sub-1".to_string()),
        "the interrupted job was not restarted: {:?}",
        host.open_stream_ids()
    );

    // And reopening again does not settle it a second time — the first wake
    // IS the ending, so asking "did it answer" would settle it forever.
    host.close("chat");
    let mut again = reopened(ledger.clone());
    again.open("chat", "chat").unwrap();
    again.run_all().unwrap();
    assert_eq!(
        events(&again, "chat")
            .iter()
            .filter(|e| e.event_type == ce::WAKE)
            .count(),
        1,
        "reopening does not settle the same job twice"
    );
}

/// A host on an existing ledger whose model does NOT hand out more work: what
/// is under test is how the reopen treats the job already on the record.
fn reopened(path: std::path::PathBuf) -> StreamHost {
    host(
        &fresh_dir(),
        json!([{"status": "ok", "text": "I see that job was cut off"}]),
        json!([{"status": "ok", "text": "unused"}]),
    )
    .with_ledger_path(move |_| Some(path.clone()))
}

/// Handing work over in the foreground, where the call stays open until the
/// expert answers.
fn foreground_script() -> Value {
    json!([
        {"status": "ok", "toolCalls": [
            {"id": "t1", "tool": "ask", "arguments": {
                "expert": "explorer", "prompt": "count the files", "background": false}}]},
        {"status": "ok", "text": "so there are 42"},
    ])
}

#[test]
fn asking_with_no_expert_named_is_answered_here_and_hands_nothing_over() {
    let dir = fresh_dir();
    let mut host = host(
        &dir,
        json!([
            {"status": "ok", "toolCalls": [
                {"id": "t1", "tool": "ask", "arguments": {"prompt": "who can search"}}]},
            {"status": "ok", "text": "I will ask the explorer"},
        ]),
        json!([{"status": "ok", "text": "unused"}]),
    );
    let mut subagents = SubagentHost::new();
    host.open("chat", "chat").unwrap();
    say(&host, "chat", "find something");
    settle(&mut host, &mut subagents, |h| {
        reached(h, ce::TOOL_EXEC_COMPLETED)
    });

    let parent = events(&host, "chat");
    let listed = parent
        .iter()
        .find(|e| e.event_type == ce::TOOL_EXEC_COMPLETED)
        .expect("the question was answered");
    assert_eq!(listed.payload["status"], "ok");
    assert_eq!(listed.payload["result"]["experts"][0]["name"], "explorer");

    // Asking who is there hands no work over, so nothing was opened and
    // nothing will wake anyone later.
    assert!(
        !parent
            .iter()
            .any(|e| e.event_type == subagent::STREAM_REQUESTED),
        "no stream was asked for"
    );
    assert!(
        !parent.iter().any(|e| e.event_type == ce::WAKE),
        "and nothing wakes later"
    );
}

#[test]
fn a_foreground_hand_off_is_answered_by_the_call_itself_and_never_also_by_a_wake() {
    let dir = fresh_dir();
    let mut host = host(
        &dir,
        foreground_script(),
        json!([{"status": "ok", "text": "there are 42 files"}]),
    );
    let mut subagents = SubagentHost::new();
    host.open("chat", "chat").unwrap();
    say(&host, "chat", "how many files are there");
    settle(&mut host, &mut subagents, |h| {
        events(h, "chat")
            .iter()
            .any(|e| e.event_type == ce::TOOL_EXEC_COMPLETED)
    });

    let parent = events(&host, "chat");
    let outcomes: Vec<_> = parent
        .iter()
        .filter(|e| e.event_type == ce::TOOL_EXEC_COMPLETED)
        .collect();
    // One call, one outcome. Two would be the same call id answered twice,
    // which every dialect refuses — and the pair would stay in the material.
    assert_eq!(outcomes.len(), 1, "exactly one outcome for one call");
    assert_eq!(outcomes[0].payload["status"], "ok");
    assert_eq!(outcomes[0].payload["result"]["text"], "there are 42 files");
    assert_eq!(
        outcomes[0].payload["call"], "t1",
        "and it names the call it answers"
    );

    // The answer came as the outcome, so nothing wakes anyone.
    assert!(
        !parent.iter().any(|e| e.event_type == ce::WAKE),
        "a foreground answer does not also arrive as a wake"
    );
}

#[test]
fn a_foreground_call_cut_off_by_a_restart_is_left_for_the_kernel_to_settle() {
    let dir = tempfile::tempdir().unwrap();
    let ledger = dir.path().join("chat.jsonl");

    // Life one: the call is made, the work is handed over, and the process
    // dies before any answer. The call is still OPEN on the ledger.
    {
        let path = ledger.clone();
        let dir = fresh_dir();
        let mut host = host(
            &dir,
            foreground_script(),
            json!([{"status": "ok", "text": "unused"}]),
        )
        .with_ledger_path(move |_| Some(path.clone()));
        host.open("chat", "chat").unwrap();
        say(&host, "chat", "how many files are there");
        host.run_all().unwrap();
        let first = events(&host, "chat");
        assert!(
            first
                .iter()
                .any(|e| e.event_type == subagent::STREAM_REQUESTED),
            "work was handed over"
        );
        assert!(
            !first
                .iter()
                .any(|e| e.event_type == ce::TOOL_EXEC_COMPLETED),
            "and the call was left open, which is what makes this case different"
        );
        host.close("chat");
    }

    // Life two. The kernel settles open calls on reopen; this component must
    // not say the same thing a second time through a different door.
    let mut again = reopened(ledger.clone());
    again.open("chat", "chat").unwrap();
    again.run_all().unwrap();

    let after = events(&again, "chat");
    let mine: Vec<_> = after
        .iter()
        .filter(|e| e.event_type == ce::WAKE)
        .filter(|e| e.payload["body"]["interrupted"] == "restart")
        .collect();
    assert!(
        mine.is_empty(),
        "a foreground call is the kernel's to settle, not this component's: {:?}",
        mine.iter().map(|e| &e.payload).collect::<Vec<_>>()
    );
}

/// Every built-in expert is a real assembly that starts, and what it may do is
/// which components are in it — not a claim it makes.
#[test]
fn each_expert_is_an_assembly_that_starts_and_omits_what_it_may_not_do() {
    std::env::set_var("LATTICE_SCRIPTED", "1");
    let cfg = lattice::preset::PresetConfig::from_env();
    std::env::remove_var("LATTICE_SCRIPTED");

    assert!(
        !lattice::preset::EXPERTS.is_empty(),
        "there is at least one expert to send work to"
    );
    for expert in lattice::preset::EXPERTS {
        let (registry, mut factories, assembly) =
            lattice::preset::expert_assembly(&cfg, expert).expect("the expert assembles");

        // An expert cannot hand work on, and the way it cannot is that the
        // component offering `ask` is not in its stream at all.
        assert!(
            !assembly.instances.contains_key("subagent"),
            "{}: an expert has no way to ask for another expert",
            expert.name
        );
        // What it was told it is for reaches the thing that assembles prompts.
        assert_eq!(
            assembly.instances["ctx"].config.as_ref().unwrap()["system"],
            json!(expert.prompt),
            "{}: the expert is told what it is",
            expert.name
        );
        // Inspection is the real test: a wire naming a component that was
        // filtered out would fail here rather than at run time.
        let options = lattice::KernelOptions {
            stream: Some(format!("expert-{}", expert.name)),
            ..Default::default()
        };
        let mut kernel = lattice::Kernel::start(&assembly, &registry, &mut factories, options)
            .unwrap_or_else(|e| panic!("{} does not start: {e}", expert.name));

        // Starting is not enough. What an expert is GIVEN is one message, and
        // the path it travels runs through several instances — drop one that
        // only looked like a toolset and the message reaches nobody. The first
        // real run did exactly that: the expert opened, was told what to do,
        // and never took a turn.
        kernel.injector("ui").emit(
            "user",
            EventDraft::new(ce::USER_MESSAGE, &[], json!({"text": "what is here"})),
        );
        kernel.run_until_quiescent().unwrap();
        let events = kernel.log().replay(1).unwrap();
        assert!(
            events
                .iter()
                .any(|e| e.event_type == ce::MODEL_CALL_STARTED),
            "{}: the message it was sent has to REACH the loop, and did not: {:?}",
            expert.name,
            events
                .iter()
                .map(|e| e.event_type.as_str())
                .collect::<Vec<_>>()
        );
        kernel.shutdown();
    }

    // And the boundary is real for at least one of them: the reader cannot run
    // commands, because there is no component in it that runs commands.
    let explorer = lattice::preset::EXPERTS
        .iter()
        .find(|e| e.name == "explorer")
        .expect("an explorer exists");
    let (_, _, assembly) = lattice::preset::expert_assembly(&cfg, explorer).unwrap();
    assert!(
        !assembly.instances.contains_key("shell"),
        "the explorer has no shell to reach for: {:?}",
        assembly.instances.keys().collect::<Vec<_>>()
    );
}

/// The path the TUI actually takes: a real `Session`, driving its own kernel,
/// with an expert host beside it.
///
/// Everything else here drives the subagent host directly. This drives the thing
/// the product runs — the session thread, its command queue, its wake
/// forwarding — because "the parts pass" and "the wiring works" are different
/// claims and only one of them was being made.
#[test]
fn a_session_hands_work_to_an_expert_and_the_answer_comes_back_into_the_conversation() {
    let dir = fresh_dir();
    let parent = template(parent_script(), 0);
    let ledger = dir.join("chat.jsonl");

    let expert_dir = dir.clone();
    let experts: Box<dyn FnOnce() -> StreamHost + Send> = Box::new(move || {
        let templates: HashMap<String, StreamTemplate> = [(
            "explorer".to_string(),
            template(json!([{"status": "ok", "text": "there are 42 files"}]), 1),
        )]
        .into();
        StreamHost::new(templates)
            .with_ledger_path(move |s| Some(expert_dir.join(format!("{s}.jsonl"))))
    });

    let session = lattice::Session::spawn_with_subagents(
        "ui",
        None,
        Some((String::new(), experts)),
        move |_render_tx| {
            let StreamTemplate {
                registry,
                mut factories,
                assembly,
            } = parent;
            lattice::Kernel::start(
                &assembly,
                &registry,
                &mut factories,
                lattice::KernelOptions {
                    stream: Some("chat".to_string()),
                    log_file: Some(ledger),
                    ..Default::default()
                },
            )
        },
    )
    .expect("the session starts");

    session.send_text("how many files are there");

    // The conversation's own record is the observable: the session owns its
    // kernel, and this is what any frontend would be rendering from.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        let events = from_ledger(&dir, "chat");
        // The WAKE is the signal, not the reply text: a scripted model answers
        // from its list whether or not an expert ever spoke, so waiting for a
        // reply would pass without the expert having done anything at all.
        if let Some(wake) = events.iter().find(|e| e.event_type == ce::WAKE) {
            assert_eq!(wake.payload["body"]["text"], "there are 42 files");
            assert_eq!(wake.payload["body"]["stream"], "chat-sub-1");
            let path = wake.payload["body"]["ledger"].as_str().unwrap_or_default();
            assert!(
                std::path::Path::new(path).exists(),
                "the expert left a record at {path}"
            );
            session.shutdown();
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the conversation never heard back: {:?}",
            events
                .iter()
                .map(|e| e.event_type.as_str())
                .collect::<Vec<_>>()
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

/// The record an expert leaves has to be as auditable as the conversation's.
///
/// Its stream is unlike every other one here: the host opens it, hands the
/// kernel to a thread of its own, injects one message, and closes it the
/// moment the answer is in. Nothing else in the assembly has that lifecycle,
/// and the enforcement it is checked against is the same enforcement the
/// conversation gets — numbering that does not jump, causes that are older
/// than what names them, and one ending per request.
#[test]
fn an_experts_own_record_obeys_the_same_laws_as_the_conversation() {
    let dir = fresh_dir();
    let mut host = host(
        &dir,
        parent_script(),
        json!([{"status": "ok", "text": "there are 42 files"}]),
    );
    let mut subagents = SubagentHost::new();
    host.open("chat", "chat").unwrap();
    say(&host, "chat", "how many files are there");
    settle(&mut host, &mut subagents, |h| reached(h, ce::WAKE));

    let child = from_ledger(&dir, "chat-sub-1");
    assert!(
        !child.is_empty(),
        "the expert wrote a record of its own: {:?}",
        std::fs::read_dir(&dir).map(|d| d.count())
    );

    let mut seen: HashMap<&str, u64> = HashMap::new();
    for (at, event) in child.iter().enumerate() {
        assert_eq!(
            event.seq,
            at as u64 + 1,
            "the expert's numbering jumps at {}",
            event.id
        );
        assert_eq!(
            event.stream, "chat-sub-1",
            "{} is on another stream",
            event.id
        );
        for cause in &event.causes {
            let older = seen
                .get(cause.as_str())
                .unwrap_or_else(|| panic!("{} names a cause that is not on this stream", event.id));
            assert!(
                *older < event.seq,
                "{} (#{}) names #{older} as a cause",
                event.id,
                event.seq
            );
        }
        seen.insert(event.id.as_str(), event.seq);
    }

    // Whatever the expert asked a tool for, it got exactly one ending —
    // including across the close, which happens as soon as it answers.
    for request in child
        .iter()
        .filter(|e| e.event_type == ce::TOOL_EXEC_STARTED)
    {
        let ends = child
            .iter()
            .filter(|e| {
                matches!(
                    e.event_type.as_str(),
                    ce::TOOL_EXEC_COMPLETED | ce::INTERRUPTED
                ) && e.causes.contains(&request.id)
            })
            .count();
        assert_eq!(
            ends, 1,
            "{} has {ends} endings on the expert's record",
            request.id
        );
    }
}
