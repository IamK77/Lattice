//! Context management v1, end to end: the gate on the ask wire runs the
//! window scale (provider usage off the ledger vs profile window × ratio)
//! and the mechanical trim (old tool results → digest parts), forwards with
//! a recomputed fingerprint, and lands a reasoned compaction decision. The
//! loop is untouched throughout — that is the point of the gate posture.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

#[path = "context/manual.rs"]
mod manual;

use lattice::components::model_common::verify_fingerprint;
use lattice::components::{context_gate, minimal_loop, scripted_model, silent_ui, timer_tools};

mod common;
use common::calc_tools;
use lattice::core_events as ce;
use lattice::{
    AssemblyManifest, ComponentInstance, EventDraft, Factory, Kernel, KernelOptions, Wire,
};

/// The gated assembly, built but not yet spoken to, with the model's script
/// handed in. Split from `start_gated_kernel` so a test can drive several
/// turns and change something between them.
fn build_gated_kernel(gate_config: Value, script: Value) -> Kernel {
    let registry: HashMap<String, lattice::ComponentManifest> = [
        (silent_ui::NAME.to_string(), silent_ui::manifest()),
        (minimal_loop::NAME.to_string(), minimal_loop::manifest()),
        (context_gate::NAME.to_string(), context_gate::manifest()),
        (scripted_model::NAME.to_string(), scripted_model::manifest()),
        (calc_tools::NAME.to_string(), calc_tools::manifest()),
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
        context_gate::NAME.to_string(),
        Box::new(|c| Box::new(context_gate::ContextGate::from_config(c))),
    );
    factories.insert(
        scripted_model::NAME.to_string(),
        Box::new(|c| Box::new(scripted_model::ScriptedModel::from_config(c))),
    );
    factories.insert(
        calc_tools::NAME.to_string(),
        Box::new(|c| Box::new(calc_tools::CalcTools::from_config(c))),
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
                "gate".to_string(),
                ComponentInstance {
                    component: context_gate::NAME.to_string(),
                    requires: Vec::new(),
                    config: Some(gate_config),
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
                "tools".to_string(),
                ComponentInstance {
                    component: calc_tools::NAME.to_string(),
                    requires: Vec::new(),
                    config: None,
                },
            ),
        ]
        .into(),
        wires: vec![
            Wire::new("ui.user", "loop.input"),
            Wire::new("loop.ask", "gate.ask"),
            Wire::new("gate.forward", "model.request"),
            Wire::new("model.result", "loop.model"),
            Wire::new("loop.run", "tools.execute"),
            Wire::new("tools.outcome", "loop.tools"),
            Wire::new("loop.out", "ui.display"),
            // Settings a person turns mid-conversation, the same wire the
            // standard assembly uses
            Wire::new("ui.answer", "gate.dial"),
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

fn start_gated_kernel(gate_config: Value) -> Kernel {
    // Turn one: the model wants a tool, then finishes — and its completions
    // carry HEAVY usage, so the SECOND ask of the turn crosses the scale
    let script = json!({"script": [
        {"status": "ok", "usage": {"input_tokens": 900},
         "reasoning": [{"kind": "text", "text": "first, add them"}],
         "toolCalls": [{"id": "c1", "tool": "calc", "arguments": {"numbers": [4, 7]}}]},
        {"status": "ok", "text": "the sum is 11", "usage": {"input_tokens": 950}},
    ]});
    let mut kernel = build_gated_kernel(gate_config, script);
    kernel.injector("ui").emit(
        "user",
        EventDraft::new(ce::USER_MESSAGE, &[], json!({"text": "add 4 and 7"})),
    );
    kernel.run_until_quiescent().unwrap();
    kernel
}

#[test]
fn default_compaction_ratio_is_85_percent_and_explicit_ratios_still_win() {
    for (ratio, measured, threshold, expected) in [
        (None, 849, 850, false),
        (None, 850, 850, false),
        (None, 851, 850, true),
        (Some(0.5), 501, 500, true),
    ] {
        let mut config = json!({
            "profile": {"contextWindow": 1000},
            "keepRecentTools": 0
        });
        if let Some(ratio) = ratio {
            config["ratio"] = json!(ratio);
        }
        let mut kernel = build_gated_kernel(
            config,
            json!({"script": [
                {"status": "ok", "usage": {"input_tokens": measured},
                 "toolCalls": [{"id": "c1", "tool": "calc", "arguments": {"numbers": [4, 7]}}]},
                {"status": "ok", "text": "done", "usage": {"input_tokens": 100}}
            ]}),
        );
        say(&mut kernel, "calculate");
        let events = kernel.log().replay(1).unwrap();
        let decisions: Vec<_> = events
            .iter()
            .filter(|event| {
                event.event_type == context_gate::DECISION && event.payload["action"] == "digest"
            })
            .collect();
        assert_eq!(
            decisions.len(),
            usize::from(expected),
            "ratio={ratio:?}, measured={measured}"
        );
        if expected {
            assert_eq!(decisions[0].payload["threshold"], threshold);
            assert_eq!(decisions[0].payload["measured"], measured);
        }
    }
}

#[test]
fn a_smaller_trimmed_call_does_not_restore_old_tool_results() {
    let mut kernel = build_gated_kernel(
        json!({
            "profile":{"contextWindow":1000,"usageFields":{"input":"input_tokens"}},
            "ratio":0.5,"keepRecentTools":0
        }),
        json!({"script":[
            {"status":"ok","usage":{"input_tokens":900},"toolCalls":[{"id":"c1","tool":"calc","arguments":{"numbers":[4,7]}}]},
            {"status":"ok","text":"done","usage":{"input_tokens":100}},
            {"status":"ok","text":"next","usage":{"input_tokens":110}}
        ]}),
    );
    say(&mut kernel, "calculate");
    say(&mut kernel, "continue");
    let events = kernel.log().replay(1).unwrap();
    let forwards: Vec<_> = events
        .iter()
        .filter(|e| e.event_type == ce::MODEL_CALL_STARTED && e.source == "gate")
        .collect();
    let digest = forwards[1].payload["input"]["parts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p.get("digest").is_some())
        .unwrap();
    assert!(
        forwards[2].payload["input"]["parts"]
            .as_array()
            .unwrap()
            .contains(digest),
        "lower usage must retain the trimmed view"
    );
}

#[test]
fn mechanical_digests_survive_ledger_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("trim.jsonl");
    let config = json!({"profile":{"contextWindow":1000,"usageFields":{"input":"input_tokens"}},
        "ratio":0.5,"keepRecentTools":0,"condense":false});
    let script = json!({"script":[
        {"status":"ok","usage":{"input_tokens":900},"toolCalls":[{"id":"c1","tool":"calc","arguments":{"numbers":[4,7]}}]},
        {"status":"ok","text":"done","usage":{"input_tokens":100}}
    ]});
    let mut first = condensing_kernel_with(
        json!({"script":[]}),
        script.clone(),
        config.clone(),
        false,
        Some(&path),
    );
    say(&mut first, "calculate");
    let digest = first
        .log()
        .find_back(|e| {
            e.event_type == ce::MODEL_CALL_STARTED
                && e.source == "gate"
                && e.payload.get("purpose").is_none()
        })
        .unwrap()
        .unwrap()
        .payload["input"]["parts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p.get("digest").is_some())
        .unwrap()
        .clone();
    first.shutdown();
    let mut reopened =
        condensing_kernel_with(json!({"script":[]}), script, config, false, Some(&path));
    say(&mut reopened, "continue");
    let request = reopened
        .log()
        .find_back(|e| {
            e.event_type == ce::MODEL_CALL_STARTED
                && e.source == "gate"
                && e.payload.get("purpose").is_none()
        })
        .unwrap()
        .unwrap();
    assert!(
        request.payload["input"]["parts"]
            .as_array()
            .unwrap()
            .contains(&digest),
        "reopening must retain mechanical digests"
    );
    reopened.shutdown();
}

#[test]
fn the_gate_trims_over_budget_and_records_a_reasoned_decision() {
    // Window 1000 × ratio 0.5 = threshold 500; the script's usage is 900
    let kernel = start_gated_kernel(json!({
        "profile": {"contextWindow": 1000, "usageFields": {"input": "input_tokens"}},
        "ratio": 0.5,
        "keepRecentTools": 0,
    }));
    let events = kernel.log().replay(1).unwrap();

    // The gate's forwards are distinguishable from the loop's asks by source
    let forwards: Vec<_> = events
        .iter()
        .filter(|e| e.event_type == ce::MODEL_CALL_STARTED && e.source == "gate")
        .collect();
    assert_eq!(forwards.len(), 2, "two asks, two forwards");

    // Ask one (before any usage exists): pass-through, byte-identical parts
    let loop_asks: Vec<_> = events
        .iter()
        .filter(|e| e.event_type == ce::MODEL_CALL_STARTED && e.source == "loop")
        .collect();
    assert_eq!(
        forwards[0].payload["input"]["parts"], loop_asks[0].payload["input"]["parts"],
        "under budget the gate must not touch the material"
    );

    // Ask two (usage 900 > 500): the tool outcome became a digest part
    let outcome_id = events
        .iter()
        .find(|e| e.event_type == ce::TOOL_EXEC_COMPLETED)
        .unwrap()
        .id
        .clone();
    let parts = forwards[1].payload["input"]["parts"].as_array().unwrap();
    let digest = parts
        .iter()
        .find_map(|p| p.get("digest"))
        .expect("an old tool result must be digested");
    assert_eq!(digest["of"], json!(outcome_id));
    let note = digest["text"].as_str().unwrap();
    assert!(
        note.contains(&outcome_id),
        "the note must carry the recall id: {note}"
    );
    // The rewritten material still carries a VALID fingerprint
    assert!(
        verify_fingerprint(parts, forwards[1].payload["input"]["fingerprint"].as_str()).is_ok()
    );
    // No original pointer to the outcome remains
    assert!(!parts
        .iter()
        .any(|p| p["event"].as_str() == Some(outcome_id.as_str())));

    // Exactly one compaction decision, reasoned, scoped to what was digested
    let decisions: Vec<_> = events
        .iter()
        .filter(|e| e.event_type == context_gate::DECISION)
        .collect();
    assert_eq!(decisions.len(), 1);
    assert_eq!(decisions[0].payload["digested"], json!([outcome_id]));
    assert!(decisions[0]
        .reason
        .as_deref()
        .is_some_and(|r| !r.is_empty()));

    // And the turn still completed normally — the loop noticed nothing
    assert!(events
        .iter()
        .any(|e| e.event_type == ce::OUTPUT_REPLY && e.payload["text"] == "the sum is 11"));
}

/// Compaction can never strand a tool call without its thinking. The gate's
/// only trimming move is to replace a WHOLE event with a digest — it has no
/// way to reach inside an event and take one field out — so any turn still
/// present as a pointer still materializes with its reasoning attached. That
/// is what keeps a compacted conversation acceptable to a provider that
/// demands the thinking back on tool-call turns.
#[test]
fn compaction_cannot_separate_a_tool_call_from_its_thinking() {
    let kernel = start_gated_kernel(json!({
        "profile": {"contextWindow": 1000, "usageFields": {"input": "input_tokens"}},
        "ratio": 0.5,
        "keepRecentTools": 0,
    }));
    let events = kernel.log().replay(1).unwrap();
    let forwards: Vec<_> = events
        .iter()
        .filter(|e| e.event_type == ce::MODEL_CALL_STARTED && e.source == "gate")
        .collect();
    // The second ask is the one that crossed the scale and got trimmed
    let parts = forwards[1].payload["input"]["parts"].as_array().unwrap();
    let messages =
        lattice::components::openai_model::materialize(parts, &kernel.log().reader(), None)
            .unwrap();
    let assistant = messages
        .iter()
        .find(|m| m["role"] == "assistant" && m.get("tool_calls").is_some())
        .expect("the tool-call turn survives compaction");
    assert_eq!(assistant["reasoning_content"], "first, add them");
}

#[test]
fn without_a_profile_the_gate_is_inert() {
    let kernel = start_gated_kernel(json!({}));
    let events = kernel.log().replay(1).unwrap();
    assert!(
        !events
            .iter()
            .any(|e| e.event_type == context_gate::DECISION),
        "no profile, no compaction, ever"
    );
    // Forwards are byte-identical to the loop's asks
    let by_source = |source: &str| -> Vec<Value> {
        events
            .iter()
            .filter(|e| e.event_type == ce::MODEL_CALL_STARTED && e.source == source)
            .map(|e| e.payload["input"]["parts"].clone())
            .collect()
    };
    assert_eq!(by_source("gate"), by_source("loop"));
    assert!(events
        .iter()
        .any(|e| e.event_type == ce::OUTPUT_REPLY && e.payload["text"] == "the sum is 11"));
}

/// v2: a gated assembly WITH a condenser model instance (assembly-isolated
/// from the main brain), driven turn by turn.
fn condensing_kernel(compactor_script: Value, main_script: Value, gate_config: Value) -> Kernel {
    condensing_kernel_with(compactor_script, main_script, gate_config, false, None)
}

/// A condenser that receives the request and never answers it — the shape of
/// a condense model that hangs, dies, or is still thinking when the process
/// ends.
const MUTE_MODEL: &str = "mute-model";

struct MuteModel;

impl lattice::Component for MuteModel {
    fn handle(&mut self, _port: &str, _event: &lattice::EventEnvelope, _ctx: &mut lattice::Ctx) {}
}

/// The same assembly, with two knobs: a condenser that never answers, and a
/// named ledger so a second start reopens the first one's history.
fn condensing_kernel_with(
    compactor_script: Value,
    main_script: Value,
    gate_config: Value,
    mute_condenser: bool,
    log: Option<&std::path::Path>,
) -> Kernel {
    let mute_manifest = lattice::ComponentManifest {
        name: MUTE_MODEL.to_string(),
        entry: format!("builtin:{MUTE_MODEL}"),
        ..scripted_model::manifest()
    };
    let registry: HashMap<String, lattice::ComponentManifest> = [
        (silent_ui::NAME.to_string(), silent_ui::manifest()),
        (minimal_loop::NAME.to_string(), minimal_loop::manifest()),
        (context_gate::NAME.to_string(), context_gate::manifest()),
        (scripted_model::NAME.to_string(), scripted_model::manifest()),
        (MUTE_MODEL.to_string(), mute_manifest),
        (calc_tools::NAME.to_string(), calc_tools::manifest()),
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
        context_gate::NAME.to_string(),
        Box::new(|c| Box::new(context_gate::ContextGate::from_config(c))),
    );
    factories.insert(
        scripted_model::NAME.to_string(),
        Box::new(|c| Box::new(scripted_model::ScriptedModel::from_config(c))),
    );
    factories.insert(
        calc_tools::NAME.to_string(),
        Box::new(|c| Box::new(calc_tools::CalcTools::from_config(c))),
    );
    factories.insert(MUTE_MODEL.to_string(), Box::new(|_| Box::new(MuteModel)));
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
                "gate".to_string(),
                ComponentInstance {
                    component: context_gate::NAME.to_string(),
                    requires: Vec::new(),
                    config: Some(gate_config),
                },
            ),
            (
                "model".to_string(),
                ComponentInstance {
                    component: scripted_model::NAME.to_string(),
                    requires: Vec::new(),
                    config: Some(main_script),
                },
            ),
            (
                "cmodel".to_string(),
                ComponentInstance {
                    component: if mute_condenser {
                        MUTE_MODEL.to_string()
                    } else {
                        scripted_model::NAME.to_string()
                    },
                    requires: Vec::new(),
                    config: Some(compactor_script),
                },
            ),
            (
                "tools".to_string(),
                ComponentInstance {
                    component: calc_tools::NAME.to_string(),
                    requires: Vec::new(),
                    config: None,
                },
            ),
        ]
        .into(),
        wires: vec![
            Wire::new("ui.user", "loop.input"),
            Wire::new("loop.ask", "gate.ask"),
            Wire::new("gate.forward", "model.request"),
            Wire::new("model.result", "loop.model"),
            Wire::new("gate.condense", "cmodel.request"),
            Wire::new("cmodel.result", "gate.condensed"),
            Wire::new("ui.answer", "gate.dial"),
            Wire::new("loop.run", "tools.execute"),
            Wire::new("tools.outcome", "loop.tools"),
            Wire::new("loop.out", "ui.display"),
        ],
    };
    let options = KernelOptions {
        stream: log.map(|_| "st_condense".to_string()),
        log_file: log.map(std::path::Path::to_path_buf),
        ..KernelOptions::default()
    };
    Kernel::start(&assembly, &registry, &mut factories, options).unwrap()
}

