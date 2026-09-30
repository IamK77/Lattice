use super::*;
use crate::experts::activation::AuditRef;
use crate::experts::management::Failure;
use crate::LogReader;

impl ExpertDefinitions {
    pub(super) fn audit_reference(
        event: &EventEnvelope,
        reader: &LogReader,
    ) -> Result<AuditRef, String> {
        let ledger = reader
            .path()
            .ok_or("expert management requires a persistent ledger")?
            .canonicalize()
            .map_err(|error| error.to_string())?;
        Ok(AuditRef {
            ledger,
            stream: event.stream.clone(),
            event: event.id.clone(),
        })
    }

    pub(super) fn finish(
        request: &EventEnvelope,
        answer: Option<&EventEnvelope>,
        result: Result<Value, Failure>,
        ctx: &mut Ctx,
    ) {
        let mut causes = vec![request.id.as_str()];
        if let Some(answer) = answer {
            causes.push(&answer.id);
        }
        let mut payload = match result {
            Ok(result) => json!({"status":"ok","result":result}),
            Err(error) => {
                json!({"status":"error","error":{"code":"expert.management_failed","message":error.message,
                "blame":"request","retryable":false,"transient":false},
                "partial":{"activationChanged":error.activation_changed,"definitionChanged":error.definition_changed}})
            }
        };
        payload["call"] = request.payload["call"].clone();
        ctx.emit(
            "outcome",
            EventDraft::new(ce::TOOL_EXEC_COMPLETED, &causes, payload),
        );
    }

    pub(super) fn handle_confirmation(
        &mut self,
        event: &EventEnvelope,
        ctx: &mut Ctx,
    ) -> Result<(), String> {
        if event.event_type == ce::INTERRUPTED {
            let mut remove = Vec::new();
            for (id, request) in &self.pending {
                if ctx
                    .log()
                    .has_outcome(id)
                    .map_err(|error| error.to_string())?
                {
                    remove.push(id.clone());
                } else if event.payload["by"] == "user" && event.seq > request.seq {
                    ctx.emit(
                        "interrupted",
                        EventDraft::new(
                            ce::INTERRUPTED,
                            &[id, &event.id],
                            json!({"by":"user","call":request.payload["call"]}),
                        ),
                    );
                    remove.push(id.clone());
                }
            }
            for id in remove {
                self.pending.remove(&id);
            }
            return Ok(());
        }
        if event.payload["channel"] != crate::components::trust_policy::AUTH_CHANNEL {
            return Ok(());
        }
        let data = &event.payload;
        let Some(question_id) = data["request"].as_str() else {
            return Ok(());
        };
        let reader = ctx.log().clone();
        let Some(question) = reader.get(question_id).map_err(|error| error.to_string())? else {
            return Ok(());
        };
        if question.event_type != AUTH_REQUESTED || question.payload["tool"] != DELETE {
            return Ok(());
        }
        let Some(held) = question.payload["request"].as_str() else {
            return Ok(());
        };
        if question.payload["held"] != held || !question.causes.iter().any(|cause| cause == held) {
            return Ok(());
        }
        let Some(request) = self.pending.remove(held) else {
            return Ok(());
        };
        if reader
            .has_outcome(held)
            .map_err(|error| error.to_string())?
        {
            return Ok(());
        }
        let approve =
            data["approve"].as_bool().unwrap_or(false) && !ctx.cancellation().is_cancelled();
        ctx.emit(
            "decision",
            EventDraft::new(
                DECISION,
                &[held, &question.id, &event.id],
                json!({"held":held,"verdict":if approve {"granted"} else {"refused"}}),
            )
            .with_reason(if approve {
                "The user confirmed deleting this expert revision"
            } else {
                "The user did not approve deleting this expert revision"
            }),
        );
        let result = if approve {
            (|| -> Result<Value, Failure> {
                let catalog = self.catalog.as_ref().map_err(Clone::clone)?;
                catalog.apply_mutation(
                    &request.payload["arguments"],
                    Self::audit_reference(event, &reader)?,
                )
            })()
        } else {
            Err(
                "The user refused expert deletion. Do not retry or route around this decision."
                    .to_string()
                    .into(),
            )
        };
        Self::finish(&request, Some(event), result, ctx);
        Ok(())
    }
}
