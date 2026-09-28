//! User-owned complete baseline. Installation additions remain a separate file.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use serde_json::Value;

use crate::{AssemblyManifest, ComponentManifest};

fn validate(schema: &str, value: &Value) -> Result<(), String> {
    let schema: Value = serde_json::from_str(schema).expect("embedded canon is JSON");
    jsonschema::validator_for(&schema)
        .expect("embedded canon compiles")
        .validate(value)
        .map_err(|e| e.to_string())
}

/// Stage a complete baseline, committing only after shape and reference checks.
/// A selected but missing or invalid document is an error, never a fallback.
pub fn load(
    registry: &mut HashMap<String, ComponentManifest>,
    path: &Path,
    runtime: &AssemblyManifest,
) -> Result<AssemblyManifest, String> {
    let parse = || -> Result<_, String> {
        let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
        let document: Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
        validate(include_str!("../schemas/product_assembly.json"), &document)?;
        validate(
            include_str!("../schemas/assembly_manifest.json"),
            &document["assembly"],
        )?;
        let mut assembly: AssemblyManifest =
            serde_json::from_value(document["assembly"].clone()).map_err(|e| e.to_string())?;
        for name in ["model", "cmodel", "ctx"] {
            if !assembly.instances.contains_key(name) {
                return Err(format!(
                    "runtime slot {name:?} is absent from the complete assembly"
                ));
            }
            let instance = runtime
                .instances
                .get(name)
                .ok_or_else(|| format!("host has no runtime slot {name:?}"))?;
            assembly
                .instances
                .insert(name.to_string(), instance.clone());
        }
        let mut staged = registry.clone();
        if let Some(components) = document["components"].as_array() {
            for raw in components {
                crate::overlay::validate_component(raw)?;
                let component: ComponentManifest =
                    serde_json::from_value(raw.clone()).map_err(|e| e.to_string())?;
                if staged.contains_key(&component.name) {
                    return Err(format!("duplicate component name: {}", component.name));
                }
                staged.insert(component.name.clone(), component);
            }
        }
        let issues = crate::inspect_assembly(&assembly, &staged);
        if !issues.is_empty() {
            return Err(format!("assembly inspection failed: {issues:?}"));
        }
        Ok((staged, assembly))
    };
    let (staged, assembly) =
        parse().map_err(|e| format!("complete assembly {}: {e}", path.display()))?;
    *registry = staged;
    Ok(assembly)
}

/// Export the effective assembly and only non-built-in manifests. Configurations
/// are literal: callers must treat this document as potentially sensitive.
pub fn document(
    registry: &HashMap<String, ComponentManifest>,
    assembly: &AssemblyManifest,
    builtin_names: &HashSet<String>,
) -> Value {
    let mut components: Vec<_> = registry
        .values()
        .filter(|m| !builtin_names.contains(&m.name))
        .collect();
    components.sort_by(|a, b| a.name.cmp(&b.name));
    serde_json::json!({"components": components, "assembly": assembly, "runtimeSlots": ["model", "cmodel", "ctx"]})
}