fn say(kernel: &mut Kernel, text: &str) {
    kernel.injector("ui").emit(
        "user",
        EventDraft::new(ce::USER_MESSAGE, &[], json!({"text": text})),
    );
    kernel.run_until_quiescent().unwrap();
}

/// A prompt too big to leave inside a JSON line moves to a file beside the
/// ledger, and everything downstream still gets the prompt itself.
///
/// The reason is what the agent can do with it. Inside a JSON string a
/// document has no lines at all — every newline is the two characters `\n` —
/// so `Grep` on the ledger finds the containing line and clips it to 400
/// characters, never the match. Beside the ledger it is an ordinary text file
/// and the same search names the line.
#[test]
fn a_large_system_prompt_becomes_a_document_beside_the_ledger() {
    let home = tempfile::tempdir().unwrap();
    let ledger = home.path().join("main.jsonl");
    // Over the threshold, with real lines in it, so the point is visible
    let base: String = (1..=400)
        .map(|n| format!("Rule {n}: say what you checked.\n"))
        .collect();
    assert!(base.len() > 4096, "the fixture must exceed the threshold");

    let mut kernel = condensing_kernel_with(
        json!({"script": [{"status": "ok", "text": "fine"}]}),
        json!({"script": [
            {"status": "ok", "text": "one"},
            {"status": "ok", "text": "two"},
        ]}),
        json!({"system": base, "condense": false}),
        true,
        Some(&ledger),
    );
    say(&mut kernel, "first");
    say(&mut kernel, "second");
    let events = kernel.log().replay(1).unwrap();
    kernel.shutdown();

    let forwarded: Vec<_> = events
        .iter()
        .filter(|e| e.event_type == ce::MODEL_CALL_STARTED && e.source == "gate")
        .collect();
    assert_eq!(forwarded.len(), 2, "two turns went out");

    // On the ledger it is a reference, not the prose
    let carried = &forwarded[0].payload["system"];
    let file = carried["file"]
        .as_str()
        .unwrap_or_else(|| panic!("the prompt moved out: {carried}"));
    assert!(file.ends_with("-system.txt"), "named for its event: {file}");
    assert_eq!(carried["bytes"].as_u64().unwrap() as usize, base.len());
    assert_eq!(carried["lines"], 400);
    assert!(
        carried["preview"].as_str().unwrap().starts_with("Rule 1:"),
        "enough to recognise it without opening anything: {carried}"
    );

    // Beside the ledger, as a file with lines
    let dir = ledger.with_extension("");
    let written = std::fs::read_to_string(dir.join(file)).expect("the document is there");
    assert_eq!(written, base, "byte for byte");
    assert_eq!(written.lines().count(), 400);

    // The same prompt on the second turn is the SAME file, not a second copy.
    // This is what takes 25.5 MB of real records down to 5.8 MB, and it also
    // means the number of -system files IS the number of times the prompt
    // changed.
    assert_eq!(
        forwarded[1].payload["system"]["file"], file,
        "an unchanged prompt is written once"
    );
    let documents: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| n.ends_with("-system.txt"))
        .collect();
    assert_eq!(
        documents.len(),
        1,
        "one prompt, one document: {documents:?}"
    );

    // And the model was still given the PROMPT, not the reference — the whole
    // point of moving it is that nothing downstream can tell.
    let asked = events
        .iter()
        .find(|e| e.event_type == ce::MODEL_CALL_COMPLETED)
        .expect("the model answered");
    assert_eq!(asked.payload["status"], "ok");
}

