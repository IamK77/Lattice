//! Terminal host assembly: preview its declared parts, or construct a kernel
//! and attach its render subscription. No terminal, UI state, or tab lifecycle.

use lattice::preset::{self, PresetConfig};
use lattice::{core_events, Kernel, KernelError, KernelOptions, RenderEvent};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::mpsc::Sender;

/// A best-effort display snapshot, not a validated kernel or reusable factories.
/// Only overlay instances are removable. Debug configurations have no overlay.
pub(super) fn preview(cfg: &PresetConfig) -> Vec<lattice::Assembled> {
    preset::standard(cfg)
        .map(|(registry, _, assembly)| {
            let installed: std::collections::HashSet<String> = cfg
                .overlay
                .as_ref()
                .and_then(|path| std::fs::read_to_string(path).ok())
                .and_then(|text| serde_json::from_str::<Value>(&text).ok())
                .and_then(|doc| doc["instances"].as_object().cloned())
                .map(|m| m.keys().cloned().collect())
                .unwrap_or_default();
            let mut rows: Vec<lattice::Assembled> = assembly
                .instances
                .iter()
                .map(|(instance, decl)| {
                    let manifest = registry.get(&decl.component);
                    let runtime = match manifest.map(|m| &m.runtime) {
                        Some(lattice::RuntimeKind::Inproc) => "in-process",
                        Some(_) => "subprocess",
                        None => "?",
                    };
                    let tools = manifest
                        .map(|m| {
                            m.tools
                                .iter()
                                .filter_map(|t| t["name"].as_str().map(str::to_string))
                                .collect::<Vec<_>>()
                                .join(" ")
                        })
                        .unwrap_or_default();
                    let wires: Vec<String> = assembly
                        .wires
                        .iter()
                        .filter(|w| {
                            w.from.starts_with(&format!("{instance}."))
                                || w.to.starts_with(&format!("{instance}."))
                        })
                        .map(|w| format!("{} → {}", w.from, w.to))
                        .collect();
                    (
                        instance.clone(),
                        decl.component.clone(),
                        runtime,
                        tools,
                        installed.contains(instance),
                        wires,
                    )
                })
                .collect();
            rows.sort_by(|a, b| a.0.cmp(&b.0));
            rows
        })
        .unwrap_or_default()
}

/// Called by the session's build closure, on its thread. The subscription is
/// installed before the caller can capture a history boundary from the session.
pub(super) fn build(
    render_tx: Sender<RenderEvent>,
    cfg: &PresetConfig,
    ledger_path: PathBuf,
) -> Result<Kernel, KernelError> {
    observing(render_tx, cfg, ledger_path, Default::default())
}

pub(super) fn observing(
    render_tx: Sender<RenderEvent>,
    cfg: &PresetConfig,
    ledger_path: PathBuf,
    foreign: lattice::kernel::host::ForeignReaders,
) -> Result<Kernel, KernelError> {
    let (registry, mut factories, assembly) = preset::standard(cfg).map_err(|problem| {
        KernelError::Inspection(vec![lattice::InspectionIssue {
            location: "preset".to_string(),
            problem,
        }])
    })?;
    let options = KernelOptions {
        // Reopening must retain the stream identifier carried by that ledger.
        stream: lattice::EventLog::stream_of(&ledger_path),
        log_file: Some(ledger_path),
        stream_note: Some(json!({
            "host": "tui",
            "model": cfg.model,
            "adapter": cfg.adapter,
            // Process location and tool confinement are deliberately distinct.
            "cwd": std::env::current_dir()
                .map(|d| d.display().to_string())
                .unwrap_or_default(),
            "workspace": cfg.workspace.clone().unwrap_or_else(|| ".".to_string()),
        })),
        // Process components do not inherit model keys. Redaction protects chat
        // only: tool requests and results retain their original contents.
        child_env_deny: lattice::models::key_env_names(&cfg.key_env),
        redact: lattice::models::key_values(&cfg.key_env),
        ..KernelOptions::default()
    };
    let mut kernel =
        Kernel::start_with_foreign(&assembly, &registry, &mut factories, options, foreign)?;
    // This host owns the factories, allowing later model replacement in place.
    kernel.adopt_factories(factories);
    let for_log = render_tx.clone();
    kernel.subscribe_log(move |event| {
        if event.event_type == core_events::USER_MESSAGE
            && event.source == "ui"
            && event.causes.is_empty()
        {
            let _ = for_log.send(RenderEvent::InputObserved {
                event: event.id.clone(),
                pid: std::process::id(),
                clock_ns: lattice::input_latency::clock_ns(),
            });
        }
        let _ = for_log.send(RenderEvent::Appended(Box::new(event.clone())));
    });
    kernel.set_notice_handler(move |source, payload| {
        let _ = render_tx.send(RenderEvent::Notice {
            source: source.to_string(),
            payload: payload.clone(),
        });
    });
    Ok(kernel)
}

#[cfg(test)]
#[path = "session_build/tests.rs"]
mod tests;
