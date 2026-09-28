//! Segmented storage protocol tests. Never opens a real conversation ledger.
#![cfg(unix)]

use super as segmented;

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};

use crate::{EventDraft, EventEnvelope, EventLog, EventTypeDecl};
use segmented::{Ledger, Recovery, RotationStep};
use serde_json::{json, Value};

fn path(root: &Path, number: u64) -> PathBuf {
    root.join(format!("{number:020}.jsonl"))
}

#[test]
fn releasing_a_lease_unlocks_even_while_an_inherited_descriptor_remains_open() {
    for explicit_release in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("lease.ledger");
        let mut ledger = Ledger::create(&root, "fixture", 4096).unwrap();
        // A duplicated descriptor shares the same lock as a descriptor inherited
        // by a concurrent fork before exec closes close-on-exec descriptors.
        let inherited = ledger.lease.as_ref().unwrap().try_clone().unwrap();
        assert!(Ledger::snapshot(&root).is_err());
        if explicit_release {
            ledger.release_writer();
        }
        drop(ledger);
        let reopened = Ledger::snapshot(&root);
        drop(inherited);
        let snapshot = reopened.expect("the owner released its lease before the child execs");
        assert!(Ledger::snapshot(&root).is_err());
        let inherited_snapshot = snapshot.lease.as_ref().unwrap().try_clone().unwrap();
        drop(snapshot);
        let reopened = Ledger::snapshot(&root);
        drop(inherited_snapshot);
        assert!(
            reopened.is_ok(),
            "snapshot leases must also release explicitly"
        );
    }
}

#[test]
fn closed_snapshot_skips_old_bodies_and_leaves_uncommitted_tail_untouched() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("snapshot.ledger");
    let mut ledger = Ledger::create(&root, "fixture", 1).unwrap();
    for event in events() {
        ledger.append(&event).unwrap();
    }
    assert!(Ledger::snapshot(&root).is_err());
    let count = ledger.count();
    let last = ledger.catalog.segments.last().unwrap().number;
    drop(ledger);
    let mut tail = OpenOptions::new()
        .append(true)
        .open(path(&root, last))
        .unwrap();
    tail.write_all(b"{\"v\":").unwrap();
    drop(tail);
    let before = fs::read(path(&root, last)).unwrap();
    let snapshot = Ledger::snapshot(&root).unwrap();
    assert_eq!(snapshot.count(), count);
    assert_eq!(snapshot.open_stats.sealed_body_bytes, 0);
    assert_eq!(snapshot.recovery.discarded_tail_bytes, 0);
    assert_eq!(fs::read(path(&root, last)).unwrap(), before);
    drop(snapshot);
    assert!(Ledger::verify_path(&root).is_err());
    assert_eq!(fs::read(path(&root, last)).unwrap(), before);
}

#[test]
fn oversized_index_scans_bounded_pages_and_pins_the_current_record_for_body_reads() {
    use crate::kernel::history::History;
    use std::sync::{Arc, Mutex};
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("large.ledger");
    let mut ledger = Ledger::create(&root, "fixture", u64::MAX).unwrap();
    let mut event = events().remove(0);
    event.event_type = crate::core_events::TOOL_EXEC_STARTED.into();
    event.payload = json!({"tool": "x".repeat(96 * 1024), "call": "fixture", "arguments": {}});
    for seq in 1..=100 {
        event.seq = seq;
        event.id = format!("ev_{seq}_large_index");
        ledger.append(&event).unwrap();
    }
    ledger.rotate().unwrap();
    let source = Arc::new(Mutex::new(ledger));
    let history = History::segmented(Arc::clone(&source), 0).unwrap();
    let before = source.lock().unwrap().page_cache.borrow().loads;
    {
        let mut cursor = crate::kernel::history::HeaderCursor::new(0, history.len(), false);
        while let Some((header, _)) = cursor.next(&history).unwrap() {
            let loaded = source.lock().unwrap().page_cache.borrow().loads;
            assert_eq!(cursor.load(&history, header.seq).unwrap().seq, header.seq);
            assert_eq!(
                source.lock().unwrap().page_cache.borrow().loads,
                loaded,
                "body reads use the pinned record without loading index pages again"
            );
            assert!(source.lock().unwrap().page_cache.borrow().bytes <= 8 * 1024 * 1024);
        }
    }
    let loaded = source.lock().unwrap().page_cache.borrow().loads - before;
    assert!(
        loaded > 100 && loaded < 200,
        "one linear pass should load physical pages, not a whole index per record: {loaded}"
    );
    assert!(!history.has_outcome("absent").unwrap());
    assert_eq!(
        history
            .hanging(crate::core_events::TOOL_EXEC_STARTED)
            .unwrap()
            .len(),
        100
    );
    let source = source.lock().unwrap();
    assert!(
        source.page_cache.borrow().loads - before < 400,
        "successive scans remain linear in physical page count"
    );
    assert!(source.page_cache.borrow().bytes <= 8 * 1024 * 1024);
}