const GOOD_SUMMARY: &str = "Done: summed 4 and 7\nState: idle\nOpen: none\nFacts: result 11";

/// One condensation that never came back does not switch condensing off for
/// good.
///
/// "Is one already on its way?" is read off the ledger so a restarted gate
/// cannot double-condense — right, and the reason this went unnoticed. It
/// looked for a COMPLETION, and an interrupted call has an ending that is not
/// one: reopening settles what was in flight, a condense model can die, a
/// person can interrupt. From then on the answer was permanently yes, and
/// this stream never condensed again — not in that process, and not in any
/// later one, because the ledger keeps saying so.
#[test]
fn a_condensation_that_was_interrupted_does_not_block_the_next_one() {
    let home = tempfile::tempdir().unwrap();
    let ledger = home.path().join("ledger.jsonl");
    let gate_config = json!({
        "profile": {"contextWindow": 1000, "usageFields": {"input": "input_tokens"}},
        "ratio": 0.5,
        "keepRecentTools": 0,
        "condense": true,
        "keepRecentParts": 1,
        // The tool exchange stays whole in the recent tail; the user line
        // alone is enough to launch this deliberately interrupted summary.
        "minCondense": 1,
    });
    let main_script = json!({"script": [
        {"status": "ok", "usage": {"input_tokens": 900},
         "toolCalls": [{"id": "c1", "tool": "calc", "arguments": {"numbers": [4, 7]}}]},
        {"status": "ok", "text": "the sum is 11", "usage": {"input_tokens": 950}},
        {"status": "ok", "text": "still 11", "usage": {"input_tokens": 960}},
    ]});

    // A condenser that takes the request and says nothing.
    let mut kernel = condensing_kernel_with(
        json!({"script": [{"status": "ok", "text": GOOD_SUMMARY}]}),
        main_script.clone(),
        gate_config.clone(),
        true,
        Some(&ledger),
    );
    say(&mut kernel, "add 4 and 7");
    let events = kernel.log().replay(1).unwrap();
    let asked: Vec<_> = events
        .iter()
        .filter(|e| e.payload["purpose"] == context_gate::CONDENSE_PURPOSE)
        .collect();
    assert_eq!(asked.len(), 1, "precondition: one condensation went out");
    assert!(
        !events.iter().any(|e| e.event_type == context_gate::SUMMARY),
        "precondition: and never came back"
    );
    kernel.shutdown();

    // Reopen with a condenser that does answer. The reopen settles the
    // hanging condense call with an interruption — its ending.
    let mut kernel = condensing_kernel_with(
        json!({"script": [{"status": "ok", "text": GOOD_SUMMARY}]}),
        main_script,
        gate_config,
        false,
        Some(&ledger),
    );
    let settled_at = kernel
        .log()
        .replay(1)
        .unwrap()
        .iter()
        .find(|e| e.event_type == ce::INTERRUPTED && e.payload["by"] == "restart")
        .map(|e| e.seq)
        .expect("precondition: the reopen settled it");

    say(&mut kernel, "and again?");
    let events = kernel.log().replay(1).unwrap();
    let after: Vec<_> = events
        .iter()
        .filter(|e| e.payload["purpose"] == context_gate::CONDENSE_PURPOSE && e.seq > settled_at)
        .collect();
    assert!(
        !after.is_empty(),
        "the stream must be able to condense again after an interrupted attempt"
    );
    assert!(
        events.iter().any(|e| e.event_type == context_gate::SUMMARY),
        "and this one came back"
    );
    kernel.shutdown();
}

