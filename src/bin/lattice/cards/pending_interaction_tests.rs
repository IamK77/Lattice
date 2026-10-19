use super::*;

#[test]
fn idle_input_is_visible_before_request_echo_and_busy_input_waits_for_inclusion() {
    let temp = tempfile::tempdir().unwrap();
    let mut ledger = log(&temp.path().join("interaction.ledger"));
    let mut memory = Ui::replayed(&[]);
    let mut paged = Ui::replayed(&[]);
    paged.replay_prefix(&ledger.reader(), 0).unwrap();
    // Submission optimistically marks the terminal busy before the ledger echo.
    // That local flag must not misclassify an idle submission as queued input.
    for ui in [&mut paged, &mut memory] {
        ui.domain.turns.submitted_input();
        assert!(ui.busy());
    }
    let first = append(&mut ledger, ce::USER_MESSAGE, json!({"text":"idle"}));
    fold(&mut paged, &mut memory, &first);
    for ui in [&paged, &memory] {
        assert_eq!(materialize(ui), vec![Entry::User("idle".into())]);
        assert!(ui.unseen().is_empty());
    }
    let busy = append(&mut ledger, ce::USER_MESSAGE, json!({"text":"busy"}));
    fold(&mut paged, &mut memory, &busy);
    let unrelated = append(
        &mut ledger,
        ce::MODEL_CALL_STARTED,
        json!({"input":{"parts":[{"event":first.id}]}}),
    );
    fold(&mut paged, &mut memory, &unrelated);
    for ui in [&paged, &memory] {
        assert_eq!(ui.unseen(), ["busy"]);
        assert_eq!(materialize(ui), vec![Entry::User("idle".into())]);
    }
    let included = append(
        &mut ledger,
        ce::MODEL_CALL_STARTED,
        json!({"input":{"parts":[{"event":busy.id}]}}),
    );
    fold(&mut paged, &mut memory, &included);
    for ui in [&paged, &memory] {
        assert!(ui.unseen().is_empty());
        assert_eq!(
            materialize(ui),
            vec![Entry::User("idle".into()), Entry::User("busy".into())]
        );
        assert!(
            ui.browsing.position().is_none(),
            "bottom-follow must stay unanchored"
        );
        let frame = crate::terminal_host::transcript::page(ui, ' ', 40, 1).unwrap();
        assert!(frame
            .rows
            .last()
            .unwrap()
            .row
            .line
            .to_string()
            .contains("busy"));
    }
    for through in 1..=included.seq {
        for _ in 0..2 {
            let mut recovered = Ui::replayed(&[]);
            recovered.replay_prefix(&ledger.reader(), through).unwrap();
            assert!(
                matches!(materialize(&recovered).first(), Some(Entry::User(text)) if text == "idle")
            );
            assert_eq!(
                recovered.unseen().is_empty(),
                through == first.seq || through == included.seq
            );
        }
    }
}

#[test]
fn clearing_while_busy_preserves_pending_visibility_even_after_rebuild() {
    let temp = tempfile::tempdir().unwrap();
    let mut ledger = log(&temp.path().join("clear-busy.ledger"));
    let busy = append(
        &mut ledger,
        ce::MODEL_CALL_STARTED,
        json!({"input":{"parts":[]}}),
    );
    let mut memory = Ui::replayed(std::slice::from_ref(&busy));
    let mut paged = Ui::replayed(&[]);
    paged.replay_prefix(&ledger.reader(), busy.seq).unwrap();
    let full = History::recover(ledger.reader(), busy.seq).unwrap();
    let mut cleared = full.empty_tail().unwrap();
    for ui in [&mut paged, &mut memory] {
        ui.clear_cards();
    }
    let input = append(&mut ledger, ce::USER_MESSAGE, json!({"text":"after clear"}));
    fold(&mut paged, &mut memory, &input);
    for ui in [&paged, &memory] {
        assert!(materialize(ui).is_empty());
        assert_eq!(ui.unseen(), ["after clear"]);
    }
    cleared.catch_up(input.seq).unwrap();
    assert_eq!(cleared.visible_len(), 0);
    let mut rebuilt = cleared
        .rebuild(input.seq, "test reconstruction".into())
        .unwrap();
    assert_eq!(rebuilt.visible_len(), 0);
    let included = append(
        &mut ledger,
        ce::MODEL_CALL_STARTED,
        json!({"input":{"parts":[{"event":input.id}]}}),
    );
    fold(&mut paged, &mut memory, &included);
    rebuilt.catch_up(included.seq).unwrap();
    assert_eq!(rebuilt.visible_len(), 1);
    assert_eq!(materialize(&paged), vec![Entry::User("after clear".into())]);
}

