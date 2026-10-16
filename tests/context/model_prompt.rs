//! Local profile supplements are selected data, not model-name templates.
use super::*;

fn system(kernel: &Kernel) -> String {
    kernel
        .log()
        .replay(1)
        .unwrap()
        .into_iter()
        .rfind(|e| e.event_type == ce::MODEL_CALL_STARTED && e.source == "gate")
        .expect("a forwarded request")
        .payload["system"]
        .as_str()
        .expect("small inline system text")
        .to_owned()
}

#[test]
fn ordinary_requests_preserve_common_text_and_append_the_authored_text_once() {
    let supplement = " \nSYNTHETIC_A {model}\n ";
    let mut kernel = dial_kernel_with(Some(json!({
        "system": "common {model}", "systemTail": "standing rules", "modelName": "shared-model",
        "profile": {"systemPromptSupplement": supplement},
    })));
    for _ in 0..2 {
        let payload = forwarded(&mut kernel);
        assert_eq!(
            payload["system"],
            format!("common shared-model\n\nstanding rules\n\n{supplement}")
        );
        assert_eq!(
            payload["system"]
                .as_str()
                .unwrap()
                .matches("SYNTHETIC_A")
                .count(),
            1
        );
    }
    kernel.shutdown();
}

#[test]
fn absent_and_empty_supplements_do_not_change_existing_prompts() {
    for profile in [json!({}), json!({"systemPromptSupplement": ""})] {
        let mut kernel = dial_kernel_with(Some(json!({
            "system": "common {model}", "modelName": "shared-model", "profile": profile,
        })));
        assert_eq!(forwarded(&mut kernel)["system"], "common shared-model");
        kernel.shutdown();
    }
}

#[test]
fn upstream_system_is_preserved_without_template_expansion_or_double_injection() {
    let mut kernel = dial_kernel_with(Some(json!({
        "system": "unused common text", "modelName": "shared-model",
        "profile": {"systemPromptSupplement": "SYNTHETIC_A {model}"},
    })));
    for _ in 0..2 {
        kernel.injector("probe").emit(
            "ask",
            EventDraft::new(
                ce::MODEL_CALL_STARTED,
                &[],
                json!({
                    "model": "main", "system": "upstream {model}",
                    "input": {"parts": [], "fingerprint": EMPTY_PARTS},
                }),
            ),
        );
        kernel.run_until_quiescent().unwrap();
        assert_eq!(system(&kernel), "upstream {model}\n\nSYNTHETIC_A {model}");
    }
    kernel.shutdown();
}

#[test]
fn internal_requests_do_not_acquire_the_selected_supplement() {
    let mut kernel = dial_kernel_with(Some(json!({
        "system":"common", "profile":{"systemPromptSupplement":"SYNTHETIC_A"},
    })));
    kernel.injector("probe").emit(
        "ask",
        EventDraft::new(
            ce::MODEL_CALL_STARTED,
            &[],
            json!({
                "model":"main", "purpose":"synthetic.internal", "system":"internal {model}",
                "input":{"parts":[],"fingerprint":EMPTY_PARTS},
            }),
        ),
    );
    kernel.run_until_quiescent().unwrap();
    assert_eq!(system(&kernel), "internal {model}");
    kernel.shutdown();
}

#[test]
fn document_backed_upstream_text_is_resolved_then_supplemented_once() {
    let home = tempfile::tempdir().unwrap();
    let ledger = home.path().join("large.jsonl");
    let base = "UPSTREAM {model}\n".repeat(400);
    let supplement = " \nSYNTHETIC_A {model}\n ";
    assert!(base.len() > 4096);
    let reply = json!({"status":"ok","text":"synthetic reply"});
    let mut kernel = condensing_kernel_with(
        json!({"script":[]}),
        json!({"script":[reply.clone(),reply]}),
        json!({"system":"unused common", "condense":false,
            "profile":{"systemPromptSupplement":supplement}}),
        false,
        Some(&ledger),
    );
    for _ in 0..2 {
        kernel.injector("loop").emit(
            "ask",
            EventDraft::new(
                ce::MODEL_CALL_STARTED,
                &[],
                json!({
                    "model":"main", "system":base, "input":{"parts":[],"fingerprint":EMPTY_PARTS},
                }),
            ),
        );
        kernel.run_until_quiescent().unwrap();
    }
    let events = kernel.log().replay(1).unwrap();
    kernel.shutdown();
    let requests: Vec<_> = events
        .iter()
        .filter(|event| event.event_type == ce::MODEL_CALL_STARTED)
        .collect();
    assert_eq!(requests.len(), 4);
    let mut forwarded_files = Vec::new();
    for request in requests {
        let file = request.payload["system"]["file"]
            .as_str()
            .expect("large text is document-backed");
        let text = std::fs::read_to_string(ledger.with_extension("").join(file)).unwrap();
        if request.source == "gate" {
            assert_eq!(text, format!("{base}\n\n{supplement}"));
            assert_eq!(text.matches("SYNTHETIC_A").count(), 1);
            forwarded_files.push(file);
        } else {
            assert_eq!(text, base, "the upstream audit is not rewritten");
        }
    }
    assert_eq!(forwarded_files.len(), 2);
    assert_eq!(
        forwarded_files[0], forwarded_files[1],
        "unchanged text reuses its document"
    );
}

