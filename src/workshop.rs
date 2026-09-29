//! The workshop — host-side machinery for "the agent builds its own tool".
//!
//! An agent calls the `InstallComponent` tool with a name-card and a body of
//! source. The workshop turns that request into a running component through
//! the real gates: write the source, assemble a manifest, run the conformance
//! exam, and — only if it passes and the human approves — hot-install it. A
//! failing exam returns its problem list verbatim so the agent can read it and
//! fix its code (self-debugging). None of this touches the kernel's internals;
//! it is ordinary host code plus `Kernel::install`.

use std::collections::HashMap;
use std::io::Write;
use std::path::Path;

use serde_json::{json, Value};

use crate::conformance::examine_tool_provider;
use crate::contracts::assembly::{parse_endpoint, AssemblyManifest, Wire};
use crate::contracts::component::{ComponentManifest, EffectSurface, PortDecl, RuntimeKind};
use crate::contracts::core_events as ce;
use crate::contracts::event::EventEnvelope;
use crate::kernel::host::{Kernel, KERNEL_SOURCE};

#[cfg(test)]
mod history_tests;

/// The tool declaration the agent sees. Offer it to the model in the loop's
/// tool list; requests land as ordinary `InstallComponent` tool calls.
pub fn install_tool_decl() -> Value {
    json!({
        "name": "InstallComponent",
        "description": "Build yourself a new tool. Provide `handler`: Python \
            (stdlib only) that defines a function handle(tool, arguments) returning a \
            JSON-serializable result (raise an exception to signal an error). The \
            protocol wrapper is generated for you. The component is exam-graded and, \
            if the user approves, installed into this running session immediately.",
        "parameters": {
            "type": "object",
            "required": ["instance", "tool_name", "tool_description", "tool_parameters", "handler", "reason"],
            "properties": {
                "instance": {"type": "string", "description": "instance name for the new component"},
                "tool_name": {"type": "string"},
                "tool_description": {"type": "string"},
                "tool_parameters": {"type": "object", "description": "JSON Schema of the tool's arguments"},
                "handler": {"type": "string", "description": "Python defining handle(tool, arguments)"},
                "source": {"type": "string", "description": "advanced: a full bridge-protocol program instead of handler"},
                "reason": {"type": "string"}
            }
        },
        // What it really does: writes the component's source into the install
        // directory and runs it. Declaring only `admits` was untrue, and a
        // declaration is the only thing an effects policy has to go on — a
        // policy refusing `executes` would have waved this through while
        // stopping a shell command that does less. Not `reversible` either:
        // the code it installs starts running immediately.
        "effects": {"writes": ["<components>"], "executes": true,
                    "admits": "components"}
    })
}

/// The tool declaration for taking an installed component back out.
///
/// Only what an install PUT there may be taken out — see `removable`. Without
/// that rule this tool would let a model dismantle the gate that judges it;
/// with it, an agent can undo its own doing and nothing else.
pub fn uninstall_tool_decl() -> Value {
    json!({
        "name": UNINSTALL_TOOL,
        "description": "Remove a component you (or the user) installed earlier, by its \
            instance name. Its tools stop being offered and it stops running. Only \
            installed components can be removed — the assembly you were started with \
            is not yours to change. Nothing is erased from the record: the removal is \
            itself recorded, and everything the component did stays in the history.",
        "parameters": {
            "type": "object",
            "properties": {
                "instance": {"type": "string", "description": "the tool it provides, or \
                    the instance name if you know it"},
                "reason": {"type": "string", "description": "why it should go"},
            },
            "required": ["instance", "reason"],
        },
        // Removing changes what this agent can DO next, but touches nothing
        // outside the runtime: no file, no program, no network.
        "effects": {"reversible": true},
    })
}

/// The tool declaration for installing a READY-MADE component from a source:
/// a local directory or a git URL containing `component.json` (the
/// self-description, canon schemas/component_manifest.json; `{dir}` in its
/// entry is replaced with the installed location) plus whatever the entry
/// runs. Process form only — foreign code lives next door.
pub fn install_from_tool_decl() -> Value {
    json!({
        "name": "InstallComponentFrom",
        "description": "Install a ready-made component from a local directory path or \
            a git repository URL. The source must contain component.json (the \
            component's self-description; `{dir}` in its entry resolves to the \
            installed location) and the files its entry runs. It is validated, \
            exam-graded, and — with the user's consent — hot-installed and \
            persisted; it survives restarts. `reason` is recorded on the ledger.",
        "parameters": {
            "type": "object",
            "required": ["source", "reason"],
            "properties": {
                "source": {"type": "string"},
                "instance": {"type": "string", "description": "instance name (default: the component's name)"},
                "reason": {"type": "string"}
            }
        },
        // `admits`: this call INTRODUCES new code into the runtime
        "effects": {"writes": ["<components>"], "network": ["*"], "executes": true,
                    "admits": "components"}
    })
}

/// The two install tools, named once so the scan, the answer and the
/// declarations cannot drift apart.
pub const INSTALL_TOOL: &str = "InstallComponent";
pub const UNINSTALL_TOOL: &str = "UninstallComponent";
pub const INSTALL_FROM_TOOL: &str = "InstallComponentFrom";

/// A pending install request found in the log: a tool call for
/// `InstallComponent` that has not been answered yet.
pub struct PendingInstall {
    pub call: String,
    pub cause: String,
    pub args: Value,
}

