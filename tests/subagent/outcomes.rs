use super::*;

fn delegated_report(
    background: bool,
    child: StreamTemplate,
) -> (EventEnvelope, Vec<EventEnvelope>) {
    let dir = tempfile::tempdir().unwrap();
    let parent = template(
        if background {
            parent_script()
        } else {
            foreground_script()
        },
        0,
    );
    let path = dir.path().to_path_buf();
    let mut host = StreamHost::new(
        [
            ("chat".to_string(), parent),
            ("explorer".to_string(), child),
        ]
        .into(),
    )
    .with_ledger_path(move |stream| Some(path.join(format!("{stream}.jsonl"))));
    let mut subagents = SubagentHost::new();
    host.open("chat", "chat").unwrap();
    say(&host, "chat", "inspect the fixture");
    let kind = if background {
        ce::WAKE
    } else {
        ce::TOOL_EXEC_COMPLETED
    };
    settle(&mut host, &mut subagents, |h| reached(h, kind));
    let parent = events(&host, "chat");
    let reports: Vec<_> = parent.iter().filter(|e| e.event_type == kind).collect();
    assert_eq!(
        reports.len(),
        1,
        "one terminal report, not a retry or duplicate"
    );
    let report = reports[0].clone();
    let child = from_ledger(dir.path(), "chat-sub-1");
    assert!(!child.is_empty());
    let origin = report
        .origin
        .as_ref()
        .expect("report points back to its child outcome");
    assert_eq!(origin.stream, "chat-sub-1");
    assert!(child.iter().any(|e| e.id == origin.event));
    let started = parent
        .iter()
        .find(|e| e.event_type == ce::TOOL_EXEC_STARTED)
        .unwrap();
    assert_eq!(report.causes, vec![started.id.clone()]);
    if background {
        assert_eq!(report.payload["source"], "expert:1");
        assert_eq!(
            parent
                .iter()
                .filter(|e| e.event_type == ce::TOOL_EXEC_COMPLETED)
                .count(),
            1
        );
    } else {
        assert_eq!(report.payload["call"], "t1");
        assert!(!parent.iter().any(|e| e.event_type == ce::WAKE));
    }
    let error = if background {
        &report.payload["body"]["error"]
    } else {
        &report.payload["error"]
    };
    if let Some(code) = error["code"].as_str() {
        let parts: Vec<_> = parent
            .iter()
            .filter(|event| {
                matches!(
                    event.event_type.as_str(),
                    ce::USER_MESSAGE
                        | ce::MODEL_CALL_COMPLETED
                        | ce::TOOL_EXEC_COMPLETED
                        | ce::WAKE
                )
            })
            .map(|event| json!({"event": event.id}))
            .collect();
        let reader = host.kernel("chat").unwrap().log().reader();
        for messages in [
            lattice::components::anthropic_model::materialize(&parts, &reader, None).unwrap(),
            lattice::components::openai_model::materialize(&parts, &reader, None).unwrap(),
        ] {
            assert!(
                serde_json::to_string(&messages).unwrap().contains(code),
                "the parent's model must see the expert error, not a success receipt"
            );
        }
    }
    (report, child)
}

fn provider_error(background: bool) {
    let error = json!({"code":"provider.synthetic", "message":"synthetic upstream failure",
        "blame":"provider", "retryable":true, "transient":true, "retryAfterMs":1234});
    let (report, child) = delegated_report(
        background,
        template(
            json!([
                {"status":"error", "error":error}
            ]),
            1,
        ),
    );
    assert_eq!(
        child
            .iter()
            .filter(|e| e.event_type == ce::MODEL_CALL_COMPLETED)
            .count(),
        1,
        "an audited failure must not trigger a hidden retry"
    );
    let reply = child
        .iter()
        .find(|e| e.event_type == ce::OUTPUT_REPLY)
        .unwrap();
    assert_eq!(report.origin.as_ref().unwrap().event, reply.id);
    if background {
        assert_eq!(report.payload["body"]["error"], error);
        assert!(report.payload["body"]["failed"].is_string());
        assert!(report.payload["summary"]
            .as_str()
            .unwrap()
            .contains("failed"));
        assert!(!report.payload["summary"]
            .as_str()
            .unwrap()
            .contains("could not start"));
    } else {
        assert_eq!(report.payload["status"], "error");
        assert_eq!(report.payload["details"]["error"], error);
        for (key, value) in error.as_object().unwrap() {
            if key != "message" {
                assert_eq!(&report.payload["error"][key], value);
            }
        }
        let message = report.payload["error"]["message"].as_str().unwrap();
        let rendered: Value =
            serde_json::from_str(message.strip_prefix("Expert call failed: ").unwrap()).unwrap();
        assert_eq!(rendered, report.payload["details"]);
        assert_eq!(report.payload["details"]["stream"], "chat-sub-1");
        assert!(report.payload["details"]["ledger"].is_string());
    }
}

