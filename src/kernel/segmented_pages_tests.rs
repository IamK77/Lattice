use super::*;

fn fixture() -> (tempfile::TempDir, PathBuf, Vec<EventEnvelope>, Vec<Hash>) {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("pages.ledger");
    let mut log = crate::EventLog::in_memory(
        vec![crate::EventTypeDecl::new("fixture", "fixture")],
        "pages",
    );
    let event = log
        .append(
            crate::EventDraft::new("fixture", &[], serde_json::json!({})),
            "fixture",
        )
        .unwrap();
    let mut ledger = Ledger::create(&root, "pages", u64::MAX).unwrap();
    let mut rows = Vec::new();
    let mut expected = vec![prefix_seed("pages")];
    for seq in 1..=2100 {
        let mut event = event.clone();
        event.seq = seq;
        event.id = format!("arbitrary/{seq}");
        let raw = serde_json::to_vec(&event).unwrap();
        let mut hash = Sha256::new();
        hash.update(expected.last().unwrap());
        hash.update(Sha256::digest(&raw));
        expected.push(hash.finalize().into());
        ledger.append(&event).unwrap();
        rows.push(event);
    }
    ledger.rotate().unwrap();
    (temp, root, rows, expected)
}

#[test]
fn warm_open_reads_no_old_record_pages_and_prefixes_match_at_page_boundaries() {
    let (_temp, root, rows, expected) = fixture();
    let ledger = Ledger::open(&root, u64::MAX).unwrap();
    assert_eq!(ledger.page_cache.borrow().loads, 0);
    assert_eq!(ledger.open_stats.sealed_body_bytes, 0);
    assert_eq!(ledger.get_at(1300).unwrap().unwrap().id, rows[1299].id);
    assert!(
        ledger.page_cache.borrow().loads <= 3,
        "one record must not load a whole volume"
    );
    for through in [0, 1, 1023, 1024, 1025, 2047, 2048, 2049, 2099, 2100] {
        assert_eq!(
            ledger.prefix_digest(through).unwrap(),
            expected[through as usize]
        );
    }
    for seq in [1, 800, 1638, 1639, 2100] {
        assert_eq!(
            ledger.get(&rows[seq - 1].id).unwrap().unwrap().seq,
            seq as u64
        );
    }
}

#[test]
fn missing_identity_uses_logarithmic_work_inside_each_sorted_page() {
    let (_temp, root, _, _) = fixture();
    let ledger = Ledger::open(&root, u64::MAX).unwrap();
    let directory = &ledger.directories[&0];
    // Use an actual digest in each page to force a search even at the bounds.
    // The different full ID remains absent despite the matching digest.
    for &(low, high) in &directory.id_ranges {
        for digest in [low, high] {
            let cache = RefCell::new(PageCache::default());
            assert!(directory
                .lookup_digest(&root, "absent synthetic thinking ID", digest, &cache)
                .unwrap()
                .is_none());
            let comparisons = cache.borrow().identity_comparisons;
            assert!(comparisons > 0);
            assert!(
                comparisons <= 32,
                "sorted identity lookup made {comparisons} comparisons"
            );
        }
    }
}

#[test]
fn identity_hash_collisions_across_pages_still_compare_complete_ids() {
    let (_temp, root, rows, _) = fixture();
    let ledger = Ledger::open(&root, u64::MAX).unwrap();
    let mut directory = ledger.directories[&0].clone();
    let mut writer = Writer::new(&root, 0, "collision-fixture").unwrap();
    // Substitute a colliding hash function without relying on a real SHA-256
    // collision. The reader must compare IDs after examining every candidate.
    directory.id_ranges.clear();
    for chunk in rows.chunks(IDS_PER_PAGE) {
        directory.id_ranges.push(([0; 32], [0; 32]));
        for event in chunk {
            writer.append(&[0; 32]).unwrap();
            writer.append(&event.seq.to_le_bytes()).unwrap();
        }
        writer.flush().unwrap();
    }
    directory.ids = writer.finish().unwrap();
    directory
        .validate("pages", &ledger.catalog.segments[0], prefix_seed("pages"))
        .unwrap();
    let cache = RefCell::new(PageCache::default());
    for at in [0, IDS_PER_PAGE - 1, IDS_PER_PAGE, rows.len() - 1] {
        assert_eq!(
            directory
                .lookup_digest(&root, &rows[at].id, [0; 32], &cache)
                .unwrap()
                .unwrap()
                .header
                .seq,
            rows[at].seq
        );
    }
    assert!(directory
        .lookup_digest(&root, "absent", [0; 32], &cache)
        .unwrap()
        .is_none());
}

#[test]
fn cached_pages_remain_bound_to_the_expected_digest_and_length() {
    let temp = tempfile::tempdir().unwrap();
    let mut writer = Writer::new(temp.path(), 0, "cache-binding").unwrap();
    writer.append(b"fixture").unwrap();
    let pages = writer.finish().unwrap();
    let cache = RefCell::new(PageCache::default());
    assert_eq!(
        &pages.read(temp.path(), 0, &cache).unwrap()[..7],
        b"fixture"
    );
    let mut wrong_digest = pages.clone();
    wrong_digest.pages[0].digest[0] ^= 1;
    assert!(wrong_digest.read(temp.path(), 0, &cache).is_err());
    let mut wrong_length = pages.clone();
    wrong_length.pages[0].used -= 1;
    assert!(wrong_length.read(temp.path(), 0, &cache).is_err());
    assert!(pages.read(temp.path(), 0, &cache).is_ok());
    assert_eq!(cache.borrow().loads, 1);
}

#[test]
fn changed_page_is_not_absence_and_is_not_required_by_unrelated_lookup() {
    let (_temp, root, rows, _) = fixture();
    let ledger = Ledger::open(&root, u64::MAX).unwrap();
    let directory = &ledger.directories[&0];
    let file = OpenOptions::new()
        .write(true)
        .open(root.join(&directory.headers.file))
        .unwrap();
    // Change valid JSON text without changing the physical length or identity.
    let mut page = fs::read(root.join(&directory.headers.file)).unwrap();
    let at = page
        .windows(b"fixture".len())
        .position(|s| s == b"fixture")
        .unwrap();
    page[at] = b'F';
    file.write_all_at(&page[..PAGE], 0).unwrap();
    assert!(ledger.get(&rows[0].id).is_err());
    assert_eq!(ledger.get_at(2100).unwrap().unwrap().id, rows[2099].id);
    assert!(ledger.get(&rows[0].id).is_err());
}
