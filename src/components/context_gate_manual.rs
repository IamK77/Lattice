//! Explicit, non-replayed compaction. This bypasses the automatic failure pause
//! for one attempt, never changes the model epoch, and never sends a main ask.
use super::*;

pub(super) struct Submission {
    origin: String,
    through: u64,
    seen: bool,
    calls: ce::PendingCalls,
    outcome: Option<String>,
}

impl Submission {
    pub(super) fn new(origin: &str, through: u64) -> Self {
        Self {
            origin: origin.into(),
            through,
            seen: false,
            calls: ce::PendingCalls::new(ce::MODEL_CALL_STARTED),
            outcome: None,
        }
    }
}

impl ContextGate {
    pub(super) fn refresh_submission(&mut self, reader: &crate::LogReader) -> Result<(), String> {
        let Some(mut submission) = self.submission.take() else {
            return Ok(());
        };
        let mut published = false;
        let end = reader.snapshot_end();
        if submission.through >= end {
            self.submission = Some(submission);
            return Ok(());
        }
        let result =
            reader.try_visit_header_range(submission.through.saturating_add(1), end, |headers| {
                for header in headers {
                    let compact = recovery::condense(header);
                    let follows = submission
                        .calls
                        .requests()
                        .iter()
                        .any(|id| header.causes.contains(id));
                    if header.event_type == ce::MODEL_CALL_STARTED
                        && compact
                        && ((!submission.seen && header.causes.contains(&submission.origin))
                            || follows)
                    {
                        submission.seen = true;
                        submission.calls.observe(header.relations());
                    } else if submission.seen && ce::is_outcome(&header.event_type) {
                        let pending = !submission.calls.requests().is_empty();
                        submission.calls.observe(header.relations());
                        if pending && submission.calls.requests().is_empty() {
                            submission.outcome = Some(header.id.clone());
                            if header.event_type == ce::INTERRUPTED {
                                published = true;
                            }
                        }
                    }
                    if matches!(header.event_type.as_str(), DECISION | SUMMARY)
                        && submission
                            .outcome
                            .as_ref()
                            .is_some_and(|id| header.causes.contains(id))
                    {
                        published = true;
                    }
                    submission.through = header.seq;
                }
                Ok(())
            });
        if !published {
            self.submission = Some(submission);
        }
        result.map_err(|error| error.to_string())
    }

    pub(super) fn on_manual_compact(
        &mut self,
        event: &EventEnvelope,
        ctx: &mut Ctx,
    ) -> Result<(), String> {
        if !self.condense {
            self.manual_compact_skipped(event, "Compaction is not enabled in this assembly", ctx);
            return Ok(());
        }
        if self.condense_in_flight(ctx.log())? {
            self.manual_compact_skipped(event, "Compaction is already in progress", ctx);
            return Ok(());
        }
        let Some(id) = self.observation(ctx.log(), |state| state.forwarded_request.clone())? else {
            self.manual_compact_skipped(
                event,
                "There is no conversation request to compact yet",
                ctx,
            );
            return Ok(());
        };
        let assembled = ctx
            .log()
            .get(&id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| format!("indexed forwarded request is missing: {id}"))?;
        let original_id = assembled
            .causes
            .first()
            .ok_or_else(|| format!("forwarded request has no original request: {id}"))?;
        let original = ctx
            .log()
            .get(original_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| format!("original context request is missing: {original_id}"))?;
        if original.event_type != ce::MODEL_CALL_STARTED
            || original.payload.get("purpose").is_some()
        {
            return Err(format!(
                "forwarded request does not refer to an ordinary model request: {id}"
            ));
        }
        // The forwarded request contains mechanical receipts. Select from the
        // original pointers, while retaining the already-assembled instructions
        // and tools used by that conversation. Existing summaries stay covered.
        let original_parts = original.payload["input"]["parts"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let (_, covered) = self.substitute_summary(&original_parts, ctx)?;
        match self.maybe_condense(
            CondenseTrigger::Manual {
                command: event,
                assembled: &assembled.payload,
            },
            &original_parts,
            &covered,
            ctx,
        )? {
            CondenseOutcome::Started => {}
            CondenseOutcome::Skipped(reason) => self.manual_compact_skipped(event, reason, ctx),
        }
        Ok(())
    }

    pub(super) fn compact_without_summary(
        &self,
        event: &EventEnvelope,
        reject: bool,
        reason: &str,
        ctx: &mut Ctx,
    ) -> Result<(), String> {
        let current = self.observation(ctx.log(), |state| {
            state.condense_completion.as_deref() == Some(&event.id)
        })?;
        let mut manual = false;
        if let Some(request_id) = event.causes.first() {
            if let Some(request) = ctx
                .log()
                .get(request_id)
                .map_err(|error| error.to_string())?
            {
                if let Some(cause) = request.causes.first() {
                    let origin = ctx
                        .log()
                        .get(cause)
                        .map_err(|error| error.to_string())?
                        .ok_or_else(|| format!("compaction origin missing: {cause}"))?;
                    manual = origin.event_type == ce::EXTERNAL_INPUT
                        && origin.payload["channel"] == COMPACT_CHANNEL;
                }
            }
        }
        let suspend = reject && current;
        let reason = if reject && !current {
            "Compaction result belongs to an older configuration or attempt; the current pause is unchanged"
        } else {
            reason
        };
        ctx.emit(
            "decision",
            EventDraft::new(
                DECISION,
                &[&event.id],
                json!({
                    "scale": if manual { "manual" } else { "window" },
                    "action": if suspend { "suspend" } else { "compact_skipped" },
                }),
            )
            .with_reason(reason),
        );
        Ok(())
    }

    fn manual_compact_skipped(&self, event: &EventEnvelope, reason: &str, ctx: &mut Ctx) {
        ctx.emit(
            "decision",
            EventDraft::new(
                DECISION,
                &[&event.id],
                json!({
                    "scale":"manual", "action":"compact_skipped"
                }),
            )
            .with_reason(reason),
        );
    }
}