#[test]
fn legacy_indexes_migrate_from_verified_source_without_changing_original_records() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("legacy.ledger");
    let rows = events();
    let mut ledger = Ledger::create(&root, "fixture", u64::MAX).unwrap();
    for event in &rows {
        ledger.append(event).unwrap();
    }
    let records = ledger.active_index.clone();
    ledger.rotate().unwrap();
    let original = fs::read(path(&root, 0)).unwrap();
    let stamp = segmented::index::write_index(&root, 0, &records).unwrap();
    ledger.catalog.version = 1;
    ledger.catalog.segments[0].seal.as_mut().unwrap().index = Some(stamp);
    segmented::prepare_catalog(&root, &ledger.catalog).unwrap();
    segmented::publish_catalog(&root).unwrap();
    drop(ledger);
    let ledger = Ledger::open(&root, u64::MAX).unwrap();
    assert_eq!(ledger.catalog.version, 2);
    assert_eq!(ledger.recovery.rebuilt_indexes, 1);
    assert_eq!(ledger.open_stats.sealed_body_bytes, original.len() as u64);
    assert_eq!(fs::read(path(&root, 0)).unwrap(), original);
    drop(ledger);
    let ledger = Ledger::open(&root, u64::MAX).unwrap();
    assert_eq!(ledger.open_stats.sealed_body_bytes, 0);
    assert_eq!(
        ledger.page_cache.borrow().loads,
        0,
        "empty active volume needs no sealed record pages on warm open"
    );
    assert_eq!(ledger.recovery.rebuilt_indexes, 0);
    for event in &rows {
        same(&ledger.get(&event.id).unwrap().unwrap(), event);
    }
}

#[test]
fn bounded_indexes_preserve_arbitrary_ids_and_propagate_evicted_cache_errors() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("history.ledger");
    let mut rows = events();
    rows[0].id = "ev_999_foreign".into();
    rows[1].id = "arbitrary-id".into();
    rows[1].causes = vec![rows[0].id.clone()];
    rows[2].id = "ev_1_misleading".into();
    rows[2].causes = vec![rows[1].id.clone()];
    let mut ledger = Ledger::create(&root, "fixture", 1).unwrap();
    for event in &rows {
        ledger.append(event).unwrap();
    }
    drop(ledger);
    let ledger = Ledger::open_for(&root, 1, Some("fixture")).unwrap();
    for event in &rows {
        assert_eq!(ledger.get(&event.id).unwrap().unwrap().id, event.id);
    }
    assert!(ledger.get("ev_2_absent").unwrap().is_none());
    assert_eq!(
        ledger.active_locations.len(),
        1,
        "only active-volume identities remain resident"
    );
    assert_eq!(ledger.active_index.len(), 1);
    *ledger.page_cache.borrow_mut() = segmented::pages::PageCache::default();
    let directory: Value =
        serde_json::from_slice(&fs::read(root.join("00000000000000000000.index")).unwrap())
            .unwrap();
    let page = root.join(directory["headers"]["file"].as_str().unwrap());
    let file = OpenOptions::new().write(true).open(page).unwrap();
    file.write_all_at(b"!", 0).unwrap();
    assert!(
        ledger.get(&rows[0].id).is_err(),
        "an evicted damaged index is not an absent event"
    );
}