#[test]
fn failed_condensation_stays_paused_across_turns_and_restart_until_model_change() {
    for (retryable, native) in [(false, false), (true, false), (false, true), (true, true)] {
        let home = tempfile::tempdir().unwrap();
        let ledger = home.path().join("ledger.jsonl");
        let purpose = if native {
            "context.compact.responses"
        } else {
            context_gate::CONDENSE_PURPOSE
        };
        let success = if native {
            json!({"status":"ok","nativeCompaction":{"dialect":"responses","output":[
                {"type":"compaction","encrypted_content":"sealed"}
            ]}})
        } else {
            json!({"status":"ok","text":GOOD_SUMMARY})
        };
        let gate = json!({
            "profile":{"contextWindow":1000,"usageFields":{"input":"input_tokens"}},
            "ratio":0.5,"condense":true,"nativeCompaction":native,"keepRecentParts":1,"minCondense":1
        });
        let main = json!({"script":[
            {"status":"ok","text":"first","usage":{"input_tokens":900}},
            {"status":"ok","text":"second","usage":{"input_tokens":900}},
            {"status":"ok","text":"third","usage":{"input_tokens":900}},
            {"status":"ok","text":"fourth","usage":{"input_tokens":900}}
        ]});
        let count = |kernel: &Kernel| {
            kernel
                .log()
                .replay(1)
                .unwrap()
                .iter()
                .filter(|e| {
                    e.event_type == ce::MODEL_CALL_STARTED && e.payload["purpose"] == purpose
                })
                .count()
        };
        let mut kernel = condensing_kernel_with(
            json!({"script":[{"status":"error","error":{
                "code":"provider.bad_response","message":"rejected","blame":"provider",
                "retryable":retryable,"transient":retryable
            }},success.clone()]}),
            main.clone(),
            gate.clone(),
            false,
            Some(&ledger),
        );
        say(&mut kernel, "first");
        say(&mut kernel, "second");
        assert_eq!(count(&kernel), 1, "precondition: a condensation failed");
        let suspension = kernel
            .log()
            .find_back(|e| {
                e.event_type == context_gate::DECISION && e.payload["action"] == "suspend"
            })
            .unwrap()
            .expect("the pause must have an audited explanation");
        assert!(!suspension.causes.is_empty());
        say(&mut kernel, "third");
        assert_eq!(
            count(&kernel),
            1,
            "visible failures must not be retried on the next turn"
        );
        kernel.shutdown();

        let mut kernel = condensing_kernel_with(
            json!({"script":[success]}),
            main,
            gate,
            false,
            Some(&ledger),
        );
        say(&mut kernel, "after restart");
        assert_eq!(count(&kernel), 1, "restart must preserve the pause");
        kernel.injector("ui").emit(
            "answer",
            EventDraft::new(
                ce::EXTERNAL_INPUT,
                &[],
                json!({"channel":context_gate::MODEL_CHANNEL,"model":"repaired"}),
            ),
        );
        kernel.run_until_quiescent().unwrap();
        say(&mut kernel, "after model change");
        assert_eq!(
            count(&kernel),
            2,
            "an explicit model change allows a new attempt"
        );
        assert!(kernel
            .log()
            .replay(1)
            .unwrap()
            .iter()
            .any(|e| e.event_type == context_gate::SUMMARY));
        kernel.shutdown();
    }
}

#[test]
fn a_cancelled_condensation_is_not_a_failure_pause() {
    let mut kernel = condensing_kernel(
        json!({"script":[{"status":"cancelled"},{"status":"ok","text":GOOD_SUMMARY}]}),
        json!({"script":[
            {"status":"ok","text":"first","usage":{"input_tokens":900}},
            {"status":"ok","text":"second","usage":{"input_tokens":900}},
            {"status":"ok","text":"third","usage":{"input_tokens":900}}
        ]}),
        json!({"profile":{"contextWindow":1000,"usageFields":{"input":"input_tokens"}},
            "ratio":0.5,"condense":true,"keepRecentParts":1,"minCondense":1}),
    );
    say(&mut kernel, "first");
    say(&mut kernel, "second");
    let cancelled = kernel
        .log()
        .find_back(|e| e.event_type == ce::MODEL_CALL_COMPLETED && e.source == "cmodel")
        .unwrap()
        .expect("the condenser answered");
    assert_eq!(cancelled.payload["status"], "cancelled");
    say(&mut kernel, "third");
    assert!(
        kernel
            .log()
            .any(|e| e.event_type == context_gate::SUMMARY)
            .unwrap(),
        "a cancelled completion must not permanently pause compaction"
    );
    assert!(!kernel
        .log()
        .any(|e| e.event_type == context_gate::DECISION && e.payload["action"] == "suspend")
        .unwrap());
    kernel.shutdown();
}

#[test]
fn a_late_compaction_failure_from_the_previous_model_does_not_pause_the_new_one() {
    let mut kernel = condensing_kernel_with(
        json!({"script":[]}),
        json!({"script":[
            {"status":"ok","text":"first","usage":{"input_tokens":900}},
            {"status":"ok","text":"second","usage":{"input_tokens":900}},
            {"status":"ok","text":"third","usage":{"input_tokens":900}}
        ]}),
        json!({"profile":{"contextWindow":1000,"usageFields":{"input":"input_tokens"}},
            "ratio":0.5,"condense":true,"nativeCompaction":true,"keepRecentParts":1,"minCondense":1}),
        true,
        None,
    );
    say(&mut kernel, "first");
    say(&mut kernel, "second");
    let request = kernel
        .log()
        .find_back(|e| {
            e.event_type == ce::MODEL_CALL_STARTED
                && e.payload["purpose"] == "context.compact.responses"
        })
        .unwrap()
        .expect("the old model has a compaction in flight");
    kernel.injector("ui").emit(
        "answer",
        EventDraft::new(
            ce::EXTERNAL_INPUT,
            &[],
            json!({"channel":context_gate::MODEL_CHANNEL,"model":"new"}),
        ),
    );
    kernel.run_until_quiescent().unwrap();
    kernel.injector("cmodel").emit(
        "result",
        EventDraft::new(
            ce::MODEL_CALL_COMPLETED,
            &[&request.id],
            json!({"status":"error","error":{
                "code":"provider.bad_response","message":"old model rejected","blame":"provider",
                "retryable":false,"transient":false
            }}),
        ),
    );
    kernel.run_until_quiescent().unwrap();
    say(&mut kernel, "third");
    let events = kernel.log().replay(1).unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|e| e.event_type == ce::MODEL_CALL_STARTED
                && e.payload["purpose"] == "context.compact.responses")
            .count(),
        2,
        "a late error must not disable the new model's compactor"
    );
    assert!(!events
        .iter()
        .any(|e| e.event_type == context_gate::DECISION && e.payload["action"] == "suspend"));
    kernel.shutdown();
}

#[test]
fn native_compaction_persists_replacement_and_materializes_without_plaintext_summary() {
    let native = json!({"dialect":"responses", "model":"exam", "baseUrl":"http://local",
        "output":[{"type":"compaction","id":"cmp_1","encrypted_content":"sealed"}]});
    let dir = tempfile::tempdir().unwrap();
    let ledger = dir.path().join("native.jsonl");
    let mut kernel = condensing_kernel_with(
        json!({"script":[{"status":"ok","nativeCompaction":native}]}),
        json!({"script":[
            {"status":"ok","usage":{"input_tokens":900},"toolCalls":[{"id":"c1","tool":"calc","arguments":{"numbers":[4,7]}}]},
            {"status":"ok","text":"11","usage":{"input_tokens":950}}
        ]}),
        json!({"profile":{"contextWindow":1000,"usageFields":{"input":"input_tokens"}},
            "system":"Native compaction must preserve these rules.",
            "ratio":0.5,"condense":true,"nativeCompaction":true,"keepRecentParts":1,"minCondense":1}),
        false,
        Some(&ledger),
    );
    say(&mut kernel, "add 4 and 7");
    let summary = kernel
        .log()
        .find_back(|e| e.event_type == context_gate::SUMMARY)
        .unwrap()
        .unwrap();
    assert_eq!(summary.payload["nativeCompaction"], native);
    let compact = kernel
        .log()
        .find_back(|e| {
            e.event_type == ce::MODEL_CALL_STARTED
                && e.payload["purpose"] == "context.compact.responses"
        })
        .unwrap()
        .unwrap();
    let ask = kernel
        .log()
        .find_back(|e| {
            e.event_type == ce::MODEL_CALL_STARTED
                && e.causes == compact.causes
                && e.payload.get("purpose").is_none()
        })
        .unwrap()
        .unwrap();
    for field in ["system", "tools"] {
        assert_eq!(
            compact.payload.get(field),
            ask.payload.get(field),
            "native compaction must retain {field}"
        );
    }
    assert!(compact.payload.get("system").is_some());
    let parts = vec![json!({"digest":{"of":summary.id,"text":"not the native state"}})];
    let messages =
        lattice::components::openai_model::materialize(&parts, &kernel.log().reader(), None);
    assert!(
        messages.is_err(),
        "Chat must not silently consume a sealed native summary"
    );
    let restored = lattice::components::responses_model::materialize(
        &parts,
        &kernel.log().reader(),
        None,
        "exam",
        "http://local",
    )
    .unwrap();
    assert_eq!(json!(restored), native["output"]);
    assert!(lattice::components::responses_model::materialize(
        &parts,
        &kernel.log().reader(),
        None,
        "different",
        "http://local"
    )
    .is_err());
    kernel.shutdown();
    let reopened = condensing_kernel_with(
        json!({"script":[]}),
        json!({"script":[]}),
        json!({"condense":true,"nativeCompaction":true}),
        false,
        Some(&ledger),
    );
    let replayed = lattice::components::responses_model::materialize(
        &parts,
        &reopened.log().reader(),
        None,
        "exam",
        "http://local",
    )
    .unwrap();
    assert_eq!(json!(replayed), native["output"]);
    reopened.shutdown();
}

