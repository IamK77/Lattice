//! The assembly overlay — the persistence half of hot installation.
//!
//! The product's baseline assembly is GENERATED at startup (src/preset.rs);
//! there is no on-disk document to write an install into. The overlay is
//! that document: a small file in the user directory carrying what the user
//! installed on top of the baseline — components, instances, wires. Installs
//! append to it ([`record_install`]), startup merges it ([`apply`]), so a
//! hot-installed component survives a restart. Canon:
//! schemas/assembly_overlay.json; the carried parts are held to the same
//! canons as everything else (component manifests, endpoint shapes).
//!
//! The merge is ATOMIC: an overlay with any problem — unparseable, failing a
//! canon, colliding with a baseline name — is skipped whole, with an
//! explanation, rather than half-applied. A skipped overlay never prevents
//! startup; a startup without one's installs is degraded, not broken.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use serde_json::{json, Value};

use crate::contracts::assembly::{AssemblyManifest, ComponentInstance, Wire};
use crate::contracts::component::ComponentManifest;

/// Where the overlay lives unless configured otherwise: the user directory,
/// next to the trust grants and the skill folders.
pub fn default_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home).join(".lattice").join("assembly.json")
}

fn canon(
    source: &'static str,
    cell: &'static OnceLock<jsonschema::Validator>,
) -> &'static jsonschema::Validator {
    cell.get_or_init(|| {
        let schema: Value =
            serde_json::from_str(source).expect("canon schema files are valid JSON");
        jsonschema::validator_for(&schema).expect("canon schema files compile")
    })
}

fn overlay_canon() -> &'static jsonschema::Validator {
    static CELL: OnceLock<jsonschema::Validator> = OnceLock::new();
    canon(include_str!("../schemas/assembly_overlay.json"), &CELL)
}

fn component_canon() -> &'static jsonschema::Validator {
    static CELL: OnceLock<jsonschema::Validator> = OnceLock::new();
    canon(include_str!("../schemas/component_manifest.json"), &CELL)
}

/// Validate one raw self-description against the component canon — shared by
/// the overlay merge and the fetch-install path.
pub(crate) fn validate_component(raw: &Value) -> Result<(), String> {
    component_canon()
        .validate(raw)
        .map_err(|error| format!("the manifest fails the component canon: {error}"))
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct OverlayReport {
    pub components: usize,
    pub instances: usize,
    pub wires: usize,
}

/// Merge the overlay at `path` into the registry and assembly, atomically.
/// A missing file is an empty overlay (the normal fresh state); any problem
/// in a present file rejects the WHOLE overlay with an explanation and
/// leaves registry and assembly untouched.
pub fn apply(
    registry: &mut HashMap<String, ComponentManifest>,
    assembly: &mut AssemblyManifest,
    path: &Path,
) -> Result<OverlayReport, String> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(OverlayReport::default()),
        Err(e) => return Err(format!("cannot read the overlay: {e}")),
    };
    let overlay: Value =
        serde_json::from_str(&text).map_err(|e| format!("the overlay is not valid JSON: {e}"))?;
    if let Err(error) = overlay_canon().validate(&overlay) {
        return Err(format!("the overlay fails its canon: {error}"));
    }

    // Stage everything first — nothing lands until every check passes
    let mut new_components: Vec<ComponentManifest> = Vec::new();
    if let Some(components) = overlay["components"].as_array() {
        for raw in components {
            if let Err(error) = component_canon().validate(raw) {
                return Err(format!("an overlay component fails the canon: {error}"));
            }
            let manifest: ComponentManifest = serde_json::from_value(raw.clone())
                .map_err(|e| format!("an overlay component does not parse: {e}"))?;
            if registry.contains_key(&manifest.name) {
                return Err(format!(
                    "overlay component {:?} collides with the baseline",
                    manifest.name
                ));
            }
            new_components.push(manifest);
        }
    }

    let mut new_instances: Vec<(String, ComponentInstance)> = Vec::new();
    if let Some(instances) = overlay["instances"].as_object() {
        for (name, raw) in instances {
            if assembly.instances.contains_key(name) {
                return Err(format!(
                    "overlay instance {name:?} collides with the baseline"
                ));
            }
            let instance: ComponentInstance = serde_json::from_value(raw.clone())
                .map_err(|e| format!("overlay instance {name:?} does not parse: {e}"))?;
            let known = registry.contains_key(&instance.component)
                || new_components.iter().any(|c| c.name == instance.component);
            if !known {
                return Err(format!(
                    "overlay instance {name:?} references unknown component {:?}",
                    instance.component
                ));
            }
            new_instances.push((name.clone(), instance));
        }
    }

    let mut new_wires: Vec<Wire> = Vec::new();
    if let Some(wires) = overlay["wires"].as_array() {
        for raw in wires {
            let wire: Wire = serde_json::from_value(raw.clone())
                .map_err(|e| format!("an overlay wire does not parse: {e}"))?;
            // A doubled wire would mean doubled delivery — drop exact repeats
            let repeated = assembly
                .wires
                .iter()
                .chain(new_wires.iter())
                .any(|w| w.from == wire.from && w.to == wire.to);
            if !repeated {
                new_wires.push(wire);
            }
        }
    }

    let report = OverlayReport {
        components: new_components.len(),
        instances: new_instances.len(),
        wires: new_wires.len(),
    };
    for manifest in new_components {
        registry.insert(manifest.name.clone(), manifest);
    }
    for (name, instance) in new_instances {
        assembly.instances.insert(name, instance);
    }
    assembly.wires.extend(new_wires);
    Ok(report)
}

