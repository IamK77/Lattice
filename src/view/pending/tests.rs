use super::*;
use serde_json::json;

fn event(id: &str, kind: &str, payload: serde_json::Value) -> EventEnvelope {
    serde_json::from_value(json!({"v":1,"id":id,"seq":1,"stream":"fixture","time":"t","type":kind,"source":"fixture","causes":[],"payload":payload})).unwrap()
}

#[test]
fn visible_ordinals_and_groups_match_explicit_filter_for_every_pending_subset() {
    for mask in 0..64 {
        let mut pending = PendingInputs::with_activity(crate::view::activity::Activity {
            busy: true,
            ..Default::default()
        });
        let mut entries = Vec::new();
        let mut hidden = Vec::new();
        for n in 0..6 {
            let id = format!("input-{n}");
            pending.observe(&event(&id, ce::USER_MESSAGE, json!({"text":"same"})));
            let start = entries.len();
            entries.push(Entry::User("same".into()));
            for image in 0..n % 3 {
                entries.push(Entry::Attachment(format!("{n}-{image}")));
            }
            let end = entries.len();
            pending.cards(&id, start..end);
            let is_pending = mask & (1 << n) != 0;
            hidden.extend(std::iter::repeat_n(is_pending, end - start));
            if !is_pending {
                pending.observe(&event(
                    "request",
                    ce::MODEL_CALL_STARTED,
                    json!({"input":{"parts":[{"event":id}]}}),
                ));
            }
            entries.push(Entry::Agent(format!("reply-{n}")));
            hidden.push(false);
        }
        let sources: Vec<_> = (0..entries.len()).filter(|at| !hidden[*at]).collect();
        let visible: Vec<_> = sources.iter().map(|at| entries[*at].clone()).collect();
        assert!(pending.valid(entries.len()));
        assert_eq!(pending.before(entries.len()), visible.len());
        for boundary in 0..=entries.len() {
            assert_eq!(
                pending.before(boundary),
                hidden[..boundary].iter().filter(|hidden| !**hidden).count()
            );
        }
        for (at, source) in sources.iter().enumerate() {
            assert_eq!(pending.source(at), *source);
        }
        for at in 0..=visible.len() {
            let actual = pending.transcript_group(&entries, at).unwrap();
            let expected = TranscriptGroup::from_entries(&visible, at).unwrap();
            assert_eq!(
                (actual.first, actual.total, actual.entries),
                (expected.first, expected.total, expected.entries)
            );
        }
    }
}

#[test]
fn invalid_recovered_ranges_are_rejected_before_mapping() {
    let mut pending = PendingInputs::with_activity(crate::view::activity::Activity {
        busy: true,
        ..Default::default()
    });
    pending.observe(&event("input", ce::USER_MESSAGE, json!({"text":"same"})));
    pending.cards("input", 1..3);
    assert!(!pending.valid(2));
    assert!(pending.valid(3));
    pending.observe(&event("next", ce::USER_MESSAGE, json!({"text":"same"})));
    pending.cards("next", 2..4);
    assert!(!pending.valid(4));
}