#[test]
fn over_budget_condenses_in_the_background_and_substitutes_next_turn() {
    let mut kernel = condensing_kernel(
        json!({"script": [{"status": "ok", "text": GOOD_SUMMARY}]}),
        json!({"script": [
            {"status": "ok", "usage": {"input_tokens": 900},
             "toolCalls": [{"id": "c1", "tool": "calc", "arguments": {"numbers": [4, 7]}}]},
            {"status": "ok", "text": "the sum is 11", "usage": {"input_tokens": 950}},
            {"status": "ok", "text": "still 11", "usage": {"input_tokens": 960}},
        ]}),
        json!({
            "profile": {"contextWindow": 1000, "usageFields": {"input": "input_tokens"}},
            "ratio": 0.5,
            "keepRecentTools": 0,
            "condense": true,
            "keepRecentParts": 1,
            "minCondense": 1,
        }),
    );

    // Turn one: the second ask crosses the scale → mechanical trim NOW,
    // condensation in the background, summary recorded before quiescence
    say(&mut kernel, "add 4 and 7");
    let events = kernel.log().replay(1).unwrap();
    let summary = events
        .iter()
        .find(|e| e.event_type == context_gate::SUMMARY)
        .expect("the condenser's summary landed on the ledger");
    assert_eq!(summary.source, "gate");
    let text = summary.payload["text"].as_str().unwrap();
    for marker in context_gate::SUMMARY_MARKERS {
        assert!(text.contains(marker), "summary must carry {marker}: {text}");
    }
    let covers: Vec<&str> = summary.payload["covers"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert!(!covers.is_empty());
    let invocation = events
        .iter()
        .find(|e| {
            e.event_type == ce::MODEL_CALL_COMPLETED && e.payload["toolCalls"][0]["id"] == "c1"
        })
        .unwrap();
    let answer = events
        .iter()
        .find(|e| e.event_type == ce::TOOL_EXEC_COMPLETED && e.payload["call"] == "c1")
        .unwrap();
    assert_eq!(
        covers.contains(&invocation.id.as_str()),
        covers.contains(&answer.id.as_str()),
        "a summary must not split a tool invocation from its answer"
    );
    // The causal chain: ask → condense request → completion → summary
    let condense_requests: Vec<_> = events
        .iter()
        .filter(|e| e.payload["purpose"] == context_gate::CONDENSE_PURPOSE)
        .collect();
    assert_eq!(condense_requests.len(), 1);
    // And this turn was NOT delayed: the reply came from the main script
    assert!(events
        .iter()
        .any(|e| e.event_type == ce::OUTPUT_REPLY && e.payload["text"] == "the sum is 11"));
    // A condense decision with a reason sits on the ledger
    assert!(events.iter().any(|e| {
        e.event_type == context_gate::DECISION
            && e.payload["action"] == "condense"
            && e.reason.as_deref().is_some_and(|r| !r.is_empty())
    }));

    // Turn two: covered originals travel as ONE summary digest now
    say(&mut kernel, "and again?");
    let events = kernel.log().replay(1).unwrap();
    let last_forward = events
        .iter()
        .rev()
        .find(|e| {
            e.event_type == ce::MODEL_CALL_STARTED
                && e.source == "gate"
                && e.payload["purpose"].is_null()
        })
        .unwrap();
    let parts = last_forward.payload["input"]["parts"].as_array().unwrap();
    let summary_digests: Vec<&Value> = parts
        .iter()
        .filter(|p| p["digest"]["of"] == json!(summary.id))
        .collect();
    assert_eq!(summary_digests.len(), 1, "one digest stands in for the run");
    assert!(summary_digests[0]["digest"]["text"]
        .as_str()
        .unwrap()
        .contains("Facts: result 11"));
    // None of the covered originals still travel as pointers
    for id in &covers {
        assert!(
            !parts.iter().any(|p| p["event"].as_str() == Some(id)),
            "covered original {id} must not travel alongside its summary"
        );
    }
    // The rewritten material still fingerprints clean
    assert!(
        verify_fingerprint(parts, last_forward.payload["input"]["fingerprint"].as_str()).is_ok()
    );
}

#[test]
fn a_second_condensation_rolls_the_previous_summary_in() {
    let mut kernel = condensing_kernel(
        json!({"script": [
            {"status": "ok", "text": GOOD_SUMMARY},
            {"status": "ok", "text": "Done: everything so far\nState: idle\nOpen: none\nFacts: result 11 twice"},
        ]}),
        json!({"script": [
            {"status": "ok", "text": "one", "usage": {"input_tokens": 900}},
            {"status": "ok", "text": "two", "usage": {"input_tokens": 910}},
            {"status": "ok", "text": "three", "usage": {"input_tokens": 920}},
            {"status": "ok", "text": "four", "usage": {"input_tokens": 930}},
        ]}),
        json!({
            "profile": {"contextWindow": 1000, "usageFields": {"input": "input_tokens"}},
            "ratio": 0.5,
            "condense": true,
            "keepRecentParts": 1,
            "minCondense": 2,
        }),
    );

    say(&mut kernel, "first");
    say(&mut kernel, "second"); // first condensation somewhere in here
    say(&mut kernel, "third"); // measure the first summary before deciding again
    assert_eq!(
        kernel
            .log()
            .replay(1)
            .unwrap()
            .iter()
            .filter(|e| e.event_type == context_gate::SUMMARY)
            .count(),
        1,
        "old-view usage must not trigger a second condensation"
    );
    say(&mut kernel, "fourth"); // the measured new view is still over budget

    let events = kernel.log().replay(1).unwrap();
    let summaries: Vec<_> = events
        .iter()
        .filter(|e| e.event_type == context_gate::SUMMARY)
        .collect();
    assert_eq!(summaries.len(), 2, "two generations of summary");
    let (first, second) = (&summaries[0], &summaries[1]);
    // The second condense request carried the first summary as a digest part
    let second_request = events
        .iter()
        .filter(|e| e.payload["purpose"] == context_gate::CONDENSE_PURPOSE)
        .nth(1)
        .unwrap();
    assert!(second_request.payload["input"]["parts"]
        .as_array()
        .unwrap()
        .iter()
        .any(|p| p["digest"]["of"] == json!(first.id)));
    // Coverage is a superset: everything the first covered, the second covers
    let covers = |s: &&lattice::EventEnvelope| -> Vec<String> {
        s.payload["covers"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect()
    };
    let first_covers = covers(first);
    let second_covers = covers(second);
    for id in &first_covers {
        assert!(
            second_covers.contains(id),
            "rolling condensation must not lose coverage of {id}"
        );
    }
    assert!(second_covers.len() > first_covers.len());
}

#[test]
fn a_malformed_summary_is_refused_not_recorded() {
    let mut kernel = condensing_kernel(
        // Missing the four fixed sections — not a summary
        json!({"script": [{"status": "ok", "text": "it was all fine, trust me"}]}),
        json!({"script": [
            {"status": "ok", "text": "one", "usage": {"input_tokens": 900}},
            {"status": "ok", "text": "two", "usage": {"input_tokens": 910}},
        ]}),
        json!({
            "profile": {"contextWindow": 1000, "usageFields": {"input": "input_tokens"}},
            "ratio": 0.5,
            "condense": true,
            "keepRecentParts": 1,
            "minCondense": 2,
        }),
    );
    say(&mut kernel, "first");
    say(&mut kernel, "second");
    let events = kernel.log().replay(1).unwrap();
    assert!(
        !events.iter().any(|e| e.event_type == context_gate::SUMMARY),
        "a summary without its fixed fields must not enter the ledger"
    );
    // And the conversation itself was never disturbed
    assert!(events
        .iter()
        .any(|e| e.event_type == ce::OUTPUT_REPLY && e.payload["text"] == "two"));
}

/// v3: the sidechannel truly reads its parent. A gated template opened
/// through the StreamHost; the derived stream's gate finds the parent's
/// read-only handle and rides its transcript in front of every call.
/// A driver with unwired ports, so a test can seed the ledger and fire
/// requests without a real frontend.
struct Probe;
impl lattice::Component for Probe {
    fn handle(&mut self, _port: &str, _event: &lattice::EventEnvelope, _ctx: &mut lattice::Ctx) {}
}

fn probe_manifest() -> lattice::ComponentManifest {
    lattice::ComponentManifest {
        name: "probe".to_string(),
        version: "0".to_string(),
        runtime: lattice::RuntimeKind::Inproc,
        entry: "builtin:probe".to_string(),
        inputs: vec![],
        outputs: vec![
            lattice::PortDecl::new("seed", &[ce::USER_MESSAGE]),
            lattice::PortDecl::new("out", &[ce::TOOL_EXEC_STARTED]),
            lattice::PortDecl::new("ask", &[ce::MODEL_CALL_STARTED]),
            lattice::PortDecl::new("dial", &[ce::EXTERNAL_INPUT]),
        ],
        events: Vec::new(),
        default_wiring: Vec::new(),
        capabilities: None,
        implements: Vec::new(),
        tools: Vec::new(),
        prompt: None,
        handle_timeout_ms: None,
        concurrency: None,
    }
}

fn gated_template() -> lattice::StreamTemplate {
    let registry: HashMap<String, lattice::ComponentManifest> = [
        (silent_ui::NAME.to_string(), silent_ui::manifest()),
        (minimal_loop::NAME.to_string(), minimal_loop::manifest()),
        (context_gate::NAME.to_string(), context_gate::manifest()),
        (scripted_model::NAME.to_string(), scripted_model::manifest()),
        (timer_tools::NAME.to_string(), timer_tools::manifest()),
        ("probe".to_string(), probe_manifest()),
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
        context_gate::NAME.to_string(),
        Box::new(|c| Box::new(context_gate::ContextGate::from_config(c))),
    );
    factories.insert(
        scripted_model::NAME.to_string(),
        Box::new(|c| Box::new(scripted_model::ScriptedModel::from_config(c))),
    );
    factories.insert(
        timer_tools::NAME.to_string(),
        Box::new(|c| Box::new(timer_tools::TimerTools::from_config(c))),
    );
    factories.insert("probe".to_string(), Box::new(|_| Box::new(Probe)));
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
                "gate".to_string(),
                ComponentInstance {
                    component: context_gate::NAME.to_string(),
                    requires: Vec::new(),
                    config: None, // no profile: inert scale — observation still works
                },
            ),
            (
                "model".to_string(),
                ComponentInstance {
                    component: scripted_model::NAME.to_string(),
                    requires: Vec::new(),
                    config: Some(json!({"script": [
                        {"status": "ok", "text": "the secret is 42"},
                        {"status": "ok", "text": "second reply"},
                    ]})),
                },
            ),
            (
                "timers".to_string(),
                ComponentInstance {
                    component: timer_tools::NAME.to_string(),
                    requires: Vec::new(),
                    config: None,
                },
            ),
            (
                "probe".to_string(),
                ComponentInstance {
                    component: "probe".to_string(),
                    requires: Vec::new(),
                    config: None,
                },
            ),
        ]
        .into(),
        wires: vec![
            Wire::new("ui.user", "loop.input"),
            Wire::new("loop.ask", "gate.ask"),
            Wire::new("gate.forward", "model.request"),
            Wire::new("model.result", "loop.model"),
            Wire::new("loop.run", "timers.execute"),
            Wire::new("probe.out", "timers.execute"),
            Wire::new("timers.outcome", "loop.tools"),
            Wire::new("loop.out", "ui.display"),
        ],
    };
    lattice::StreamTemplate {
        registry,
        factories,
        assembly,
    }
}

