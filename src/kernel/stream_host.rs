//! The multi-stream host — one layer above the kernel.
//!
//! The kernel is unchanged: it is still the whole of ONE stream (one ledger,
//! one set of component instances, four enforcement layers). This host owns a
//! table — stream id → that stream's kernel — and does nothing but route: it
//! never runs a component or reads an event's content.
//!
//! Streams are isolated by construction ("one matter, one dossier"): each has
//! its own ledger, its own instances, its own memory. The only cross-stream
//! tie is a weak `origin` reference plus read-only observation.
//!
//! v1 is "one core, many streams": every stream's kernel lives in this one
//! process. Form two (one subprocess per stream, physical isolation) is a
//! future swap behind this same interface, reusing the cross-process bridge.

use std::collections::HashMap;
use std::path::PathBuf;

use serde_json::Value;

use crate::contracts::assembly::AssemblyManifest;
use crate::contracts::component::ComponentManifest;
use crate::contracts::event::EventDraft;
use crate::kernel::host::{ForeignReaders, Injector, Kernel, KernelError, KernelOptions};
use crate::kernel::log::LogReader;

/// A named recipe for building a stream: the same registry + factories +
/// assembly, instantiated fresh (its own component instances) per stream.
/// Different templates suit different frontends (a terminal socket vs a
/// Telegram socket).
pub struct StreamTemplate {
    pub registry: HashMap<String, ComponentManifest>,
    pub factories: HashMap<String, crate::kernel::host::Factory>,
    pub assembly: AssemblyManifest,
}

/// Where a stream's ledger is written, given its id (None = memory only)
pub type LedgerPath = Box<dyn Fn(&str) -> Option<PathBuf>>;

/// The multi-stream host.
pub struct StreamHost {
    templates: HashMap<String, StreamTemplate>,
    streams: HashMap<String, Kernel>,
    ledger_path: LedgerPath,
    /// Passed to every stream this host opens (see
    /// [`KernelOptions::child_env_deny`])
    child_env_deny: Vec<String>,
    /// Passed to every stream this host opens (see [`KernelOptions::redact`])
    redact: Vec<String>,
    stream_note: Option<Value>,
}

impl StreamHost {
    pub fn new(templates: HashMap<String, StreamTemplate>) -> Self {
        Self {
            templates,
            streams: HashMap::new(),
            ledger_path: Box::new(|_| None),
            child_env_deny: Vec::new(),
            redact: Vec::new(),
            stream_note: None,
        }
    }

    /// Variables no component subprocess of any stream may inherit.
    pub fn withholding(mut self, names: Vec<String>) -> Self {
        self.child_env_deny = names;
        self
    }

    /// Strings no stream's ledger may record (see `KernelOptions::redact`).
    pub fn redacting(mut self, secrets: Vec<String>) -> Self {
        self.redact = secrets;
        self
    }

    /// Choose where each stream's ledger file goes (default: memory only)
    pub fn with_ledger_path(mut self, f: impl Fn(&str) -> Option<PathBuf> + 'static) -> Self {
        self.ledger_path = Box::new(f);
        self
    }

    /// What this host says about itself on every stream it opens — which
    /// program, which model (see `KernelOptions::stream_note`). The kernel
    /// records what it can see; this is what only the host knows.
    pub fn noting(mut self, note: Value) -> Self {
        self.stream_note = Some(note);
        self
    }

    /// Open a fresh stream from a template. Fails if the id is taken, the
    /// template is unknown, or the assembly fails inspection.
    pub fn open(&mut self, stream_id: &str, template: &str) -> Result<(), KernelError> {
        self.open_inner(stream_id, template, ForeignReaders::default(), |_| {})
    }

    /// Like `open`, but runs `configure` on the freshly built kernel before it
    /// starts serving — the daemon uses this to wire log subscribers and the
    /// notice handler for broadcasting.
    pub fn open_with(
        &mut self,
        stream_id: &str,
        template: &str,
        configure: impl FnOnce(&mut Kernel),
    ) -> Result<(), KernelError> {
        self.open_inner(stream_id, template, ForeignReaders::default(), configure)
    }

    /// Open a derived stream (a sidechannel). It observes `parent` as a
    /// read-only observer — its components may read the parent ledger via
    /// `ctx.foreign_log(parent)`, and by construction cannot emit into it.
    /// The caller stamps `origin` on the derived stream's first injected
    /// event (see `derived_root`) to record the provenance.
    pub fn open_derived(
        &mut self,
        stream_id: &str,
        template: &str,
        parent: &str,
    ) -> Result<(), KernelError> {
        self.open_derived_with(stream_id, template, parent, |_| {})
    }

    /// Like `open_derived`, with a `configure` hook on the fresh kernel (the
    /// daemon uses it to wire broadcast for the sidechannel stream too).
    pub fn open_derived_with(
        &mut self,
        stream_id: &str,
        template: &str,
        parent: &str,
        configure: impl FnOnce(&mut Kernel),
    ) -> Result<(), KernelError> {
        let parent_reader = self
            .streams
            .get(parent)
            .map(|k| k.log().reader())
            .ok_or_else(|| {
                KernelError::Inspection(vec![crate::kernel::inspect::InspectionIssue {
                    location: format!("stream {stream_id}"),
                    problem: format!("derives from unknown parent stream: {parent}"),
                }])
            })?;
        let mut foreign = HashMap::new();
        foreign.insert(parent.to_string(), parent_reader);
        self.open_observing(stream_id, template, foreign, configure)
    }

