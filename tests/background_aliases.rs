//! Model-facing wakes use the same assignments as the shared background view.
use lattice::background_aliases::{identity_for_event, Projection};
use lattice::components::{anthropic_model, openai_model};
use lattice::{core_events as ce, EventDraft, EventEnvelope, EventLog};
use serde_json::{json, Value};

#[path = "background_aliases/correlation.rs"]
mod correlation;
#[path = "background_aliases/media.rs"]
mod media;
#[path = "background_aliases/presentation.rs"]
mod presentation;
#[path = "background_aliases/receipts.rs"]
mod receipts;

fn responses(
    parts: &[Value],
    log: &lattice::LogReader,
    docs: Option<&std::path::Path>,
) -> Vec<Value> {
    lattice::components::responses_model::materialize(
        parts,
        log,
        docs,
        "fixture-model",
        "https://fixture.invalid/v1",
    )
    .unwrap()
}

fn append(log: &mut EventLog, kind: &str, causes: &[&str], payload: Value) -> EventEnvelope {
    log.append(EventDraft::new(kind, causes, payload), "fixture")
        .unwrap()
}

fn texts(messages: &[Value]) -> Vec<String> {
    let mut out = Vec::new();
    for message in messages {
        if let Some(text) = message["content"].as_str() {
            out.push(text.to_owned());
        }
        if let Some(blocks) = message["content"].as_array() {
            out.extend(
                blocks
                    .iter()
                    .filter_map(|block| block["text"].as_str().map(str::to_owned)),
            );
        }
    }
    out
}

#[test]
fn model_wakes_share_aliases_across_kinds_material_order_and_reopening() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("aliases.ledger");
    let open = || {
        EventLog::open_segmented(ce::core_event_decls(), "owner-stream", path.clone(), 4096)
            .unwrap()
    };
    let mut log = open();
    let mut wakes = Vec::new();
    for (tool, kind, result, body) in [
        (
            "ask",
            "Expert",
            json!({"job":1}),
            json!({"job":1,"answer":"done"}),
        ),
        (
            "Run",
            "Run",
            json!({"job":"long-call-id","background":true}),
            json!({"job":"long-call-id","exit_code":0}),
        ),
        (
            "Schedule",
            "Timer",
            json!({"timer":1}),
            json!({"timer":1,"fire":1}),
        ),
        (
            "Watch",
            "Watch",
            json!({"watch":1}),
            json!({"watch":1,"fire":1}),
        ),
    ] {
        let request = append(
            &mut log,
            ce::TOOL_EXEC_STARTED,
            &[],
            json!({"call":tool,"tool":tool,"arguments":{}}),
        );
        append(
            &mut log,
            ce::TOOL_EXEC_COMPLETED,
            &[&request.id],
            json!({"call":tool,"status":"ok","result":result}),
        );
        let wake = append(
            &mut log,
            ce::WAKE,
            &[&request.id],
            json!({
                "source":format!("original-{tool}"),"summary":"finished","body":body
            }),
        );
        wakes.push((kind, wake));
    }
    let reader = log.reader();
    let aliases = Projection::recover(&reader, reader.snapshot_end()).unwrap();
    let expected: Vec<_> = wakes
        .iter()
        .map(|(kind, wake)| {
            let identity = identity_for_event(&reader, wake).unwrap().unwrap();
            (
                kind.to_string(),
                aliases.alias(&identity).unwrap().to_string(),
            )
        })
        .collect();
    drop(reader);
    drop(log);

    // Reconstruct from persisted events, including receipts not in the model's
    // material. Reverse the material: presentation order must not assign IDs.
    for reverse in [true, false] {
        let log = open();
        let reader = log.reader();
        let mut parts: Vec<_> = wakes
            .iter()
            .map(|(_, wake)| json!({"event":wake.id}))
            .collect();
        if reverse {
            parts.reverse();
        }
        let anthropic = texts(&anthropic_model::materialize(&parts, &reader, None).unwrap());
        let openai = texts(&openai_model::materialize(&parts, &reader, None).unwrap());
        assert_eq!(
            anthropic, openai,
            "dialects must not invent competing reference text"
        );
        assert_eq!(texts(&responses(&parts, &reader, None)), openai);
        for ((kind, alias), (_, wake)) in expected.iter().zip(&wakes) {
            let header = format!("[woken by {kind} {alias}; stream=\"owner-stream\"]");
            let matching: Vec<_> = openai
                .iter()
                .filter(|text| text.starts_with(&header))
                .collect();
            assert_eq!(
                matching.len(),
                1,
                "each background wake must expose its shared alias"
            );
            assert!(matching[0].contains(&wake.payload["body"].to_string()));
            assert!(matching[0].contains(&format!(
                "Original source: {}",
                wake.payload["source"].as_str().unwrap()
            )));
            assert_eq!(
                reader.get(&wake.id).unwrap().unwrap().payload,
                wake.payload,
                "rendering must not rewrite ledger evidence"
            );
        }
    }
}

#[test]
fn internal_wakes_without_a_user_invocation_keep_their_existing_text() {
    let mut log = EventLog::in_memory(ce::core_event_decls(), "owner-stream");
    let wake = append(
        &mut log,
        ce::WAKE,
        &[],
        json!({"source":"skills:refresh","summary":"changed","body":{"watch":"internal"}}),
    );
    let parts = [json!({"event":wake.id})];
    let reader = log.reader();
    let expected = vec!["[woken by skills:refresh] changed\n{\"watch\":\"internal\"}".to_string()];
    assert_eq!(
        texts(&anthropic_model::materialize(&parts, &reader, None).unwrap()),
        expected
    );
    assert_eq!(
        texts(&openai_model::materialize(&parts, &reader, None).unwrap()),
        expected
    );
}