#[test]
fn reopening_uses_new_local_text_instead_of_old_dials_or_a_warm_cached_prompt() {
    for selected in [Some("SYNTHETIC_C"), None, Some("")] {
        let home = tempfile::tempdir().unwrap();
        let ledger = home.path().join("profile.ledger");
        let config = |text: Option<&str>| {
            let mut config = json!({"system":"common {model}", "modelName":"shared-model",
                "condense":false, "profile":{"contextWindow":100_000,
                "usageFields":{"input":"input_tokens","cacheRead":"cache_read_input_tokens"}}});
            if let Some(text) = text {
                config["profile"]["systemPromptSupplement"] = json!(text);
            }
            config
        };
        let reply = json!({"status":"ok","text":"synthetic reply",
            "usage":{"input_tokens":20,"cache_read_input_tokens":10}});
        let script = json!({"script":[reply.clone(),reply]});
        let mut kernel = condensing_kernel_with(
            json!({"script":[]}),
            script.clone(),
            config(Some("SYNTHETIC_A")),
            false,
            Some(&ledger),
        );
        say(&mut kernel, "first");
        kernel.injector("ui").emit("answer", EventDraft::new(ce::EXTERNAL_INPUT, &[], json!({
            "channel":context_gate::MODEL_CHANNEL,"model":"shared-model","systemPromptSupplement":"SYNTHETIC_B",
        })));
        say(&mut kernel, "after switching");
        assert!(system(&kernel).ends_with("SYNTHETIC_B"));
        kernel.shutdown();
        let mut reopened = condensing_kernel_with(
            json!({"script":[]}),
            script,
            config(selected),
            false,
            Some(&ledger),
        );
        say(
            &mut reopened,
            "after reopening with edited local configuration",
        );
        let text = system(&reopened);
        assert!(text.contains("common shared-model"));
        assert!(!text.contains("SYNTHETIC_A"));
        assert!(
            !text.contains("SYNTHETIC_B"),
            "selected={selected:?}; text={text}; recent={:?}",
            reopened
                .log()
                .replay(1)
                .unwrap()
                .into_iter()
                .rev()
                .take(8)
                .collect::<Vec<_>>()
        );
        assert_eq!(
            text.matches("SYNTHETIC_C").count(),
            usize::from(selected == Some("SYNTHETIC_C"))
        );
        reopened.shutdown();
    }
}

#[test]
fn an_upstream_prompt_does_not_become_the_next_ordinary_requests_cached_base() {
    let reply = json!({"status":"ok","text":"synthetic reply",
        "usage":{"input_tokens":20,"cache_read_input_tokens":10}});
    let mut kernel = build_gated_kernel(
        json!({"system":"common", "profile":{
            "contextWindow":100_000,"systemPromptSupplement":"SYNTHETIC_A",
            "usageFields":{"input":"input_tokens","cacheRead":"cache_read_input_tokens"}
        }}),
        json!({"script":[reply.clone(),reply]}),
    );
    kernel.injector("loop").emit(
        "ask",
        EventDraft::new(
            ce::MODEL_CALL_STARTED,
            &[],
            json!({
                "model":"main", "system":"upstream-only {model}",
                "input":{"parts":[],"fingerprint":EMPTY_PARTS},
            }),
        ),
    );
    kernel.run_until_quiescent().unwrap();
    assert_eq!(system(&kernel), "upstream-only {model}\n\nSYNTHETIC_A");
    say(&mut kernel, "ordinary request without an upstream system");
    let text = system(&kernel);
    assert!(
        text.starts_with("common"),
        "upstream text leaked into the common base: {text}"
    );
    assert!(!text.contains("upstream-only"));
    assert_eq!(text.matches("SYNTHETIC_A").count(), 1);
    kernel.shutdown();
}

#[test]
fn warm_cache_does_not_retain_a_previous_entry_supplement() {
    let reply = json!({"status": "ok", "text": "synthetic reply",
        "usage": {"input_tokens": 20, "cache_read_input_tokens": 10}});
    let mut kernel = build_gated_kernel(
        json!({"system": "common {model}", "modelName": "shared-model",
            "profile": {"contextWindow": 1000, "systemPromptSupplement": "SYNTHETIC_A",
                "usageFields": {"input": "input_tokens", "cacheRead": "cache_read_input_tokens"}}}),
        json!({"script": [reply.clone(), reply.clone(), reply.clone(), reply.clone(), reply]}),
    );
    say(&mut kernel, "first entry");
    assert!(system(&kernel).ends_with("\n\nSYNTHETIC_A"));
    say(&mut kernel, "same entry again");
    assert_eq!(system(&kernel).matches("SYNTHETIC_A").count(), 1);
    let latest_reply = kernel
        .log()
        .replay(1)
        .unwrap()
        .into_iter()
        .rfind(|e| e.event_type == ce::MODEL_CALL_COMPLETED)
        .unwrap();
    assert_eq!(latest_reply.payload["usage"]["cache_read_input_tokens"], 10);

    // The underlying model name is unchanged: the selected entry supplies data.
    for (supplement, expected) in [
        (Some("SYNTHETIC_B"), Some("SYNTHETIC_B")),
        (None, None),
        (Some(""), None),
    ] {
        let dial = json!({"channel": context_gate::MODEL_CHANNEL, "model": "shared-model",
            "systemPromptSupplement": supplement});
        kernel
            .injector("ui")
            .emit("answer", EventDraft::new(ce::EXTERNAL_INPUT, &[], dial));
        say(&mut kernel, "after switching entries");
        let text = system(&kernel);
        assert!(text.contains("common shared-model"));
        assert!(!text.contains("SYNTHETIC_A"), "old entry leaked: {text}");
        assert_eq!(
            text.matches("SYNTHETIC_B").count(),
            usize::from(expected.is_some())
        );
        if let Some(expected) = expected {
            assert!(text.ends_with(expected));
        }
    }
    kernel.shutdown();
}
