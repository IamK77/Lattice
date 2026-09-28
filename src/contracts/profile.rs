use serde::{Deserialize, Serialize};

use super::component::{ComponentManifest, PortDecl};
use super::core_events as ce;

/// A standard port profile — the "socket shape" of a class of interchangeable
/// components. A component claiming a profile must carry every profile port
/// (same name, same direction, exactly the same event set); extra ports are
/// allowed. Swapping components of one profile changes a component name in
/// the assembly manifest and not a single wire.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PortProfile {
    pub name: String,
    pub version: String,
    pub inputs: Vec<PortDecl>,
    pub outputs: Vec<PortDecl>,
}

/// The profiles the core defines. Components declare claims via
/// `ComponentManifest::implements`; inspection verifies them structurally,
/// conformance exams (src/conformance.rs) verify them behaviorally.
pub fn core_profiles() -> Vec<PortProfile> {
    vec![
        PortProfile {
            name: "model-adapter".to_string(),
            version: "1".to_string(),
            inputs: vec![PortDecl::new("request", &[ce::MODEL_CALL_STARTED])],
            outputs: vec![PortDecl::new("result", &[ce::MODEL_CALL_COMPLETED])],
        },
        // Any component offering tools: requests in, completions out.
        // Convention under fan-out wiring: answer only your own tools,
        // stay silent on foreign ones (the exam probes a declared tool).
        PortProfile {
            name: "tool-provider".to_string(),
            version: "1".to_string(),
            inputs: vec![PortDecl::new("execute", &[ce::TOOL_EXEC_STARTED])],
            outputs: vec![PortDecl::new("outcome", &[ce::TOOL_EXEC_COMPLETED])],
        },
        // The gate: receives tool requests, forwards the allowed ones
        // unchanged (a re-emission with a causal link — the hop is meant to
        // be audit-visible), and answers denied ones itself so the loop
        // needs no changes. Its own decision letter type is component-owned
        // and therefore not part of the generic profile.
        PortProfile {
            name: "policy".to_string(),
            version: "1".to_string(),
            inputs: vec![PortDecl::new("review", &[ce::TOOL_EXEC_STARTED])],
            outputs: vec![
                PortDecl::new("forward", &[ce::TOOL_EXEC_STARTED]),
                PortDecl::new("verdict", &[ce::TOOL_EXEC_COMPLETED]),
            ],
        },
        // The context manager: the thin pipe on the ask wire. Receives a
        // model call, forwards it (a re-emission with a causal link) with
        // the material possibly rewritten — trimmed under budget, digest
        // parts in place of originals — and a valid fingerprint over the
        // parts it actually forwards. The compaction decision letter type
        // is component-owned, not part of the generic profile.
        PortProfile {
            name: "context-manager".to_string(),
            version: "1".to_string(),
            inputs: vec![PortDecl::new("ask", &[ce::MODEL_CALL_STARTED])],
            outputs: vec![PortDecl::new("forward", &[ce::MODEL_CALL_STARTED])],
        },
        // The conversation surface toward the human — the MINIMUM any
        // frontend must speak: replies and turn boundaries in, user messages
        // out. Anything beyond that (answering authorization, interrupting)
        // is a separate capability claim, so its absence is visible rather
        // than assumed.
        PortProfile {
            name: "frontend".to_string(),
            version: "1".to_string(),
            inputs: vec![PortDecl::new(
                "display",
                &[ce::OUTPUT_REPLY, ce::TURN_COMPLETED],
            )],
            outputs: vec![PortDecl::new("user", &[ce::USER_MESSAGE])],
        },
        // The optional frontend capability of answering authorization
        // requests (the trust gate's y/n). Rides the core external-input
        // letter, so the port is legal in gate-less assemblies. No input
        // half: requests reach frontends by ledger observation, not a port.
        // An assembly whose gate stance is "ask" needs SOME instance with
        // this claim wired to the gate — the assembler enforces that
        // combination (the kernel does not know what "trust" means).
        PortProfile {
            name: "frontend-authorize".to_string(),
            version: "1".to_string(),
            inputs: vec![],
            outputs: vec![PortDecl::new("answer", &[ce::EXTERNAL_INPUT])],
        },
        // The optional frontend capability of offering a palette: the
        // frontend folds skill.listing events off the ledger into a `/`
        // menu, and invocations leave through the ordinary `user` port as
        // plain text — so this claim needs no ports of its own (a pure
        // marker, like a marker trait). Its absence is cosmetic: the menu
        // does not appear, but a typed /skill-name still expands server-side.
        PortProfile {
            name: "frontend-palette".to_string(),
            version: "1".to_string(),
            inputs: vec![],
            outputs: vec![],
        },
    ]
}

/// Structural check of one profile claim. Returns problems; empty = holds.
pub fn check_claim(manifest: &ComponentManifest, profile: &PortProfile) -> Vec<String> {
    let mut problems = Vec::new();
    let mut check = |wanted: &[PortDecl], have: &[PortDecl], direction: &str| {
        fn same_events(a: &[String], b: &[String]) -> bool {
            let mut a: Vec<&str> = a.iter().map(String::as_str).collect();
            let mut b: Vec<&str> = b.iter().map(String::as_str).collect();
            a.sort_unstable();
            a.dedup();
            b.sort_unstable();
            b.dedup();
            a == b
        }
        for port in wanted {
            match have.iter().find(|p| p.name == port.name) {
                None => problems.push(format!(
                    "missing {direction} port {} required by profile {}",
                    port.name, profile.name
                )),
                // The event SET, as the contract says — compared as one, not
                // as a list in a particular order. A foreign component's
                // author writes its self-description by hand against the
                // canon, where the field is a set; listing the same two types
                // the other way round was refused, with a message showing two
                // lists that read as identical.
                Some(actual) if !same_events(&actual.events, &port.events) => {
                    problems.push(format!(
                        "{direction} port {} deviates from profile {}: wants {:?}, has {:?}",
                        port.name, profile.name, port.events, actual.events
                    ))
                }
                Some(_) => {}
            }
        }
    };
    check(&profile.inputs, &manifest.inputs, "input");
    check(&profile.outputs, &manifest.outputs, "output");
    problems
}
