use super::*;

#[test]
fn startup_is_lazy_and_warm_lookup_reads_no_unrelated_documents() {
    let temp = tempfile::tempdir().unwrap();
    let ledger = temp.path().join("conversation.ledger");
    let mut documents = Documents::beside(&ledger);
    assert!(documents.index.is_none());
    assert!(!documents.dir.exists());
    for index in 0..260 {
        let name = format!("ev_{index}-system.txt");
        assert_eq!(
            documents.put(&name, &format!("prompt {index}")).unwrap(),
            name
        );
        assert!(documents.index.as_ref().unwrap().pages.resident_records() <= 128);
    }
    let dir = documents.dir.clone();
    drop(documents);
    // If startup re-enumerated all attachments, this would enter the index.
    fs::write(
        dir.join("unrelated.log"),
        "unrelated output created by another tool",
    )
    .unwrap();
    let mut resumed = Documents::beside(&ledger);
    assert!(resumed.index.is_none());
    assert_eq!(
        resumed.put("ev_999-system.txt", "prompt 0").unwrap(),
        "ev_0-system.txt"
    );
    assert_eq!(resumed.index.as_ref().unwrap().pages.len(), 260);
    assert!(
        (1..=3).contains(&resumed.index.as_ref().unwrap().pages.read_count()),
        "Bloom candidates may read other index pages, not other documents"
    );
    assert!(!dir.join("ev_999-system.txt").exists());
}

#[test]
fn corrupt_lookup_rebuilds_from_originals_and_never_overwrites_a_document() {
    let temp = tempfile::tempdir().unwrap();
    let ledger = temp.path().join("conversation.jsonl");
    let mut documents = Documents::beside(&ledger);
    assert_eq!(
        documents.put("ev_1-system.txt", "first prompt").unwrap(),
        "ev_1-system.txt"
    );
    let dir = documents.dir.clone();
    let state = documents.index.as_ref().unwrap().pages.directory().unwrap();
    let value = serde_json::to_value(&state).unwrap();
    let digest = value[0]["digest"]
        .as_array()
        .unwrap()
        .iter()
        .map(|byte| format!("{:02x}", byte.as_u64().unwrap()))
        .collect::<String>();
    fs::write(
        dir.join(".document-index")
            .join(format!("cards-{digest}.json")),
        "bad cache",
    )
    .unwrap();
    drop(documents);
    let mut resumed = Documents::beside(&ledger);
    assert_eq!(
        resumed.put("ev_2-system.txt", "first prompt").unwrap(),
        "ev_1-system.txt"
    );
    assert_eq!(
        fs::read_to_string(dir.join("ev_1-system.txt")).unwrap(),
        "first prompt"
    );
    assert!(resumed.put("ev_1-system.txt", "different prompt").is_err());
    assert_eq!(
        fs::read_to_string(dir.join("ev_1-system.txt")).unwrap(),
        "first prompt"
    );
    // A modified selected file must not be returned for its former contents.
    fs::write(dir.join("ev_1-system.txt"), "externally changed").unwrap();
    assert_eq!(
        resumed.put("ev_3-system.txt", "first prompt").unwrap(),
        "ev_3-system.txt"
    );
    assert_eq!(
        fs::read_to_string(dir.join("ev_1-system.txt")).unwrap(),
        "externally changed"
    );
}

#[test]
fn a_lookup_symlink_cannot_redirect_cache_writes() {
    let temp = tempfile::tempdir().unwrap();
    let ledger = temp.path().join("conversation.ledger");
    let mut documents = Documents::beside(&ledger);
    fs::create_dir_all(&documents.dir).unwrap();
    let elsewhere = temp.path().join("elsewhere");
    fs::create_dir(&elsewhere).unwrap();
    std::os::unix::fs::symlink(&elsewhere, documents.dir.join(".document-index")).unwrap();
    assert!(documents.put("ev_1-system.txt", "prompt").is_err());
    assert_eq!(fs::read_dir(elsewhere).unwrap().count(), 0);
}
