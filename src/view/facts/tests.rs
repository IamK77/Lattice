use super::*;
use crate::{EventDraft, EventLog};
use serde_json::json;

fn log(root: &std::path::Path) -> EventLog {
    let declarations = ce::core_event_decls()
        .into_iter()
        .map(|mut declaration| {
            declaration.schema = None;
            declaration
        })
        .collect();
    EventLog::open_segmented(declarations, "facts", root.into(), 4096).unwrap()
}

fn append(log: &mut EventLog, kind: &str, causes: &[&str], payload: Value) -> EventEnvelope {
    log.append(EventDraft::new(kind, causes, payload), "fixture")
        .unwrap()
}

#[test]
fn long_material_working_set_reuses_sizes_and_absence() {
    let mut log = EventLog::in_memory(
        vec![crate::EventTypeDecl::new(ce::USER_MESSAGE, "fixture")],
        "large-sizes",
    );
    // The real migrated ledger had requests with 16,019 distinct material IDs.
    // Each also probes its synthetic thinking key. A small cache passed tiny
    // fixtures but evicted the entire working set on every real request.
    let ids: Vec<_> = (0..16_019)
        .map(|_| append(&mut log, ce::USER_MESSAGE, &[], json!({"text":"x"})).id)
        .collect();
    let through = log.reader().snapshot_end();
    let facts = EventFacts::recover(log.reader(), through).unwrap();
    for _ in 0..2 {
        for id in &ids {
            assert!(facts.size_at(id, through).unwrap().is_some());
            assert!(facts
                .size_at(&format!("{id}#thinking"), through)
                .unwrap()
                .is_none());
        }
    }
    assert_eq!(
        facts.sizes.borrow().lookups,
        ids.len() * 2,
        "the next large request must reuse its already resolved materials"
    );
}

#[test]
fn material_size_cache_is_bounded_and_respects_prefix_growth() {
    let temp = tempfile::tempdir().unwrap();
    let mut log = log(&temp.path().join("sizes.ledger"));
    let first = append(&mut log, ce::USER_MESSAGE, &[], json!({"text":"first"}));
    let mut facts = EventFacts::recover(log.reader(), first.seq).unwrap();
    let expected = facts
        .find_at(&first.id, first.seq)
        .unwrap()
        .map(|(seq, fact)| (seq, fact.size));
    let missing = format!("{}#thinking", first.id);
    for _ in 0..20 {
        assert_eq!(facts.size_at(&first.id, first.seq).unwrap(), expected);
        assert!(facts.size_at(&missing, first.seq).unwrap().is_none());
    }
    assert_eq!(
        facts.sizes.borrow().lookups,
        2,
        "repeated material sizes must not requery the index"
    );
    assert!(facts.size_at(&first.id, 0).unwrap().is_none());
    assert!(facts.size_at(&first.id, first.seq + 1).is_err());

    let later = append(&mut log, ce::USER_MESSAGE, &[], json!({"text":"later"}));
    assert!(facts.size_at(&later.id, first.seq).unwrap().is_none());
    assert!(!facts.sizes.borrow().entries.contains_key(&later.id));
    // Model a negative answer obtained before this generated ID was appended.
    // It is valid for the old prefix, never for the grown one.
    facts
        .sizes
        .borrow_mut()
        .entries
        .insert(later.id.clone(), CachedSize::Absent(first.seq));
    facts.catch_up(later.seq).unwrap();
    assert!(facts.size_at(&later.id, first.seq).unwrap().is_none());
    assert_eq!(
        facts.size_at(&later.id, later.seq).unwrap().unwrap().0,
        later.seq
    );
    assert!(facts.size_at(&later.id, 0).unwrap().is_none());
    facts.failed = true;
    assert!(
        facts.size_at(&later.id, later.seq).is_err(),
        "cached sizes cannot hide a failed fold"
    );
    facts.failed = false;

    for index in 0..SIZE_CACHE_ENTRIES * 2 {
        assert!(facts
            .size_at(&format!("absent-{index}"), later.seq)
            .unwrap()
            .is_none());
        assert!(facts.sizes.borrow().entries.len() <= SIZE_CACHE_ENTRIES);
    }
    let huge = "x".repeat(SIZE_CACHE_KEY_BYTES + 1);
    assert!(facts.size_at(&huge, later.seq).unwrap().is_none());
    assert!(!facts.sizes.borrow().entries.contains_key(&huge));
    let rebuilt = facts.rebuild(later.seq, "fixture".into()).unwrap();
    assert!(rebuilt.sizes.borrow().entries.is_empty());
}

