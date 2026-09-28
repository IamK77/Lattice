//! Dependencies of one UI fold, resolved before mutating any displayed state.
//! The fixture/legacy fold can accumulate these maps. A reader-backed fold
//! replaces them for each event and releases them immediately afterwards.

use std::collections::HashMap;
use std::io;

use lattice::core_events as ce;
use lattice::view::facts::EventFacts;
use lattice::EventEnvelope;
use serde_json::Value;

#[derive(Default, Clone)]
#[cfg_attr(test, derive(Debug, PartialEq))]
pub(super) struct EventInputs {
    sizes: HashMap<String, (&'static str, u64)>,
    calls: HashMap<String, (String, Value)>,
    started: HashMap<String, (String, i64)>,
}

/// RFC 3339 (what the ledger writes) to epoch milliseconds.
pub(super) fn stamp_millis(time: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(time)
        .ok()
        .map(|t| t.timestamp_millis())
}

impl EventInputs {
    /// Counts for startup diagnostics, in tool, size, model-start order.
    pub fn counts(&self) -> [usize; 3] {
        [self.calls.len(), self.sizes.len(), self.started.len()]
    }

    /// Release allocations too: an indexed fold must not retain its peak map capacity.
    pub fn release(&mut self) {
        *self = Self::default();
    }

    pub fn size(&self, key: &str) -> Option<(&'static str, u64)> {
        self.sizes.get(key).copied()
    }

    pub fn observe_size(&mut self, event: &EventEnvelope) {
        let size = lattice::view::facts::measure(event);
        if event.event_type == ce::MODEL_CALL_COMPLETED {
            self.sizes.insert(
                format!("{}#thinking", event.id),
                ("thinking", size.thinking),
            );
        }
        self.sizes
            .insert(event.id.clone(), (size.kind.label(), size.bytes));
    }

    pub fn observe_model_start(&mut self, event: &EventEnvelope) {
        if event.event_type == ce::MODEL_CALL_STARTED {
            if let Some(at) = stamp_millis(&event.time) {
                let model = event.payload["model"].as_str().unwrap_or_default();
                self.started
                    .insert(event.id.clone(), (model.to_string(), at));
            }
        }
    }

    pub fn model_start(&self, causes: &[String]) -> Option<&(String, i64)> {
        causes.iter().find_map(|cause| self.started.get(cause))
    }

    pub fn consume_model_start(&mut self, causes: &[String]) {
        let _ = causes.iter().find_map(|cause| self.started.remove(cause));
    }

    pub fn observe_tool_start(&mut self, event: &EventEnvelope) {
        if event.event_type == ce::TOOL_EXEC_STARTED {
            let payload = &event.payload;
            if let Some(call) = payload["call"].as_str() {
                self.calls.insert(
                    call.to_string(),
                    (
                        payload["tool"].as_str().unwrap_or_default().to_string(),
                        payload["arguments"].clone(),
                    ),
                );
            }
        }
    }

    pub fn tool_request(&self, call: &str) -> Option<&(String, Value)> {
        self.calls.get(call)
    }

    /// Preserve the original size-map overwrite order even for imported IDs
    /// ending in the UI's historical synthetic `#thinking` suffix.
    fn size_cell(
        facts: &EventFacts,
        key: &str,
        through: u64,
    ) -> io::Result<Option<(&'static str, u64)>> {
        let mut cell = facts
            .size_at(key, through)?
            .map(|(seq, size)| (seq, (size.kind.label(), size.bytes)));
        if let Some(parent) = key.strip_suffix("#thinking") {
            if let Some((seq, size)) = facts.size_at(parent, through)? {
                if size.thinking > 0 && cell.as_ref().is_none_or(|(previous, _)| seq > *previous) {
                    cell = Some((seq, ("thinking", size.thinking)));
                }
            }
        }
        Ok(cell.map(|(_, value)| value))
    }

