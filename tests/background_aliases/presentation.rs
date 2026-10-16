use super::*;
use lattice::background_aliases::{operations, Reference};
use lattice::view::{history::History, Entry, ToolStatus};

fn display(entry: &Entry) -> String {
    match entry {
        Entry::Tool(card) => card.output.join("\n"),
        Entry::Wake(text) => text.clone(),
        other => panic!("unexpected card: {other:?}"),
    }
}

#[test]
fn transcript_receipts_wakes_and_operation_feedback_share_references_after_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("presentation.ledger");
    let open = || {
        EventLog::open_segmented(ce::core_event_decls(), "owner-stream", path.clone(), 4096)
            .unwrap()
    };
    let mut log = open();
    let mut expected = Vec::new();
    let mut operation_receipts = Vec::new();
    for (tool, result, body, stop, field) in [
        (
            "ask",
            json!({"job":1,"provider":"kept"}),
            json!({"job":1}),
            "CancelExpert",
            "job",
        ),
        (
            "Run",
            json!({"job":"Run","background":true}),
            json!({"job":"Run","exit_code":0,"stdout":"kept stdout","stderr":"kept stderr"}),
            "",
            "",
        ),
        (
            "Schedule",
            json!({"timer":1,"provider":"kept"}),
            json!({"timer":1,"fire":1}),
            "Unschedule",
            "timer",
        ),
        (
            "Watch",
            json!({"watch":1,"provider":"kept"}),
            json!({"watch":1,"fire":1}),
            "Unwatch",
            "watch",
        ),
    ] {
        let request = append(
            &mut log,
            ce::TOOL_EXEC_STARTED,
            &[],
            json!({"call":tool,"tool":tool,"arguments":{}}),
        );
        let forwarded = append(
            &mut log,
            ce::TOOL_EXEC_STARTED,
            &[&request.id],
            request.payload.clone(),
        );
        let receipt = append(
            &mut log,
            ce::TOOL_EXEC_COMPLETED,
            &[&forwarded.id],
            json!({"call":tool,"status":"ok","result":result}),
        );
        let reader = log.reader();
        let projection = Projection::recover(&reader, reader.snapshot_end()).unwrap();
        let identity = identity_for_event(&reader, &receipt).unwrap().unwrap();
        let reference = Reference {
            alias: projection.alias(&identity).unwrap().to_owned(),
            identity,
        };
        let wake = append(
            &mut log,
            ce::WAKE,
            &[&forwarded.id],
            json!({"source":format!("background:{tool}"),"summary":"provider wake kept","body":body}),
        );
        if tool == "Run" {
            // Duplicate command completions must not add a notification or
            // replace the already settled output, even when references exist.
            append(
                &mut log,
                ce::WAKE,
                &[&forwarded.id],
                json!({"source":"background:Run","body":{"job":"Run","exit_code":9,"stdout":"must not replace"}}),
            );
        }
        if !stop.is_empty() {
            let mut payload = json!({"call":format!("stop-{tool}"),"tool":stop,"arguments":{field:reference.alias,"reason":"fixture"}});
            operations::Resolver::default().normalize(&reader, &mut payload);
            assert!(operations::reference(&payload).unwrap().is_some());
            let request = append(&mut log, ce::TOOL_EXEC_STARTED, &[], payload);
            let receipt = append(
                &mut log,
                ce::TOOL_EXEC_COMPLETED,
                &[&request.id],
                json!({"call":format!("stop-{tool}"),"status":"error","error":{"code":"fixture.denied","message":"provider denial kept","blame":"request","retryable":false,"transient":false}}),
            );
            operation_receipts.push((receipt, reference.clone()));
        }
        expected.push((tool, reference, wake));
    }
    let end = log.reader().snapshot_end();
    drop(log);
    for warm in [false, true] {
        let log = open();
        let reader = log.reader();
        let mut history = History::recover(reader.clone(), end).unwrap();
        if warm {
            assert!(history.cold_reason().is_none());
        }
        let entries = history.load(0..history.len()).unwrap();
        assert_eq!(
            entries.len(),
            10,
            "four creation cards, three standalone wakes, three operation cards"
        );
        for (tool, reference, _) in &expected {
            let card = entries
                .iter()
                .find_map(|entry| match entry {
                    Entry::Tool(card) if card.call.as_deref() == Some(*tool) => Some(card),
                    _ => None,
                })
                .unwrap();
            let text = card.output.join("\n");
            assert_eq!(text.matches(&reference.to_string()).count(), 1);
            assert_eq!(card.status, ToolStatus::Ok);
            if *tool == "Run" {
                assert!(text.contains("kept stdout") && text.contains("kept stderr"));
                assert!(!text.contains("must not replace"));
            } else {
                assert!(text.contains("kept"));
                let wake = entries.iter().find(|entry| matches!(entry, Entry::Wake(text) if text.starts_with(&reference.to_string()))).unwrap();
                assert!(display(wake).contains("provider wake kept"));
            }
        }
        for (receipt, reference) in &operation_receipts {
            let card = entries
                .iter()
                .find_map(|entry| match entry {
                    Entry::Tool(card)
                        if card.call.as_deref() == receipt.payload["call"].as_str() =>
                    {
                        Some(card)
                    }
                    _ => None,
                })
                .unwrap();
            assert_eq!(card.status, ToolStatus::Failed);
            assert_eq!(
                card.output,
                [
                    "provider denial kept".to_owned(),
                    format!("[background reference: {reference}]")
                ]
            );
            let parts = [json!({"event":receipt.id})];
            for messages in [
                anthropic_model::materialize(&parts, &reader, None).unwrap(),
                openai_model::materialize(&parts, &reader, None).unwrap(),
                responses(&parts, &reader, None),
            ] {
                let rendered = serde_json::to_string(&messages).unwrap();
                assert!(rendered.contains(&reference.alias) && rendered.contains("owner-stream"));
                assert!(rendered.contains("provider denial kept"));
            }
        }
        history.save().unwrap();
        let cleared = history.empty_tail().unwrap();
        assert!(cleared.is_empty());
        assert_eq!(
            reader.snapshot_end(),
            end,
            "presentation never writes ledger events"
        );
    }
}