#[test]
fn material_size_read_errors_never_become_cached_absence() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("size-error.ledger");
    let mut log = log(&root);
    let first = append(&mut log, ce::USER_MESSAGE, &[], json!({"text":"first"}));
    drop(EventFacts::recover(log.reader(), first.seq).unwrap());
    let facts = EventFacts::recover(log.reader(), first.seq).unwrap();
    for entry in std::fs::read_dir(&root).unwrap() {
        let path = entry.unwrap().path();
        if path
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("cards-")
        {
            std::fs::write(path, "damaged").unwrap();
        }
    }
    // An event beyond the queried prefix is absent without reading its page.
    // Do not let this answer conceal that page's error at a later boundary.
    assert!(facts.size_at(&first.id, 0).unwrap().is_none());
    for _ in 0..2 {
        assert!(facts.size_at(&first.id, first.seq).is_err());
        assert!(facts.sizes.borrow().entries.is_empty());
    }
}

#[test]
fn warm_tail_rebuilds_damaged_facts_but_never_repairs_unreadable_originals() {
    for damage_original in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("rebuild.ledger");
        let mut ledger = log(&root);
        let start = append(
            &mut ledger,
            ce::TOOL_EXEC_STARTED,
            &[],
            json!({"call":"one","tool":"Run"}),
        );
        drop(EventFacts::recover(ledger.reader(), start.seq).unwrap());
        append(
            &mut ledger,
            ce::USER_MESSAGE,
            &[],
            json!({"text":"padding".repeat(2000)}),
        );
        let completion = append(
            &mut ledger,
            ce::TOOL_EXEC_COMPLETED,
            &[],
            json!({"call":"one","status":"ok"}),
        );
        drop(ledger);
        let ledger = log(&root);
        let first_volume = root.join("00000000000000000000.jsonl");
        let mut original = std::fs::read(&first_volume).unwrap();
        if damage_original {
            original[0] ^= 1;
            std::fs::write(&first_volume, &original).unwrap();
        }
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
                std::fs::write(path, bytes).unwrap();
                damaged += 1;
            }
        }
        assert!(damaged > 0);
        let restored = EventFacts::recover(ledger.reader(), completion.seq);
        if damage_original {
            assert!(
                restored.is_err(),
                "a derived rebuild cannot hide unreadable source"
            );
        } else {
            let restored = restored.unwrap();
            assert!(restored.cold_reason().is_some());
            assert_eq!(
                restored.related_event(&completion.id).unwrap().unwrap().id,
                start.id
            );
            // An early visible event must not move the saved facts backward.
            let rebuilt = restored.rebuild(start.seq, "fixture".into()).unwrap();
            assert_eq!(rebuilt.through(), completion.seq);
            let warm = EventFacts::recover(ledger.reader(), completion.seq).unwrap();
            assert!(warm.cold_reason().is_none());
            assert_eq!(
                warm.related_event(&completion.id).unwrap().unwrap().id,
                start.id
            );
        }
        assert_eq!(std::fs::read(first_volume).unwrap(), original);
        assert_eq!(ledger.reader().snapshot_end(), completion.seq);
    }
}