    /// Prepare live dependencies, rebuilding the derived index at most once.
    /// History recovery has a different boundary and keeps its own repair path.
    pub fn read_live(
        facts: &mut EventFacts,
        event: &EventEnvelope,
    ) -> io::Result<(Self, Option<String>)> {
        match Self::read(facts, event) {
            Ok(inputs) => Ok((inputs, None)),
            Err(initial) => {
                let reason = initial.to_string();
                *facts = facts.rebuild(event.seq, reason.clone()).map_err(|error| {
                    io::Error::new(error.kind(), format!("UI dependencies unreadable ({reason}); rebuilding facts failed: {error}"))
                })?;
                // One cold recomputation, never a loop and never tool replay.
                let inputs = Self::read(facts, event)?;
                Ok((inputs, Some(reason)))
            }
        }
    }

    pub fn read(facts: &mut EventFacts, event: &EventEnvelope) -> io::Result<Self> {
        if facts.through() < event.seq {
            facts.catch_up(event.seq)?;
        }
        let mut inputs = Self::default();
        if event.event_type == ce::MODEL_CALL_STARTED && event.payload.get("purpose").is_none() {
            let parts = event.payload["input"]["parts"]
                .as_array()
                .or_else(|| event.payload["input"].as_array());
            for part in parts.into_iter().flatten() {
                let Some(id) = part.get("event").and_then(Value::as_str) else {
                    continue;
                };
                for key in [id.to_string(), format!("{id}#thinking")] {
                    if let Some(cell) = Self::size_cell(facts, &key, event.seq)? {
                        inputs.sizes.insert(key, cell);
                    }
                }
            }
        } else if event.event_type == ce::MODEL_CALL_COMPLETED
            && event.payload.get("purpose").is_none()
        {
            if let Some(id) = facts.get(&event.id)?.related_start {
                let start = facts.get(&id)?.model_start.ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        "paired model request has no timestamp",
                    )
                })?;
                inputs.started.insert(id, (start.model, start.millis));
            }
        } else if event.event_type == ce::TOOL_EXEC_COMPLETED {
            if let Some(request) = facts.related_event(&event.id)? {
                if request.event_type != ce::TOOL_EXEC_STARTED {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "paired tool request has the wrong event type",
                    ));
                }
                let call = event.payload["call"].as_str().unwrap_or_default();
                inputs.calls.insert(
                    call.into(),
                    (
                        request.payload["tool"].as_str().unwrap_or_default().into(),
                        request.payload["arguments"].clone(),
                    ),
                );
            }
        }
        Ok(inputs)
    }
}

#[cfg(test)]
mod tests {
    use super::super::{fold_render, Ui, SETTLED_TICK};
    use super::*;
    use lattice::{EventDraft, EventLog, RenderEvent};
    use serde_json::json;

    fn append(log: &mut EventLog, kind: &str, causes: &[&str], payload: Value) -> EventEnvelope {
        log.append(EventDraft::new(kind, causes, payload), "fixture")
            .unwrap()
    }

    fn fixed_event(
        seq: u64,
        id: &str,
        kind: &str,
        causes: &[&str],
        payload: Value,
    ) -> EventEnvelope {
        serde_json::from_value(json!({
            "v":1, "id":id, "seq":seq, "stream":"pairing", "source":"fixture",
            "time":format!("2026-09-15T00:00:{seq:02}Z"),
            "type":kind, "causes":causes, "payload":payload
        }))
        .unwrap()
    }

    fn fixed_log(dir: &std::path::Path, events: &[EventEnvelope]) -> EventLog {
        let path = dir.join("pairing.jsonl");
        let bytes: String = events
            .iter()
            .map(|event| format!("{}\n", serde_json::to_string(event).unwrap()))
            .collect();
        std::fs::write(&path, bytes).unwrap();
        EventLog::open(ce::core_event_decls(), "pairing", Some(path)).unwrap()
    }

