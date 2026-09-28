use std::sync::{Arc, Mutex};

use crate::contracts::component::{ComponentManifest, PortDecl, RuntimeKind};
use crate::contracts::core_events as ce;
use crate::contracts::event::{EventEnvelope, EventTypeDecl};
use crate::kernel::host::{Component, Ctx};

pub const NAME: &str = "silent-ui";

/// The user added or deleted a model in the catalog.
///
/// A frontend event and not a core one: the kernel does not know what a model
/// catalog is, and teaching it would be the first special hook for a layer
/// above it. The panel that edits the file is the frontend's, so the record of
/// the edit is the frontend's too.
///
/// It exists because the edit is otherwise invisible. Deleting is by decision
/// a real delete with no backup, so afterwards nothing on disk and nothing on
/// the ledger says which entry was there — the file simply has one fewer, and
/// whoever asks later (the agent reading its own ledger, or a person) has no
/// way to find out even the name.
pub const CATALOG_CHANGED: &str = "ui.model_catalog_changed";

/// What the turn that just ended cost the frontend to draw.
///
/// Not a decision — nothing was chosen, this is an observation — so it carries
/// no reason. One entry per TURN, never per frame: a frame is not a completed
/// state in the sense the ledger means, and twenty entries a second would bury
/// the record they exist to make readable.
///
/// It is here because the frontend was the one part of the runtime whose work
/// never reached the ledger at all. "It got slow" could only be noticed by a
/// person at the keyboard, while it was happening; afterwards nothing knew.
pub const RENDER_COST: &str = "ui.render_cost";

/// Submission through the first completed frame after message absorption.
pub const INPUT_COST: &str = "ui.input_cost";

/// One completed launch, measured from main entry through the first TUI draw.
pub const STARTUP_COST: &str = "ui.startup_cost";

/// Completed frontend cleanup, excluding this observation's own final write.
pub const SHUTDOWN_COST: &str = "ui.shutdown_cost";

pub fn manifest() -> ComponentManifest {
    ComponentManifest {
        name: NAME.to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        runtime: RuntimeKind::Inproc,
        entry: format!("builtin:{NAME}"),
        inputs: vec![PortDecl::new(
            "display",
            &[ce::OUTPUT_REPLY, ce::TURN_COMPLETED],
        )],
        outputs: vec![
            PortDecl::new("user", &[ce::USER_MESSAGE]),
            PortDecl::new("interrupt", &[ce::INTERRUPTED]),
            // The user's answers to authorization requests (and any other
            // out-of-band frontend input) leave here as external input — a
            // core type, so this port is legal in gate-less assemblies too
            PortDecl::new("answer", &[ce::EXTERNAL_INPUT]),
            // Nothing is wired to these: the point of these events is that they
            // are on the ledger, not that anybody is told
            PortDecl::new("catalog", &[CATALOG_CHANGED]),
            PortDecl::new(
                "stats",
                &[RENDER_COST, STARTUP_COST, SHUTDOWN_COST, INPUT_COST],
            ),
        ],
        events: vec![
            EventTypeDecl::decision(
                CATALOG_CHANGED,
                "The user added or deleted a model in the catalog",
            ),
            EventTypeDecl::new(
                RENDER_COST,
                "What the turn that just ended cost the frontend to draw",
            ),
            EventTypeDecl::new(STARTUP_COST, "Startup through the first completed TUI draw"),
            EventTypeDecl::new(
                INPUT_COST,
                "Submission through first successful frame of the user message",
            ),
            EventTypeDecl::new(
                SHUTDOWN_COST,
                "Frontend cleanup through the final index update",
            ),
        ],
        default_wiring: Vec::new(),
        capabilities: None,
        // The minimum frontend claim, plus the optional capabilities the
        // sessions driving this component (TUI keys, daemon clients)
        // provide: answering authorization requests, and a `/` palette
        // folded from the ledger's skill listings
        implements: vec![
            "frontend".to_string(),
            "frontend-authorize".to_string(),
            "frontend-palette".to_string(),
        ],
        tools: Vec::new(),
        prompt: None,
        handle_timeout_ms: None,
        concurrency: None,
    }
}

/// A headless frontend: user input is injected from outside (tests, examples),
/// and whatever it is asked to display is recorded into a shared buffer.
pub struct SilentUi {
    displayed: Arc<Mutex<Vec<String>>>,
}

impl SilentUi {
    pub fn new(displayed: Arc<Mutex<Vec<String>>>) -> Self {
        Self { displayed }
    }
}

impl Component for SilentUi {
    fn handle(&mut self, _port: &str, event: &EventEnvelope, _ctx: &mut Ctx) {
        if let Some(text) = event.payload["text"].as_str() {
            self.displayed
                .lock()
                .expect("display buffer lock poisoned")
                .push(text.to_string());
        }
    }
}