/// Scan the ledger for `InstallComponentFrom` requests still awaiting an
/// answer (same shape as [`pending_installs`], different tool).
pub fn pending_fetch_installs(kernel: &Kernel) -> Result<Vec<PendingInstall>, String> {
    pending_requests(kernel, INSTALL_FROM_TOOL)
}

/// Scan the ledger for removal requests still awaiting an answer.
pub fn pending_removals(kernel: &Kernel) -> Result<Vec<PendingInstall>, String> {
    pending_requests(kernel, UNINSTALL_TOOL)
}

/// Scan the ledger for install requests still awaiting an answer.
pub fn pending_installs(kernel: &Kernel) -> Result<Vec<PendingInstall>, String> {
    pending_requests(kernel, INSTALL_TOOL)
}

fn pending_requests(kernel: &Kernel, tool: &str) -> Result<Vec<PendingInstall>, String> {
    // The sink is the instance whose component declares this tool — the one
    // the host answers in the name of. A request is only ready to service once
    // it has REACHED that sink; while it still sits at a trust gate, answering
    // it would be servicing something nobody has approved.
    let sink = sink_instance(kernel, tool);

    // Reuse the exact settlement projection used by restart cleanup. Its
    // checkpoint reads only the new tail, and retains only unfinished calls.
    // Keep forwards here: a root waiting at a gate has not reached the sink.
    let reader = kernel.log().reader();
    let requests = reader
        .pending_requests(ce::TOOL_EXEC_STARTED)
        .map_err(|e| e.to_string())?;

    // One pending entry per call, keeping the LATEST start that reached the
    // sink — a gated assembly records the request twice (the loop's emission
    // to the gate and the gate's forward to the sink); only the forward
    // reached the sink, and its id is the cause a valid completion must cite.
    let mut by_call: std::collections::BTreeMap<String, PendingInstall> = Default::default();
    for id in &requests {
        let e = reader
            .header(id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("pending request {id} missing from history"))?;
        if e.tool.as_deref() != Some(tool) {
            continue;
        }
        let Some(call) = e.call.as_deref() else {
            continue;
        };
        // Skip a request that has not reached the sink yet (still under review
        // at a gate). No sink found = no gate topology to reason about; fall
        // back to surfacing it (the ungated case wired straight to the sink).
        //
        // After a reopen this says yes to everything, because the durable past
        // counts as witnessed by everyone — the rule that lets a restored
        // component cite events from before it existed. What keeps an
        // unapproved request from being serviced on the next start is
        // therefore NOT this line but the outcome check above: reopening
        // settles every unfinished chain, and a settled chain is answered.
        // Both halves of that sentence have to stay true; the test named
        // `an_unanswered_card_is_still_unanswered_after_a_restart` fails if
        // either stops being.
        if let Some(sink) = &sink {
            if !kernel
                .witnessed_by(sink, &e.id)
                .map_err(|error| error.to_string())?
            {
                continue;
            }
        }
        let body = reader
            .get(&e.id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("install request {} missing from history", e.id))?;
        by_call.insert(
            call.to_string(),
            PendingInstall {
                call: call.to_string(),
                cause: e.id.clone(),
                args: body.payload["arguments"].clone(),
            },
        );
    }
    Ok(by_call.into_values().collect())
}

/// The instance whose component declares `tool` — the sink the host answers
/// for. Tool names are unique across instances (inspection enforces it), so
/// at most one matches.
fn sink_instance(kernel: &Kernel, tool: &str) -> Option<String> {
    let registry = kernel.component_registry();
    kernel
        .assembly()
        .instances
        .iter()
        .find(|(_, inst)| {
            registry
                .get(&inst.component)
                .is_some_and(|m| m.tools.iter().any(|t| t["name"] == tool))
        })
        .map(|(name, _)| name.clone())
}

/// Compute the wires a newly installed component should get, consuming its
/// manifest's `default_wiring` and wiring the rest by rule:
///
/// 1. `default_wiring` suggestions land first, but ONLY those wholly inside
///    the newcomer (`self.a → self.b`) — the self-referential rings no
///    environment rule could guess, which is the entire reason the field
///    exists (the one built-in that uses it, skill-consumer, is exactly that
///    shape). A suggestion naming anybody else is refused and reported.
///
///    Taking foreign endpoints at their word made installing one component
///    enough to undo the assembly: `loop.run → self.execute` wires the
///    newcomer straight off the loop, skipping the gate every peer sits
///    behind, and `self.approve → trust.answer` wires it into the gate's
///    authorization port, where the gate does not check who an answer came
///    from — so one approved install could approve all its own later ones.
///    Both passed inspection, because both are structurally valid wiring.
///    A component describes ITSELF; how it relates to everyone else is the
///    assembler's call, and the rules below are how the assembler makes it.
/// 2. PROFILE FIRST: a newcomer claiming tool-provider gets its profile
///    ports (execute/outcome) wired the way CLAIMING peers' same-named
///    ports are wired — nominal and by port name, so a crowd of unclaimed
///    tools-having components can never outvote the real providers (and
///    the gate they all sit behind).
/// 3. Peer majority as the fallback: ports of the three well-known event
///    families are wired the way same-kind instances are wired (majority,
///    deterministic tie-break); in a gated assembly that is the gate's
///    forward — a hardcoded loop.run would silently BYPASS the gates.
///    No peers at all = the loop's own ports.
///
/// Only the three well-known event families are wired mechanically; other
/// ports (a component's own letters) stay unwired unless suggested.
pub fn suggested_wires(
    assembly: &AssemblyManifest,
    registry: &HashMap<String, ComponentManifest>,
    manifest: &ComponentManifest,
    instance: &str,
    loop_instance: &str,
) -> Vec<Wire> {
    wire_the_newcomer(assembly, registry, manifest, instance, loop_instance).0
}

