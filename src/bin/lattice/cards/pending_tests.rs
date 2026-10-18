use super::*;

#[test]
fn releasing_an_earlier_user_message_keeps_the_rendered_top_on_the_later_message() {
    let temp = tempfile::tempdir().unwrap();
    let mut ledger = log(&temp.path().join("reading.ledger"));
    let first = append(&mut ledger, ce::USER_MESSAGE, json!({"text":"A first"}));
    let second = append(
        &mut ledger,
        ce::USER_MESSAGE,
        json!({"text":"B first\nB second\nB third\nB fourth\nB fifth"}),
    );
    let request = append(
        &mut ledger,
        ce::MODEL_CALL_STARTED,
        json!({"input":{"parts":[{"event":second.id}]}}),
    );
    let mut memory = Ui::replayed(&[]);
    let mut paged = Ui::replayed(&[]);
    paged.replay_prefix(&ledger.reader(), 0).unwrap();
    for event in [&first, &second, &request] {
        fold(&mut paged, &mut memory, event);
    }
    let top = |ui: &Ui| {
        let page = crate::terminal_host::transcript::page(ui, ' ', 40, 3).unwrap();
        page.rows[0]
            .row
            .line
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>()
    };
    for ui in [&mut paged, &mut memory] {
        ui.browsing.scroll(
            0,
            Some(lattice::view::TranscriptPosition {
                block: lattice::view::TranscriptBlock::Entries(0),
                line: 1,
                byte: 0,
            }),
            true,
            true,
            None,
        );
        assert!(top(ui).contains("B second"));
    }
    let request = append(
        &mut ledger,
        ce::MODEL_CALL_STARTED,
        json!({"input":{"parts":[{"event":first.id}]}}),
    );
    fold(&mut paged, &mut memory, &request);
    for ui in [&paged, &memory] {
        assert!(
            top(ui).contains("B second"),
            "reading moved to {:?}",
            top(ui)
        );
    }
}