fn events() -> Vec<EventEnvelope> {
    let mut source = EventLog::in_memory(
        vec![
            EventTypeDecl::new("fixture.request", "request"),
            EventTypeDecl::new("fixture.result", "result"),
        ],
        "fixture",
    );
    let first = source
        .append(
            EventDraft::new("fixture.request", &[], json!({"text": "multibyte: 中"})),
            "driver",
        )
        .unwrap();
    let second = source
        .append(
            EventDraft::new("fixture.result", &[&first.id], json!({"text": "result"})),
            "tool",
        )
        .unwrap();
    let third = source
        .append(
            EventDraft::new(
                "fixture.request",
                &[&second.id],
                json!({"text": "large".repeat(4096)}),
            ),
            "driver",
        )
        .unwrap();
    vec![first, second, third]
}

fn same(actual: &EventEnvelope, expected: &EventEnvelope) {
    assert_eq!(
        serde_json::to_value(actual).unwrap(),
        serde_json::to_value(expected).unwrap()
    );
}

#[test]
fn cross_segment_round_trip_preserves_ids_causes_and_oversized_events() {
    let home = tempfile::tempdir().unwrap();
    let root = home.path().join("ledger");
    let events = events();
    let mut ledger = Ledger::create(&root, "fixture", 1).unwrap();
    for event in &events {
        ledger.append(event).unwrap();
    }
    assert_eq!(ledger.segment_count(), 3);
    assert_eq!(ledger.next_seq(), 4);
    for (number, event) in events.iter().enumerate() {
        let location = ledger.location(&event.id).unwrap();
        assert_eq!(location.segment, number as u64);
        assert_eq!(location.offset, 0);
        let expected = [serde_json::to_vec(event).unwrap(), vec![b'\n']].concat();
        assert_eq!(fs::read(path(&root, number as u64)).unwrap(), expected);
        same(&ledger.get(&event.id).unwrap().unwrap(), event);
    }
    assert!(ledger.get("absent").unwrap().is_none());
    drop(ledger);
    let ledger = Ledger::open(&root, 1).unwrap();
    assert_eq!(ledger.next_seq(), 4);
    for event in &events {
        same(&ledger.get(&event.id).unwrap().unwrap(), event);
    }
    assert_eq!(ledger.recovery, Recovery::default());
}

#[test]
fn sealed_indexes_skip_old_bodies_and_rebuild_only_from_verified_source() {
    for mode in [
        "warm",
        "missing",
        "damaged",
        "changed-header",
        "damaged-source",
    ] {
        let home = tempfile::tempdir().unwrap();
        let root = home.path().join("ledger");
        let events = events();
        let mut ledger = Ledger::create(&root, "fixture", 1).unwrap();
        for event in &events {
            ledger.append(event).unwrap();
        }
        ledger.verify().unwrap();
        drop(ledger);
        let index = root.join("00000000000000000000.index");
        match mode {
            "missing" => fs::rename(&index, root.join("saved-index")).unwrap(),
            "damaged" | "damaged-source" => fs::write(&index, b"broken cache\n").unwrap(),
            "changed-header" => {
                let raw = fs::read_to_string(&index)
                    .unwrap()
                    .replace("fixture", "FIXTURE");
                fs::write(&index, raw).unwrap();
            }
            _ => {}
        }
        if mode == "damaged-source" {
            let source = path(&root, 0);
            let raw = fs::read_to_string(&source)
                .unwrap()
                .replace("multibyte", "MULTIBYTE");
            fs::write(source, raw).unwrap();
            let before = directory_bytes(&root);
            assert!(Ledger::open(&root, 1).is_err());
            assert_eq!(
                directory_bytes(&root),
                before,
                "bad source must not produce a new index"
            );
            continue;
        }
        let ledger = Ledger::open(&root, 1).unwrap();
        if mode == "warm" {
            assert_eq!(ledger.open_stats.sealed_body_bytes, 0);
            assert_eq!(ledger.directories.len(), 2);
            assert_eq!(ledger.recovery.rebuilt_indexes, 0);
        } else {
            assert_eq!(
                ledger.open_stats.sealed_body_bytes,
                fs::metadata(path(&root, 0)).unwrap().len()
            );
            assert_eq!(ledger.directories.len(), 2);
            assert_eq!(ledger.recovery.rebuilt_indexes, 1);
        }
        for event in &events {
            same(&ledger.get(&event.id).unwrap().unwrap(), event);
        }
        ledger.verify().unwrap();
        drop(ledger);
        let ledger = Ledger::open(&root, 1).unwrap();
        assert_eq!(ledger.open_stats.sealed_body_bytes, 0);
        assert_eq!(ledger.directories.len(), 2);
    }
}

