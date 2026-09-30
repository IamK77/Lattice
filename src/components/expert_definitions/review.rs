//! Validate activation content before the authorization question is asked.
//! This observer declares no tools, so ordinary owner routing still lets it
//! receive and forward all tool requests. It is not the authorization authority.
use super::*;

pub const NAME: &str = "expert-definition-review";

pub fn manifest() -> ComponentManifest {
    let mut declaration = super::manifest();
    declaration.name = NAME.into();
    declaration.entry = format!("builtin:{NAME}");
    declaration.inputs = vec![PortDecl::new("review", &[ce::TOOL_EXEC_STARTED])];
    declaration.outputs = vec![
        PortDecl::new("forward", &[ce::TOOL_EXEC_STARTED]),
        PortDecl::new("verdict", &[ce::TOOL_EXEC_COMPLETED]),
    ];
    declaration.tools.clear();
    declaration.events.clear();
    declaration.implements.clear();
    declaration
}

pub struct ExpertReview {
    definitions: ExpertDefinitions,
}

impl ExpertReview {
    pub fn from_config(config: Option<&Value>) -> Self {
        Self {
            definitions: ExpertDefinitions::from_config(config),
        }
    }
}

impl Component for ExpertReview {
    fn handle(&mut self, _port: &str, event: &EventEnvelope, ctx: &mut Ctx) {
        let mut payload = event.payload.clone();
        // The summary is derived from checked content, never accepted from a
        // tool caller. The immutable arguments remain the permission fingerprint.
        if let Some(object) = payload.as_object_mut() {
            object.remove("admissionReview");
        }
        if [ACTIVATE, SAVE, DELETE].contains(&payload["tool"].as_str().unwrap_or("")) {
            let reviewed = self
                .definitions
                .catalog
                .as_ref()
                .map_err(Clone::clone)
                .and_then(|catalog| {
                    if payload["tool"] == ACTIVATE {
                        catalog.review(&payload["arguments"])
                    } else {
                        use crate::experts::management::{Mutation, Operation};
                        let request = Mutation::parse(&payload["arguments"])?;
                        let expected = if payload["tool"] == SAVE {
                            Operation::Put
                        } else {
                            Operation::Delete
                        };
                        if request.operation != expected {
                            return Err("expert tool and operation do not match".into());
                        }
                        catalog.review_mutation(&payload["arguments"])
                    }
                });
            match reviewed {
                Ok(summary) => payload["admissionReview"] = json!(summary),
                Err(message) => {
                    ctx.emit("verdict", EventDraft::new(ce::TOOL_EXEC_COMPLETED, &[&event.id], json!({
                        "call":payload["call"],"status":"error",
                        "error":{"code":"expert.invalid_activation","message":message,"blame":"request","retryable":false,"transient":false}
                    })));
                    return;
                }
            }
        }
        ctx.emit(
            "forward",
            EventDraft::new(ce::TOOL_EXEC_STARTED, &[&event.id], payload),
        );
    }
}
