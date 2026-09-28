//! The standard exam papers: components claiming a profile must pass them.
//! Machine-graded — this is what lets an agent generate a new component and
//! prove it fits without anyone taking its word for it.

use lattice::components::{
    anthropic_model, context_gate, fs_tools, fs_watch, net_tools, openai_model, scripted_model,
    search_tools, shell_tools, silent_ui, skill_library, timer_tools, trust_policy,
};
use lattice::conformance::{
    examine_context_manager, examine_frontend, examine_model_adapter, examine_policy,
    examine_tool_provider,
};
use lattice::{Component, Ctx, EventEnvelope};

#[test]
fn responses_model_passes_the_exam_without_network() {
    use lattice::components::responses_model;
    let problems = examine_model_adapter(
        &responses_model::manifest(),
        Box::new(|config| Box::new(responses_model::ResponsesModel::from_config(config))),
    );
    assert_eq!(problems, Vec::<String>::new());
}

#[test]
fn scripted_model_passes_the_model_adapter_exam() {
    let problems = examine_model_adapter(
        &scripted_model::manifest(),
        Box::new(|config| Box::new(scripted_model::ScriptedModel::from_config(config))),
    );
    assert_eq!(problems, Vec::<String>::new());
}

#[test]
fn anthropic_model_passes_the_exam_without_network() {
    // Point the key at a variable that is never set: the adapter must still
    // complete the call (an error completion is a valid completion — the
    // exam grades contract shape, not intelligence). CI stays offline.
    let problems = examine_model_adapter(
        &anthropic_model::manifest(),
        Box::new(|_| {
            Box::new(anthropic_model::AnthropicModel::from_config(Some(
                &serde_json::json!({"apiKeyEnv": "LATTICE_EXAM_KEY_THAT_DOES_NOT_EXIST"}),
            )))
        }),
    );
    assert_eq!(problems, Vec::<String>::new());
}

#[test]
fn openai_model_passes_the_exam_without_network() {
    // The second real adapter sitting the SAME exam as the first is the
    // point: the model-adapter profile is interchangeable, machine-verified.
    let problems = examine_model_adapter(
        &openai_model::manifest(),
        Box::new(|_| {
            Box::new(openai_model::OpenAiModel::from_config(Some(
                &serde_json::json!({"apiKeyEnv": "LATTICE_EXAM_KEY_THAT_DOES_NOT_EXIST"}),
            )))
        }),
    );
    assert_eq!(problems, Vec::<String>::new());
}

/// Claims the profile, swallows every request silently
struct Mute;
impl Component for Mute {
    fn handle(&mut self, _port: &str, _event: &EventEnvelope, _ctx: &mut Ctx) {}
}

#[test]
fn a_silent_candidate_fails_the_exam() {
    let mut manifest = scripted_model::manifest();
    manifest.name = "mute-model".to_string();
    let problems = examine_model_adapter(&manifest, Box::new(|_| Box::new(Mute)));
    assert!(problems
        .iter()
        .any(|p| p.contains("exactly one model_call_completed")));
}

/// Claims the profile, completes the call, but hands back reasoning nobody
/// can use: a sealed part with nothing sealed inside it.
struct EmptyThinker;
impl Component for EmptyThinker {
    fn handle(&mut self, _port: &str, event: &EventEnvelope, ctx: &mut Ctx) {
        ctx.emit(
            "result",
            lattice::EventDraft::new(
                lattice::core_events::MODEL_CALL_COMPLETED,
                &[&event.id],
                serde_json::json!({"status": "ok", "text": "hi", "reasoning": [{"kind": "hidden"}]}),
            ),
        );
    }
}

/// The exam grades the SHAPE of thinking, never its content: a part that
/// cannot be used is a defect, a part that is merely wrong-headed is not.
#[test]
fn reasoning_that_carries_nothing_fails_the_exam() {
    let mut manifest = scripted_model::manifest();
    manifest.name = "empty-thinker".to_string();
    let problems = examine_model_adapter(&manifest, Box::new(|_| Box::new(EmptyThinker)));
    assert!(
        problems.iter().any(|p| p.contains("carries nothing")),
        "expected the empty sealed part to be caught, saw {problems:?}"
    );
}

#[test]
fn context_gate_passes_the_context_manager_exam() {
    let problems = examine_context_manager(
        &context_gate::manifest(),
        Box::new(|config| Box::new(context_gate::ContextGate::from_config(config))),
    );
    assert_eq!(problems, Vec::<String>::new());
}

#[test]
fn search_tools_passes_the_tool_provider_exam() {
    let problems = examine_tool_provider(
        &search_tools::manifest(),
        Some(Box::new(|config| {
            Box::new(search_tools::SearchTools::from_config(config))
        })),
        "Find",
    );
    assert_eq!(problems, Vec::<String>::new());
}

#[test]
fn fs_tools_passes_the_tool_provider_exam() {
    let problems = examine_tool_provider(
        &fs_tools::manifest(),
        Some(Box::new(|config| {
            Box::new(fs_tools::FsTools::from_config(config))
        })),
        "Ls",
    );
    assert_eq!(problems, Vec::<String>::new());
}

#[test]
fn shell_tools_passes_the_tool_provider_exam() {
    let problems = examine_tool_provider(
        &shell_tools::manifest(),
        Some(Box::new(|config| {
            Box::new(shell_tools::ShellTools::from_config(config))
        })),
        "Run",
    );
    assert_eq!(problems, Vec::<String>::new());
}

