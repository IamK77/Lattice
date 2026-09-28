use std::collections::HashMap;

use crate::contracts::assembly::{parse_endpoint, AssemblyManifest, Wire};

/// Router (the mail carrier) — kernel duty #3.
/// Looks up the table, never reads the letter: which input ports should receive
/// an event emitted from a given instance's output port.
/// Delivery is implemented in `host.rs` (mailbox concurrency, one thread and
/// FIFO mailbox per component instance). Decided-but-unbuilt semantics:
/// observer queues bounded, overflow switches to catch-up-from-log.
pub struct Router {
    table: HashMap<String, Vec<(String, String)>>,
}

impl Router {
    pub fn new(assembly: &AssemblyManifest) -> Self {
        let mut table: HashMap<String, Vec<(String, String)>> = HashMap::new();
        for wire in &assembly.wires {
            // Inspection rejects malformed endpoints; no double enforcement here
            let (Some(from), Some(to)) = (parse_endpoint(&wire.from), parse_endpoint(&wire.to))
            else {
                continue;
            };
            table
                .entry(format!("{}.{}", from.0, from.1))
                .or_default()
                .push((to.0.to_string(), to.1.to_string()));
        }
        Self { table }
    }

    /// Extend the table with one wire (hot install)
    pub fn add_wire(&mut self, wire: &Wire) {
        let (Some(from), Some(to)) = (parse_endpoint(&wire.from), parse_endpoint(&wire.to)) else {
            return; // inspection rejects malformed endpoints before this
        };
        self.table
            .entry(format!("{}.{}", from.0, from.1))
            .or_default()
            .push((to.0.to_string(), to.1.to_string()));
    }

    /// Forget every wire that touches `instance`, in either direction — what
    /// a removal needs, and the exact inverse of the `add_wire` calls an
    /// install made. Nothing may still be routed to a component that is gone,
    /// and nothing may still be routed FROM its ports either.
    pub fn forget_instance(&mut self, instance: &str) {
        let leaving = format!("{instance}.");
        self.table.retain(|from, _| !from.starts_with(&leaving));
        for destinations in self.table.values_mut() {
            destinations.retain(|(dest, _)| dest != instance);
        }
        self.table
            .retain(|_, destinations| !destinations.is_empty());
    }

    pub fn routes_from(&self, instance: &str, port: &str) -> &[(String, String)] {
        self.table
            .get(&format!("{instance}.{port}"))
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }
}