/// [`suggested_wires`], plus what it refused to take from the manifest's own
/// `default_wiring`. A refusal means the component asked to be connected to
/// somebody else and was told no; it belongs on the record rather than in a
/// dropped value, so the install path reports it.
pub fn wire_the_newcomer(
    assembly: &AssemblyManifest,
    registry: &HashMap<String, ComponentManifest>,
    manifest: &ComponentManifest,
    instance: &str,
    loop_instance: &str,
) -> (Vec<Wire>, Vec<String>) {
    let mut wires: Vec<Wire> = Vec::new();
    let mut refused: Vec<String> = Vec::new();
    let mut covered_inputs: Vec<String> = Vec::new();
    let mut covered_outputs: Vec<String> = Vec::new();

    for suggestion in &manifest.default_wiring {
        let (Some(from_port), Some(to_port)) = (
            suggestion.from.strip_prefix("self."),
            suggestion.to.strip_prefix("self."),
        ) else {
            refused.push(format!(
                "{} → {}: a component may only wire its own ports to its own ports",
                suggestion.from, suggestion.to
            ));
            continue;
        };
        covered_outputs.push(from_port.to_string());
        covered_inputs.push(to_port.to_string());
        wires.push(Wire::new(
            &format!("{instance}.{from_port}"),
            &format!("{instance}.{to_port}"),
        ));
    }

    // How do the peers wire a port of this event type? Majority vote over
    // the assembly (deterministic tie-break by name).
    let majority = |candidates: Vec<String>| -> Option<String> {
        let mut counts: std::collections::BTreeMap<String, usize> = Default::default();
        for c in candidates {
            *counts.entry(c).or_default() += 1;
        }
        counts
            .into_iter()
            .max_by(|a, b| a.1.cmp(&b.1).then(b.0.cmp(&a.0)))
            .map(|(name, _)| name)
    };
    let port_accepts = |m: &ComponentManifest, port: &str, event_type: &str| {
        m.inputs
            .iter()
            .any(|p| p.name == port && p.events.iter().any(|e| e == event_type))
    };
    let port_emits = |m: &ComponentManifest, port: &str, event_type: &str| {
        m.outputs
            .iter()
            .any(|p| p.name == port && p.events.iter().any(|e| e == event_type))
    };
    // A peer is an instance LIKE the one being wired: a tool provider. A
    // policy gate also accepts tool requests, but how the gate is fed is not
    // how providers are fed — counting it would wire a newcomer around the
    // gate (or the gate around itself).
    let is_provider_peer = |m: &ComponentManifest| {
        !m.tools.is_empty() || m.implements.iter().any(|p| p == "tool-provider")
    };
    let peer_source_feeding = |event_type: &str| -> Option<String> {
        majority(
            assembly
                .wires
                .iter()
                .filter_map(|wire| {
                    let (who, port) = parse_endpoint(&wire.to)?;
                    if who == instance {
                        return None;
                    }
                    let peer = registry.get(&assembly.instances.get(who)?.component)?;
                    (is_provider_peer(peer) && port_accepts(peer, port, event_type))
                        .then(|| wire.from.clone())
                })
                .collect(),
        )
    };
    let peer_destination_of = |event_type: &str| -> Option<String> {
        majority(
            assembly
                .wires
                .iter()
                .filter_map(|wire| {
                    let (who, port) = parse_endpoint(&wire.from)?;
                    if who == instance {
                        return None;
                    }
                    let peer = registry.get(&assembly.instances.get(who)?.component)?;
                    (is_provider_peer(peer) && port_emits(peer, port, event_type))
                        .then(|| wire.to.clone())
                })
                .collect(),
        )
    };

    // Profile-first lookups: only instances whose component CLAIMS the
    // profile count, and only their same-named profile port — the standard
    // says which port is the tool port, no shape-guessing.
    let claims_tool_provider =
        |m: &ComponentManifest| -> bool { m.implements.iter().any(|p| p == "tool-provider") };
    let newcomer_claims = claims_tool_provider(manifest);
    let profile_source_feeding = |port_name: &str| -> Option<String> {
        majority(
            assembly
                .wires
                .iter()
                .filter_map(|wire| {
                    let (who, port) = parse_endpoint(&wire.to)?;
                    if who == instance || port != port_name {
                        return None;
                    }
                    let peer = registry.get(&assembly.instances.get(who)?.component)?;
                    claims_tool_provider(peer).then(|| wire.from.clone())
                })
                .collect(),
        )
    };
    let profile_destination_of = |port_name: &str| -> Option<String> {
        majority(
            assembly
                .wires
                .iter()
                .filter_map(|wire| {
                    let (who, port) = parse_endpoint(&wire.from)?;
                    if who == instance || port != port_name {
                        return None;
                    }
                    let peer = registry.get(&assembly.instances.get(who)?.component)?;
                    claims_tool_provider(peer).then(|| wire.to.clone())
                })
                .collect(),
        )
    };

    for port in &manifest.inputs {
        if covered_inputs.contains(&port.name) {
            continue;
        }
        if port.events.iter().any(|e| e == ce::TOOL_EXEC_STARTED) {
            let profile_first = (newcomer_claims && port.name == "execute")
                .then(|| profile_source_feeding("execute"))
                .flatten();
            let source = profile_first
                .or_else(|| peer_source_feeding(ce::TOOL_EXEC_STARTED))
                .unwrap_or_else(|| format!("{loop_instance}.run"));
            wires.push(Wire::new(&source, &format!("{instance}.{}", port.name)));
        }
    }
    for port in &manifest.outputs {
        if covered_outputs.contains(&port.name) {
            continue;
        }
        if port.events.iter().any(|e| e == ce::TOOL_EXEC_COMPLETED) {
            let profile_first = (newcomer_claims && port.name == "outcome")
                .then(|| profile_destination_of("outcome"))
                .flatten();
            let destination = profile_first
                .or_else(|| peer_destination_of(ce::TOOL_EXEC_COMPLETED))
                .unwrap_or_else(|| format!("{loop_instance}.tools"));
            wires.push(Wire::new(
                &format!("{instance}.{}", port.name),
                &destination,
            ));
        } else if port.events.iter().any(|e| e == ce::WAKE) {
            let destination =
                peer_destination_of(ce::WAKE).unwrap_or_else(|| format!("{loop_instance}.input"));
            wires.push(Wire::new(
                &format!("{instance}.{}", port.name),
                &destination,
            ));
        }
    }

    wires.dedup_by(|a, b| a.from == b.from && a.to == b.to);
    (wires, refused)
}