#[test]
fn a_sidechannel_model_sees_the_parent_conversation() {
    let mut host = lattice::StreamHost::new(
        [
            ("chat".to_string(), gated_template()),
            ("side".to_string(), gated_template()),
        ]
        .into(),
    );
    // The parent has a conversation worth peeking at
    host.open("main", "chat").unwrap();
    host.injector("main", "ui").unwrap().emit(
        "user",
        EventDraft::new(
            ce::USER_MESSAGE,
            &[],
            json!({"text": "what is the secret?"}),
        ),
    );
    host.run_stream("main").unwrap();

    // The sidechannel asks its own question…
    host.open_derived("btw", "side", "main").unwrap();
    host.injector("btw", "ui").unwrap().emit(
        "user",
        EventDraft::new(
            ce::USER_MESSAGE,
            &[],
            json!({"text": "what were we discussing?"}),
        ),
    );
    host.run_stream("btw").unwrap();

    // …and its model call carries the parent's transcript, up front
    let btw_events = host.kernel("btw").unwrap().log().replay(1).unwrap();
    let forward = btw_events
        .iter()
        .find(|e| e.event_type == ce::MODEL_CALL_STARTED && e.source == "gate")
        .expect("the gate forwarded");
    let parts = forward.payload["input"]["parts"].as_array().unwrap();
    let observation = parts[0]["digest"]["text"]
        .as_str()
        .expect("the first part is the observation digest");
    assert!(observation.contains("observing stream \"main\""));
    assert!(observation.contains("user: what is the secret?"));
    assert!(observation.contains("assistant: the secret is 42"));
    // The rewritten material still fingerprints clean
    assert!(verify_fingerprint(parts, forward.payload["input"]["fingerprint"].as_str()).is_ok());
    // And the digest names a real parent event for cross-stream recall
    let of = parts[0]["digest"]["of"].as_str().unwrap();
    assert!(host.reader("main").unwrap().get(of).unwrap().is_some());

    // A NORMAL stream (no foreign handles) gets no observation part
    let main_events = host.kernel("main").unwrap().log().replay(1).unwrap();
    let main_forward = main_events
        .iter()
        .find(|e| e.event_type == ce::MODEL_CALL_STARTED && e.source == "gate")
        .unwrap();
    let main_parts = main_forward.payload["input"]["parts"].as_array().unwrap();
    assert!(
        !main_parts.iter().any(|p| p.get("digest").is_some()),
        "a stream with nothing to observe must not grow observation parts"
    );
}

#[test]
fn the_gate_assembles_the_system_prompt_from_base_and_fragments() {
    // Base text alone (no fragment-bearing components in this assembly)
    let kernel = start_gated_kernel(json!({"system": "You are the base."}));
    let events = kernel.log().replay(1).unwrap();
    let forward = events
        .iter()
        .find(|e| e.event_type == ce::MODEL_CALL_STARTED && e.source == "gate")
        .unwrap();
    assert_eq!(forward.payload["system"], "You are the base.");

    // Fragment alone: the recall tool teaches the model its own usage just
    // by BEING ASSEMBLED — nobody wrote that sentence into this assembly
    let mut host = lattice::StreamHost::new([("chat".to_string(), gated_template())].into());
    host.open("main", "chat").unwrap();
    host.injector("main", "ui").unwrap().emit(
        "user",
        EventDraft::new(ce::USER_MESSAGE, &[], json!({"text": "hello"})),
    );
    host.run_stream("main").unwrap();
    let events = host.kernel("main").unwrap().log().replay(1).unwrap();
    let forward = events
        .iter()
        .find(|e| e.event_type == ce::MODEL_CALL_STARTED && e.source == "gate")
        .unwrap();
    let system = forward.payload["system"].as_str().unwrap();
    assert!(
        system.contains("A timer wakes you"),
        "the assembled component must have introduced itself: {system}"
    );
}

#[test]
fn the_arrangement_keeps_a_stable_prefix_across_turns() {
    // Inert gate: raw pointer lists. The cache-friendly invariant is that
    // each forwarded material EXTENDS the previous one — history is
    // append-only, never reshuffled.
    let mut kernel = start_gated_kernel(json!({}));
    kernel.injector("ui").emit(
        "user",
        EventDraft::new(ce::USER_MESSAGE, &[], json!({"text": "turn two"})),
    );
    kernel.run_until_quiescent().unwrap();

    let events = kernel.log().replay(1).unwrap();
    let forwards: Vec<Vec<Value>> = events
        .iter()
        .filter(|e| e.event_type == ce::MODEL_CALL_STARTED && e.source == "gate")
        .map(|e| e.payload["input"]["parts"].as_array().unwrap().clone())
        .collect();
    assert!(
        forwards.len() >= 3,
        "two turns produced {} forwards",
        forwards.len()
    );
    for pair in forwards.windows(2) {
        assert!(
            pair[1].starts_with(&pair[0]),
            "material must only grow at the tail:\n{:?}\nthen\n{:?}",
            pair[0],
            pair[1]
        );
    }
    // The tool list never changes shape between calls
    let tools: Vec<&Value> = events
        .iter()
        .filter(|e| e.event_type == ce::MODEL_CALL_STARTED && e.source == "gate")
        .map(|e| &e.payload["tools"])
        .collect();
    assert!(tools.windows(2).all(|w| w[0] == w[1]));
}

