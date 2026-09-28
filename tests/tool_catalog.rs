//! Tools the model cannot see, and the way it reaches them anyway.
//!
//! The reason any of this exists is the prompt cache. The tool schema sits in
//! the cached prefix of every call, so a list that grows — and in a runtime
//! where the agent installs its own tools it grows by design — throws the
//! cache away for the whole conversation behind it each time it changes. The
//! answer the industry converged on, and the one used here, is to keep the
//! schema small and fixed, and to hand a discovered declaration back at the
//! TAIL of the conversation, where an append costs nothing.
//!
//! There is a second reason, and it is not about money: a model's ability to
//! pick the right tool falls off as the list grows.
#![cfg(unix)]

use serde_json::json;

use lattice::components::tool_catalog;
use lattice::core_events as ce;
use lattice::preset::{self, PresetConfig};
use lattice::{EventDraft, Kernel, KernelOptions};

fn scripted(script: serde_json::Value) -> PresetConfig {
    PresetConfig {
        adapter: "scripted".to_string(),
        model: "scripted".to_string(),
        base_url: String::new(),
        key_env: String::new(),
        workspace: Some(".".to_string()),
        context_window: 64000,
        usage_input_field: "input_tokens".to_string(),
        profile: None,
        catalog_problems: Vec::new(),
        system: "test".to_string(),
        scripted: Some(script),
        thinking: None,
        overlay: None,
        assembly: None,
    }
}

/// Drive one turn and hand back the whole ledger.
fn run(cfg: &PresetConfig, text: &str) -> Vec<lattice::EventEnvelope> {
    let (registry, mut factories, assembly) = preset::standard(cfg).expect("preset builds");
    let mut kernel = Kernel::start(
        &assembly,
        &registry,
        &mut factories,
        KernelOptions::default(),
    )
    .expect("the standard assembly must pass inspection");
    kernel.injector("ui").emit(
        "user",
        EventDraft::new(ce::USER_MESSAGE, &[], json!({ "text": text })),
    );
    kernel.run_until_quiescent().unwrap();
    let events = kernel.log().replay(1).unwrap();
    kernel.shutdown();
    events
}