    #[test]
    fn preflight_preserves_model_pairing_and_the_fold_consumes_even_without_usage() {
        for indexed in [false, true] {
            for payload in [
                json!({"usage":{"prompt_tokens":17}}),
                json!({"text":"no accounting"}),
            ] {
                let start = fixed_event(
                    1,
                    "start",
                    ce::MODEL_CALL_STARTED,
                    &[],
                    json!({"model":"request-model"}),
                );
                let done = fixed_event(2, "done", ce::MODEL_CALL_COMPLETED, &["start"], payload);
                let dir = tempfile::tempdir().unwrap();
                let log = fixed_log(dir.path(), &[start.clone(), done.clone()]);
                let mut ui = Ui::replayed(&[]);
                if indexed {
                    ui.bind_event_facts(&log.reader(), 2).unwrap();
                }
                fold_render(&mut ui, RenderEvent::Appended(Box::new(start))).unwrap();
                if indexed {
                    ui.domain.event_inputs =
                        EventInputs::read(ui.event_facts.as_mut().unwrap(), &done).unwrap();
                }
                for _ in 0..2 {
                    let usage = ui.completed_usage(&done);
                    assert_eq!(
                        usage.map(|usage| usage.millis),
                        done.payload.get("usage").map(|_| 1000)
                    );
                    let (model, _) = ui.domain.event_inputs.model_start(&done.causes).unwrap();
                    assert_eq!(model, "request-model");
                }
                // Exercise the actual consuming stage before the indexed scope releases its maps.
                ui.note_usage(&done);
                assert!(ui.domain.event_inputs.model_start(&done.causes).is_none());
                if done.payload.get("usage").is_some() {
                    assert_eq!(ui.domain.accounting.last_call().unwrap().millis, 1000);
                    assert_eq!(ui.domain.accounting.session_total().calls, 1);
                } else {
                    assert!(ui.domain.accounting.last_call().is_none());
                }
            }
        }
    }

    #[test]
    fn tool_request_queries_do_not_consume_and_reused_calls_overwrite() {
        let first = fixed_event(
            1,
            "first",
            ce::TOOL_EXEC_STARTED,
            &[],
            json!({"call":"same","tool":"Run","arguments":{"command":"first"}}),
        );
        let done = fixed_event(
            2,
            "done",
            ce::TOOL_EXEC_COMPLETED,
            &["first"],
            json!({"call":"same","status":"ok","result":{"job":"job","background":true}}),
        );
        let second = fixed_event(
            3,
            "second",
            ce::TOOL_EXEC_STARTED,
            &[],
            json!({"call":"same","tool":"Run","arguments":{"command":"second"}}),
        );
        let later = fixed_event(
            4,
            "later",
            ce::TOOL_EXEC_COMPLETED,
            &["second"],
            done.payload.clone(),
        );
        let dir = tempfile::tempdir().unwrap();
        let log = fixed_log(
            dir.path(),
            &[first.clone(), done.clone(), second.clone(), later.clone()],
        );
        for indexed in [false, true] {
            let mut ui = Ui::replayed(&[]);
            let mut facts = EventFacts::recover(log.reader(), 4).unwrap();
            for (start, completion, label) in
                [(&first, &done, "first"), (&second, &later, "second")]
            {
                if indexed {
                    ui.domain.event_inputs = EventInputs::read(&mut facts, completion).unwrap();
                } else {
                    ui.note_background(start, 0);
                }
                for _ in 0..2 {
                    ui.note_background(completion, 0);
                    assert_eq!(ui.domain.background.rows().len(), 1);
                    assert_eq!(ui.domain.background.rows()[0].label, label);
                    assert_eq!(
                        ui.domain.event_inputs.tool_request("same").unwrap().1["command"],
                        label
                    );
                }
            }
            ui.domain.event_inputs.release();
            assert_eq!(ui.domain.event_inputs.counts(), [0; 3]);
            assert_eq!(ui.domain.event_inputs.calls.capacity(), 0);
        }
    }

    #[test]
    fn model_pairing_uses_first_matching_cause_and_purpose_completion_keeps_it() {
        let mut ui = Ui::replayed(&[]);
        for (seq, id) in [(1, "first"), (2, "second")] {
            ui.note_usage(&fixed_event(
                seq,
                id,
                ce::MODEL_CALL_STARTED,
                &[],
                json!({"model":id,"purpose":"condense"}),
            ));
        }
        let mut done = fixed_event(
            3,
            "done",
            ce::MODEL_CALL_COMPLETED,
            &["missing", "second", "first"],
            json!({"purpose":"condense"}),
        );
        ui.note_usage(&done);
        assert_eq!(
            ui.domain.event_inputs.model_start(&done.causes).unwrap().0,
            "second"
        );
        done.payload = json!({});
        ui.note_usage(&done);
        assert_eq!(
            ui.domain.event_inputs.model_start(&done.causes).unwrap().0,
            "first"
        );
        ui.note_usage(&done);
        assert!(ui.domain.event_inputs.model_start(&done.causes).is_none());
    }