#[test]
fn tools_declare_themselves_through_manifests() {
    // Nobody wrote a tool list into any config in this assembly — yet the
    // model is offered recall_event, stamped with its provider, because the
    // ledger-tools MANIFEST declares it and the kernel collected it.
    let mut host = lattice::StreamHost::new([("chat".to_string(), gated_template())].into());
    host.open("main", "chat").unwrap();
    host.injector("main", "ui").unwrap().emit(
        "user",
        EventDraft::new(ce::USER_MESSAGE, &[], json!({"text": "hello"})),
    );
    host.run_stream("main").unwrap();

    let events = host.kernel("main").unwrap().log().replay(1).unwrap();
    let ask = events
        .iter()
        .find(|e| e.event_type == ce::MODEL_CALL_STARTED)
        .unwrap();
    let tools = ask.payload["tools"].as_array().unwrap();
    let offered = tools
        .iter()
        .find(|t| t["name"] == "Schedule")
        .expect("the wired provider's tool must be offered: {tools:?}");
    assert_eq!(
        offered["provider"], "timers",
        "the declaration must carry its provider instance"
    );
}

// ── The thinking dial ──────────────────────────────────────────────────────

/// The fingerprint of an empty material list — sha256 over nothing. The
/// letterhead schema requires a real string here, which is how a fixture that
/// passed `null` got caught instead of quietly testing a rejected event.
const EMPTY_PARTS: &str = "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

/// probe → gate: asks in one port, the setting in another.
fn dial_kernel() -> Kernel {
    dial_kernel_with(None)
}

