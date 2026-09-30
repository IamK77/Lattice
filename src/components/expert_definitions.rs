//! Management of reusable expert activation. Definition files remain editable
//! data; this provider is the authorized transition into reusable availability.

use std::collections::HashSet;

use serde_json::{json, Value};

use crate::contracts::component::{ComponentManifest, EffectSurface, PortDecl, RuntimeKind};
use crate::experts::activation::ACTIVATE;
use crate::experts::catalog::{Catalog, Config};
use crate::{core_events as ce, Component, Ctx, EventDraft, EventEnvelope};

pub mod review;

pub const NAME: &str = "expert-definitions";
pub const INSPECT: &str = "InspectExpert";

pub fn manifest() -> ComponentManifest {
    ComponentManifest {
        name: NAME.into(),
        version: env!("CARGO_PKG_VERSION").into(),
        runtime: RuntimeKind::Inproc,
        entry: format!("builtin:{NAME}"),
        inputs: vec![PortDecl::new("execute", &[ce::TOOL_EXEC_STARTED])],
        outputs: vec![PortDecl::new("outcome", &[ce::TOOL_EXEC_COMPLETED])],
        events: vec![],
        default_wiring: vec![],
        implements: vec!["tool-provider".into()],
        capabilities: Some(EffectSurface {
            reads: vec!["*".into()],
            writes: vec!["*".into()],
            network: vec!["*".into()],
            executes: true,
            ..Default::default()
        }),
        prompt: None,
        handle_timeout_ms: None,
        concurrency: None,
        tools: vec![
            json!({
                "name":INSPECT,
                "description":"Inspect a reusable expert definition, its exact file version, activation and availability. Use project:<id> or personal:<id>. A definition file alone is not permission to run it. The result supplies the full arguments for ActivateExpert; add a reason before requesting approval.",
                "parameters":{"type":"object","properties":{"expert":{"type":"string"}},"required":["expert"],"additionalProperties":false},
                "effects":{"reads":["expert definitions, model catalog and authorization evidence"],"writes":["process credential references"],"network":[],"executes":false}
            }),
            json!({
                "name":ACTIVATE,
                "description":"Activate the exact reusable expert revision returned by InspectExpert. Requires explicit admission authorization; neither a file nor a model-supplied boolean grants permission. Copy all activateArguments, add a reason, and do not replace changed content silently. Conflicts require a fresh inspection. Activation does not start an expert job.",
                "parameters":{
                    "type":"object",
                    "properties":{
                        "operation":{"type":"string","enum":["activate"]},
                        "target":{"type":"object","properties":{"scope":{"type":"string","enum":["project","personal"]},"root":{"type":"string"},"id":{"type":"string"}},"required":["scope","root","id"],"additionalProperties":false},
                        "definition":{"type":"object"},
                        "fileVersion":{"type":"string"},
                        "expectedActivation":{"type":["object","null"]},
                        "reason":{"type":"string","minLength":1}
                    },
                    "required":["operation","target","definition","fileVersion","expectedActivation","reason"],
                    "additionalProperties":false
                },
                "effects":{"reads":["*"],"writes":["*"],"network":["*"],"executes":true,"admits":"reusable-expert-definition","reversible":false}
            }),
        ],
    }
}

pub struct ExpertDefinitions {
    catalog: Result<Catalog, String>,
    handled: HashSet<String>,
}

impl ExpertDefinitions {
    pub fn from_config(config: Option<&Value>) -> Self {
        let catalog = config
            .ok_or_else(|| "expert definitions require configured roots".to_string())
            .and_then(|value| {
                serde_json::from_value::<Config>(value.clone()).map_err(|error| error.to_string())
            })
            .and_then(Catalog::new);
        Self {
            catalog,
            handled: HashSet::new(),
        }
    }
}

impl Component for ExpertDefinitions {
    fn handle(&mut self, _port: &str, event: &EventEnvelope, ctx: &mut Ctx) {
        let tool = event.payload["tool"].as_str().unwrap_or("");
        if tool != INSPECT && tool != ACTIVATE {
            return;
        }
        match ctx.log().has_outcome(&event.id) {
            Ok(true) => return,
            Err(error) => {
                ctx.fail(
                    "inspect expert operation outcome",
                    error.to_string(),
                    std::slice::from_ref(&event.id),
                );
                return;
            }
            Ok(false) => {}
        }
        if !self.handled.insert(event.id.clone()) {
            return;
        }
        let result = self
            .catalog
            .as_ref()
            .map_err(Clone::clone)
            .and_then(|catalog| {
                let reader = ctx.log();
                if tool == INSPECT {
                    let name = event.payload["arguments"]["expert"]
                        .as_str()
                        .ok_or("expert must be qualified")?;
                    catalog.inspect(name, Some(reader))
                } else {
                    if event.payload["arguments"]["reason"]
                        .as_str()
                        .is_none_or(|reason| reason.trim().is_empty())
                    {
                        return Err("expert activation requires a nonempty reason".into());
                    }
                    catalog
                        .activate(event, reader)
                        .map(|activation| json!({"activation":activation}))
                }
            });
        let mut payload = match result {
            Ok(value) => json!({"status":"ok","result":value}),
            Err(message) => {
                json!({"status":"error","error":{"code":"expert.unavailable","message":message,"blame":"request","retryable":false,"transient":false}})
            }
        };
        payload["call"] = event.payload["call"].clone();
        ctx.emit(
            "outcome",
            EventDraft::new(ce::TOOL_EXEC_COMPLETED, &[&event.id], payload),
        );
    }
}