#[test]
fn repeated_card_reads_reuse_the_alias_projection_instead_of_rescanning_creations() {
    let dir = tempfile::tempdir().unwrap();
    let mut log = EventLog::open_segmented(
        ce::core_event_decls(),
        "owner-stream",
        dir.path().join("cost.ledger"),
        4096,
    )
    .unwrap();
    for id in 1..=128 {
        let request = append(
            &mut log,
            ce::TOOL_EXEC_STARTED,
            &[],
            json!({"call":format!("timer-{id}"),"tool":"Schedule","arguments":{}}),
        );
        append(
            &mut log,
            ce::TOOL_EXEC_COMPLETED,
            &[&request.id],
            json!({"call":format!("timer-{id}"),"status":"ok","result":{"timer":id}}),
        );
    }
    let reader = log.reader();
    let history = History::recover(reader.clone(), reader.snapshot_end()).unwrap();
    assert!(display(&history.get(0).unwrap()).contains("background reference: Timer"));
    let reads = || {
        let cache = reader.memory_stats().unwrap().cache.unwrap();
        cache.hits + cache.decodes
    };
    let before = reads();
    assert!(display(&history.get(127).unwrap()).contains("background reference: Timer"));
    assert!(
        reads() - before <= 6,
        "a second card read must not rescan the 128 creation receipts"
    );
}

#[test]
fn ui_supersession_uses_the_events_prefix_even_when_facts_are_ahead_or_revisited() {
    let mut log = EventLog::in_memory(ce::core_event_decls(), "owner");
    let mut receipts = Vec::new();
    for call in ["old", "new"] {
        let request = append(
            &mut log,
            ce::TOOL_EXEC_STARTED,
            &[],
            json!({"call":call,"tool":"Schedule","arguments":{}}),
        );
        receipts.push(append(
            &mut log,
            ce::TOOL_EXEC_COMPLETED,
            &[&request.id],
            json!({"call":call,"status":"ok","result":{"timer":1}}),
        ));
    }
    let reader = log.reader();
    let mut facts =
        lattice::view::facts::EventFacts::recover(reader.clone(), reader.snapshot_end()).unwrap();
    for event in [&receipts[0], &receipts[1], &receipts[0]] {
        let fact = facts.background_reference(event).unwrap().unwrap();
        assert!(
            !fact.superseded,
            "later evidence cannot change the event's own historical prefix"
        );
        assert_eq!(fact.reference.identity.created_by, event.causes[0]);
    }
}