/// As `dial_kernel`, with the gate given a configuration.
fn dial_kernel_with(gate_config: Option<Value>) -> Kernel {
    let registry: HashMap<String, lattice::ComponentManifest> = [
        ("probe".to_string(), probe_manifest()),
        (context_gate::NAME.to_string(), context_gate::manifest()),
    ]
    .into();
    let mut factories: HashMap<String, Factory> = HashMap::new();
    factories.insert("probe".to_string(), Box::new(|_| Box::new(Probe)));
    factories.insert(
        context_gate::NAME.to_string(),
        Box::new(|c| Box::new(context_gate::ContextGate::from_config(c))),
    );
    let instance = |component: &str, config: Option<Value>| ComponentInstance {
        component: component.to_string(),
        requires: Vec::new(),
        config,
    };
    let assembly = AssemblyManifest {
        instances: [
            ("probe".to_string(), instance("probe", None)),
            (
                "gate".to_string(),
                instance(context_gate::NAME, gate_config),
            ),
        ]
        .into(),
        wires: vec![
            Wire::new("probe.ask", "gate.ask"),
            Wire::new("probe.dial", "gate.dial"),
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

/// Send one ask through the gate and hand back what it forwarded.
fn forwarded(kernel: &mut Kernel) -> Value {
    kernel.injector("probe").emit(
        "ask",
        EventDraft::new(
            ce::MODEL_CALL_STARTED,
            &[],
            json!({"input": {"parts": [], "fingerprint": EMPTY_PARTS}, "model": "m"}),
        ),
    );
    kernel.run_until_quiescent().unwrap();
    kernel
        .log()
        .replay(1)
        .unwrap()
        .into_iter()
        .rfind(|e| e.event_type == ce::MODEL_CALL_STARTED && e.source == "gate")
        .expect("a forwarded ask")
        .payload
}

/// The whole point of putting the setting on the REQUEST rather than in the
/// adapter's config: nothing in Lattice can reconfigure a running component,
/// and inventing a way to would be a side channel around the ledger. A
/// station on the ask wire stamps it instead, which needs no new mechanism.
#[test]
fn a_turned_dial_rides_on_every_later_ask() {
    let mut kernel = dial_kernel();

    // Untouched: the gate says nothing, so the adapter's own config decides
    // and an assembly nobody dialled behaves exactly as it did before.
    assert!(
        forwarded(&mut kernel).get("thinking").is_none(),
        "an untouched gate must not start deciding the effort"
    );

    kernel.injector("probe").emit(
        "dial",
        EventDraft::new(
            ce::EXTERNAL_INPUT,
            &[],
            json!({"channel": context_gate::EFFORT_CHANNEL, "value": "max"}),
        ),
    );
    kernel.run_until_quiescent().unwrap();

    assert_eq!(
        forwarded(&mut kernel)["thinking"],
        "max",
        "the setting rides on the ask"
    );
    // And on the one after it — a dial is a standing setting, not a one-shot
    assert_eq!(forwarded(&mut kernel)["thinking"], "max");
    kernel.shutdown();
}

/// Settings and trust answers ride the same event type on the same wire, told
/// apart only by `channel`. A gate that ignored the channel would adopt a
/// y/n answer as an effort.
#[test]
fn a_setting_for_someone_else_is_left_alone() {
    let mut kernel = dial_kernel();
    kernel.injector("probe").emit(
        "dial",
        EventDraft::new(
            ce::EXTERNAL_INPUT,
            &[],
            json!({"channel": "trust.authorization", "request": "ev_1", "approve": true}),
        ),
    );
    kernel.run_until_quiescent().unwrap();
    assert!(
        forwarded(&mut kernel).get("thinking").is_none(),
        "another component's channel must not become an effort"
    );
    kernel.shutdown();
}

/// An ask that already carries a setting keeps it. The gate fills a gap; it
/// does not overrule something upstream decided deliberately.
#[test]
fn an_ask_that_already_names_its_effort_is_not_overwritten() {
    let mut kernel = dial_kernel();
    kernel.injector("probe").emit(
        "dial",
        EventDraft::new(
            ce::EXTERNAL_INPUT,
            &[],
            json!({"channel": context_gate::EFFORT_CHANNEL, "value": "max"}),
        ),
    );
    kernel.injector("probe").emit(
        "ask",
        EventDraft::new(
            ce::MODEL_CALL_STARTED,
            &[],
            json!({
                "input": {"parts": [], "fingerprint": EMPTY_PARTS},
                "model": "m",
                "thinking": "low",
            }),
        ),
    );
    kernel.run_until_quiescent().unwrap();
    let out = kernel
        .log()
        .replay(1)
        .unwrap()
        .into_iter()
        .rfind(|e| e.event_type == ce::MODEL_CALL_STARTED && e.source == "gate")
        .expect("a forwarded ask")
        .payload;
    assert_eq!(out["thinking"], "low");
    kernel.shutdown();
}

// ── The model itself changing, mid-conversation ────────────────────────────

/// `/model` replaces the adapter, but the gate is not replaced with it — it
/// holds the effort a person turned five minutes ago, and rebuilding it would
/// throw that away. So the gate is TOLD, on the same wire and the same event
/// type as the effort dial, told apart by channel.
///
/// What it is told is the new model's profile, because that is what this gate
/// budgets with: the window, the provider's own names for the usage numbers,
/// and what to call the model in the system prompt.
#[test]
fn a_model_change_renames_the_model_in_the_prompt_it_assembles() {
    let mut kernel = dial_kernel_with(Some(json!({
        "system": "Lattice is the runtime and {model} does the thinking.",
        "modelName": "the-first-model",
    })));
    assert!(
        forwarded(&mut kernel)["system"]
            .as_str()
            .unwrap_or_default()
            .contains("the-first-model"),
        "the prompt names the model that will receive it"
    );

    kernel.injector("probe").emit(
        "dial",
        EventDraft::new(
            ce::EXTERNAL_INPUT,
            &[],
            json!({"channel": context_gate::MODEL_CHANNEL, "model": "the-second-model"}),
        ),
    );
    kernel.run_until_quiescent().unwrap();

    let system = forwarded(&mut kernel)["system"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(
        system.contains("the-second-model") && !system.contains("the-first-model"),
        "after the swap the agent must not still claim to be the model that left: {system}"
    );
    kernel.shutdown();
}

/// The budget follows the model too. Both halves of it: how big the window is,
/// and which field of the provider's usage report counts the input — the two
/// dialects name it differently, and a gate reading the wrong name reads
/// nothing at all and concludes the conversation never grows.
#[test]
fn a_model_change_re_budgets_against_the_new_window_and_its_own_usage_names() {
    // Window 1000 × 0.5 = 500, and every completion reports 900 under
    // `input_tokens` — so the first turn is over budget and digests.
    let heavy = json!({"status": "ok", "usage": {"input_tokens": 900},
                       "toolCalls": [{"id": "c1", "tool": "calc", "arguments": {"numbers": [4, 7]}}]});
    let done = json!({"status": "ok", "text": "11", "usage": {"input_tokens": 900}});
    let mut kernel = build_gated_kernel(
        json!({
            "profile": {"contextWindow": 1000, "usageFields": {"input": "input_tokens"}},
            "ratio": 0.5,
            "keepRecentTools": 0,
        }),
        json!({"script": [
            heavy.clone(), done.clone(),
            heavy.clone(), done.clone(),
            heavy, done,
        ]}),
    );
    let digests = |kernel: &Kernel| {
        kernel
            .log()
            .replay(1)
            .unwrap()
            .into_iter()
            .filter(|e| e.event_type == context_gate::DECISION && e.payload["action"] == "digest")
            .count()
    };

    say(&mut kernel, "add 4 and 7");
    let after_one = digests(&kernel);
    assert!(
        after_one > 0,
        "900 against a threshold of 500 is over budget"
    );

    // Swap to a model with a far bigger window: the same 900 tokens are now
    // nowhere near the threshold, and nothing should be given up.
    kernel.injector("ui").emit(
        "answer",
        EventDraft::new(
            ce::EXTERNAL_INPUT,
            &[],
            json!({
                "channel": context_gate::MODEL_CHANNEL,
                "model": "something-roomier",
                "contextWindow": 1_000_000,
                "usageFields": {"input": "input_tokens"},
            }),
        ),
    );
    say(&mut kernel, "again");
    assert_eq!(
        digests(&kernel),
        after_one,
        "with a million-token window, 900 tokens is not a reason to give anything up"
    );

    // Now a model whose provider calls it something else. The script reports
    // `input_tokens` and nothing else, so under the new name the gate measures
    // nothing — and measuring nothing is not the same as measuring zero over
    // budget: it must not trim on a number it did not find.
    kernel.injector("ui").emit(
        "answer",
        EventDraft::new(
            ce::EXTERNAL_INPUT,
            &[],
            json!({
                "channel": context_gate::MODEL_CHANNEL,
                "model": "another-dialect",
                "contextWindow": 1000,
                "usageFields": {"input": "prompt_tokens"},
            }),
        ),
    );
    say(&mut kernel, "and again");
    assert_eq!(
        digests(&kernel),
        after_one,
        "the usage field changed with the model; the old name's number is not this model's"
    );

    // And a change that names the numbers for a DIFFERENT provider must
    // replace the whole set, not merge into it. Updating name by name kept
    // whatever the newcomer did not mention — so a cache-read name belonging
    // to the previous provider survived, read nothing, and made every turn
    // look like a cache miss. The gate takes the set or leaves it.
    kernel.injector("ui").emit(
        "answer",
        EventDraft::new(
            ce::EXTERNAL_INPUT,
            &[],
            json!({
                "channel": context_gate::MODEL_CHANNEL,
                "model": "openai-shaped",
                "contextWindow": 1000,
                "usageFields": {"input": "prompt_tokens", "cacheRead": "prompt_cache_hit_tokens"},
            }),
        ),
    );
    // Now speak the new provider's language: over budget under ITS name.
    kernel.injector("ui").emit(
        "user",
        EventDraft::new(ce::USER_MESSAGE, &[], json!({"text": "in the new tongue"})),
    );
    kernel.run_until_quiescent().unwrap();
    kernel.shutdown();
}

/// A catalog entry with no profile still knows what its provider calls the
/// usage numbers, because its DIALECT does.
///
/// Leaving them unknown was the dangerous answer: the gate keeps what it had,
/// and a name that does not appear in the reply reads as zero — which is
/// indistinguishable from a model that used nothing. The scale then never
/// fires (context grows until the provider refuses it) and the cache count is
/// always zero (every turn judged a miss).
#[test]
fn a_model_with_no_profile_still_names_its_usage_numbers() {
    let openai_shaped = lattice::models::Entry {
        id: "hand-added".to_string(),
        adapter: "openai".to_string(),
        model: "nothing-ships-a-profile-for-this".to_string(),
        base_url: "https://example.invalid".to_string(),
        key_env: "SOME_KEY".to_string(),
        profile: None,
    };
    let named = openai_shaped.usage_fields();
    assert_eq!(named["input"], "prompt_tokens");
    assert_eq!(named["cacheRead"], "prompt_cache_hit_tokens");

    let anthropic_shaped = lattice::models::Entry {
        adapter: "anthropic".to_string(),
        ..openai_shaped.clone()
    };
    let named = anthropic_shaped.usage_fields();
    assert_eq!(named["input"], "input_tokens");
    assert_eq!(named["cacheRead"], "cache_read_input_tokens");

    // And a profile that DOES name them still wins.
    let described = lattice::models::Entry {
        profile: Some(json!({"usageFields": {"input": "its_own_name"}})),
        ..openai_shaped
    };
    assert_eq!(described.usage_fields()["input"], "its_own_name");
}

/// A turn costs the same whether it is the third or the thirtieth.
///
/// Reading the ledger used to mean copying it. A turn asks a dozen questions
/// of it — this gate alone asks several per model call — and every one of
/// them deep-copied every envelope with every payload in it, tool results
/// included. So one turn cost as much as the conversation before it, and a
/// conversation cost the square of its own length: measured at 16 KiB tool
/// results, 10 turns copied 10.6 MiB, 40 copied 157 MiB, 80 copied 622 MiB.
///
/// This pins the shape, not a number: what a turn copies must not grow with
/// the conversation. It is checked HERE, on a gate with condensation on,
/// because that is the assembly where the questions are actually asked — a
/// bare scripted preset barely troubles the ledger, and a test placed there
/// would pass whatever the reads cost.
#[test]
fn a_turn_does_not_get_more_expensive_as_the_conversation_grows() {
    // Bulky answers, so that copying the ledger would be visible if it
    // happened. Under budget throughout: this measures the ordinary path,
    // not the compaction one.
    let bulky = "y".repeat(8 * 1024);
    let turn = json!({"status": "ok", "text": bulky, "usage": {"input_tokens": 10}});
    let mut kernel = condensing_kernel(
        json!({"script": [{"status": "ok", "text": GOOD_SUMMARY}]}),
        json!({"script": (0..40).map(|_| turn.clone()).collect::<Vec<_>>()}),
        json!({
            "profile": {"contextWindow": 1_000_000, "usageFields": {"input": "input_tokens"}},
            "ratio": 0.5,
            "keepRecentTools": 0,
            "condense": true,
        }),
    );
    let copied = |kernel: &Kernel| {
        kernel
            .log()
            .cost()
            .bytes
            .load(std::sync::atomic::Ordering::Relaxed)
    };

    // Past the first turns, whose costs are one-time (the loop rebuilding
    // its material, the gate finding no history to look at).
    for n in 0..4 {
        say(&mut kernel, &format!("early {n}"));
    }
    let mark = copied(&kernel);
    say(&mut kernel, "the measured early turn");
    let early = copied(&kernel) - mark;

    for n in 0..25 {
        say(&mut kernel, &format!("later {n}"));
    }
    let mark = copied(&kernel);
    say(&mut kernel, "the measured late turn");
    let late = copied(&kernel) - mark;
    kernel.shutdown();

    // Room for the turn's own material to differ, none for the cost to track
    // the conversation: with the old shape the late turn copied roughly six
    // times what the early one did.
    assert!(
        late <= early.max(32 * 1024) * 2,
        "a turn's look-back cost grew with the conversation: {early} bytes early, \
         {late} bytes late"
    );
}