/// Write one successful install into the overlay: upsert the component
/// manifest and the instance, append the wires that are not already there.
/// This is what makes "installed" mean installed — present after a restart.
/// Take an instance back out of the overlay, with every wire that touched it.
///
/// Without this a removal only lasted until the next start: the overlay is
/// what brings hot installs back, so a component removed from the running
/// kernel was reinstalled on reopen — and one that crashed every time came
/// back every time.
///
/// Whether the instance is one the overlay may remove is the CALLER's
/// judgement; this reports plainly whether it found anything to remove, so a
/// caller can tell "not there" from "taken out".
pub fn record_removal(path: &Path, instance: &str) -> Result<bool, String> {
    let mut overlay: Value = match std::fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text)
            .map_err(|e| format!("the existing overlay is not valid JSON: {e}"))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(format!("cannot read the overlay: {e}")),
    };
    let removed = overlay["instances"]
        .as_object_mut()
        .and_then(|instances| instances.remove(instance));
    let Some(removed) = removed else {
        return Ok(false);
    };
    // The definition goes too, unless another instance still stands on it.
    // Keeping it was defensible while two instances might share one component;
    // keeping it once NOTHING references it just leaves the file growing a
    // definition per install and never losing one, and a reader of the file
    // seeing components that no longer run.
    if let Some(component) = removed["component"].as_str() {
        let still_used = overlay["instances"]
            .as_object()
            .into_iter()
            .flatten()
            .any(|(_, spec)| spec["component"] == component);
        if !still_used {
            if let Some(components) = overlay["components"].as_array_mut() {
                components.retain(|c| c["name"] != component);
            }
        }
    }
    let leaving = format!("{instance}.");
    if let Some(wires) = overlay["wires"].as_array_mut() {
        wires.retain(|wire| {
            let touches = |key: &str| {
                wire[key]
                    .as_str()
                    .is_some_and(|endpoint| endpoint.starts_with(&leaving))
            };
            !touches("from") && !touches("to")
        });
    }
    let text = serde_json::to_string_pretty(&overlay)
        .map_err(|e| format!("the overlay does not serialize: {e}"))?;
    std::fs::write(path, text).map_err(|e| format!("cannot write the overlay: {e}"))?;
    Ok(true)
}

pub fn record_install(
    path: &Path,
    manifest: &ComponentManifest,
    instance: &str,
    config: Option<&Value>,
    wires: &[Wire],
) -> Result<(), String> {
    let mut overlay: Value = match std::fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text)
            .map_err(|e| format!("the existing overlay is not valid JSON: {e}"))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            json!({"components": [], "instances": {}, "wires": []})
        }
        Err(e) => return Err(format!("cannot read the overlay: {e}")),
    };
    // Check the document we are about to edit BEFORE editing it. Only nulls
    // were filled in here, so a hand-edited file whose `wires` was an object
    // (or whose `instances` was a list) went straight into the code below,
    // which reaches for the shape it expects — and `as_array().unwrap()` on
    // the host thread is the end of the session, from a typo in a file the
    // person is invited to edit. Refusing a document we cannot safely amend
    // is the same posture `apply` takes when it skips a bad overlay whole.
    if let Err(problem) = overlay_canon().validate(&overlay) {
        return Err(format!(
            "the overlay at {} is not a valid overlay document, so nothing was \
             written to it: {problem}",
            path.display()
        ));
    }
    for key in ["components", "instances", "wires"] {
        if overlay[key].is_null() {
            overlay[key] = if key == "instances" {
                json!({})
            } else {
                json!([])
            };
        }
    }

    let manifest_value =
        serde_json::to_value(manifest).map_err(|e| format!("manifest does not serialize: {e}"))?;
    let components = overlay["components"]
        .as_array_mut()
        .ok_or("overlay 'components' is not an array")?;
    match components
        .iter_mut()
        .find(|c| c["name"] == manifest.name.as_str())
    {
        Some(existing) => *existing = manifest_value,
        None => components.push(manifest_value),
    }

    overlay["instances"][instance] = match config {
        Some(config) => json!({"component": manifest.name, "config": config}),
        None => json!({"component": manifest.name}),
    };

    let recorded = overlay["wires"]
        .as_array_mut()
        .ok_or("overlay 'wires' is not an array")?;
    for wire in wires {
        let present = recorded
            .iter()
            .any(|w| w["from"] == wire.from.as_str() && w["to"] == wire.to.as_str());
        if !present {
            recorded.push(json!({"from": wire.from, "to": wire.to}));
        }
    }

    if let Err(error) = overlay_canon().validate(&overlay) {
        return Err(format!(
            "refusing to write an overlay that fails its canon: {error}"
        ));
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    }
    std::fs::write(
        path,
        serde_json::to_string_pretty(&overlay).expect("overlay serializes"),
    )
    .map_err(|e| format!("cannot write the overlay: {e}"))
}
