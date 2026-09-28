use super::*;

fn forwarded(kernel: &Kernel) -> lattice::EventEnvelope {
    kernel
        .log()
        .find_back(|event| {
            event.event_type == ce::MODEL_CALL_STARTED
                && event.source == "gate"
                && event.payload.get("purpose").is_none()
        })
        .unwrap()
        .unwrap()
}

fn rendered(kernel: &Kernel, request: &lattice::EventEnvelope) -> String {
    let parts = request.payload["input"]["parts"].as_array().unwrap();
    verify_fingerprint(parts, request.payload["input"]["fingerprint"].as_str()).unwrap();
    let input = lattice::components::responses_model::materialize(
        parts,
        &kernel.log().reader(),
        None,
        "exam",
        "http://local",
    )
    .unwrap();
    serde_json::to_string(&input).unwrap()
}

#[test]
fn truncated_manual_summary_keeps_original_material_and_pause_after_restart() {
    for stop_reason in ["length", "max_tokens"] {
        for prior_failure in [false, true] {
            let home = tempfile::tempdir().unwrap();
            let ledger = home.path().join("truncated.ledger");
            let cfg = config(false, false);
            let mut script = Vec::new();
            if prior_failure {
                script.push(failure(false));
            }
            script.push(json!({
                "status":"ok", "purpose":purpose(false),
                "text":"Done: completed\nState: ready\nOpen: pending\nFacts: cut off",
                "stopReason":stop_reason,
            }));
            let mut kernel = condensing_kernel_with(
                json!({"script":script}),
                main_script(10),
                cfg.clone(),
                false,
                Some(&ledger),
            );
            say(&mut kernel, "original-material-must-survive-truncation");
            say(&mut kernel, "recent-material");
            if prior_failure {
                manual(&mut kernel);
            }
            manual(&mut kernel);
            let attempts = requests(&kernel);
            assert_eq!(attempts, if prior_failure { 2 } else { 1 });
            assert!(
                !kernel
                    .log()
                    .any(|event| event.event_type == context_gate::SUMMARY)
                    .unwrap(),
                "a truncated result cannot replace history merely because all headings exist"
            );
            for restart in [false, true] {
                if restart {
                    drop(kernel);
                    kernel = condensing_kernel_with(
                        json!({"script":[]}),
                        main_script(10),
                        cfg.clone(),
                        false,
                        Some(&ledger),
                    );
                    kernel.run_until_quiescent().unwrap();
                }
                let reader = kernel.log().reader();
                let status = context_gate::CompactionObserver::default()
                    .status_at(&reader, reader.snapshot_end())
                    .unwrap()
                    .unwrap();
                assert!(!status.in_flight);
                assert_eq!(status.failure.unwrap().code, "summary_rejected");
                say(&mut kernel, "check unchanged material");
                assert!(rendered(&kernel, &forwarded(&kernel))
                    .contains("original-material-must-survive-truncation"));
                assert_eq!(requests(&kernel), attempts);
            }
            kernel.shutdown();
        }
    }
}

#[test]
fn manual_summary_replaces_model_material_live_and_after_restart() {
    for native in [false, true] {
        for restart in [false, true] {
            let home = tempfile::tempdir().unwrap();
            let ledger = home.path().join("effective.ledger");
            let cfg = config(native, false);
            let mut response = success(native);
            if native {
                response["nativeCompaction"]["model"] = json!("exam");
                response["nativeCompaction"]["baseUrl"] = json!("http://local");
            }
            let mut kernel = condensing_kernel_with(
                json!({"script":[response]}),
                main_script(10),
                cfg.clone(),
                false,
                Some(&ledger),
            );
            let old_text = "old-material-must-be-replaced ".repeat(1000);
            say(&mut kernel, &old_text);
            say(&mut kernel, "recent-material-must-survive");
            let before = rendered(&kernel, &forwarded(&kernel));
            assert!(before.contains("old-material-must-be-replaced"));
            assert_eq!(requests(&kernel), 0);
            let ordinary = ordinary_requests(&kernel);
            manual(&mut kernel);
            assert_eq!(requests(&kernel), 1);
            assert_eq!(ordinary_requests(&kernel), ordinary);
            let summary = kernel
                .log()
                .find_back(|event| event.event_type == context_gate::SUMMARY)
                .unwrap()
                .unwrap();
            let covers = summary.payload["covers"].as_array().unwrap();
            assert!(!covers.is_empty());

            if restart {
                drop(kernel);
                kernel = condensing_kernel_with(
                    json!({"script":[]}),
                    main_script(10),
                    cfg,
                    false,
                    Some(&ledger),
                );
                kernel.run_until_quiescent().unwrap();
                assert_eq!(requests(&kernel), 1, "restart cannot replay the command");
            }
            say(&mut kernel, "new-material-must-survive");
            let request = forwarded(&kernel);
            let parts = request.payload["input"]["parts"].as_array().unwrap();
            assert_eq!(
                parts
                    .iter()
                    .filter(|part| part["digest"]["of"] == summary.id)
                    .count(),
                1,
                "the next actual model request must use the accepted summary"
            );
            for covered in covers {
                assert!(
                    !parts.iter().any(|part| &part["event"] == covered),
                    "covered originals must not accompany their replacement"
                );
            }
            let after = rendered(&kernel, &request);
            assert!(!after.contains("old-material-must-be-replaced"));
            assert!(after.contains("recent-material-must-survive"));
            assert!(after.contains("new-material-must-survive"));
            assert!(after.contains(if native {
                "test-capsule"
            } else {
                "Done: tested"
            }));
            assert!(
                after.len() < before.len(),
                "fixture model input must shrink"
            );
            assert_eq!(requests(&kernel), 1, "no automatic retry below threshold");
            assert!(
                kernel
                    .log()
                    .any(|event| event.event_type == ce::USER_MESSAGE
                        && event.payload["text"] == old_text)
                    .unwrap(),
                "compaction must not delete original history"
            );
            kernel.shutdown();
        }
    }
}
