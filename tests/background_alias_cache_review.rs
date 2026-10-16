//! Post-merge review regressions: disposable cache publication is not authority.
use lattice::background_aliases::{identity_for_event, operations, Projection};
use lattice::components::{anthropic_model, openai_model, responses_model};
use lattice::{core_events as ce, EventDraft, EventEnvelope, EventLog};
use serde_json::json;
use sha2::{Digest, Sha256};

fn fixture() -> (tempfile::TempDir, EventLog, EventEnvelope, String) {
    fixture_with_rotation(4096)
}

fn fixture_with_rotation(rotation: u64) -> (tempfile::TempDir, EventLog, EventEnvelope, String) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("history.ledger");
    let mut log =
        EventLog::open_segmented(ce::core_event_decls(), "review-stream", root, rotation).unwrap();
    let request = log
        .append(
            EventDraft::new(
                ce::TOOL_EXEC_STARTED,
                &[],
                json!({"call":"create","tool":"Schedule","arguments":{"delay_ms":60000}}),
            ),
            "fixture",
        )
        .unwrap();
    log.append(
        EventDraft::new(
            ce::TOOL_EXEC_COMPLETED,
            &[&request.id],
            json!({"call":"create","status":"ok","result":{"timer":1}}),
        ),
        "fixture",
    )
    .unwrap();
    let wake = log
        .append(
            EventDraft::new(
                ce::WAKE,
                &[&request.id],
                json!({"source":"timer:1","summary":"fired","body":{"timer":1}}),
            ),
            "fixture",
        )
        .unwrap();
    let reader = log.reader();
    let projection = Projection::recover(&reader, reader.snapshot_end()).unwrap();
    let identity = identity_for_event(&reader, &wake).unwrap().unwrap();
    let alias = projection.alias(&identity).unwrap().to_owned();
    (dir, log, wake, alias)
}

fn block_only_alias_cache(dir: &tempfile::TempDir) {
    let path = dir.path().join("history.ledger").join(format!(
        "checkpoint-{:x}.next",
        Sha256::digest(b"background-aliases")
    ));
    std::fs::create_dir(path).unwrap();
}

#[test]
fn valid_alias_resolution_survives_disposable_cache_write_failure() {
    let (dir, mut log, wake, alias) = fixture();
    let request = || json!({"tool":"Unschedule","arguments":{"timer":alias}});
    let mut control = request();
    operations::Resolver::default().normalize(&log.reader(), &mut control);
    assert_eq!(control["arguments"]["timer"], 1);
    block_only_alias_cache(&dir);
    // The source ledger remains both writable and readable.
    log.append(
        EventDraft::new(ce::USER_MESSAGE, &[], json!({"text":"still writable"})),
        "fixture",
    )
    .unwrap();
    assert_eq!(
        log.reader().get(&wake.id).unwrap().unwrap().payload,
        wake.payload
    );
    let mut actual = request();
    operations::Resolver::default().normalize(&log.reader(), &mut actual);
    assert_eq!(
        actual["arguments"]["timer"], 1,
        "valid alias rejected only because the derived cache could not be written: {actual}"
    );
    assert!(actual.get(operations::RESOLUTION_ERROR).is_none());
}

#[test]
fn model_materialization_survives_disposable_cache_write_failure() {
    let (dir, log, wake, _) = fixture();
    let reader = log.reader();
    let parts = [json!({"event":wake.id})];
    assert!(openai_model::materialize(&parts, &reader, None).is_ok());
    assert!(anthropic_model::materialize(&parts, &reader, None).is_ok());
    assert!(responses_model::materialize(
        &parts,
        &reader,
        None,
        "fixture",
        "http://fixture.invalid"
    )
    .is_ok());
    block_only_alias_cache(&dir);
    let results = [
        ("openai", openai_model::materialize(&parts, &reader, None)),
        (
            "anthropic",
            anthropic_model::materialize(&parts, &reader, None),
        ),
        (
            "responses",
            responses_model::materialize(
                &parts,
                &reader,
                None,
                "fixture",
                "http://fixture.invalid",
            ),
        ),
    ];
    let failures: Vec<_> = results
        .into_iter()
        .filter_map(|(name, result)| result.err().map(|error| (name, error)))
        .collect();
    assert!(
        failures.is_empty(),
        "readable model material blocked by disposable cache publication: {failures:?}"
    );
}

#[test]
fn source_read_failure_still_rejects_resolution_and_all_model_dialects() {
    let (dir, log, wake, alias) = fixture_with_rotation(1);
    drop(log);
    let root = dir.path().join("history.ledger");
    // Damage only a sealed receipt body without changing its size. Startup
    // checks size/index; reconstruction must reject the unreadable source body.
    let source = root.join("00000000000000000001.jsonl");
    let mut bytes = std::fs::read(&source).unwrap();
    bytes[0] = b'!';
    std::fs::write(source, bytes).unwrap();
    let log = EventLog::open_segmented(ce::core_event_decls(), "review-stream", root, 1).unwrap();
    let reader = log.reader();
    assert!(
        reader.get(&wake.id).unwrap().is_some(),
        "the selected wake itself remains readable"
    );
    assert!(Projection::recover(&reader, reader.snapshot_end()).is_err());
    let mut request = json!({"tool":"Unschedule","arguments":{"timer":alias}});
    operations::Resolver::default().normalize(&reader, &mut request);
    assert_eq!(request["arguments"]["timer"], alias);
    assert!(request.get(operations::REFERENCE).is_none());
    assert!(request[operations::RESOLUTION_ERROR]
        .as_str()
        .is_some_and(|message| !message.is_empty()));
    let parts = [json!({"event":wake.id})];
    for (dialect, result) in [
        ("openai", openai_model::materialize(&parts, &reader, None)),
        (
            "anthropic",
            anthropic_model::materialize(&parts, &reader, None),
        ),
        (
            "responses",
            responses_model::materialize(
                &parts,
                &reader,
                None,
                "fixture",
                "http://fixture.invalid",
            ),
        ),
    ] {
        assert!(
            result.is_err(),
            "{dialect} must not hide unreadable source history"
        );
    }
}