#[test]
fn missing_completion_calls_keep_the_legacy_empty_identity_fallback() {
    let temp = tempfile::tempdir().unwrap();
    let mut log = log(&temp.path().join("empty-call.ledger"));
    let start = append(
        &mut log,
        ce::TOOL_EXEC_STARTED,
        &[],
        json!({"call":"","tool":"Run"}),
    );
    let missing = append(
        &mut log,
        ce::TOOL_EXEC_COMPLETED,
        &[],
        json!({"status":"ok"}),
    );
    let non_string = append(
        &mut log,
        ce::TOOL_EXEC_COMPLETED,
        &[],
        json!({"call":7,"status":"ok"}),
    );
    let mut facts = EventFacts::recover(log.reader(), non_string.seq).unwrap();
    for event in [&missing, &non_string] {
        assert_eq!(
            facts.get(&event.id).unwrap().related_start.as_deref(),
            Some(start.id.as_str())
        );
        // Version one persisted no pairing for these legacy completions.
        let mut old = facts.get(&event.id).unwrap();
        old.related_start = None;
        facts.pages.replace((event.seq - 1) as usize, old).unwrap();
    }
    let old_state = State {
        pages: facts.pages.directory().unwrap(),
        count: facts.pages.len(),
    };
    log.reader()
        .save_checkpoint(CONSUMER, 1, non_string.seq, &old_state)
        .unwrap();
    drop(facts);
    let restored = EventFacts::recover(log.reader(), non_string.seq).unwrap();
    assert!(
        restored.cold_reason().is_some(),
        "version-one semantics must be rebuilt"
    );
    for event in [&missing, &non_string] {
        assert_eq!(
            restored.get(&event.id).unwrap().related_start.as_deref(),
            Some(start.id.as_str())
        );
    }
}

#[test]
fn facts_keep_pairing_at_each_events_boundary_not_the_latest_reused_call() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("facts.ledger");
    let mut log = log(&root);
    let first = append(
        &mut log,
        ce::TOOL_EXEC_STARTED,
        &[],
        json!({"call":"same","tool":"Run","arguments":{"command":"first"}}),
    );
    let completion = append(
        &mut log,
        ce::TOOL_EXEC_COMPLETED,
        &[],
        json!({"call":"same","status":"ok"}),
    );
    let mut facts = EventFacts::recover(log.reader(), completion.seq).unwrap();
    assert_eq!(
        facts.get(&completion.id).unwrap().related_start.as_deref(),
        Some(first.id.as_str())
    );
    for index in 0..140 {
        append(
            &mut log,
            ce::USER_MESSAGE,
            &[],
            json!({"text":index.to_string()}),
        );
    }
    let later = append(
        &mut log,
        ce::TOOL_EXEC_STARTED,
        &[],
        json!({"call":"same","tool":"Other","arguments":{"command":"later"}}),
    );
    let later_completion = append(
        &mut log,
        ce::TOOL_EXEC_COMPLETED,
        &[],
        json!({"call":"same","status":"ok"}),
    );
    let duplicate = append(
        &mut log,
        ce::TOOL_EXEC_COMPLETED,
        &[],
        json!({"call":"same","status":"ok"}),
    );
    facts.catch_up(duplicate.seq).unwrap();
    facts.save().unwrap();
    drop(facts);
    let mut facts = EventFacts::recover(log.reader(), duplicate.seq).unwrap();
    assert!(facts.cold_reason().is_none());
    assert_eq!(facts.pages.read_count(), 0);
    assert_eq!(
        facts.get(&completion.id).unwrap().related_start.as_deref(),
        Some(first.id.as_str())
    );
    for event in [&later_completion, &duplicate] {
        assert_eq!(
            facts.get(&event.id).unwrap().related_start.as_deref(),
            Some(later.id.as_str())
        );
    }
    assert!(facts.get("missing").is_err());
    assert!(facts.find_at("missing", completion.seq).unwrap().is_none());
    assert!(facts.find_at(&later.id, completion.seq).unwrap().is_none());
    assert!(facts.find_at(&first.id, first.seq).unwrap().is_some());
    assert!(facts.find_at(&first.id, duplicate.seq + 1).is_err());
    let original = facts.related_event(&completion.id).unwrap().unwrap();
    assert_eq!(original.id, first.id);
    assert_eq!(original.payload, first.payload);
    let reused = facts.related_event(&duplicate.id).unwrap().unwrap();
    assert_eq!(reused.id, later.id);
    assert_eq!(reused.payload, later.payload);
    assert!(facts.related_event(&first.id).unwrap().is_none());

    let mut invalid_pair = facts.get(&completion.id).unwrap();
    invalid_pair.related_start = Some(later.id.clone());
    facts
        .pages
        .replace((completion.seq - 1) as usize, invalid_pair.clone())
        .unwrap();
    assert!(facts.related_event(&completion.id).is_err());
    invalid_pair.related_start = Some("absent-request".into());
    facts
        .pages
        .replace((completion.seq - 1) as usize, invalid_pair)
        .unwrap();
    assert!(facts.related_event(&completion.id).is_err());
}