/// Service one `InstallComponentFrom` request: fetch the source, validate
/// its self-description against the canon, exam-grade it, and — with
/// consent — hot-install it wired like its peers and persist it into the
/// overlay. A rejected source leaves no trace under `components_dir`.
pub fn fetch_and_install(
    kernel: &mut Kernel,
    req: &PendingInstall,
    components_dir: &Path,
    loop_instance: &str,
    overlay: Option<&Path>,
    approved: impl FnOnce(&ComponentManifest, &Value) -> bool,
) -> std::io::Result<BuildOutcome> {
    let args = &req.args;
    let Some(source) = args["source"].as_str() else {
        return Ok(BuildOutcome::Rejected("no source was provided".to_string()));
    };

    std::fs::create_dir_all(components_dir)?;
    let staging = components_dir.join(".staging");
    let _ = std::fs::remove_dir_all(&staging);
    if let Err(e) = crate::fetch::fetch(source, &staging, &|| false) {
        let why = match e {
            crate::fetch::FetchError::Cancelled => "cancelled".to_string(),
            crate::fetch::FetchError::Failed(why) => why,
        };
        let _ = std::fs::remove_dir_all(&staging);
        return Ok(BuildOutcome::Rejected(format!("fetch failed: {why}")));
    }

    // The self-description, held to the canon before anything lands
    let reject = |staging: &Path, why: String| {
        let _ = std::fs::remove_dir_all(staging);
        Ok(BuildOutcome::Rejected(why))
    };
    let raw: Value = match std::fs::read_to_string(staging.join("component.json"))
        .map_err(|e| format!("the source has no readable component.json: {e}"))
        .and_then(|text| {
            serde_json::from_str(&text).map_err(|e| format!("component.json is not JSON: {e}"))
        }) {
        Ok(raw) => raw,
        Err(why) => return reject(&staging, why),
    };
    if let Err(why) = crate::overlay::validate_component(&raw) {
        return reject(&staging, why);
    }
    let mut manifest: ComponentManifest =
        serde_json::from_value(raw).expect("canon-valid manifests parse");
    if manifest.runtime != RuntimeKind::Process {
        return reject(
            &staging,
            "only process-form components install from a source (foreign code lives next door)"
                .to_string(),
        );
    }

    // Land under its own name, resolve `{dir}`, then grade it in place —
    // a failed exam removes what just landed
    let home = components_dir.join(&manifest.name);
    if home.exists() {
        return reject(
            &staging,
            format!("a component named {:?} is already installed", manifest.name),
        );
    }
    std::fs::rename(&staging, &home)?;
    manifest.entry = manifest.entry.replace("{dir}", &home.display().to_string());
    if manifest.implements.iter().any(|p| p == "tool-provider") {
        // Claiming to provide tools while providing none is a contradiction,
        // and reaching for the first of an empty list to find out was a panic
        // on the host thread — from the contents of a URL somebody approved.
        // The canon permits the combination (tools are optional, and the
        // claim is not tied to them), so it has to be caught here.
        let Some(probe) = manifest.tools.first().and_then(|t| t["name"].as_str()) else {
            let _ = std::fs::remove_dir_all(&home);
            return Ok(BuildOutcome::Rejected(
                "it claims the tool-provider profile but declares no tools".to_string(),
            ));
        };
        let problems = examine_tool_provider(&manifest, None, probe);
        if !problems.is_empty() {
            let _ = std::fs::remove_dir_all(&home);
            return Ok(BuildOutcome::Rejected(format!(
                "conformance exam failed: {}",
                problems.join("; ")
            )));
        }
    }
    if manifest.implements.iter().any(|p| p == "frontend") {
        let problems = crate::conformance::examine_frontend(&manifest, None);
        if !problems.is_empty() {
            let _ = std::fs::remove_dir_all(&home);
            return Ok(BuildOutcome::Rejected(format!(
                "frontend conformance exam failed: {}",
                problems.join("; ")
            )));
        }
    }

    let first_tool = manifest.tools.first().cloned().unwrap_or(Value::Null);
    if !approved(&manifest, &first_tool) {
        let _ = std::fs::remove_dir_all(&home);
        return Ok(BuildOutcome::Rejected("declined by the user".to_string()));
    }

    let instance = args["instance"]
        .as_str()
        .unwrap_or(&manifest.name)
        .to_string();
    let (wires, refused) = wire_the_newcomer(
        kernel.assembly(),
        kernel.component_registry(),
        &manifest,
        &instance,
        loop_instance,
    );
    // Blank is not absent: the model can send `"reason": ""`, and the ledger
    // refuses a decision event with no reason — which used to take the host
    // thread down with it, from a value a tool schema cannot forbid.
    let reason = args["reason"]
        .as_str()
        .map(str::trim)
        .filter(|r| !r.is_empty())
        .unwrap_or("user-requested component");
    match kernel.install(
        manifest.clone(),
        &instance,
        None,
        &wires,
        reason,
        &[&req.cause],
    ) {
        Ok(()) => {
            if let Some(overlay) = overlay {
                if let Err(problem) =
                    crate::overlay::record_install(overlay, &manifest, &instance, None, &wires)
                {
                    eprintln!(
                        "warning: install not persisted to {}: {problem}",
                        overlay.display()
                    );
                }
            }
            // The agent is told what its component asked for and did not get.
            // Silence here would read as "wired as requested".
            let note = if refused.is_empty() {
                String::new()
            } else {
                format!(
                    " (wiring refused, it may only connect its own ports to its own: {})",
                    refused.join("; ")
                )
            };
            Ok(BuildOutcome::Installed(format!(
                "installed {} from {}{note}",
                manifest.name, source
            )))
        }
        Err(err) => {
            let _ = std::fs::remove_dir_all(&home);
            Ok(BuildOutcome::Rejected(format!("install refused: {err}")))
        }
    }
}

