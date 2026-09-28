use std::collections::{BTreeSet, HashMap};

use crate::contracts::assembly::{parse_endpoint, AssemblyManifest};
use crate::contracts::component::{ComponentManifest, PortDecl};
use crate::contracts::core_events::core_event_decls;
use crate::contracts::profile::{check_claim, core_profiles};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InspectionIssue {
    pub location: String,
    pub problem: String,
}

fn issue(location: impl Into<String>, problem: impl Into<String>) -> InspectionIssue {
    InspectionIssue {
        location: location.into(),
        problem: problem.into(),
    }
}

enum Side {
    Output,
    Input,
}

/// Assembly inspection — the first half of kernel duty #2: catch every assembly
/// mistake before anything runs. Checks component references, event-type
/// registration (including cross-component duplicates), port existence and
/// direction, and per-wire type compatibility.
/// An empty result means pass; any single issue must prevent startup.
pub fn inspect_assembly(
    assembly: &AssemblyManifest,
    registry: &HashMap<String, ComponentManifest>,
) -> Vec<InspectionIssue> {
    let mut issues = Vec::new();

    // Components referenced by instances must exist (BTreeSet keeps report order stable)
    let mut used_components: BTreeSet<&str> = BTreeSet::new();
    for (name, instance) in &assembly.instances {
        if name == "core" {
            issues.push(issue(
                format!("instance {name}"),
                "\"core\" is reserved for kernel-recorded events",
            ));
        }
        if registry.contains_key(&instance.component) {
            used_components.insert(&instance.component);
        } else {
            issues.push(issue(
                format!("instance {name}"),
                format!("component not found: {}", instance.component),
            ));
        }
    }

    // Tool declarations: no two instances may claim the same tool name
    // (fan-out routing selects by name — a collision would double-answer),
    // and a tool-declaring instance must be reachable by tool requests
    // (an unreachable provider means a tool the model can call into
    // silence, hanging the turn).
    {
        let mut owners: std::collections::BTreeMap<&str, &String> =
            std::collections::BTreeMap::new();
        let mut instance_names: Vec<&String> = assembly.instances.keys().collect();
        instance_names.sort();
        for name in instance_names {
            let instance = &assembly.instances[name];
            let Some(manifest) = registry.get(&instance.component) else {
                continue; // missing component already reported above
            };
            for decl in &manifest.tools {
                let Some(tool) = decl["name"].as_str() else {
                    issues.push(issue(
                        format!("instance {name}"),
                        "a tool declaration carries no name".to_string(),
                    ));
                    continue;
                };
                if let Some(previous) = owners.insert(tool, name) {
                    issues.push(issue(
                        format!("instance {name}"),
                        format!("tool \"{tool}\" is already provided by instance {previous}"),
                    ));
                }
            }
            if !manifest.tools.is_empty() {
                let reachable = assembly.wires.iter().any(|wire| {
                    parse_endpoint(&wire.to).is_some_and(|(dst_instance, dst_port)| {
                        dst_instance == *name
                            && manifest.inputs.iter().any(|port| {
                                port.name == dst_port
                                    && (port.events.iter().any(|e| e == "*")
                                        || port.events.iter().any(|e| {
                                            e == crate::contracts::core_events::TOOL_EXEC_STARTED
                                        }))
                            })
                    })
                });
                if !reachable {
                    issues.push(issue(
                        format!("instance {name}"),
                        "declares tools but no wire delivers tool requests to it".to_string(),
                    ));
                }
            }
        }
    }

    // Profile claims must hold structurally
    let profiles = core_profiles();
    for component_name in &used_components {
        let manifest = &registry[*component_name];
        for claim in &manifest.implements {
            match profiles.iter().find(|p| &p.name == claim) {
                None => issues.push(issue(
                    format!("component {component_name}"),
                    format!("claims unknown profile: {claim}"),
                )),
                Some(profile) => {
                    for problem in check_claim(manifest, profile) {
                        issues.push(issue(format!("component {component_name}"), problem));
                    }
                }
            }
        }
    }

    // Slot bounds: an instance's `requires` names profiles its component
    // must CLAIM (nominal, like a trait bound — matching ports without the
    // declaration do not count; the declaration is what carries the exam).
    // The claims themselves were verified structurally just above.
    for (name, instance) in &assembly.instances {
        let Some(manifest) = registry.get(&instance.component) else {
            continue; // missing component already reported above
        };
        for required in &instance.requires {
            if !profiles.iter().any(|p| &p.name == required) {
                issues.push(issue(
                    format!("instance {name}"),
                    format!("requires unknown profile: {required}"),
                ));
            } else if !manifest.implements.contains(required) {
                issues.push(issue(
                    format!("instance {name}"),
                    format!(
                        "requires profile {required:?} but component {} does not implement it",
                        instance.component
                    ),
                ));
            }
        }
    }

    // Event-type registration: core claims its types first, components add theirs;
    // no type may be registered by two different components
    let mut declared_by: HashMap<String, String> = core_event_decls()
        .into_iter()
        .map(|d| (d.event_type, "core".to_string()))
        .collect();
    for component_name in &used_components {
        for decl in &registry[*component_name].events {
            match declared_by.get(&decl.event_type) {
                Some(owner) if owner != component_name => {
                    issues.push(issue(
                        format!("component {component_name}"),
                        format!(
                            "event type {} already registered by {owner}",
                            decl.event_type
                        ),
                    ));
                }
                _ => {
                    declared_by.insert(decl.event_type.clone(), component_name.to_string());
                }
            }
        }
    }

    // Event types referenced by ports must be registered; "*" is allowed on input ports only
    let check_ports = |issues: &mut Vec<InspectionIssue>,
                       component_name: &str,
                       ports: &[PortDecl],
                       side: Side| {
        let dir = match side {
            Side::Input => "input",
            Side::Output => "output",
        };
        for port in ports {
            for event_type in &port.events {
                if event_type == "*" {
                    if matches!(side, Side::Output) {
                        issues.push(issue(
                            format!("component {component_name} output port {}", port.name),
                            "output ports must not declare \"*\"",
                        ));
                    }
                    continue;
                }
                if !declared_by.contains_key(event_type) {
                    issues.push(issue(
                        format!("component {component_name} {dir} port {}", port.name),
                        format!("unregistered event type: {event_type}"),
                    ));
                }
            }
        }
    };
    for component_name in &used_components {
        let manifest = &registry[*component_name];
        check_ports(&mut issues, component_name, &manifest.inputs, Side::Input);
        check_ports(&mut issues, component_name, &manifest.outputs, Side::Output);
    }

    // Each wire: endpoint format, instance/port existence, direction, type compatibility
    let resolve = |endpoint: &str, side: Side| -> Result<Option<&PortDecl>, String> {
        let Some((instance_name, port_name)) = parse_endpoint(endpoint) else {
            return Err(format!("endpoint must be \"instance.port\": {endpoint}"));
        };
        let Some(instance) = assembly.instances.get(instance_name) else {
            return Err(format!("instance not found: {instance_name}"));
        };
        let Some(manifest) = registry.get(&instance.component) else {
            return Ok(None); // missing component already reported above; don't repeat
        };
        let (ports, kind) = match side {
            Side::Output => (&manifest.outputs, "output"),
            Side::Input => (&manifest.inputs, "input"),
        };
        match ports.iter().find(|p| p.name == port_name) {
            Some(port) => Ok(Some(port)),
            None => Err(format!(
                "instance {instance_name} (component {}) has no {kind} port: {port_name}",
                instance.component
            )),
        }
    };

    for wire in &assembly.wires {
        let location = format!("wire {} → {}", wire.from, wire.to);
        let from = resolve(&wire.from, Side::Output);
        let to = resolve(&wire.to, Side::Input);
        if let Err(problem) = &from {
            issues.push(issue(&location, problem));
        }
        if let Err(problem) = &to {
            issues.push(issue(&location, problem));
        }
        if let (Ok(Some(from_port)), Ok(Some(to_port))) = (from, to) {
            if !to_port.events.iter().any(|t| t == "*") {
                for event_type in &from_port.events {
                    if !to_port.events.contains(event_type) {
                        issues.push(issue(
                            &location,
                            format!("type mismatch: output port may emit {event_type}, input port does not accept it"),
                        ));
                    }
                }
            }
        }
    }

    issues
}
