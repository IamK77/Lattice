//! Behavior-contract tests for assembly inspection and router lookup (ported from the TypeScript skeleton, same coverage).

mod common;

use std::collections::HashMap;

use lattice::core_events as ce;
use lattice::{
    inspect_assembly, AssemblyManifest, ComponentInstance, ComponentManifest, EventTypeDecl,
    PortDecl, Router, RuntimeKind, Wire,
};

fn component(
    name: &str,
    runtime: RuntimeKind,
    inputs: Vec<PortDecl>,
    outputs: Vec<PortDecl>,
) -> ComponentManifest {
    ComponentManifest {
        name: name.to_string(),
        version: "0.1.0".to_string(),
        runtime,
        entry: format!("./{name}"),
        inputs,
        outputs,
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

/// The four-component assembly from the walkthrough scenario, reused across tests
fn registry() -> HashMap<String, ComponentManifest> {
    let mut reg = HashMap::new();
    reg.insert(
        "terminal-ui".to_string(),
        component(
            "terminal-ui",
            RuntimeKind::Inproc,
            vec![PortDecl::new(
                "显示口",
                &[ce::TURN_COMPLETED, ce::MODEL_CALL_COMPLETED],
            )],
            vec![PortDecl::new("用户输入", &[ce::USER_MESSAGE])],
        ),
    );
    reg.insert(
        "main-loop".to_string(),
        component(
            "main-loop",
            RuntimeKind::Inproc,
            vec![
                PortDecl::new("输入口", &[ce::USER_MESSAGE]),
                PortDecl::new("回复口", &[ce::MODEL_CALL_COMPLETED]),
                PortDecl::new("结果口", &[ce::TOOL_EXEC_COMPLETED]),
            ],
            vec![
                PortDecl::new("模型请求", &[ce::MODEL_CALL_STARTED]),
                PortDecl::new("工具请求", &[ce::TOOL_EXEC_STARTED]),
                PortDecl::new("对外输出", &[ce::TURN_COMPLETED, ce::MODEL_CALL_COMPLETED]),
            ],
        ),
    );
    reg.insert(
        "anthropic-adapter".to_string(),
        component(
            "anthropic-adapter",
            RuntimeKind::Inproc,
            vec![PortDecl::new("请求口", &[ce::MODEL_CALL_STARTED])],
            vec![PortDecl::new("模型结果", &[ce::MODEL_CALL_COMPLETED])],
        ),
    );
    reg.insert(
        "tool-runner".to_string(),
        component(
            "tool-runner",
            RuntimeKind::Process,
            vec![PortDecl::new("执行口", &[ce::TOOL_EXEC_STARTED])],
            vec![PortDecl::new("执行结果", &[ce::TOOL_EXEC_COMPLETED])],
        ),
    );
    reg
}

fn instance(component: &str) -> ComponentInstance {
    ComponentInstance {
        component: component.to_string(),
        requires: Vec::new(),
        config: None,
    }
}

fn valid_assembly() -> AssemblyManifest {
    AssemblyManifest {
        instances: [
            ("界面".to_string(), instance("terminal-ui")),
            ("主循环".to_string(), instance("main-loop")),
            ("模型适配".to_string(), instance("anthropic-adapter")),
            ("工具执行器".to_string(), instance("tool-runner")),
        ]
        .into(),
        wires: vec![
            Wire::new("界面.用户输入", "主循环.输入口"),
            Wire::new("主循环.模型请求", "模型适配.请求口"),
            Wire::new("模型适配.模型结果", "主循环.回复口"),
            Wire::new("主循环.工具请求", "工具执行器.执行口"),
            Wire::new("工具执行器.执行结果", "主循环.结果口"),
            Wire::new("主循环.对外输出", "界面.显示口"),
        ],
    }
}

#[test]
fn valid_assembly_passes() {
    assert_eq!(inspect_assembly(&valid_assembly(), &registry()), vec![]);
}

#[test]
fn unknown_component_is_caught() {
    let assembly = AssemblyManifest {
        instances: [("幽灵".to_string(), instance("没这个"))].into(),
        wires: vec![],
    };
    let issues = inspect_assembly(&assembly, &registry());
    assert_eq!(issues.len(), 1);
    assert!(issues[0].problem.contains("component not found"));
}

#[test]
fn wire_to_nonexistent_port_is_caught_with_location() {
    let mut assembly = valid_assembly();
    assembly.wires = vec![Wire::new("主循环.工具请求", "工具执行器.运行口")];
    let issues = inspect_assembly(&assembly, &registry());
    assert_eq!(issues.len(), 1);
    assert!(issues[0].location.contains("工具执行器.运行口"));
    assert!(issues[0].problem.contains("has no input port"));
}

#[test]
fn type_incompatible_wire_is_caught() {
    let mut assembly = valid_assembly();
    assembly.wires = vec![Wire::new("模型适配.模型结果", "工具执行器.执行口")];
    let issues = inspect_assembly(&assembly, &registry());
    assert!(issues.iter().any(|i| i.problem.contains("type mismatch")));
}

#[test]
fn wildcard_input_accepts_everything() {
    let mut reg = registry();
    reg.insert(
        "audit-viewer".to_string(),
        component(
            "audit-viewer",
            RuntimeKind::Inproc,
            vec![PortDecl::new("全收口", &["*"])],
            vec![],
        ),
    );
    let mut assembly = valid_assembly();
    assembly
        .instances
        .insert("观察者".to_string(), instance("audit-viewer"));
    assembly
        .wires
        .push(Wire::new("主循环.对外输出", "观察者.全收口"));
    assert_eq!(inspect_assembly(&assembly, &reg), vec![]);
}

#[test]
fn wildcard_output_is_forbidden() {
    let mut reg = registry();
    reg.insert(
        "bad-emitter".to_string(),
        component(
            "bad-emitter",
            RuntimeKind::Inproc,
            vec![],
            vec![PortDecl::new("乱发口", &["*"])],
        ),
    );
    let assembly = AssemblyManifest {
        instances: [("乱".to_string(), instance("bad-emitter"))].into(),
        wires: vec![],
    };
    let issues = inspect_assembly(&assembly, &reg);
    assert!(issues
        .iter()
        .any(|i| i.problem.contains("must not declare")));
}

#[test]
fn unregistered_port_event_type_is_caught() {
    let mut reg = registry();
    reg.insert(
        "stray".to_string(),
        component(
            "stray",
            RuntimeKind::Inproc,
            vec![PortDecl::new("入", &["nobody.registered.this"])],
            vec![],
        ),
    );
    let assembly = AssemblyManifest {
        instances: [("散".to_string(), instance("stray"))].into(),
        wires: vec![],
    };
    let issues = inspect_assembly(&assembly, &reg);
    assert!(issues
        .iter()
        .any(|i| i.problem.contains("unregistered event type")));
}

#[test]
fn duplicate_event_type_registration_is_caught() {
    let mut reg = registry();
    let decl = EventTypeDecl::new("weather.forecast.fetched", "weather fetched");
    for name in ["weather-a", "weather-b"] {
        let mut manifest = component(
            name,
            RuntimeKind::Inproc,
            vec![],
            vec![PortDecl::new("出", &["weather.forecast.fetched"])],
        );
        manifest.events = vec![decl.clone()];
        reg.insert(name.to_string(), manifest);
    }
    let assembly = AssemblyManifest {
        instances: [
            ("甲".to_string(), instance("weather-a")),
            ("乙".to_string(), instance("weather-b")),
        ]
        .into(),
        wires: vec![],
    };
    let issues = inspect_assembly(&assembly, &reg);
    assert!(issues
        .iter()
        .any(|i| i.problem.contains("already registered by")));
}

#[test]
fn router_returns_all_destinations_per_manifest() {
    let router = Router::new(&valid_assembly());
    assert_eq!(
        router.routes_from("主循环", "工具请求"),
        &[("工具执行器".to_string(), "执行口".to_string())]
    );
    assert!(router.routes_from("主循环", "没这个口").is_empty());
}

#[test]
fn duplicate_tool_names_across_providers_are_refused() {
    use crate::common::calc_tools;
    use lattice::components::{minimal_loop, scripted_model, silent_ui};
    // Two instances of calc-tools: both declare the tool "calc"
    let registry: std::collections::HashMap<String, lattice::ComponentManifest> = [
        (silent_ui::NAME.to_string(), silent_ui::manifest()),
        (minimal_loop::NAME.to_string(), minimal_loop::manifest()),
        (scripted_model::NAME.to_string(), scripted_model::manifest()),
        (calc_tools::NAME.to_string(), calc_tools::manifest()),
    ]
    .into();
    let assembly = lattice::AssemblyManifest {
        instances: [
            (
                "ui".to_string(),
                lattice::ComponentInstance {
                    component: silent_ui::NAME.to_string(),
                    requires: Vec::new(),
                    config: None,
                },
            ),
            (
                "loop".to_string(),
                lattice::ComponentInstance {
                    component: minimal_loop::NAME.to_string(),
                    requires: Vec::new(),
                    config: None,
                },
            ),
            (
                "model".to_string(),
                lattice::ComponentInstance {
                    component: scripted_model::NAME.to_string(),
                    requires: Vec::new(),
                    config: None,
                },
            ),
            (
                "tools-a".to_string(),
                lattice::ComponentInstance {
                    component: calc_tools::NAME.to_string(),
                    requires: Vec::new(),
                    config: None,
                },
            ),
            (
                "tools-b".to_string(),
                lattice::ComponentInstance {
                    component: calc_tools::NAME.to_string(),
                    requires: Vec::new(),
                    config: None,
                },
            ),
        ]
        .into(),
        wires: vec![
            lattice::Wire::new("ui.user", "loop.input"),
            lattice::Wire::new("loop.ask", "model.request"),
            lattice::Wire::new("model.result", "loop.model"),
            lattice::Wire::new("loop.run", "tools-a.execute"),
            lattice::Wire::new("loop.run", "tools-b.execute"),
            lattice::Wire::new("tools-a.outcome", "loop.tools"),
            lattice::Wire::new("tools-b.outcome", "loop.tools"),
            lattice::Wire::new("loop.out", "ui.display"),
        ],
    };
    let issues = lattice::inspect_assembly(&assembly, &registry);
    assert!(
        issues
            .iter()
            .any(|i| i.problem.contains("already provided by instance")),
        "a name collision must be caught before anything runs: {issues:?}"
    );
}

#[test]
fn a_tool_provider_no_wire_can_reach_is_refused() {
    use crate::common::calc_tools;
    use lattice::components::{minimal_loop, scripted_model, silent_ui};
    let registry: std::collections::HashMap<String, lattice::ComponentManifest> = [
        (silent_ui::NAME.to_string(), silent_ui::manifest()),
        (minimal_loop::NAME.to_string(), minimal_loop::manifest()),
        (scripted_model::NAME.to_string(), scripted_model::manifest()),
        (calc_tools::NAME.to_string(), calc_tools::manifest()),
    ]
    .into();
    let assembly = lattice::AssemblyManifest {
        instances: [
            (
                "ui".to_string(),
                lattice::ComponentInstance {
                    component: silent_ui::NAME.to_string(),
                    requires: Vec::new(),
                    config: None,
                },
            ),
            (
                "loop".to_string(),
                lattice::ComponentInstance {
                    component: minimal_loop::NAME.to_string(),
                    requires: Vec::new(),
                    config: None,
                },
            ),
            (
                "model".to_string(),
                lattice::ComponentInstance {
                    component: scripted_model::NAME.to_string(),
                    requires: Vec::new(),
                    config: None,
                },
            ),
            (
                "tools".to_string(),
                lattice::ComponentInstance {
                    component: calc_tools::NAME.to_string(),
                    requires: Vec::new(),
                    config: None,
                },
            ),
        ]
        .into(),
        // calc-tools is assembled, its tools get offered — but nothing wires
        // tool requests to it: the model could call into silence
        wires: vec![
            lattice::Wire::new("ui.user", "loop.input"),
            lattice::Wire::new("loop.ask", "model.request"),
            lattice::Wire::new("model.result", "loop.model"),
            lattice::Wire::new("tools.outcome", "loop.tools"),
            lattice::Wire::new("loop.out", "ui.display"),
        ],
    };
    let issues = lattice::inspect_assembly(&assembly, &registry);
    assert!(
        issues
            .iter()
            .any(|i| i.problem.contains("no wire delivers tool requests")),
        "an unreachable provider must be caught before the model hangs a turn: {issues:?}"
    );
}

/// The slot bound: `requires` on an instance is the trait bound of
/// assemblies — whatever fills the slot must CLAIM the profile.
#[test]
fn a_satisfied_slot_bound_passes_and_an_unmet_one_is_caught() {
    let mut reg = registry();
    let mut adapter = component(
        "true-adapter",
        RuntimeKind::Inproc,
        vec![PortDecl::new("request", &[ce::MODEL_CALL_STARTED])],
        vec![PortDecl::new("result", &[ce::MODEL_CALL_COMPLETED])],
    );
    adapter.implements = vec!["model-adapter".to_string()];
    reg.insert("true-adapter".to_string(), adapter);

    let mut assembly = AssemblyManifest {
        instances: [(
            "适配".to_string(),
            ComponentInstance {
                component: "true-adapter".to_string(),
                config: None,
                requires: vec!["model-adapter".to_string()],
            },
        )]
        .into(),
        wires: vec![],
    };
    assert_eq!(inspect_assembly(&assembly, &reg), vec![]);

    // The same slot demanding a policy is unmet: the adapter never claimed one
    assembly.instances.get_mut("适配").unwrap().requires = vec!["policy".to_string()];
    let issues = inspect_assembly(&assembly, &reg);
    assert!(
        issues
            .iter()
            .any(|i| i.location == "instance 适配" && i.problem.contains("policy")),
        "{issues:?}"
    );
}

/// Bounds are NOMINAL, like trait bounds: having the right-shaped ports
/// without the implements declaration does not satisfy them (the
/// declaration is what carries the exam).
#[test]
fn matching_ports_without_the_claim_do_not_satisfy_a_bound() {
    let mut reg = registry();
    reg.insert(
        "duck-adapter".to_string(),
        component(
            "duck-adapter",
            RuntimeKind::Inproc,
            vec![PortDecl::new("request", &[ce::MODEL_CALL_STARTED])],
            vec![PortDecl::new("result", &[ce::MODEL_CALL_COMPLETED])],
        ),
    );
    let assembly = AssemblyManifest {
        instances: [(
            "适配".to_string(),
            ComponentInstance {
                component: "duck-adapter".to_string(),
                config: None,
                requires: vec!["model-adapter".to_string()],
            },
        )]
        .into(),
        wires: vec![],
    };
    let issues = inspect_assembly(&assembly, &reg);
    assert!(
        issues
            .iter()
            .any(|i| i.problem.contains("does not implement")),
        "{issues:?}"
    );
}

#[test]
fn an_unknown_profile_in_a_bound_is_caught() {
    let mut assembly = valid_assembly();
    assembly.instances.get_mut("界面").unwrap().requires = vec!["telepathy".to_string()];
    let issues = inspect_assembly(&assembly, &registry());
    assert!(
        issues
            .iter()
            .any(|i| i.problem.contains("unknown profile: telepathy")),
        "{issues:?}"
    );
}
