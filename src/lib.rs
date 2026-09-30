pub mod checkpoint;
pub mod components;
pub mod conformance;
pub mod contracts;
pub mod daemon;
mod derived_pages;
pub mod edit_diff;
pub mod editor;
pub mod experts;
mod fetch;
pub mod input_latency;
pub mod kernel;
pub mod mathtext;
pub mod models;
pub mod overlay;
pub mod preferences;
pub mod preset;
pub mod product_assembly;
pub mod profile;
pub mod prompts;
pub mod recovery;

/// The Cargo package version, prefixed with `v`. Ordinary builds add a `dev`
/// prerelease identifier and, when available, the source commit as metadata.
/// Official release builds explicitly confirm the version through `build.rs`.
pub const VERSION: &str = env!("LATTICE_VERSION");
pub mod richtext;
pub mod session;
pub mod shutdown;
pub mod startup;
pub mod subagent_host;
pub mod view;
pub mod workshop;
pub mod wrap;

pub use contracts::assembly::{parse_endpoint, AssemblyManifest, ComponentInstance, Wire};
pub use contracts::component::{
    deferred_dispatcher_decl, ComponentManifest, EffectSurface, PortDecl, RuntimeKind,
    WireSuggestion, DEFERRED_DISPATCHER,
};
pub use contracts::core_events;
pub mod ledgers;
pub mod memory;
pub use contracts::document;
pub use contracts::event::{EventDraft, EventEnvelope, EventTypeDecl, StreamRef, ENVELOPE_VERSION};
pub use contracts::profile::{check_claim, core_profiles, PortProfile};
pub use daemon::{ClientMessage, ServerMessage};
pub use editor::Editor;
pub use kernel::compact::{compact, export, Compacted, Exported};
pub use kernel::host::{
    run_bridge_child, Component, Ctx, Factory, ForeignReaders, Injector, Kernel, KernelError,
    KernelOptions, KERNEL_SOURCE,
};
pub use kernel::inspect::{inspect_assembly, InspectionIssue};
pub use kernel::log::{AuditViolation, EventLog, LogReader, Redactor};
pub use kernel::router::Router;
pub use kernel::stream_host::{LedgerPath, StreamHost, StreamTemplate};
pub use session::{
    render_line, CatalogNote, FrontendCommand, RenderCostNote, RenderEvent, Session,
};
pub use tokio_util::sync::CancellationToken;
pub use view::{
    ingest, Assembled, Entry, Material, ToolCard, ToolStatus, Usage, UsageReport, View,
};