#[test]
fn pending_inputs_keep_identity_attachments_and_order_across_every_recovery_cut() {
    let temp = tempfile::tempdir().unwrap();
    let mut ledger = log(&temp.path().join("pending.ledger"));
    let first = append(
        &mut ledger,
        ce::USER_MESSAGE,
        json!({"text":"same","images":[{"name":"first.png"}]}),
    );
    let tool = append(
        &mut ledger,
        ce::TOOL_EXEC_STARTED,
        json!({"call":"tool","tool":"Read","arguments":{}}),
    );
    let second = append(
        &mut ledger,
        ce::USER_MESSAGE,
        json!({"text":"same","images":[{"name":"second.png"}]}),
    );
    let forwarded = ledger
        .append(
            EventDraft::new(
                ce::USER_MESSAGE,
                &[&first.id],
                json!({"text":"expanded skill body"}),
            ),
            "gate",
        )
        .unwrap();
    let forwarded_twice = ledger
        .append(
            EventDraft::new(
                ce::USER_MESSAGE,
                &[&forwarded.id],
                json!({"text":"expanded again"}),
            ),
            "gate",
        )
        .unwrap();
    let unrelated = ledger
        .append(
            EventDraft::new(
                ce::USER_MESSAGE,
                &[&tool.id],
                json!({"text":"not an input copy"}),
            ),
            "fixture",
        )
        .unwrap();
    let auxiliary = append(
        &mut ledger,
        ce::MODEL_CALL_STARTED,
        json!({"purpose":"condense","input":{"parts":[{"event":forwarded_twice.id},{"event":second.id}]}}),
    );
    let unrelated_request = append(
        &mut ledger,
        ce::MODEL_CALL_STARTED,
        json!({"input":{"parts":[{"event":unrelated.id}]}}),
    );
    let partial = append(
        &mut ledger,
        ce::MODEL_CALL_STARTED,
        json!({"input":{"parts":[{"event":forwarded_twice.id}]}}),
    );
    let repeated = append(
        &mut ledger,
        ce::MODEL_CALL_STARTED,
        json!({"input":[{"event":forwarded_twice.id}]}),
    );
    let final_request = append(
        &mut ledger,
        ce::MODEL_CALL_STARTED,
        json!({"input":[{"event":second.id}]}),
    );
    let events = [
        first,
        tool,
        second,
        forwarded,
        forwarded_twice,
        unrelated,
        auxiliary,
        unrelated_request,
        partial,
        repeated,
        final_request,
    ];
    let reader = ledger.reader();
    let mut memory = Ui::replayed(&[]);
    let mut paged = Ui::replayed(&[]);
    paged.replay_prefix(&reader, 0).unwrap();
    for (index, event) in events.iter().enumerate() {
        if index == 8 {
            for ui in [&mut paged, &mut memory] {
                ui.browsing.scroll(
                    1,
                    Some(lattice::view::TranscriptPosition {
                        block: lattice::view::TranscriptBlock::Entries(0),
                        line: 0,
                        byte: 0,
                    }),
                    true,
                    true,
                    None,
                );
            }
        }
        fold(&mut paged, &mut memory, event);
        if index >= 8 {
            for ui in [&paged, &memory] {
                assert_eq!(
                    ui.browsing.reading_entry(),
                    Some(2),
                    "the same tool remains anchored"
                );
            }
        }
        let entries = materialize(&paged);
        let users: Vec<_> = entries
            .iter()
            .filter_map(|entry| match entry {
                Entry::User(text) => Some(text.as_str()),
                _ => None,
            })
            .collect();
        let expected_count = if index < 8 {
            0
        } else if index < 10 {
            1
        } else {
            2
        };
        assert_eq!(users, vec!["same"; expected_count], "prefix {}", event.seq);
        let images: Vec<_> = entries
            .iter()
            .filter_map(|entry| match entry {
                Entry::Attachment(name) => Some(name.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(
            images,
            [&["first.png", "second.png"][..expected_count]].concat()
        );
        if (2..8).contains(&index) {
            assert_eq!(
                paged.unseen(),
                ["same [image: first.png]", "same [image: second.png]"]
            );
        }
        if (8..10).contains(&index) {
            assert_eq!(paged.unseen(), ["same [image: second.png]"]);
        }
        if index == 10 {
            assert!(paged.unseen().is_empty());
        }
        if expected_count > 0 {
            assert!(
                matches!(entries.first(), Some(Entry::User(_))),
                "released cards keep their original position before work"
            );
        }
        // Cold-prefix and then warm-cache recovery must agree with the live view.
        for _ in 0..2 {
            let mut recovered = Ui::replayed(&[]);
            recovered.replay_prefix(&reader, event.seq).unwrap();
            assert_eq!(materialize(&recovered), entries);
            assert_eq!(recovered.unseen(), paged.unseen());
            // Consume the remainder from this checkpoint, including aliases
            // recorded before the cut but referenced after it.
            for tail in &events[index + 1..] {
                recovered
                    .try_absorb_profiled(tail, SETTLED_TICK, None)
                    .unwrap();
            }
            assert!(recovered.unseen().is_empty());
            assert_eq!(
                materialize(&recovered)
                    .iter()
                    .filter(|entry| matches!(entry, Entry::User(_)))
                    .count(),
                2
            );
        }
    }
    assert_eq!(
        reader.snapshot_end(),
        events.last().unwrap().seq,
        "display does not append events"
    );
}

#[test]
fn local_cards_and_clear_keep_source_boundaries_when_pending_input_becomes_visible() {
    let temp = tempfile::tempdir().unwrap();
    let mut ledger = log(&temp.path().join("local.ledger"));
    let mut paged = Ui::replayed(&[]);
    paged.replay_prefix(&ledger.reader(), 0).unwrap();
    let mut memory = Ui::replayed(&[]);
    let input = append(&mut ledger, ce::USER_MESSAGE, json!({"text":"first"}));
    fold(&mut paged, &mut memory, &input);
    for ui in [&mut paged, &mut memory] {
        ui.push_local_card(Entry::Agent("local".into()));
        ui.browsing.scroll(
            2,
            Some(lattice::view::TranscriptPosition {
                block: lattice::view::TranscriptBlock::Entries(0),
                line: 3,
                byte: 7,
            }),
            true,
            true,
            None,
        );
    }
    assert_eq!(materialize(&paged), vec![Entry::Agent("local".into())]);
    let request = append(
        &mut ledger,
        ce::MODEL_CALL_STARTED,
        json!({"input":{"parts":[{"event":input.id}]}}),
    );
    fold(&mut paged, &mut memory, &request);
    assert_eq!(
        materialize(&paged),
        vec![Entry::User("first".into()), Entry::Agent("local".into())]
    );
    for ui in [&paged, &memory] {
        assert_eq!(
            ui.browsing.position(),
            Some((
                lattice::view::TranscriptPosition {
                    block: lattice::view::TranscriptBlock::Entries(1),
                    line: 3,
                    byte: 7,
                },
                -2
            )),
            "revealing input must not move reading position to another card"
        );
    }
    let pending = append(&mut ledger, ce::USER_MESSAGE, json!({"text":"cleared"}));
    fold(&mut paged, &mut memory, &pending);
    for ui in [&mut paged, &mut memory] {
        ui.clear_cards();
    }
    let request = append(
        &mut ledger,
        ce::MODEL_CALL_STARTED,
        json!({"input":{"parts":[{"event":pending.id}]}}),
    );
    fold(&mut paged, &mut memory, &request);
    assert!(
        materialize(&paged).is_empty(),
        "clear must not resurrect prior cards"
    );
}