#[test]
fn consumed_model_starts_survive_checkpoints_without_changing_auxiliary_or_duplicate_semantics() {
    let temp = tempfile::tempdir().unwrap();
    let mut log = log(&temp.path().join("models.ledger"));
    let first = append(
        &mut log,
        ce::MODEL_CALL_STARTED,
        &[],
        json!({"model":"first"}),
    );
    let second = append(
        &mut log,
        ce::MODEL_CALL_STARTED,
        &[],
        json!({"model":"second","purpose":"condense"}),
    );
    let auxiliary = append(
        &mut log,
        ce::MODEL_CALL_COMPLETED,
        &[&second.id],
        json!({"purpose":"condense"}),
    );
    let first_done = append(&mut log, ce::MODEL_CALL_COMPLETED, &[&first.id], json!({}));
    let mut facts = EventFacts::recover(log.reader(), first_done.seq).unwrap();
    assert!(facts.get(&auxiliary.id).unwrap().related_start.is_none());
    assert_eq!(
        facts.get(&first_done.id).unwrap().related_start.as_deref(),
        Some(first.id.as_str())
    );
    facts.save().unwrap();
    drop(facts);
    let duplicate = append(
        &mut log,
        ce::MODEL_CALL_COMPLETED,
        &[&first.id, &second.id],
        json!({}),
    );
    let exhausted = append(
        &mut log,
        ce::MODEL_CALL_COMPLETED,
        &[&first.id, &second.id],
        json!({}),
    );
    let facts = EventFacts::recover(log.reader(), exhausted.seq).unwrap();
    assert_eq!(
        facts.get(&duplicate.id).unwrap().related_start.as_deref(),
        Some(second.id.as_str())
    );
    assert!(facts.get(&exhausted.id).unwrap().related_start.is_none());
    assert_eq!(
        facts.get(&second.id).unwrap().model_start.unwrap().model,
        "second"
    );
}

#[test]
fn event_sizes_count_native_or_normalized_content_once_and_ignore_accounting() {
    let temp = tempfile::tempdir().unwrap();
    let mut log = log(&temp.path().join("sizes.ledger"));
    let normalized = json!({"text":"answer","toolCalls":[{"id":"call"}],"reasoning":[{"kind":"text","text":"thought"}],"usage":{"huge":"accounting".repeat(1000)}});
    let first = append(&mut log, ce::MODEL_CALL_COMPLETED, &[], normalized.clone());
    let size = measure(&first);
    assert_eq!(size.kind, MaterialKind::Reply);
    assert_eq!(
        size.bytes,
        6 + serde_json::to_vec(&normalized["toolCalls"][0])
            .unwrap()
            .len() as u64
    );
    assert_eq!(
        size.thinking,
        serde_json::to_vec(&normalized["reasoning"][0])
            .unwrap()
            .len() as u64
    );
    let mut native = normalized;
    native["responsesOutput"] =
        json!([{"type":"message","content":"native"},{"type":"reasoning","data":"sealed"}]);
    let second = append(&mut log, ce::MODEL_CALL_COMPLETED, &[], native.clone());
    let size = measure(&second);
    assert_eq!(
        size.bytes,
        serde_json::to_vec(&native["responsesOutput"][0])
            .unwrap()
            .len() as u64
    );
    assert_eq!(
        size.thinking,
        serde_json::to_vec(&native["responsesOutput"][1])
            .unwrap()
            .len() as u64
    );
    let facts = EventFacts::recover(log.reader(), second.seq).unwrap();
    assert_eq!(facts.get(&second.id).unwrap().size, size);
    for value in [
        json!(null),
        json!(true),
        json!(false),
        json!(1.25),
        json!({"unicode":"中文\n\t\u{0001}","array":["\\\"",2,false]}),
    ] {
        assert_eq!(
            json_bytes(&value),
            serde_json::to_vec(&value).unwrap().len() as u64
        );
    }
}
