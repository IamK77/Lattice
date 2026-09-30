//! Serializable execution recipes. Opening one does not resolve definition,
//! model-catalog, overlay or capability-group names a second time.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::activation::{Activation, AuditRef};
use super::{Candidate, Identity};
use crate::preset::{self, Expert, PresetConfig};
use crate::{AssemblyManifest, ComponentManifest, Kernel, KernelOptions, StreamTemplate};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Execution {
    pub v: u32,
    pub identity: Identity,
    pub revision: String,
    pub activation: AuditRef,
    pub model_id: String,
    pub assembly: AssemblyManifest,
    pub components: BTreeMap<String, ComponentManifest>,
    /// Names only. Credential values are deliberately not frozen or serialized.
    pub credential_names: Vec<String>,
}

impl Execution {
    pub fn capture(
        candidate: &Candidate,
        activation: &Activation,
        model: &crate::models::Entry,
        defaults: &PresetConfig,
    ) -> Result<Self, String> {
        candidate.definition.validate()?;
        if !activation.matches(candidate) {
            return Err("expert revision has not been activated".into());
        }
        if model.id != candidate.definition.model {
            return Err("resolved model does not match the expert's model reference".into());
        }
        if model.adapter != "scripted" && !model.key_present() {
            return Err(format!(
                "expert model {} has no available credential",
                model.id
            ));
        }
        let mut config = defaults.clone();
        config.adapter = model.adapter.clone();
        config.model = model.model.clone();
        config.base_url = model.base_url.clone();
        config.key_env = model.key_env.clone();
        config.profile = model.profile.clone();
        config.thinking = None;
        // These definitions select shipped groups only, never user baselines
        // or newly installed executable components.
        config.assembly = None;
        config.overlay = None;
        let tools = candidate.definition.tool_instances();
        let expert = Expert {
            name: &candidate.definition.id,
            description: &candidate.definition.description,
            tools: &tools,
            prompt: &candidate.definition.instructions,
        };
        let (registry, _, mut assembly) = preset::expert_assembly(&config, &expert)?;
        if model.adapter != "scripted" {
            assembly
                .instances
                .get_mut(preset::MAIN_MODEL)
                .and_then(|instance| instance.config.as_mut())
                .ok_or("expert recipe has no main model configuration")?["entryId"] =
                serde_json::json!(model.id);
        }
        let components = assembly
            .instances
            .values()
            .map(|instance| {
                let manifest = registry
                    .get(&instance.component)
                    .expect("the standard expert recipe only uses registered components");
                (instance.component.clone(), manifest.clone())
            })
            .collect();
        let mut credential_names = Vec::new();
        if !model.key_env.is_empty() {
            credential_names.push(model.key_env.clone());
        }
        // The explicit current-model reference is preserved; the consumer does
        // not reread the catalog to discover what this reference used to mean.
        let result = Self {
            v: 1,
            identity: candidate.identity.clone(),
            revision: candidate.definition.revision(),
            activation: activation.authorization.clone(),
            model_id: model.id.clone(),
            assembly,
            components,
            credential_names,
        };
        result.template()?;
        Ok(result)
    }

    pub fn template(&self) -> Result<StreamTemplate, String> {
        if self.v != 1 {
            return Err("unsupported expert execution snapshot version".into());
        }
        let (local, mut factories) = preset::builtin_implementations();
        for (name, captured) in &self.components {
            let current = local
                .get(name)
                .ok_or_else(|| format!("captured component is not shipped: {name}"))?;
            if serde_json::to_value(current).map_err(|e| e.to_string())?
                != serde_json::to_value(captured).map_err(|e| e.to_string())?
            {
                return Err(format!(
                    "captured component declaration no longer matches this build: {name}"
                ));
            }
        }
        let registry = self.components.clone().into_iter().collect();
        let issues = crate::inspect_assembly(&self.assembly, &registry);
        if !issues.is_empty() {
            return Err(format!("invalid captured expert assembly: {issues:?}"));
        }
        factories.retain(|name, _| self.components.contains_key(name));
        Ok(StreamTemplate {
            registry,
            factories,
            assembly: self.assembly.clone(),
        })
    }

    /// Product-level opening, not an expert hook in the kernel. Startup options
    /// (ledger location and credential isolation) remain the host's concern.
    pub fn open(
        &self,
        stream: &str,
        ledger: Option<PathBuf>,
        mut options: KernelOptions,
    ) -> Result<Kernel, String> {
        let mut template = self.template()?;
        options.stream = Some(stream.into());
        options.log_file = ledger;
        for name in &self.credential_names {
            if !options.child_env_deny.contains(name) {
                options.child_env_deny.push(name.clone());
            }
            // This is a new child's startup boundary, not an expansion of the
            // already-running parent's redaction list.
            if let Ok(value) = std::env::var(name) {
                if !value.is_empty() && !options.redact.contains(&value) {
                    options.redact.push(value);
                }
            }
        }
        Kernel::start(
            &template.assembly,
            &template.registry,
            &mut template.factories,
            options,
        )
        .map_err(|error| error.to_string())
    }
}