    /// Open a stream that observes the given foreign ledgers read-only. This
    /// is the general form behind `open_derived_with`, for callers that hold
    /// reader handles themselves (e.g. the daemon, whose kernels live on
    /// per-stream driver threads rather than in this host's table).
    pub fn open_observing(
        &mut self,
        stream_id: &str,
        template: &str,
        foreign: HashMap<String, LogReader>,
        configure: impl FnOnce(&mut Kernel),
    ) -> Result<(), KernelError> {
        self.open_inner(stream_id, template, ForeignReaders::new(foreign), configure)
    }

    /// Hand a freshly opened stream's kernel to the caller, forgetting the
    /// stream here. The caller now owns its lifecycle (running, shutdown) —
    /// the daemon does this to give every stream its own driver thread, so
    /// one stream's long turn never queues another stream's work. Callers
    /// must guard against reopening the id themselves.
    pub fn take(&mut self, stream_id: &str) -> Option<Kernel> {
        self.streams.remove(stream_id)
    }

    /// Startup policy shared by named templates and product-owned captured recipes.
    pub(crate) fn kernel_options(&self, stream_id: &str) -> KernelOptions {
        KernelOptions {
            stream: Some(stream_id.to_string()),
            log_file: (self.ledger_path)(stream_id),
            child_env_deny: self.child_env_deny.clone(),
            redact: self.redact.clone(),
            stream_note: self.stream_note.clone(),
            ..KernelOptions::default()
        }
    }

    fn open_inner(
        &mut self,
        stream_id: &str,
        template: &str,
        foreign: ForeignReaders,
        configure: impl FnOnce(&mut Kernel),
    ) -> Result<(), KernelError> {
        if self.streams.contains_key(stream_id) {
            return Err(KernelError::Inspection(vec![
                crate::kernel::inspect::InspectionIssue {
                    location: format!("stream {stream_id}"),
                    problem: "a stream with this id is already open".to_string(),
                },
            ]));
        }
        let options = self.kernel_options(stream_id);
        let Some(template) = self.templates.get_mut(template) else {
            return Err(KernelError::MissingFactory(format!(
                "unknown stream template: {template}"
            )));
        };
        let mut kernel = Kernel::start_with_foreign(
            &template.assembly,
            &template.registry,
            &mut template.factories,
            options,
            foreign,
        )?;
        configure(&mut kernel);
        self.streams.insert(stream_id.to_string(), kernel);
        Ok(())
    }

    /// Where a stream's ledger is (or would be) written. Callers that hand a
    /// stream's kernel to a thread of its own still need to be able to say
    /// where the record of it lives.
    pub fn ledger_for(&self, stream_id: &str) -> Option<PathBuf> {
        (self.ledger_path)(stream_id)
    }

    /// Close and seal a stream (shuts down its components' threads).
    pub fn close(&mut self, stream_id: &str) {
        if let Some(kernel) = self.streams.remove(stream_id) {
            kernel.shutdown();
        }
    }

    /// Inject into a specific stream's instance (bound to that instance name).
    /// None if the stream is not open.
    pub fn injector(&self, stream_id: &str, instance: &str) -> Option<Injector> {
        self.streams.get(stream_id).map(|k| k.injector(instance))
    }

    /// A root event for a derived stream, carrying `origin` back to the
    /// event in the parent that provoked it — the audit link across streams.
    pub fn derived_root(
        event_type: &str,
        parent: &str,
        parent_event: &str,
        payload: Value,
    ) -> EventDraft {
        EventDraft::new(event_type, &[], payload).with_origin(parent, parent_event)
    }

    /// A read-only handle onto a stream's ledger (cross-stream read-back).
    pub fn reader(&self, stream_id: &str) -> Option<LogReader> {
        self.streams.get(stream_id).map(|k| k.log().reader())
    }

    /// Run one stream to quiescence.
    pub fn run_stream(&mut self, stream_id: &str) -> Result<(), KernelError> {
        match self.streams.get_mut(stream_id) {
            Some(kernel) => kernel.run_until_quiescent(),
            None => Ok(()),
        }
    }

    /// Run every open stream to quiescence. Streams are independent, so order
    /// does not affect correctness.
    pub fn run_all(&mut self) -> Result<(), KernelError> {
        let ids: Vec<String> = self.streams.keys().cloned().collect();
        for id in ids {
            self.run_stream(&id)?;
        }
        Ok(())
    }

    /// Borrow a stream's kernel (e.g. to read its log in tests/tools).
    pub fn kernel(&self, stream_id: &str) -> Option<&Kernel> {
        self.streams.get(stream_id)
    }

    pub fn open_stream_ids(&self) -> Vec<String> {
        self.streams.keys().cloned().collect()
    }
}
