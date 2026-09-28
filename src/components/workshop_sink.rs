//! The workshop socket: a silent receiver of tool requests whose output the
//! host fills in after building the requested component (see src/workshop.rs).
//! It witnesses install requests so the host may answer them in its name.

use crate::contracts::component::{ComponentManifest, PortDecl, RuntimeKind};
use crate::contracts::core_events as ce;
use crate::contracts::event::EventEnvelope;
use crate::kernel::host::{Component, Ctx};

pub const NAME: &str = "workshop";

pub fn manifest() -> ComponentManifest {
    ComponentManifest {
        name: NAME.to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        runtime: RuntimeKind::Inproc,
        entry: format!("builtin:{NAME}"),
        inputs: vec![PortDecl::new("execute", &[ce::TOOL_EXEC_STARTED])],
        outputs: vec![PortDecl::new("outcome", &[ce::TOOL_EXEC_COMPLETED])],
        events: Vec::new(),
        default_wiring: Vec::new(),
        capabilities: None,
        implements: vec!["tool-provider".to_string()],
        tools: vec![
            crate::workshop::install_tool_decl(),
            crate::workshop::install_from_tool_decl(),
            crate::workshop::uninstall_tool_decl(),
        ],
        // No fragment: install_component's own schema already spells out the
        // handler contract, word for word.
        prompt: None,
        handle_timeout_ms: None,
        concurrency: None,
    }
}

/// Silent on every request — the host performs the real install between runs
/// and injects the answer through this instance.
pub struct WorkshopSink;

impl Component for WorkshopSink {
    fn handle(&mut self, _port: &str, _event: &EventEnvelope, _ctx: &mut Ctx) {}
}