    #[test]
    fn imported_size_keys_preserve_historical_overwrites_and_prefix_boundaries() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("imported.jsonl");
        let mut events = Vec::new();
        for (index, (id, kind, payload)) in [
            (
                "first",
                ce::MODEL_CALL_COMPLETED,
                json!({"text":"answer","reasoning":[{"type":"text","text":"thought"}]}),
            ),
            (
                "first#thinking",
                ce::USER_MESSAGE,
                json!({"text":"overwrites the synthetic key"}),
            ),
            (
                "second#thinking",
                ce::USER_MESSAGE,
                json!({"text":"overwritten by the later thought"}),
            ),
            (
                "second",
                ce::MODEL_CALL_COMPLETED,
                json!({"text":"answer","reasoning":[{"type":"text","text":"later thought"}]}),
            ),
        ]
        .into_iter()
        .enumerate()
        {
            events.push(serde_json::from_value::<EventEnvelope>(json!({"v":1,"id":id,"seq":index + 1,"stream":"imported","time":"2026-09-15T00:00:00Z","type":kind,"source":"fixture","causes":[],"payload":payload})).unwrap());
        }
        let bytes: String = events
            .iter()
            .map(|event| format!("{}\n", serde_json::to_string(event).unwrap()))
            .collect();
        std::fs::write(&path, bytes).unwrap();
        let log = EventLog::open(ce::core_event_decls(), "imported", Some(path)).unwrap();
        let facts = EventFacts::recover(log.reader(), 4).unwrap();
        let mut original = Ui::replayed(&[]);
        for event in &events {
            original.domain.event_inputs.observe_size(event);
            for key in [
                "first",
                "first#thinking",
                "second",
                "second#thinking",
                "second#thinking#thinking",
                "absent",
            ] {
                assert_eq!(
                    EventInputs::size_cell(&facts, key, event.seq).unwrap(),
                    original.domain.event_inputs.size(key),
                    "key {key} at {}",
                    event.seq
                );
            }
        }
    }

    #[test]
    fn damaged_dependency_pages_rebuild_without_replaying_ui_state() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("damaged.ledger");
        let declarations = ce::core_event_decls()
            .into_iter()
            .map(|mut d| {
                d.schema = None;
                d
            })
            .collect();
        let mut log =
            EventLog::open_segmented(declarations, "damaged", root.clone(), 4096).unwrap();
        let user = append(&mut log, ce::USER_MESSAGE, &[], json!({"text":"question"}));
        let start = append(
            &mut log,
            ce::MODEL_CALL_STARTED,
            &[],
            json!({"model":"model","input":{"parts":[{"event":user.id}]}}),
        );
        drop(EventFacts::recover(log.reader(), start.seq).unwrap());
        let mut damaged = 0;
        for entry in std::fs::read_dir(&root).unwrap() {
            let path = entry.unwrap().path();
            if path
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("cards-")
            {
                let mut bytes = std::fs::read(&path).unwrap();
                bytes[0] ^= 1;
                std::fs::write(&path, bytes).unwrap();
                damaged += 1;
            }
        }
        assert!(damaged > 0);
        let originals: Vec<_> = std::fs::read_dir(&root)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "jsonl"))
            .map(|path| {
                let bytes = std::fs::read(&path).unwrap();
                (path, bytes)
            })
            .collect();
        let original = Ui::replayed(&[user.clone(), start.clone()]);
        let mut ui = Ui::replayed(&[]);
        ui.tick = SETTLED_TICK;
        ui.bind_event_facts(&log.reader(), start.seq).unwrap();
        fold_render(&mut ui, RenderEvent::Appended(Box::new(user))).unwrap();
        fold_render(&mut ui, RenderEvent::Appended(Box::new(start.clone()))).unwrap();
        assert_eq!(ui.domain.turns.number(), original.domain.turns.number());
        assert_eq!(ui.entries, original.entries);
        assert_eq!(
            ui.domain.accounting.parts(),
            original.domain.accounting.parts()
        );
        assert!(ui
            .flash
            .as_deref()
            .unwrap()
            .contains("Rebuilt cached event facts"));
        assert_eq!(ui.domain.event_inputs.counts(), [0; 3]);
        for (path, bytes) in originals {
            assert_eq!(std::fs::read(path).unwrap(), bytes);
        }
        assert_eq!(log.reader().snapshot_end(), start.seq);
        let mut warm = EventFacts::recover(log.reader(), start.seq).unwrap();
        assert!(warm.cold_reason().is_none());
        assert!(EventInputs::read(&mut warm, &start).is_ok());
        let (_, repaired) = EventInputs::read_live(&mut warm, &start).unwrap();
        assert!(repaired.is_none(), "ordinary reads must not claim a repair");

        // Rebuilding derived data cannot make an uncommitted event real.
        let mut untouched = Ui::replayed(&[]);
        untouched
            .bind_event_facts(&log.reader(), start.seq)
            .unwrap();
        untouched.flash = Some("existing notice".into());
        untouched.live_output.seed_reply("unfinished reply".into());
        untouched
            .live_output
            .seed_thinking("unfinished reasoning".into());
        untouched
            .domain
            .authorizations
            .restore(vec![("pending".into(), "call".into())]);
        untouched.domain.event_inputs.observe_model_start(&start);
        let before_inputs = untouched.domain.event_inputs.clone();
        let mut uncommitted = start;
        uncommitted.seq += 1;
        let error =
            fold_render(&mut untouched, RenderEvent::Appended(Box::new(uncommitted))).unwrap_err();
        assert!(error.to_string().contains("rebuilding facts failed"));
        assert_eq!(untouched.domain.event_inputs, before_inputs);
        assert_eq!(untouched.domain.authorizations.next(), Some("pending"));
        assert_eq!(untouched.live_output.reply(), "unfinished reply");
        assert_eq!(untouched.live_output.thinking(), "unfinished reasoning");
        assert_eq!(untouched.flash.as_deref(), Some("existing notice"));
        assert_eq!(untouched.domain.turns.number(), 0);
        assert!(
            !untouched.domain.turns.busy()
                && untouched.domain.accounting.last_call().is_none()
                && untouched.entries.is_empty()
        );
    }

    #[test]
    fn indexed_event_inputs_match_the_original_fold_without_retaining_history_maps() {
        let dir = tempfile::tempdir().unwrap();
        let declarations = ce::core_event_decls()
            .into_iter()
            .map(|mut d| {
                d.schema = None;
                d
            })
            .collect();
        let mut log = EventLog::open_segmented(
            declarations,
            "ui-inputs",
            dir.path().join("ui.ledger"),
            4096,
        )
        .unwrap();
        let mut events = Vec::new();
        let user = append(&mut log, ce::USER_MESSAGE, &[], json!({"text":"question"}));
        events.push(user.clone());
        let start = append(
            &mut log,
            ce::MODEL_CALL_STARTED,
            &[],
            json!({"model":"first","input":{"parts":[{"event":user.id},{"event":"unknown"}]},"system":"instructions","tools":[]}),
        );
        events.push(start.clone());
        let done = append(
            &mut log,
            ce::MODEL_CALL_COMPLETED,
            &[&start.id],
            json!({"text":"reply","reasoning":[{"type":"text","text":"thought"}],"usage":{"prompt_tokens":17,"completion_tokens":5,"input_tokens":17,"output_tokens":5}}),
        );
        events.push(done.clone());
        for index in 0..140 {
            events.push(append(
                &mut log,
                ce::USER_MESSAGE,
                &[],
                json!({"text":format!("line {index}")}),
            ));
        }
        for command in ["first command", "later command"] {
            events.push(append(
                &mut log,
                ce::TOOL_EXEC_STARTED,
                &[],
                json!({"call":"reused","tool":"Run","arguments":{"command":command}}),
            ));
            events.push(append(
                &mut log,
                ce::TOOL_EXEC_COMPLETED,
                &[],
                json!({"call":"reused","status":"ok","result":{"job":"one","background":true}}),
            ));
        }
        events.push(append(&mut log, ce::MODEL_CALL_STARTED, &[], json!({"model":"second","input":[{"event":done.id},{"event":user.id},{"digest":"summary"}]})));
        events.push(append(
            &mut log,
            ce::MODEL_CALL_COMPLETED,
            &[&start.id],
            json!({"usage":{"prompt_tokens":23,"completion_tokens":2}}),
        ));
        let through = events.last().unwrap().seq;
        let mut original = Ui::replayed(&[]);
        let mut indexed = Ui::replayed(&[]);
        indexed.tick = SETTLED_TICK;
        indexed.bind_event_facts(&log.reader(), through).unwrap();
        indexed.bind_peaks(&log.reader()).unwrap();
        for event in &events {
            original.absorb(event, SETTLED_TICK);
            fold_render(&mut indexed, RenderEvent::Appended(Box::new(event.clone()))).unwrap();
            assert_eq!(indexed.entries, original.entries, "cards at {}", event.seq);
            assert_eq!(
                indexed.domain.accounting.parts(),
                original.domain.accounting.parts(),
                "composition at {}",
                event.seq
            );
            assert_eq!(
                indexed.domain.accounting.previous(),
                original.domain.accounting.previous()
            );
            assert_eq!(
                indexed.domain.accounting.turn_growth(),
                original.domain.accounting.turn_growth()
            );
            assert_eq!(
                indexed.domain.accounting.session_growth(),
                original.domain.accounting.session_growth()
            );
            assert_eq!(
                indexed.domain.accounting.indexed_history().unwrap(),
                original.domain.accounting.reference_history()
            );
            assert_eq!(
                indexed.domain.accounting.history_growth(),
                lattice::view::peaks::GrowthSummary::from_values(
                    &original
                        .domain
                        .accounting
                        .reference_history()
                        .iter()
                        .map(|(_, value)| *value)
                        .collect::<Vec<_>>()
                )
            );
            assert!(indexed.domain.accounting.reference_history().is_empty());
            assert_eq!(
                format!("{:?}", indexed.domain.accounting.last_call()),
                format!("{:?}", original.domain.accounting.last_call())
            );
            assert_eq!(
                format!("{:?}", indexed.domain.accounting.turn_total()),
                format!("{:?}", original.domain.accounting.turn_total())
            );
            assert_eq!(
                format!("{:?}", indexed.domain.accounting.session_total()),
                format!("{:?}", original.domain.accounting.session_total())
            );
            assert_eq!(
                indexed.domain.background.rows(),
                original.domain.background.rows()
            );
            assert!(
                indexed.domain.event_inputs.calls.is_empty()
                    && indexed.domain.event_inputs.sizes.is_empty()
                    && indexed.domain.event_inputs.started.is_empty()
            );
            assert_eq!(
                indexed.domain.event_inputs.calls.capacity()
                    + indexed.domain.event_inputs.sizes.capacity()
                    + indexed.domain.event_inputs.started.capacity(),
                0
            );
        }
        assert!(
            original.domain.event_inputs.counts()[0] > 0
                && original.domain.event_inputs.counts()[1] > 140
        );
        assert!(!indexed
            .domain
            .accounting
            .indexed_history()
            .unwrap()
            .is_empty());
        assert_eq!(indexed.domain.background.rows().len(), 1);
        assert_eq!(indexed.domain.background.rows()[0].label, "later command");
        let mut restored = Ui::replayed(&[]);
        restored.replay_prefix(&log.reader(), through).unwrap();
        assert_eq!(
            crate::terminal_host::cards::tests::materialize(&restored),
            original.entries
        );
        assert!(restored.entries.is_empty());
        assert_eq!(
            restored.domain.accounting.indexed_history().unwrap(),
            original.domain.accounting.reference_history()
        );
        assert!(restored.domain.accounting.reference_history().is_empty());
        assert_eq!(restored.domain.event_inputs.counts(), [0; 3]);
    }
}