#[test]
fn submission_after_completion_is_immediate_live_and_after_warm_recovery() {
    let temp = tempfile::tempdir().unwrap();
    let mut ledger = log(&temp.path().join("idle-again.ledger"));
    let first = append(&mut ledger, ce::USER_MESSAGE, json!({"text":"first"}));
    let request = append(
        &mut ledger,
        ce::MODEL_CALL_STARTED,
        json!({"input":{"parts":[{"event":first.id}]}}),
    );
    let outcome = ledger
        .append(
            EventDraft::new(
                ce::MODEL_CALL_COMPLETED,
                &[&request.id],
                json!({"status":"ok"}),
            ),
            "fixture",
        )
        .unwrap();
    let ending = ledger
        .append(
            EventDraft::new(ce::TURN_COMPLETED, &[&outcome.id], json!({})),
            "fixture",
        )
        .unwrap();
    let mut memory = Ui::replayed(&[first, request, outcome, ending.clone()]);
    let mut paged = Ui::replayed(&[]);
    paged.replay_prefix(&ledger.reader(), ending.seq).unwrap();
    // Load the same prefix again to exercise the stored empty pending state.
    paged.replay_prefix(&ledger.reader(), ending.seq).unwrap();
    let next = append(&mut ledger, ce::USER_MESSAGE, json!({"text":"next idle"}));
    fold(&mut paged, &mut memory, &next);
    for ui in [&paged, &memory] {
        assert!(ui.unseen().is_empty());
        assert!(matches!(materialize(ui).last(), Some(Entry::User(text)) if text == "next idle"));
    }
}

#[test]
fn revealing_input_keeps_a_tool_output_line_after_splitting_work() {
    for target in ["before", "after"] {
        check_tool_position_after_reveal(target);
    }
}

fn check_tool_position_after_reveal(target: &str) {
    let temp = tempfile::tempdir().unwrap();
    let mut ledger = log(&temp.path().join("reading-work.ledger"));
    let busy = append(
        &mut ledger,
        ce::MODEL_CALL_STARTED,
        json!({"input":{"parts":[]}}),
    );
    let before = append(
        &mut ledger,
        ce::TOOL_EXEC_STARTED,
        json!({"call":"before", "tool":"Read", "arguments":{}}),
    );
    let input = append(
        &mut ledger,
        ce::USER_MESSAGE,
        json!({"text":"queued input"}),
    );
    let after = append(
        &mut ledger,
        ce::TOOL_EXEC_STARTED,
        json!({"call":"after", "tool":"Run", "arguments":{"command":"example"}}),
    );
    let output = append(
        &mut ledger,
        ce::TOOL_EXEC_COMPLETED,
        json!({"call":target, "tool":if target == "before" { "Read" } else { "Run" }, "status":"ok", "result":{"stdout":"line one\nTARGET_LINE\nline three\nline four\nline five"}}),
    );
    let reply = append(
        &mut ledger,
        ce::OUTPUT_REPLY,
        json!({"text":"Tail one\n\nTail two\n\nTail three\n\nTail four"}),
    );
    let mut memory = Ui::replayed(&[]);
    let mut paged = Ui::replayed(&[]);
    paged.replay_prefix(&ledger.reader(), 0).unwrap();
    for event in [&busy, &before, &input, &after, &output, &reply] {
        fold(&mut paged, &mut memory, event);
    }
    let top = |ui: &Ui| {
        crate::terminal_host::transcript::page(ui, ' ', 60, 2)
            .unwrap()
            .rows[0]
            .row
            .line
            .to_string()
    };
    for ui in [&mut paged, &mut memory] {
        ui.browsing.expand("before".into());
        ui.browsing.expand("after".into());
        let frame = crate::terminal_host::transcript::page(ui, ' ', 60, 100).unwrap();
        let position = frame
            .rows
            .iter()
            .find(|row| row.row.line.to_string().contains("TARGET_LINE"))
            .expect("target output is initially rendered")
            .position;
        ui.browsing.scroll(0, Some(position), true, true, None);
        assert!(top(ui).contains("TARGET_LINE"));
    }
    let request = append(
        &mut ledger,
        ce::MODEL_CALL_STARTED,
        json!({"input":{"parts":[{"event":input.id}]}}),
    );
    fold(&mut paged, &mut memory, &request);
    let observed = [top(&paged), top(&memory)];
    assert!(
        observed.iter().all(|top| top.contains("TARGET_LINE")),
        "tool reading moved (paged, memory): {observed:?}"
    );
}