#[test]
fn lazy_reads_validate_the_record_delimiter_as_well_as_json_bytes() {
    let home = tempfile::tempdir().unwrap();
    let root = home.path().join("ledger");
    let events = events();
    let mut ledger = Ledger::create(&root, "fixture", 1).unwrap();
    ledger.append(&events[0]).unwrap();
    ledger.append(&events[1]).unwrap();
    drop(ledger);
    let source = path(&root, 0);
    let file = OpenOptions::new().write(true).open(&source).unwrap();
    file.write_all_at(b" ", file.metadata().unwrap().len() - 1)
        .unwrap();
    drop(file);
    let before = directory_bytes(&root);
    let ledger = Ledger::open(&root, 1).unwrap();
    assert_eq!(ledger.open_stats.sealed_body_bytes, 0);
    assert!(ledger.get(&events[0].id).is_err());
    assert!(ledger.verify().is_err());
    assert_eq!(directory_bytes(&root), before);
}

#[test]
fn cached_records_cannot_change_stream_when_the_active_segment_is_empty() {
    let home = tempfile::tempdir().unwrap();
    let root = home.path().join("ledger");
    let events = events();
    let mut ledger = Ledger::create(&root, "fixture", 1).unwrap();
    ledger.append(&events[0]).unwrap();
    ledger.fail_rotation_at(RotationStep::CatalogPublished);
    assert!(ledger.append(&events[1]).is_err());
    drop(ledger);
    let file = root.join("catalog.json");
    let mut catalog: Value = serde_json::from_slice(&fs::read(&file).unwrap()).unwrap();
    catalog["stream"] = json!("another");
    fs::write(file, serde_json::to_vec(&catalog).unwrap()).unwrap();
    let before = directory_bytes(&root);
    assert!(Ledger::open_for(&root, 1, Some("another")).is_err());
    assert_eq!(directory_bytes(&root), before);
}

#[test]
fn changed_source_seals_cannot_reuse_indexes_or_accept_future_envelopes() {
    use sha2::{Digest, Sha256};
    let home = tempfile::tempdir().unwrap();
    let root = home.path().join("ledger");
    let events = events();
    let mut ledger = Ledger::create(&root, "fixture", 1).unwrap();
    ledger.append(&events[0]).unwrap();
    ledger.append(&events[1]).unwrap();
    drop(ledger);
    let source = path(&root, 0);
    let source_bytes = fs::read_to_string(&source)
        .unwrap()
        .replace("\"v\":1", "\"v\":2");
    fs::write(source, &source_bytes).unwrap();
    let catalog_path = root.join("catalog.json");
    let mut catalog: Value = serde_json::from_slice(&fs::read(&catalog_path).unwrap()).unwrap();
    let seal = &mut catalog["segments"][0]["seal"];
    seal["digest"] = json!(format!("{:x}", Sha256::digest(source_bytes.as_bytes())));
    fs::write(catalog_path, serde_json::to_vec(&catalog).unwrap()).unwrap();
    let before = directory_bytes(&root);
    assert!(
        Ledger::open(&root, 1).is_err(),
        "index cannot bypass envelope version enforcement"
    );
    assert_eq!(directory_bytes(&root), before);
}

