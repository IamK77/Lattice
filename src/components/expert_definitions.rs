//! Management of reusable expert activation. Definition files remain editable
//! data; this provider is the authorized transition into reusable availability.

use std::collections::{HashMap, HashSet};

use serde_json::{json, Value};

use crate::contracts::component::{ComponentManifest, EffectSurface, PortDecl, RuntimeKind};
use crate::contracts::event::EventTypeDecl;
use crate::experts::activation::ACTIVATE;
use crate::experts::catalog::{Catalog, Config};
use crate::experts::management::{DELETE, LIST, SAVE};
use crate::{core_events as ce, Component, Ctx, EventDraft, EventEnvelope};

mod confirmation;
mod management_tools;
pub mod review;

pub const AUTH_REQUESTED: &str = "experts.authorization_requested";
pub const DECISION: &str = "experts.authorization_decided";

pub const NAME: &str = "expert-definitions";
pub const INSPECT: &str = "InspectExpert";

pub fn manifest() -> ComponentManifest {
    let mut manifest = ComponentManifest {
        name: NAME.into(),
        version: env!("CARGO_PKG_VERSION").into(),
        runtime: RuntimeKind::Inproc,
        entry: format!("builtin:{NAME}"),
        inputs: vec![PortDecl::new("execute", &[ce::TOOL_EXEC_STARTED]), PortDecl::new("answer", &[ce::EXTERNAL_INPUT]), PortDecl::new("control", &[ce::INTERRUPTED])],
        outputs: vec![PortDecl::new("outcome", &[ce::TOOL_EXEC_COMPLETED]), PortDecl::new("request", &[AUTH_REQUESTED]), PortDecl::new("decision", &[DECISION]), PortDecl::new("interrupted", &[ce::INTERRUPTED])],
        events: vec![
            EventTypeDecl::new(AUTH_REQUESTED,"Deleting a reusable expert awaits human confirmation")
                .with_schema(json!({"type":"object","required":["request","held","tool","confirmation","summary"]})),
            EventTypeDecl::decision(DECISION,"The user approved or refused deleting an expert")
                .with_schema(json!({"type":"object","required":["held","verdict"]})),
        ],
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
                "description":"Inspect an expert's exact content, file version, activation state and mutation arguments. Use project:<id>, personal:<id>, or builtin:<id>. Missing custom identities return an absent slot for creation. Built-ins return their actual copyTemplate; select a model and a custom destination before saving. Add a reason to the returned save, activate or delete arguments.",
                "parameters":{"type":"object","properties":{"expert":{"type":"string"}},"required":["expert"],"additionalProperties":false},
                "effects":{"reads":["expert definitions, model catalog and authorization evidence"],"writes":["expert state locks and process credential references"],"network":[],"executes":false}
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
    };
    manifest.tools.extend(management_tools::declarations());
    manifest
}

pub struct ExpertDefinitions {
    catalog: Result<Catalog, String>,
    handled: HashSet<String>,
    pending: HashMap<String, EventEnvelope>,
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
            pending: HashMap::new(),
        }
    }
}

impl Component for ExpertDefinitions {
    fn handle(&mut self, _port: &str, event: &EventEnvelope, ctx: &mut Ctx) {
        if matches!(
            event.event_type.as_str(),
            ce::EXTERNAL_INPUT | ce::INTERRUPTED
        ) {
            if let Err(error) = self.handle_confirmation(event, ctx) {
                ctx.fail(
                    "expert confirmation history",
                    error,
                    std::slice::from_ref(&event.id),
                );
            }
            return;
        }
        let tool = event.payload["tool"].as_str().unwrap_or("");
        if ![INSPECT, ACTIVATE, LIST, SAVE, DELETE].contains(&tool) {
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
        use crate::experts::management::{Failure, Mutation, Operation};
        let result = (|| -> Result<Option<Value>, Failure> {
            let catalog = self.catalog.as_ref().map_err(Clone::clone)?;
            let arguments = &event.payload["arguments"];
            let reader = ctx.log();
            let value = match tool {
                LIST => catalog.management_listing(Some(reader))?,
                INSPECT => catalog.inspect(
                    arguments["expert"]
                        .as_str()
                        .ok_or_else(|| "expert must be qualified".to_string())?,
                    Some(reader),
                )?,
                ACTIVATE => {
                    if arguments["reason"]
                        .as_str()
                        .is_none_or(|reason| reason.trim().is_empty())
                    {
                        return Err("expert activation requires a nonempty reason"
                            .to_string()
                            .into());
                    }
                    json!({"activation":catalog.activate(event, reader)?})
                }
                SAVE => {
                    if event.source != catalog.config.gate
                        || Mutation::parse(arguments)?.operation != Operation::Put
                    {
                        return Err(
                            "saving requires the reviewed put request from the admission gate"
                                .to_string()
                                .into(),
                        );
                    }
                    catalog.apply_mutation(arguments, Self::audit_reference(event, reader)?)?
                }
                DELETE => {
                    if Mutation::parse(arguments)?.operation != Operation::Delete {
                        return Err("DeleteExpert requires a delete operation"
                            .to_string()
                            .into());
                    }
                    let summary = catalog.review_mutation(arguments)?;
                    Self::audit_reference(event, reader)?;
                    self.pending.insert(event.id.clone(), event.clone());
                    ctx.emit("request", EventDraft::new(AUTH_REQUESTED, &[&event.id], json!({
                        "request":event.id,"held":event.id,"tool":DELETE,"confirmation":"expert-delete",
                        "summary":summary,"reason":arguments["reason"]
                    })));
                    return Ok(None);
                }
                _ => unreachable!(),
            };
            Ok(Some(value))
        })();
        match result {
            Ok(None) => {}
            Ok(Some(value)) => Self::finish(event, None, Ok(value), ctx),
            Err(error) => Self::finish(event, None, Err(error), ctx),
        }
    }
}