#[test]
fn background_preserves_provider_failure() {
    provider_error(true);
}

#[test]
fn foreground_preserves_provider_failure() {
    provider_error(false);
}

#[test]
fn empty_success_is_not_a_missing_reply() {
    for background in [false, true] {
        for text in [json!(""), Value::Null] {
            let (report, _) = delegated_report(
                background,
                template(
                    json!([
                        {"status":"ok", "text":text}
                    ]),
                    1,
                ),
            );
            let body = if background {
                &report.payload["body"]
            } else {
                assert_eq!(report.payload["status"], "ok");
                &report.payload["result"]
            };
            assert_eq!(body["text"], text);
            assert!(body["failed"].is_null());
            assert!(body["error"].is_null());
            assert!(body["interrupted"].is_null());
        }
    }
}

#[test]
fn child_model_cancellation_is_not_success() {
    for background in [false, true] {
        let (report, _) = delegated_report(
            background,
            template(
                json!([
                    {"status":"cancelled"}
                ]),
                1,
            ),
        );
        let body = if background {
            &report.payload["body"]
        } else {
            assert_eq!(report.payload["status"], "cancelled");
            &report.payload["result"]
        };
        assert_eq!(body["interrupted"], "cancelled");
        assert!(body["cancellation"]["reason"].is_string());
        assert!(body["error"].is_null());
    }
}

#[test]
fn a_missing_template_keeps_the_startup_error_and_one_report() {
    for background in [false, true] {
        let parent = template(
            if background {
                parent_script()
            } else {
                foreground_script()
            },
            0,
        );
        let mut host = StreamHost::new([("chat".to_string(), parent)].into());
        let mut subagents = SubagentHost::new();
        host.open("chat", "chat").unwrap();
        say(&host, "chat", "inspect the fixture");
        let kind = if background {
            ce::WAKE
        } else {
            ce::TOOL_EXEC_COMPLETED
        };
        settle(&mut host, &mut subagents, |h| reached(h, kind));
        let events = events(&host, "chat");
        let reports: Vec<_> = events.iter().filter(|e| e.event_type == kind).collect();
        assert_eq!(reports.len(), 1);
        let report = reports[0];
        assert!(report.origin.is_none(), "no child was created");
        let error = if background {
            assert_eq!(report.payload["source"], "expert:1");
            assert!(report.payload["summary"]
                .as_str()
                .unwrap()
                .contains("could not start"));
            &report.payload["body"]["error"]
        } else {
            assert_eq!(report.payload["status"], "error");
            &report.payload["error"]
        };
        assert_eq!(error["code"], "ask.no_stream");
        assert_eq!(error["blame"], "environment");
        assert_eq!(error["retryable"], false);
    }
}

#[test]
fn a_completed_turn_without_a_reply_is_a_failure() {
    struct NoReply;
    impl Component for NoReply {
        fn handle(&mut self, _: &str, event: &EventEnvelope, ctx: &mut Ctx) {
            ctx.emit(
                "out",
                EventDraft::new(ce::TURN_COMPLETED, &[&event.id], json!({})),
            );
        }
    }
    for background in [false, true] {
        let mut child = template(json!([]), 1);
        child.factories.insert(
            minimal_loop::NAME.to_string(),
            Box::new(|_| Box::new(NoReply)),
        );
        let (report, child) = delegated_report(background, child);
        assert!(!child.iter().any(|e| e.event_type == ce::OUTPUT_REPLY));
        let error = if background {
            &report.payload["body"]["error"]
        } else {
            assert_eq!(report.payload["status"], "error");
            &report.payload["error"]
        };
        assert_eq!(error["code"], "ask.no_reply");
        assert_eq!(error["blame"], "environment");
        assert_eq!(error["retryable"], false);
    }
}