/// The outcome of trying to build a requested component.
pub enum BuildOutcome {
    /// Installed and wired in; carries a one-line human summary
    Installed(String),
    /// Rejected before install; carries the reason (exam problems, etc.)
    Rejected(String),
}

/// Build and (if the exam passes and `approved`) install the requested
/// component. `dir` is where the source file is written. `loop_instance` is
/// the main loop's instance name — the new tool wires to its `Run`/`tools`
/// ports. Returns an outcome to be reported back as the tool result.
pub fn build_and_install(
    kernel: &mut Kernel,
    req: &PendingInstall,
    dir: &Path,
    loop_instance: &str,
    // The assembly overlay to persist the install into (None = this install
    // lives only as long as the process — tests, throwaway sessions)
    overlay: Option<&Path>,
    approved: impl FnOnce(&ComponentManifest, &Value) -> bool,
) -> std::io::Result<BuildOutcome> {
    let args = &req.args;
    let instance = args["instance"].as_str().unwrap_or("tool");
    if instance.is_empty()
        || !instance
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        return Ok(BuildOutcome::Rejected(
            "instance must contain only ASCII letters, digits, underscores or dashes".to_string(),
        ));
    }
    if kernel.assembly().instances.contains_key(instance) {
        return Ok(BuildOutcome::Rejected(format!(
            "instance {instance} already exists"
        )));
    }
    std::fs::create_dir_all(dir)?;
    let component_name = format!("agent:{instance}");
    // Preferred path: the agent writes only a pure handle() function and the
    // protocol wrapper is ours — the wire contract must not live in model
    // memory. A full `source` program remains the advanced escape hatch.
    let source = match (args["handler"].as_str(), args["source"].as_str()) {
        (Some(handler), _) => harness(args["tool_name"].as_str().unwrap_or_default(), handler),
        (None, Some(source)) => source.to_string(),
        (None, None) => {
            return Ok(BuildOutcome::Rejected(
                "neither handler nor source was provided".to_string(),
            ))
        }
    };

    let path = dir.join(format!("{instance}.py"));
    let mut source_file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)?;
    let mut pending_source = PendingSource {
        path: path.clone(),
        installed: false,
    };
    source_file.write_all(source.as_bytes())?;
    drop(source_file);

    let tool_decl = json!({
        "name": args["tool_name"],
        "description": args["tool_description"],
        "parameters": args["tool_parameters"],
    });
    let manifest = ComponentManifest {
        name: component_name.clone(),
        version: "0.0.0".to_string(),
        runtime: RuntimeKind::Process,
        entry: format!("python3 {}", path.display()),
        inputs: vec![PortDecl::new("execute", &[ce::TOOL_EXEC_STARTED])],
        outputs: vec![PortDecl::new("outcome", &[ce::TOOL_EXEC_COMPLETED])],
        events: Vec::new(),
        default_wiring: Vec::new(),
        capabilities: Some(EffectSurface::default()),
        implements: vec!["tool-provider".to_string()],
        // The agent-declared tool travels with the component it built —
        // hot-installing it makes the model see it on the very next ask
        tools: vec![tool_decl.clone()],
        prompt: None,
        handle_timeout_ms: Some(10_000),
        concurrency: None,
    };

    // The machine grades it: probe with the tool's own name
    let probe = args["tool_name"].as_str().unwrap_or("");
    let problems = examine_tool_provider(&manifest, None, probe);
    if !problems.is_empty() {
        return Ok(BuildOutcome::Rejected(format!(
            "conformance exam failed: {}",
            problems.join("; ")
        )));
    }

    // The human decides: agent-authored code installs only with consent
    if !approved(&manifest, &tool_decl) {
        return Ok(BuildOutcome::Rejected("declined by the user".to_string()));
    }

    // Wired like its peers: in a gated assembly the request line comes from
    // the gate's forward, not straight off the loop — a hot install must not
    // bypass the gates
    let wires = suggested_wires(
        kernel.assembly(),
        kernel.component_registry(),
        &manifest,
        instance,
        loop_instance,
    );
    let reason = args["reason"]
        .as_str()
        .map(str::trim)
        .filter(|r| !r.is_empty())
        .unwrap_or("agent-requested tool");
    match kernel.install(
        manifest.clone(),
        instance,
        None,
        &wires,
        reason,
        &[&req.cause],
    ) {
        Ok(()) => {
            pending_source.installed = true;
            // Installed means installed: write it into the overlay so it is
            // still here after a restart. A failed write degrades to a
            // process-lifetime install, said out loud — never a failed install.
            if let Some(overlay) = overlay {
                if let Err(problem) =
                    crate::overlay::record_install(overlay, &manifest, instance, None, &wires)
                {
                    eprintln!(
                        "warning: install not persisted to {}: {problem}",
                        overlay.display()
                    );
                }
            }
            Ok(BuildOutcome::Installed(format!(
                "installed {} offering tool {}",
                component_name,
                args["tool_name"].as_str().unwrap_or("?")
            )))
        }
        Err(err) => Ok(BuildOutcome::Rejected(format!("install refused: {err}"))),
    }
}

