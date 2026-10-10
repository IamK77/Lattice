use super::*;

#[test]
fn disabled_manual_compaction_reports_skip_without_clearing_an_existing_pause() {
    for native in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("disabled.jsonl");
        let mut gate = manual::config(native, true);
        let main = manual::main_script(900);
        let mut kernel = condensing_kernel_with(
            json!({"script":[manual::failure(native)]}),
            main.clone(),
            gate.clone(),
            false,
            Some(&path),
        );
        say(&mut kernel, "first");
        say(&mut kernel, "second");
        let reader = kernel.log().reader();
        let paused =
            context_gate::CompactionObserver::default().status_at(&reader, reader.snapshot_end());
        kernel.shutdown();
        let failure = paused
            .unwrap()
            .unwrap()
            .failure
            .expect("fixture must be paused");

        // Changing the assembly switch is not a model.profile reset. Historical
        // failure evidence remains even though this runtime disables new attempts.
        gate["condense"] = json!(false);
        let mut kernel = condensing_kernel_with(
            json!({"script":[manual::success(native)]}),
            main,
            gate,
            false,
            Some(&path),
        );
        let before = kernel.log().len();
        manual::manual(&mut kernel);
        manual::manual(&mut kernel);
        let reader = kernel.log().reader();
        let status =
            context_gate::CompactionObserver::default().status_at(&reader, reader.snapshot_end());
        let events = reader.replay(before as u64 + 1);
        kernel.shutdown();
        let status = status.unwrap().unwrap();
        assert_eq!(status.failure, Some(failure));
        assert!(!status.in_flight);
        let events = events.unwrap();
        assert!(!events.iter().any(|event| matches!(
            event.event_type.as_str(),
            ce::MODEL_CALL_STARTED | ce::USER_MESSAGE
        )));
        let commands: Vec<_> = events
            .iter()
            .filter(|event| {
                event.event_type == ce::EXTERNAL_INPUT
                    && event.payload["channel"] == context_gate::COMPACT_CHANNEL
            })
            .collect();
        assert_eq!(commands.len(), 2);
        let skipped: Vec<_> = events
            .iter()
            .filter(|event| {
                event.event_type == context_gate::DECISION
                    && event.payload["action"] == "compact_skipped"
            })
            .collect();
        assert_eq!(skipped.len(), 2);
        for command in commands {
            let matches: Vec<_> = skipped
                .iter()
                .filter(|event| event.causes.contains(&command.id))
                .collect();
            assert_eq!(
                matches.len(),
                1,
                "each explicit command gets one skipped explanation"
            );
            assert_eq!(
                matches[0].reason.as_deref(),
                Some("Compaction is not enabled in this assembly")
            );
        }
    }
}

#[test]
fn mechanical_reduction_and_lower_usage_do_not_clear_a_semantic_failure() {
    for native in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("context.jsonl");
        let gate = json!({
            "profile":{"contextWindow":268000,"usageFields":{"input":"input_tokens"}},
            "ratio":0.85,"keepRecentTools":0,"condense":true,
            "nativeCompaction":native,"keepRecentParts":1,"minCondense":1
        });
        let main = json!({"script":[
            {"status":"ok","usage":{"input_tokens":228025},"toolCalls":[{"id":"c1","tool":"calc","arguments":{"numbers":[4,7]}}]},
            {"status":"ok","text":"calculated","usage":{"input_tokens":228025}},
            {"status":"ok","text":"continued","usage":{"input_tokens":123661}},
            {"status":"ok","text":"continued again","usage":{"input_tokens":144281}}
        ]});
        let compactor = json!({"script":[{"status":"error","error":{
            "code":"fixture.provider_failure","message":"response protection is unavailable",
            "blame":"provider","retryable":false,"transient":false
        }}]});
        let mut kernel =
            condensing_kernel_with(compactor, main.clone(), gate.clone(), false, Some(&path));
        say(&mut kernel, "calculate");
        say(&mut kernel, "continue");
        say(&mut kernel, "continue again");
        let reader = kernel.log().reader();
        let status =
            context_gate::CompactionObserver::default().status_at(&reader, reader.snapshot_end());
        let events = reader.replay(1);
        kernel.shutdown();
        let status = status.unwrap().unwrap();
        let events = events.unwrap();
        let failure = status.failure.unwrap();
        assert_eq!(failure.message, "response protection is unavailable");
        assert!(!status.in_flight);
        let forwards: Vec<_> = events
            .iter()
            .filter(|event| {
                event.event_type == ce::MODEL_CALL_STARTED
                    && event.source == "gate"
                    && event.payload.get("purpose").is_none()
            })
            .collect();
        let reduced = forwards
            .iter()
            .find(|event| {
                event.payload["input"]["parts"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|part| part.get("digest").is_some())
            })
            .unwrap();
        let digests: Vec<_> = reduced.payload["input"]["parts"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|part| part.get("digest").is_some())
            .collect();
        let latest = forwards.last().unwrap().payload["input"]["parts"]
            .as_array()
            .unwrap();
        assert!(digests.iter().all(|digest| latest.contains(digest)));
        let failed = events
            .iter()
            .find(|event| event.id == failure.event)
            .unwrap();
        assert!(
            reduced.seq < failed.seq,
            "mechanical reduction must precede this semantic failure"
        );
        let completions: Vec<_> = events
            .iter()
            .filter(|event| event.event_type == ce::MODEL_CALL_COMPLETED && event.source == "model")
            .collect();
        assert_eq!(
            completions.last().unwrap().payload["usage"]["input_tokens"],
            144281
        );
        let attempts = |events: &[lattice::EventEnvelope]| {
            events
                .iter()
                .filter(|event| {
                    event.event_type == ce::MODEL_CALL_STARTED
                        && event.payload.get("purpose").is_some()
                })
                .count()
        };
        assert_eq!(
            attempts(&events),
            1,
            "ordinary conversation must not retry semantic failure"
        );

        let reopened = condensing_kernel_with(json!({"script":[]}), main, gate, false, Some(&path));
        let reader = reopened.log().reader();
        let restored =
            context_gate::CompactionObserver::default().status_at(&reader, reader.snapshot_end());
        let after = reader.replay(1);
        reopened.shutdown();
        assert_eq!(restored.unwrap().unwrap().failure, Some(failure));
        assert_eq!(
            attempts(&after.unwrap()),
            1,
            "recovery must not reissue compaction"
        );
    }
}