#[test]
fn rotation_failure_boundaries_recover_without_losing_or_duplicating_an_event() {
    for step in [
        RotationStep::NextFileSynced,
        RotationStep::CatalogPrepared,
        RotationStep::CatalogPublished,
    ] {
        let home = tempfile::tempdir().unwrap();
        let root = home.path().join("ledger");
        let events = events();
        let mut ledger = Ledger::create(&root, "fixture", 1).unwrap();
        ledger.append(&events[0]).unwrap();
        let first_bytes = fs::read(path(&root, 0)).unwrap();
        ledger.fail_rotation_at(step);
        assert!(ledger
            .append(&events[1])
            .unwrap_err()
            .to_string()
            .contains("injected rotation failure"));
        assert!(ledger
            .append(&events[1])
            .unwrap_err()
            .to_string()
            .contains("writer failed"));
        drop(ledger);
        let mut ledger = Ledger::open(&root, 1).unwrap();
        assert_eq!(ledger.next_seq(), 2);
        assert!(ledger.get(&events[1].id).unwrap().is_none());
        same(&ledger.get(&events[0].id).unwrap().unwrap(), &events[0]);
        ledger.append(&events[1]).unwrap();
        assert_eq!(ledger.next_seq(), 3);
        assert_eq!(fs::read(path(&root, 0)).unwrap(), first_bytes);
        assert_eq!(ledger.segment_count(), 2);
        drop(ledger);
        let ledger = Ledger::open(&root, 1).unwrap();
        assert_eq!(ledger.next_seq(), 3);
        same(&ledger.get(&events[1].id).unwrap().unwrap(), &events[1]);
    }
}

#[test]
fn active_tail_recovery_preserves_complete_json_and_discards_only_incomplete_bytes() {
    for tail in [b"{\"v\":1,".as_slice(), b"{\"text\":\"\xf0\x9f", b""] {
        let home = tempfile::tempdir().unwrap();
        let root = home.path().join("ledger");
        let events = events();
        let mut ledger = Ledger::create(&root, "fixture", u64::MAX).unwrap();
        ledger.append(&events[0]).unwrap();
        drop(ledger);
        let good = fs::read(path(&root, 0)).unwrap();
        OpenOptions::new()
            .append(true)
            .open(path(&root, 0))
            .unwrap()
            .write_all(tail)
            .unwrap();
        let mut ledger = Ledger::open(&root, u64::MAX).unwrap();
        assert_eq!(ledger.recovery.discarded_tail_bytes, tail.len() as u64);
        assert_eq!(fs::read(path(&root, 0)).unwrap(), good);
        ledger.append(&events[1]).unwrap();
        drop(ledger);
        let ledger = Ledger::open(&root, u64::MAX).unwrap();
        assert_eq!(ledger.recovery, Recovery::default());
        assert_eq!(ledger.next_seq(), 3);
    }
    let home = tempfile::tempdir().unwrap();
    let root = home.path().join("ledger");
    let events = events();
    let mut ledger = Ledger::create(&root, "fixture", u64::MAX).unwrap();
    ledger.append(&events[0]).unwrap();
    drop(ledger);
    // A complete last event must survive even if its newline was lost.
    let file = OpenOptions::new().write(true).open(path(&root, 0)).unwrap();
    file.set_len(file.metadata().unwrap().len() - 1).unwrap();
    let mut ledger = Ledger::open(&root, u64::MAX).unwrap();
    assert!(ledger.recovery.repaired_newline);
    assert_eq!(ledger.next_seq(), 2);
    ledger.append(&events[1]).unwrap();
    drop(ledger);
    let ledger = Ledger::open(&root, u64::MAX).unwrap();
    assert_eq!(ledger.next_seq(), 3);
    assert_eq!(ledger.recovery, Recovery::default());
}