struct PendingSource {
    path: std::path::PathBuf,
    installed: bool,
}

impl Drop for PendingSource {
    fn drop(&mut self) {
        if !self.installed {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// Whether an event is the workshop's own install record (for display)
pub fn is_install_record(event: &EventEnvelope) -> bool {
    event.source == KERNEL_SOURCE && event.event_type == ce::COMPONENT_INSTALLED
}

/// Wrap an agent-authored `handle(tool, arguments)` function in the bridge
/// protocol. The wire contract stays in this template — the agent never has
/// to remember event type names or line formats.
fn harness(tool_name: &str, handler: &str) -> String {
    format!(
        r#"import json, sys

{handler}

TOOL = {tool}

for _line in sys.stdin:
    _line = _line.strip()
    if not _line:
        continue
    try:
        _msg = json.loads(_line)
    except ValueError:
        continue
    if _msg.get("stop"):
        break
    if "deliver" not in _msg:
        continue
    _event = _msg["deliver"]["event"]
    _payload = _event.get("payload") or {{}}
    if _payload.get("tool") != TOOL:
        # A foreign tool: emit nothing, but STILL report the delivery handled.
        # In-process components "stay silent" by returning from handle; across
        # a pipe silence is indistinguishable from still working, and the
        # bridge waits for this line before it will deliver anything else.
        # Without it, one component's tool call hangs every OTHER installed
        # component until the watchman kills them all.
        print(json.dumps({{"processed": True}}), flush=True)
        continue
    try:
        _result = handle(_payload.get("tool"), _payload.get("arguments") or {{}})
        _out = {{"call": _payload.get("call"), "status": "ok", "result": _result}}
    except Exception as _exc:
        _out = {{"call": _payload.get("call"), "status": "error", "error": {{
            "code": "tool.failed", "message": str(_exc), "blame": "request"}}}}
    print(json.dumps({{"emit": {{"port": "outcome", "type": "core.tool.exec_completed",
                               "causes": [_event["id"]], "payload": _out}}}}), flush=True)
    print(json.dumps({{"processed": True}}), flush=True)
"#,
        handler = handler,
        tool = serde_json::to_string(tool_name).expect("a string serializes"),
    )
}

/// Where a host keeps the things an install produces, and which instance a
/// newcomer wires itself next to. Built once by a host, then handed to every
/// run — the paths are a deployment's business, not the workshop's.
pub struct Workshop {
    /// Sources the agent writes itself land here
    pub workshop_dir: std::path::PathBuf,
    /// Ready-made components fetched from a source land here
    pub components_dir: std::path::PathBuf,
    /// The instance a newcomer with no peers falls back to wiring beside
    pub loop_instance: String,
    /// The overlay an install is persisted into, so it survives a restart.
    /// None means this install lives only as long as the process.
    pub overlay: Option<std::path::PathBuf>,
}

impl Workshop {
    /// Take an installed component out, on a human's say-so. The same rule as
    /// the model's tool: only what an install put here may go.
    pub fn remove(&self, kernel: &mut Kernel, instance: &str) -> Result<String, String> {
        let resolved = resolve_live_removable(kernel, instance)?.ok_or_else(|| {
            format!("\"{instance}\" has no installation provenance in this running assembly")
        })?;
        remove_installed(
            kernel,
            &resolved,
            self.overlay.as_deref(),
            "the user asked for it to be removed",
        )?;
        Ok(format!("removed {resolved}"))
    }

    /// The product's own locations, under the user's `~/.lattice`.
    pub fn standard() -> Self {
        let base = crate::overlay::default_path()
            .parent()
            .unwrap_or(Path::new("."))
            .to_path_buf();
        Self {
            workshop_dir: base.join("workshop"),
            components_dir: base.join("components"),
            loop_instance: "loop".to_string(),
            overlay: crate::preset::overlay_from_env(),
        }
    }

    /// Run the kernel to quiet, answering anything the agent asked to install
    /// and running AGAIN so it sees the result of what it asked for.
    ///
    /// Bounded: a component that somehow kept re-requesting could otherwise
    /// wedge the host in a loop no user could interrupt. Hitting the bound
    /// simply returns — the requests stay pending and the ledger shows why.
    pub fn run(&self, kernel: &mut Kernel) -> Result<(), crate::kernel::host::KernelError> {
        const ROUNDS: usize = 8;
        for _ in 0..ROUNDS {
            kernel.run_until_quiescent()?;
            if kernel.is_stopping() {
                return Ok(());
            }
            if !service(
                kernel,
                &self.workshop_dir,
                &self.components_dir,
                &self.loop_instance,
                self.overlay.as_deref(),
            )
            .map_err(|error| crate::kernel::host::KernelError::Io(std::io::Error::other(error)))?
            {
                return Ok(());
            }
        }
        kernel.run_until_quiescent()
    }
}

/// Answer every install request that has reached the workshop, then say
/// whether anything landed — the caller runs the kernel again if so, since the
/// agent has not yet seen the outcome of what it asked for.
///
/// This is the HOST's half of installing: the workshop component only declares
/// the tools and stays silent, because installing rewires the very kernel a
/// component runs inside. Both hosts that drive a kernel to quiet (the session
/// behind the terminal UI, and the daemon) call this at the same moment, so
/// "the agent can install a tool" is not a property of one frontend.
///
/// **Consent has already happened.** In the standard assembly these calls are
/// marked as admitting new code, so the trust gate stopped them and asked the
/// human, and [`pending_installs`] only surfaces what the sink has actually
/// witnessed — that is, what was let through. The machine checks (the canon
/// schema and the conformance exam) still run here and still reject.
/// What an overlay installed: each instance and the tools it brought.
///
/// Disk inventory for display only. This file can change after startup and
/// does not prove that any named instance was installed into the live kernel.
/// Removal authorization uses running provenance in `resolve_live_removable`.
pub fn installed_instances(overlay: Option<&Path>) -> Vec<(String, Vec<String>)> {
    let Some(text) = overlay.and_then(|path| std::fs::read_to_string(path).ok()) else {
        return Vec::new();
    };
    let Ok(overlay) = serde_json::from_str::<Value>(&text) else {
        return Vec::new();
    };
    let tools_of = |component: &str| -> Vec<String> {
        overlay["components"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|c| c["name"] == component)
            .and_then(|c| c["tools"].as_array().cloned())
            .unwrap_or_default()
            .iter()
            .filter_map(|t| t["name"].as_str().map(str::to_string))
            .collect()
    };
    overlay["instances"]
        .as_object()
        .into_iter()
        .flatten()
        .map(|(instance, spec)| {
            let tools = spec["component"].as_str().map(tools_of).unwrap_or_default();
            (instance.clone(), tools)
        })
        .collect()
}

/// Resolve a name in the disk inventory; this is NOT removal authorization.
///
/// Accepts a TOOL name as readily as an instance name, because a model has no
/// way to know instance names — the tool list it is given carries tool names
/// and nothing else, so asking it for an instance was asking for something it
/// cannot see. It named the tools it knew, was refused, and concluded that
/// nothing had been installed at all.
pub fn resolve_removable(overlay: Option<&Path>, named: &str) -> Option<String> {
    let installed = installed_instances(overlay);
    if let Some((instance, _)) = installed.iter().find(|(instance, _)| instance == named) {
        return Some(instance.clone());
    }
    installed
        .iter()
        .find(|(_, tools)| tools.iter().any(|tool| tool == named))
        .map(|(instance, _)| instance.clone())
}

/// What a refusal should say instead of just "no": a request that names
/// something unremovable is usually a request that guessed the wrong name, and
/// the list is the one thing that lets the asker fix it on the next try.
fn removable_summary(kernel: &Kernel) -> Result<String, String> {
    let provenance = live_installed(kernel)?;
    let installed: Vec<_> = kernel
        .assembly()
        .instances
        .keys()
        .filter(|name| provenance.contains(*name))
        .cloned()
        .collect();
    if installed.is_empty() {
        return Ok("nothing has been installed into this assembly".to_string());
    }
    Ok(format!("what can be removed: {}", installed.join("; ")))
}

pub fn service(
    kernel: &mut Kernel,
    workshop_dir: &Path,
    components_dir: &Path,
    loop_instance: &str,
    overlay: Option<&Path>,
) -> Result<bool, String> {
    let fetches = pending_fetch_installs(kernel)?;
    let builds = pending_installs(kernel)?;
    let removals = pending_removals(kernel)?;
    if fetches.is_empty() && builds.is_empty() && removals.is_empty() {
        return Ok(false);
    }
    for req in removals {
        let outcome = remove(kernel, &req, overlay);
        answer(kernel, UNINSTALL_TOOL, &req, Ok(outcome));
    }
    for req in fetches {
        let outcome = fetch_and_install(
            kernel,
            &req,
            components_dir,
            loop_instance,
            overlay,
            |_, _| true,
        );
        answer(kernel, INSTALL_FROM_TOOL, &req, outcome);
    }
    for req in builds {
        let outcome = build_and_install(
            kernel,
            &req,
            workshop_dir,
            loop_instance,
            overlay,
            |_, _| true,
        );
        answer(kernel, INSTALL_TOOL, &req, outcome);
    }
    Ok(true)
}

/// The same provenance feeds enforcement and the host's component display.
pub(crate) fn live_installed(kernel: &Kernel) -> Result<std::collections::HashSet<String>, String> {
    let mut installed = std::collections::HashSet::new();
    if let Some(spec) = kernel.assembly().instances.get("workshop") {
        if spec.component == crate::components::workshop_sink::NAME {
            installed.extend(
                spec.config
                    .as_ref()
                    .and_then(|c| c["installedInstances"].as_array())
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .map(str::to_string),
            );
        }
    }
    let reader = kernel.log().reader();
    let boundary = reader
        .scan_back_types(&[ce::STREAM_OPENED, ce::STREAM_RESUMED], |event, _| {
            Ok((event.source == KERNEL_SOURCE).then_some(event.seq))
        })
        .map_err(|e| e.to_string())?
        .unwrap_or(0);
    let mut seen = std::collections::HashSet::new();
    reader
        .scan_back_types(
            &[ce::COMPONENT_INSTALLED, ce::COMPONENT_REMOVED],
            |event, _| {
                if event.seq > boundary && event.source == KERNEL_SOURCE {
                    if let Some(name) = event.payload["instance"].as_str() {
                        // Reverse traversal: only the latest change for each instance wins.
                        if seen.insert(name.to_string()) {
                            if event.event_type == ce::COMPONENT_INSTALLED {
                                installed.insert(name.to_string());
                            } else {
                                installed.remove(name);
                            }
                        }
                    }
                }
                Ok(None::<()>)
            },
        )
        .map_err(|e| e.to_string())?;
    Ok(installed)
}

/// Resolve against the running assembly's provenance, not editable disk claims.
fn resolve_live_removable(kernel: &Kernel, named: &str) -> Result<Option<String>, String> {
    let installed = live_installed(kernel)?;
    Ok(kernel.assembly().instances.iter().find_map(|(name, spec)| {
        if !installed.contains(name) {
            return None;
        }
        let matches = name == named
            || kernel
                .component_registry()
                .get(&spec.component)
                .is_some_and(|m| m.tools.iter().any(|tool| tool["name"] == named));
        matches.then(|| name.clone())
    }))
}

fn remove_installed(
    kernel: &mut Kernel,
    instance: &str,
    overlay: Option<&Path>,
    reason: &str,
) -> Result<(), String> {
    // With exclusive access to the kernel, uninstall's only rejection (an
    // absent instance) is checked before committing persistence. A failed
    // disk write leaves the live instance untouched.
    if !kernel.assembly().instances.contains_key(instance) {
        return Err(format!("no instance named {instance} is assembled"));
    }
    if let Some(path) = overlay {
        crate::overlay::record_removal(path, instance)?;
    }
    kernel.uninstall(instance, reason, &[])
}

/// Take a component out, if it is one that may go.
fn remove(kernel: &mut Kernel, req: &PendingInstall, overlay: Option<&Path>) -> BuildOutcome {
    let Some(instance) = req.args["instance"].as_str() else {
        return BuildOutcome::Rejected("no instance was named".to_string());
    };
    let instance = match resolve_live_removable(kernel, instance) {
        Ok(Some(instance)) => instance,
        Err(error) => {
            return BuildOutcome::Rejected(format!(
                "cannot verify installation provenance: {error}"
            ))
        }
        Ok(None) => {
            let summary = match removable_summary(kernel) {
                Ok(summary) => summary,
                Err(error) => {
                    return BuildOutcome::Rejected(format!(
                        "cannot read installation provenance: {error}"
                    ))
                }
            };
            return BuildOutcome::Rejected(format!(
                "\"{instance}\" was not installed into this assembly, so it cannot be removed \
                 — the assembly you were started with is not yours to change. {summary}"
            ));
        }
    };
    let instance = instance.as_str();
    let why = req.args["reason"]
        .as_str()
        .map(str::trim)
        .filter(|r| !r.is_empty())
        .unwrap_or("no reason given");
    if let Err(problem) = remove_installed(kernel, instance, overlay, why) {
        return BuildOutcome::Rejected(problem);
    }
    BuildOutcome::Installed(format!("removed: {instance}"))
}

/// Report an install's fate back to the agent as an ordinary tool result, so a
/// refusal reaches the model the same way any other tool failure does — it can
/// read the reason and decide what to do, which is the whole retry stance.
fn answer(
    kernel: &mut Kernel,
    tool: &str,
    req: &PendingInstall,
    outcome: std::io::Result<BuildOutcome>,
) {
    let payload = match outcome {
        Ok(BuildOutcome::Installed(message)) => {
            json!({"call": req.call, "status": "ok", "result": message})
        }
        Ok(BuildOutcome::Rejected(why)) => json!({
            "call": req.call, "status": "error",
            "error": {"code": "workshop.rejected", "message": why, "blame": "request"},
        }),
        // Disk trouble is the environment's fault, and it may well pass
        Err(err) => json!({
            "call": req.call, "status": "error",
            "error": {"code": "workshop.io", "message": err.to_string(),
                      "blame": "environment", "transient": true},
        }),
    };
    // Answer AS the sink: the completion must come from the instance the
    // request was delivered to, or the witness rule rightly rejects a cause
    // pointing at an event this speaker never saw.
    let Some(sink) = sink_instance(kernel, tool) else {
        return;
    };
    kernel.injector(&sink).emit(
        "outcome",
        crate::contracts::event::EventDraft::new(ce::TOOL_EXEC_COMPLETED, &[&req.cause], payload),
    );
}
