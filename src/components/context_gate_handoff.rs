//! Live compaction waits. The ledger retains the original requests; restarting
//! settles them, rather than rebuilding a queue that could issue model calls.
use super::*;

impl ContextGate {
    fn stopped_since(seq: u64, ctx: &Ctx) -> Result<bool, String> {
        ctx.log()
            .scan_back_types(&[ce::INTERRUPTED], |event, _| {
                Ok((event.seq > seq
                    && event.payload["by"] == "user"
                    && event.payload.get("call").is_none())
                .then_some(()))
            })
            .map(|event| event.is_some())
            .map_err(|e| e.to_string())
    }

    fn launch_compaction(&mut self, decision: &EventEnvelope, ctx: &mut Ctx) -> Result<(), String> {
        let Some(submission) = self.submission.as_ref().filter(|submission| {
            submission.prepared.is_some() && decision.causes.contains(&submission.origin)
        }) else {
            return Ok(());
        };
        let origin = ctx
            .log()
            .header(&submission.origin)
            .map_err(|e| e.to_string())?
            .ok_or("compaction origin missing")?;
        let epoch = self.observation(ctx.log(), |state| state.epoch)?;
        if origin.seq <= epoch || Self::stopped_since(origin.seq, ctx)? {
            self.submission = None;
            ctx.emit(
                "decision",
                EventDraft::new(
                    DECISION,
                    &[&decision.id],
                    json!({
                        "scale":decision.payload["scale"], "action":"compact_skipped"
                    }),
                )
                .with_reason("Compaction was cancelled before submission"),
            );
            return self.reject_waiting("Compaction was cancelled before submission", ctx);
        }
        let submission = self.submission.as_mut().expect("submission was checked");
        let payload = submission
            .prepared
            .take()
            .expect("prepared request was checked");
        submission.origin = decision.id.clone();
        ctx.emit(
            "condense",
            EventDraft::new(ce::MODEL_CALL_STARTED, &[&decision.id], payload),
        );
        Ok(())
    }

    pub(super) fn hold(&mut self, event: &EventEnvelope) {
        if !self.waiting.iter().any(|ask| ask.id == event.id) {
            self.waiting.push(event.clone());
        }
    }

    // A held request has no downstream forwarding copy yet. Check its original
    // chain as well, because an upstream gate's head may have been interrupted.
    fn waiting_is_live(ask: &EventEnvelope, ctx: &Ctx) -> Result<bool, String> {
        let mut ids = vec![ask.id.clone()];
        let mut seen = HashSet::new();
        let mut start = ask.seq;
        while let Some(id) = ids.pop() {
            if !seen.insert(id.clone()) {
                continue;
            }
            let header = ctx
                .log()
                .header(&id)
                .map_err(|e| e.to_string())?
                .ok_or_else(|| format!("waiting request missing: {id}"))?;
            for cause in &header.causes {
                let parent = ctx
                    .log()
                    .header(cause)
                    .map_err(|e| e.to_string())?
                    .ok_or_else(|| format!("waiting request cause missing: {cause}"))?;
                if parent.event_type == ce::MODEL_CALL_STARTED && !parent.has_purpose {
                    start = start.min(parent.seq);
                    ids.push(parent.id);
                }
            }
        }
        let mut pending = ce::PendingCalls::new(ce::MODEL_CALL_STARTED);
        ctx.log()
            .try_visit_header_range(start, ctx.log().snapshot_end(), |headers| {
                for header in headers {
                    if (header.event_type == ce::MODEL_CALL_STARTED && !header.has_purpose)
                        || ce::is_outcome(&header.event_type)
                    {
                        pending.observe(header.relations());
                    }
                }
                Ok(())
            })
            .map_err(|e| e.to_string())?;
        Ok(pending.requests().contains(&ask.id))
    }

    pub(super) fn reject(ask: &EventEnvelope, reason: &str, ctx: &mut Ctx) {
        // End the ordinary wait, not the compactor's unknown side effects.
        // An interruption also prevents queued user input from automatically
        // retrying the same audited compaction failure through the main loop.
        ctx.emit("interrupted", EventDraft::new(ce::INTERRUPTED, &[&ask.id], json!({
            "by":"context", "component":ctx.source()
        })).with_reason(&format!("{reason}. Original context was retained; no ordinary model request was sent. Use /compact for an explicit attempt or change the model, then continue.")));
    }

    pub(super) fn reject_waiting(&mut self, reason: &str, ctx: &mut Ctx) -> Result<(), String> {
        // Validate before draining: a read failure must not lose ownership.
        let live: Vec<_> = self
            .waiting
            .iter()
            .map(|ask| Self::waiting_is_live(ask, ctx))
            .collect::<Result<_, _>>()?;
        for (ask, live) in std::mem::take(&mut self.waiting).into_iter().zip(live) {
            if live {
                Self::reject(&ask, reason, ctx);
            }
        }
        Ok(())
    }

    pub(super) fn on_handoff(
        &mut self,
        event: &EventEnvelope,
        ctx: &mut Ctx,
    ) -> Result<(), String> {
        if event.source == ctx.source()
            && event.event_type == DECISION
            && event.payload["action"] == "condense"
        {
            return self.launch_compaction(event, ctx);
        }
        if self
            .submission
            .as_ref()
            .is_some_and(|submission| submission.published.as_deref() == Some(event.id.as_str()))
        {
            self.submission = None;
        }
        if self.waiting.is_empty() {
            return Ok(());
        }
        if self
            .waiting
            .iter()
            .map(|ask| Self::stopped_since(ask.seq, ctx))
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .any(|stopped| stopped)
        {
            return self.reject_waiting(
                "The user stopped the turn while compaction was pending",
                ctx,
            );
        }
        if event.event_type == SUMMARY
            && event.source == ctx.source()
            && self
                .latest_summary(ctx)?
                .is_some_and(|(id, _, _)| id == event.id)
        {
            let live: Vec<_> = self
                .waiting
                .iter()
                .map(|ask| Self::waiting_is_live(ask, ctx))
                .collect::<Result<_, _>>()?;
            let waiting = std::mem::take(&mut self.waiting);
            for (ask, live) in waiting.into_iter().zip(live) {
                if live {
                    // Reassemble from the original request only after the
                    // accepted summary is addressable on the committed ledger.
                    self.on_ask_after(&ask, Some(&event.id), ctx)?;
                }
            }
        } else if !self.condense_in_flight(ctx.log())?
            && (event.event_type == ce::INTERRUPTED
                || (event.source == ctx.source() && event.event_type == SUMMARY)
                || (event.source == ctx.source()
                    && event.event_type == DECISION
                    && matches!(
                        event.payload["action"].as_str(),
                        Some("suspend" | "compact_skipped")
                    )))
        {
            self.reject_waiting("Compaction ended without an accepted replacement", ctx)?;
        }
        Ok(())
    }
}