#[test]
fn committed_damage_missing_seals_and_unlisted_data_are_never_repaired_away() {
    for mode in [
        "sealed-content",
        "sealed-tail",
        "missing",
        "active-line",
        "unlisted",
    ] {
        let home = tempfile::tempdir().unwrap();
        let root = home.path().join("ledger");
        let events = events();
        let mut ledger = Ledger::create(&root, "fixture", 1).unwrap();
        ledger.append(&events[0]).unwrap();
        ledger.append(&events[1]).unwrap();
        drop(ledger);
        match mode {
            "sealed-content" => {
                let raw = fs::read_to_string(path(&root, 0))
                    .unwrap()
                    .replace("multibyte", "MULTIBYTE");
                fs::write(path(&root, 0), raw).unwrap();
            }
            "sealed-tail" => {
                let f = OpenOptions::new().write(true).open(path(&root, 0)).unwrap();
                f.set_len(f.metadata().unwrap().len() - 1).unwrap();
            }
            "missing" => {
                fs::rename(path(&root, 0), root.join("removed-for-test")).unwrap();
            }
            "active-line" => {
                OpenOptions::new()
                    .append(true)
                    .open(path(&root, 1))
                    .unwrap()
                    .write_all(b"broken\n")
                    .unwrap();
            }
            "unlisted" => {
                fs::write(path(&root, 2), b"unexplained committed data\n").unwrap();
            }
            _ => unreachable!(),
        }
        let before = directory_bytes(&root);
        if mode == "sealed-content" {
            // Fast open trusts the catalog-bound index, not unread old bodies.
            let ledger = Ledger::open(&root, 1).unwrap();
            assert_eq!(ledger.open_stats.sealed_body_bytes, 0);
            assert!(ledger.get(&events[0].id).is_err());
            assert!(ledger.verify().is_err());
        } else {
            assert!(Ledger::open(&root, 1).is_err(), "must reject {mode}");
        }
        assert_eq!(directory_bytes(&root), before, "must not repair {mode}");
    }
}

fn directory_bytes(root: &Path) -> std::collections::BTreeMap<String, Vec<u8>> {
    fs::read_dir(root)
        .unwrap()
        .map(|entry| {
            let entry = entry.unwrap();
            (
                entry.file_name().to_string_lossy().into_owned(),
                fs::read(entry.path()).unwrap(),
            )
        })
        .collect()
}

#[test]
fn identity_sequence_and_catalog_errors_are_rejected_without_writes() {
    for mode in [
        "duplicate",
        "gap",
        "stream",
        "future-event",
        "cause",
        "future-catalog",
        "catalog-gap",
    ] {
        let home = tempfile::tempdir().unwrap();
        let root = home.path().join("ledger");
        let events = events();
        let mut ledger = Ledger::create(&root, "fixture", 1).unwrap();
        ledger.append(&events[0]).unwrap();
        drop(ledger);
        if mode.contains("catalog") {
            let mut catalog: Value =
                serde_json::from_slice(&fs::read(root.join("catalog.json")).unwrap()).unwrap();
            if mode == "future-catalog" {
                catalog["version"] = json!(999);
            } else {
                catalog["segments"][0]["first"] = json!(2);
            }
            fs::write(
                root.join("catalog.json"),
                serde_json::to_vec(&catalog).unwrap(),
            )
            .unwrap();
        } else {
            let mut bad = events[1].clone();
            match mode {
                "duplicate" => bad.id = events[0].id.clone(),
                "gap" => bad.seq = 3,
                "stream" => bad.stream = "other".into(),
                "future-event" => bad.v = 999,
                "cause" => bad.causes = vec!["unknown".into()],
                _ => unreachable!(),
            }
            let mut f = OpenOptions::new()
                .append(true)
                .open(path(&root, 0))
                .unwrap();
            f.write_all(&serde_json::to_vec(&bad).unwrap()).unwrap();
            // Also ensure a valid but incompatible unterminated event is not
            // mistaken for an incomplete write and truncated.
            if mode != "future-event" {
                f.write_all(b"\n").unwrap();
            }
        }
        let before = directory_bytes(&root);
        assert!(Ledger::open(&root, 1).is_err(), "must reject {mode}");
        assert_eq!(directory_bytes(&root), before);
    }
}