/// The tool list actually sent to the model on the first call.
fn offered(events: &[lattice::EventEnvelope]) -> Vec<String> {
    events
        .iter()
        .find(|e| e.payload["system"].is_string() && e.payload["tools"].is_array())
        .map(|e| {
            e.payload["tools"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(|d| d["name"].as_str().map(str::to_string))
                .collect()
        })
        .expect("a call was made with a tool list")
}

#[test]
fn the_model_is_offered_the_common_tools_and_one_door_to_the_rest() {
    let events = run(
        &scripted(json!({"script": [{"status": "ok", "text": "hi"}]})),
        "hi",
    );
    let mut names = offered(&events);
    names.sort();

    assert_eq!(
        names,
        vec![
            "Code",
            "Edit",
            "Find",
            "FindTools",
            "Grep",
            "Read",
            "Run",
            "UseDeferredTool",
            "Write",
            // Handing work to an expert is resident on purpose. Deferring it
            // would mean the model has to go looking before it learns experts
            // exist, and a door nobody knows about is a door nobody opens.
            "ask",
        ],
        "what a coding agent reaches for in almost every turn, plus a way to \
         look up everything else and a way to call it"
    );
}

/// Everything the assembly deferred is gone from the schema — including the
/// ones the model would otherwise reach for out of habit.
#[test]
fn a_deferred_tool_is_nowhere_in_the_schema() {
    let events = run(
        &scripted(json!({"script": [{"status": "ok", "text": "hi"}]})),
        "hi",
    );
    let names = offered(&events);
    for hidden in ["Ls", "Watch", "Schedule", "Fetch", "InstallComponent"] {
        assert!(
            !names.contains(&hidden.to_string()),
            "{hidden} is deferred and must not be in the schema: {names:?}"
        );
    }
}

/// A name is roughly a tenth of a declaration, and it is enough to know that
/// something exists. Without this the model has no way to guess that `watch`
/// is even available, and a search it never thinks to run buys nothing.
#[test]
fn the_prompt_names_what_it_cannot_see() {
    let events = run(
        &scripted(json!({"script": [{"status": "ok", "text": "hi"}]})),
        "hi",
    );
    let system = events
        .iter()
        .find_map(|e| e.payload["system"].as_str())
        .expect("a system prompt");
    for hidden in ["Ls", "Watch", "Schedule", "InstallComponent"] {
        assert!(
            system.contains(hidden),
            "the prompt must name {hidden}, or nobody will search for it"
        );
    }
    assert!(system.contains("FindTools"), "and say how to look it up");
    assert!(
        system.contains("UseDeferredTool"),
        "and how to call what it finds"
    );
}

#[test]
fn the_assembled_prompt_teaches_the_tool_workflow_not_only_tool_names() {
    let default_shell = lattice::components::shell_tools::manifest().prompt.unwrap();
    assert!(
        lattice::components::shell_tools::prompt_for("/workspace").ends_with(&default_shell),
        "workspace-specific shell guidance must retain the default workflow"
    );
    let events = run(
        &scripted(json!({"script": [{"status": "ok", "text": "hi"}]})),
        "hi",
    );
    let system = events
        .iter()
        .find_map(|e| e.payload["system"].as_str())
        .expect("a system prompt");
    let required = [
        "Use `Find` for file names and `Grep` for text",
        "context and continuation cursors",
        "Use `Code` for symbols, definitions, references, and complete symbol bodies",
        "text matches are not semantic references",
        "expectedVersion",
        "knownRead only while the earlier content is still in context",
        "saved logs or documents",
        "Do not rerun a command merely to recover omitted output",
    ];
    let missing: Vec<_> = required
        .into_iter()
        .filter(|phrase| !system.contains(phrase))
        .collect();
    assert!(missing.is_empty(), "missing workflow guidance: {missing:?}");
    assert!(
        !system.contains("One `Grep` for the signature, the type, the caller"),
        "the old text-search-only workflow must not survive beside the new one"
    );
}

/// The search returns the FULL declaration, and it comes back as a tool
/// result — the tail of the conversation. That placement is the entire point:
/// the schema, which is what the cache is keyed on, never changes.
#[test]
fn searching_hands_back_a_whole_declaration_at_the_tail() {
    let events = run(
        &scripted(json!({"script": [
            {"status": "ok", "toolCalls": [
                {"id": "c1", "tool": "FindTools", "arguments": {"query": "watch a file"}}]},
            {"status": "ok", "text": "found it"},
        ]})),
        "what can watch a file",
    );

    let result = events
        .iter()
        .find(|e| e.event_type == ce::TOOL_EXEC_COMPLETED && e.payload["call"] == "c1")
        .expect("the search answered")
        .payload
        .clone();
    assert_eq!(result["status"], "ok", "{result}");

    let found = result["result"]["tools"].as_array().expect("tools");
    let watch = found
        .iter()
        .find(|t| t["name"] == "Watch")
        .unwrap_or_else(|| panic!("`watch` was not found: {found:?}"));
    assert!(
        watch["parameters"]["properties"].is_object(),
        "the whole declaration, arguments and all: {watch}"
    );
    assert!(
        watch["effects"].is_object(),
        "including its effects: {watch}"
    );

    // And the schema the model was given never mentioned it.
    assert!(!offered(&events).contains(&"Watch".to_string()));
}

#[test]
fn an_exact_short_name_returns_only_that_declaration() {
    for query in ["Ls", " ls ", "LS"] {
        let events = run(
            &scripted(json!({"script": [
                {"status": "ok", "toolCalls": [
                    {"id": "exact", "tool": "FindTools", "arguments": {"query": query, "limit": 1}}]},
                {"status": "ok", "text": "done"}
            ]})),
            "find the named tool",
        );
        let result = &events
            .iter()
            .find(|e| e.event_type == ce::TOOL_EXEC_COMPLETED && e.payload["call"] == "exact")
            .unwrap()
            .payload["result"];
        assert_eq!(
            result["tools"][0]["name"], "Ls",
            "query {query:?}: {result}"
        );
        assert_eq!(result["tools"].as_array().unwrap().len(), 1);
        assert_eq!(result["more"], false);
    }
}

/// Found by describing the job, not only by knowing the name — which is the
/// case that matters, since a model that already knew the name would not need
/// to search.
#[test]
fn a_tool_is_found_by_what_it_does() {
    for query in ["timer", "wake me later", "http url"] {
        let events = run(
            &scripted(json!({"script": [
                {"status": "ok", "toolCalls": [
                    {"id": "c1", "tool": "FindTools", "arguments": {"query": query}}]},
                {"status": "ok", "text": "ok"},
            ]})),
            "find something",
        );
        let result = events
            .iter()
            .find(|e| e.event_type == ce::TOOL_EXEC_COMPLETED && e.payload["call"] == "c1")
            .expect("answered")
            .payload
            .clone();
        assert!(
            !result["result"]["tools"].as_array().unwrap().is_empty(),
            "{query:?} found nothing"
        );
    }
}

/// The round trip: the model calls a hidden tool through the one resident
/// door, and the real provider answers. The loop unwraps the call keeping its
/// id, so the gate, the routing and the result all see an ordinary call —
/// which is why a deferred tool needs no special handling anywhere else.
#[test]
fn a_hidden_tool_can_be_called_through_the_resident_door() {
    let events = run(
        &scripted(json!({"script": [
            {"status": "ok", "toolCalls": [{
                "id": "c1",
                "tool": "UseDeferredTool",
                "arguments": {"tool": "Ls", "arguments": {"path": "src"}},
            }]},
            {"status": "ok", "text": "listed"},
        ]})),
        "list src",
    );

    let done = events
        .iter()
        .find(|e| e.event_type == ce::TOOL_EXEC_COMPLETED && e.payload["call"] == "c1")
        .expect("the relayed call was answered")
        .payload
        .clone();
    assert_eq!(done["status"], "ok", "{done}");
    let entries = done["result"]["entries"]
        .as_array()
        .unwrap_or_else(|| panic!("a real ls result: {done}"));
    assert!(
        entries.iter().any(|e| e["name"] == "components"),
        "it really listed src: {done}"
    );

    // The door itself must be in the schema, or a real provider would reject
    // the call before it ever reached us. The scripted model does not validate,
    // so this has to be asserted rather than observed.
    assert!(
        offered(&events).contains(&"UseDeferredTool".to_string()),
        "the one resident door was not offered"
    );

    // The started event the providers saw carried the REAL tool name, not the
    // door's — routing never learns that a call arrived wrapped.
    assert!(
        events
            .iter()
            .any(|e| e.event_type == ce::TOOL_EXEC_STARTED && e.payload["tool"] == "Ls"),
        "the call was unwrapped before dispatch"
    );
}

// ── The fragment, on its own ───────────────────────────────────────────────

#[test]
fn nothing_deferred_means_nothing_said() {
    assert!(
        tool_catalog::fragment(&[]).is_none(),
        "an assembly that hides nothing should not explain how to find nothing"
    );
}