#[test]
fn trust_policy_passes_the_policy_exam() {
    let problems = examine_policy(
        &trust_policy::manifest(),
        Some(Box::new(|config| {
            Box::new(trust_policy::TrustPolicy::from_config(config))
        })),
    );
    assert_eq!(problems, Vec::<String>::new());
}

#[test]
fn fs_watch_passes_the_tool_provider_exam() {
    let problems = examine_tool_provider(
        &fs_watch::manifest(),
        Some(Box::new(|config| {
            Box::new(fs_watch::FsWatch::from_config(config))
        })),
        "Watch",
    );
    assert_eq!(problems, Vec::<String>::new());
}

#[test]
fn skill_library_passes_the_tool_provider_exam() {
    let problems = examine_tool_provider(
        &skill_library::manifest(),
        Some(Box::new(|config| {
            Box::new(skill_library::SkillLibrary::from_config(config))
        })),
        "LoadSkill",
    );
    assert_eq!(problems, Vec::<String>::new());
}

#[test]
fn net_tools_passes_the_tool_provider_exam() {
    let problems = examine_tool_provider(
        &net_tools::manifest(),
        Some(Box::new(|config| {
            Box::new(net_tools::NetTools::from_config(config))
        })),
        "Fetch",
    );
    assert_eq!(problems, Vec::<String>::new());
}

#[test]
fn silent_ui_passes_the_frontend_exam() {
    let problems = examine_frontend(
        &silent_ui::manifest(),
        Some(Box::new(|_| {
            Box::new(silent_ui::SilentUi::new(std::sync::Arc::default()))
        })),
    );
    assert_eq!(problems, Vec::<String>::new());
}

/// Claims the frontend profile but dies on the first thing displayed.
struct Fainting;
impl Component for Fainting {
    fn handle(&mut self, _port: &str, _event: &EventEnvelope, _ctx: &mut Ctx) {
        panic!("fainting on display");
    }
}

#[test]
fn a_crashing_frontend_fails_the_exam() {
    let mut manifest = silent_ui::manifest();
    manifest.name = "fainting-ui".to_string();
    let problems = examine_frontend(&manifest, Some(Box::new(|_| Box::new(Fainting))));
    assert!(
        !problems.is_empty(),
        "a frontend that dies on display must fail"
    );
}

#[test]
fn a_frontend_missing_the_display_port_fails_structurally() {
    let mut manifest = silent_ui::manifest();
    manifest.name = "portless-ui".to_string();
    manifest.inputs.clear();
    let problems = examine_frontend(&manifest, None);
    assert!(
        problems.iter().any(|p| p.contains("display")),
        "{problems:?}"
    );
}

#[test]
fn an_authorize_claim_without_the_answer_port_fails_structurally() {
    let mut manifest = silent_ui::manifest();
    manifest.name = "mute-ui".to_string();
    manifest.outputs.retain(|p| p.name != "answer");
    let problems = examine_frontend(&manifest, None);
    assert!(
        problems.iter().any(|p| p.contains("answer")),
        "{problems:?}"
    );
}

#[test]
fn timer_tools_passes_the_tool_provider_exam() {
    let problems = examine_tool_provider(
        &timer_tools::manifest(),
        Some(Box::new(|config| {
            Box::new(timer_tools::TimerTools::from_config(config))
        })),
        "Schedule",
    );
    assert_eq!(problems, Vec::<String>::new());
}

/// A port's event list is a SET, and the check has to treat it as one.
///
/// The contract calls it a set and the canon describes it as one, but the
/// comparison was list equality — so a component that listed the same two
/// event types in the other order failed inspection, and the message it got
/// showed two lists that read as identical. A foreign component's author
/// writes that field by hand against the canon; nothing tells them the order
/// matters, because it does not.
#[test]
fn a_port_declaring_the_same_events_in_another_order_still_claims_the_profile() {
    use lattice::{check_claim, core_profiles, ComponentManifest, PortDecl, RuntimeKind};

    let frontend = core_profiles()
        .into_iter()
        .find(|p| p.name == "frontend")
        .expect("the frontend profile exists");
    // The profile's own display port, spelled backwards.
    let wanted = &frontend.inputs[0];
    let mut backwards: Vec<&str> = wanted.events.iter().map(String::as_str).collect();
    backwards.reverse();
    assert!(backwards.len() > 1, "this only tests anything with two");

    let manifest = ComponentManifest {
        name: "reversed-ui".to_string(),
        version: "0".to_string(),
        runtime: RuntimeKind::Inproc,
        entry: "builtin:reversed-ui".to_string(),
        inputs: vec![PortDecl::new(&wanted.name, &backwards)],
        outputs: vec![PortDecl::new(
            &frontend.outputs[0].name,
            &frontend.outputs[0]
                .events
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>(),
        )],
        events: Vec::new(),
        default_wiring: Vec::new(),
        capabilities: None,
        implements: vec!["frontend".to_string()],
        tools: Vec::new(),
        prompt: None,
        handle_timeout_ms: None,
        concurrency: None,
    };
    assert!(
        check_claim(&manifest, &frontend).is_empty(),
        "the same set in another order is the same set: {:?}",
        check_claim(&manifest, &frontend)
    );

    // A genuinely different set is still refused.
    let mut missing = manifest;
    missing.inputs = vec![PortDecl::new(&wanted.name, &backwards[..1])];
    assert!(
        !check_claim(&missing, &frontend).is_empty(),
        "a port that accepts less than the profile asks is not the profile"
    );
}
