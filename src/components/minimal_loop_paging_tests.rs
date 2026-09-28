use super::*;
use crate::EventLog;

#[test]
fn warm_material_is_paged_cow_exact_and_repair_never_admits_new_history() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("material.ledger");
    let mut log = EventLog::open_segmented(
        ce::core_event_decls(),
        "material",
        root.clone(),
        1024 * 1024,
    )
    .unwrap();
    let mut expected = Vec::<String>::new();
    for index in 0..384 {
        let superseded = (index == 300).then(|| expected[0].clone());
        let causes: Vec<_> = superseded.iter().map(String::as_str).collect();
        let event = log
            .append(
                EventDraft::new(ce::USER_MESSAGE, &causes, json!({"text":index.to_string()})),
                "ui",
            )
            .unwrap();
        if superseded.is_some() {
            expected.remove(0);
        }
        expected.push(event.id);
    }
    let reader = log.reader();
    let through = reader.snapshot_end();
    let mut projection = MinimalLoop::from_config(None);
    projection.sync_material(&reader, through).unwrap();
    assert_eq!(projection.material.parts.collect(), expected);
    assert!(projection.material.parts.stats().0 <= 128);
    let saved = projection.material.parts.snapshot().unwrap();
    assert!(serde_json::to_vec(&saved).unwrap().len() < 4096);
    let old = material_store::Store::open(&reader, saved).unwrap();
    assert_eq!(old.stats(), (0, 0));

    let mut resumed = MinimalLoop::from_config(None);
    resumed.sync_material(&reader, through).unwrap();
    assert_eq!(resumed.material_through, through);
    assert_eq!(
        resumed.material.parts.stats(),
        (0, 0),
        "warm recovery reads no pointer pages"
    );
    let original = expected[2].clone();
    let event = log
        .append(
            EventDraft::new(ce::USER_MESSAGE, &[&original], json!({"text":"expanded"})),
            "gate",
        )
        .unwrap();
    resumed.accept_material(&reader, &event).unwrap();
    assert_eq!(
        old.collect(),
        expected,
        "fork must not rewrite the old checkpoint"
    );
    expected.retain(|id| id != &original);
    expected.push(event.id);
    let input = resumed.material_input().unwrap();
    assert_eq!(
        input["parts"],
        json!(expected
            .iter()
            .map(|id| json!({"event":id}))
            .collect::<Vec<_>>())
    );
    let mut hash = Sha256::new();
    for id in &expected {
        hash.update(id.as_bytes());
        hash.update(b"\n");
    }
    assert_eq!(
        input["fingerprint"],
        format!("sha256:{:x}", hash.finalize())
    );
    assert!(resumed.parts.stats().0 <= 128);
    assert!(resumed.material.parts.stats().0 <= 128);

    // Later committed input is not a delivery. Repair republishes the seed's
    // content-addressed pages, rather than swapping in that newer projection.
    log.append(
        EventDraft::new(ce::USER_MESSAGE, &[], json!({"text":"still gated"})),
        "ui",
    )
    .unwrap();
    let source: Vec<_> = std::fs::read_dir(&root)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "jsonl")
        })
        .map(|path| {
            let bytes = std::fs::read(&path).unwrap();
            (path, bytes)
        })
        .collect();
    assert!(!source.is_empty());
    let state = serde_json::to_value(resumed.parts.snapshot().unwrap()).unwrap();
    let digest = state["pages"][0]["digest"]
        .as_array()
        .unwrap()
        .iter()
        .map(|byte| format!("{:02x}", byte.as_u64().unwrap()))
        .collect::<String>();
    std::fs::write(
        root.join(format!("cards-{digest}.json")),
        b"broken derived page",
    )
    .unwrap();
    assert!(resumed.material_input().is_err());
    resumed.repair_material_pages(&reader).unwrap();
    assert_eq!(resumed.material_input().unwrap(), input);
    for (path, bytes) in source {
        assert_eq!(std::fs::read(path).unwrap(), bytes);
    }
}