#[test]
fn exclusive_writer_and_failed_writer_rules_prevent_split_histories() {
    let home = tempfile::tempdir().unwrap();
    let root = home.path().join("ledger");
    let events = events();
    let mut ledger = Ledger::create(&root, "fixture", u64::MAX).unwrap();
    assert!(
        Ledger::open(&root, 1).is_err(),
        "a second writer must not open"
    );
    ledger.append(&events[0]).unwrap();
    let old = fs::read(path(&root, 0)).unwrap();
    fs::rename(path(&root, 0), root.join("pinned-old")).unwrap();
    fs::write(path(&root, 0), &old).unwrap();
    assert!(
        ledger.append(&events[1]).is_err(),
        "a replacement path must not split the writer"
    );
    assert!(ledger
        .append(&events[1])
        .unwrap_err()
        .to_string()
        .contains("writer failed"));
    assert_eq!(fs::read(root.join("pinned-old")).unwrap(), old);
    assert_eq!(fs::read(path(&root, 0)).unwrap(), old);
}

#[test]
fn complete_unacknowledged_append_is_preserved_and_cannot_be_appended_twice() {
    let home = tempfile::tempdir().unwrap();
    let root = home.path().join("ledger");
    let events = events();
    let mut ledger = Ledger::create(&root, "fixture", u64::MAX).unwrap();
    ledger.append(&events[0]).unwrap();
    drop(ledger);
    // Model a complete write that reached disk before the writer could return.
    let mut file = OpenOptions::new()
        .append(true)
        .open(path(&root, 0))
        .unwrap();
    file.write_all(&serde_json::to_vec(&events[1]).unwrap())
        .unwrap();
    file.write_all(b"\n").unwrap();
    file.sync_all().unwrap();
    let mut ledger = Ledger::open(&root, u64::MAX).unwrap();
    assert_eq!(ledger.next_seq(), 3);
    same(&ledger.get(&events[1].id).unwrap().unwrap(), &events[1]);
    assert!(ledger.append(&events[1]).is_err());
    ledger.append(&events[2]).unwrap();
    assert_eq!(ledger.next_seq(), 4);
}

#[test]
fn sealing_does_not_bless_same_length_external_edits() {
    let home = tempfile::tempdir().unwrap();
    let root = home.path().join("ledger");
    let events = events();
    let mut ledger = Ledger::create(&root, "fixture", 1).unwrap();
    ledger.append(&events[0]).unwrap();
    let changed = fs::read_to_string(path(&root, 0))
        .unwrap()
        .replace("multibyte", "MULTIBYTE");
    fs::write(path(&root, 0), &changed).unwrap();
    assert!(ledger.append(&events[1]).is_err());
    assert_eq!(fs::read_to_string(path(&root, 0)).unwrap(), changed);
    assert!(
        !path(&root, 1).exists(),
        "must reject damage before preparing a new segment"
    );
}

#[test]
fn indexed_reads_detect_changed_bytes_instead_of_returning_absence() {
    let home = tempfile::tempdir().unwrap();
    let root = home.path().join("ledger");
    let events = events();
    let mut ledger = Ledger::create(&root, "fixture", u64::MAX).unwrap();
    ledger.append(&events[0]).unwrap();
    let file = OpenOptions::new().write(true).open(path(&root, 0)).unwrap();
    file.write_all_at(b"!", 0).unwrap();
    assert!(ledger.get(&events[0].id).is_err());
    assert!(ledger.get("absent").unwrap().is_none());
}
