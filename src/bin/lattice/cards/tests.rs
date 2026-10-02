use super::*;
use crate::terminal_host::{Ui, SETTLED_TICK};
use lattice::view::View;
use lattice::{core_events as ce, EventDraft, EventEnvelope, EventLog};
use serde_json::{json, Value};

pub(crate) fn materialize(ui: &Ui) -> Vec<Entry> {
    let mut entries = Vec::new();
    while entries.len() < ui.entry_count() {
        let group = ui.transcript_group(entries.len()).unwrap();
        assert_eq!(group.first, entries.len());
        assert_eq!(group.total, ui.entry_count());
        assert!(!group.entries.is_empty());
        entries.extend(group.entries);
    }
    entries
}

fn append(log: &mut EventLog, kind: &str, payload: Value) -> EventEnvelope {
    log.append(EventDraft::new(kind, &[], payload), "fixture")
        .unwrap()
}

fn log(root: &std::path::Path) -> EventLog {
    let declarations = ce::core_event_decls()
        .into_iter()
        .map(|mut d| {
            d.schema = None;
            d
        })
        .collect();
    EventLog::open_segmented(declarations, "cards-ui", root.to_path_buf(), 4096).unwrap()
}

fn fold(ui: &mut Ui, reference: &mut Ui, event: &EventEnvelope) {
    reference.absorb(event, SETTLED_TICK);
    ui.try_absorb_profiled(event, SETTLED_TICK, None).unwrap();
    assert_eq!(materialize(ui), reference.entries, "event {}", event.seq);
    assert!(
        ui.entries.is_empty(),
        "reader-backed cards must not accumulate"
    );
    assert_eq!(ui.last_tool_running(), reference.last_tool_running());
    assert_eq!(ui.streaming(), reference.streaming());
    assert_eq!(ui.thinking(), reference.thinking());
}

#[test]
fn repeated_frames_do_not_materialize_a_long_disk_backed_work_group() {
    use crate::terminal_host::{draw_ui, fold_render, RenderEvent};
    let temp = tempfile::tempdir().unwrap();
    let mut ledger = log(&temp.path().join("redraw.ledger"));
    for n in 0..260 {
        append(
            &mut ledger,
            ce::TOOL_EXEC_STARTED,
            json!({"call":format!("c-{n}"),"tool":"Run","arguments":{"command":"printf test"}}),
        );
        append(
            &mut ledger,
            ce::TOOL_EXEC_COMPLETED,
            json!({"call":format!("c-{n}"),"tool":"Run","status":"ok","result":{"stdout":"output".repeat(400)}}),
        );
    }
    let reader = ledger.reader();
    let mut ui = Ui::replayed(&[]);
    ui.replay_prefix(&reader, reader.snapshot_end()).unwrap();
    ui.domain.turns.optimistic_activity();
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 30)).unwrap();
    draw_ui(&mut terminal, &mut ui).unwrap();
    let before = reader.memory_stats().unwrap().cache.unwrap();
    for _ in 0..20 {
        ui.tick += 1;
        draw_ui(&mut terminal, &mut ui).unwrap();
    }
    let after = reader.memory_stats().unwrap().cache.unwrap();
    assert_eq!(
        (after.decodes, after.hits),
        (before.decodes, before.hits),
        "unchanged frames must not even fetch cached ledger bodies"
    );
    let groups = ui.transcript_cache.borrow().group_builds();
    for _ in 0..3 {
        fold_render(
            &mut ui,
            RenderEvent::Notice {
                source: "model".into(),
                payload: json!({"phase":"reasoning","chunk":"more "}),
            },
        )
        .unwrap();
        draw_ui(&mut terminal, &mut ui).unwrap();
    }
    assert_eq!(
        ui.transcript_cache.borrow().group_builds(),
        groups,
        "streaming must reuse disk-backed committed groups"
    );
}

#[test]
fn live_output_lands_one_channel_at_a_time_on_both_card_paths() {
    let temp = tempfile::tempdir().unwrap();
    let mut log = log(&temp.path().join("live.ledger"));
    let mut ui = Ui::replayed(&[]);
    ui.replay_prefix(&log.reader(), 0).unwrap();
    let mut reference = Ui::replayed(&[]);
    let event = append(&mut log, ce::USER_MESSAGE, json!({"text":"question"}));
    fold(&mut ui, &mut reference, &event);
    for target in [&mut ui, &mut reference] {
        for payload in [
            json!({"phase":"reasoning","chunk":"thought"}),
            json!({"chunk":"reply"}),
        ] {
            crate::terminal_host::fold_render(
                target,
                crate::terminal_host::RenderEvent::Notice {
                    source: "model".into(),
                    payload,
                },
            )
            .unwrap();
        }
        crate::terminal_host::fold_render(target, crate::terminal_host::RenderEvent::Quiescent)
            .unwrap();
        assert_eq!(target.thinking(), "thought");
        assert_eq!(
            target.streaming(),
            "reply",
            "quiescence does not commit text"
        );
    }
    let event = append(
        &mut log,
        ce::MODEL_CALL_COMPLETED,
        json!({"status":"ok","reasoning":[{"kind":"text","text":"thought"}],"text":"reply"}),
    );
    fold(&mut ui, &mut reference, &event);
    assert!(ui.thinking().is_empty());
    assert_eq!(ui.streaming(), "reply", "the reply has not landed yet");
    let event = append(&mut log, ce::OUTPUT_REPLY, json!({"text":"reply"}));
    fold(&mut ui, &mut reference, &event);
    assert!(ui.streaming().is_empty());
    for target in [&mut ui, &mut reference] {
        for payload in [
            json!({"phase":"reasoning","chunk":"unfinished"}),
            json!({"chunk":"unfinished"}),
        ] {
            crate::terminal_host::fold_render(
                target,
                crate::terminal_host::RenderEvent::Notice {
                    source: "model".into(),
                    payload,
                },
            )
            .unwrap();
        }
    }
    let event = append(&mut log, ce::INTERRUPTED, json!({}));
    fold(&mut ui, &mut reference, &event);
    assert!(ui.thinking().is_empty());
    assert!(ui.streaming().is_empty());
}

#[test]
fn paged_cards_preserve_old_updates_local_errors_clear_and_recovery() {
    let temp = tempfile::tempdir().unwrap();
    let mut log = log(&temp.path().join("cards.ledger"));
    let mut ui = Ui::replayed(&[]);
    ui.replay_prefix(&log.reader(), 0).unwrap();
    let mut reference = Ui::replayed(&[]);
    let event = append(
        &mut log,
        ce::TOOL_EXEC_STARTED,
        json!({"call":"old","tool":"Run","arguments":{"command":"old command"}}),
    );
    fold(&mut ui, &mut reference, &event);
    for index in 0..140 {
        let event = append(
            &mut log,
            ce::USER_MESSAGE,
            json!({"text":format!("line {index}")}),
        );
        fold(&mut ui, &mut reference, &event);
        if index == 65 {
            for text in ["local one", "local two"] {
                ui.push_local_card(Entry::Error(text.into()));
                reference.push_local_card(Entry::Error(text.into()));
            }
        }
    }
    let event = append(
        &mut log,
        ce::TOOL_EXEC_COMPLETED,
        json!({"call":"old","status":"ok","result":{"job":"old","background":true}}),
    );
    fold(&mut ui, &mut reference, &event);
    let event = append(
        &mut log,
        ce::WAKE,
        json!({"source":"background:old","summary":"finished","body":{"job":"old","exit_code":0,"stdout":"old output"}}),
    );
    fold(&mut ui, &mut reference, &event);
    for kind in [ce::TOOL_EXEC_STARTED, ce::USER_MESSAGE] {
        let payload = if kind == ce::USER_MESSAGE {
            json!({"text":"before clear"})
        } else {
            json!({"call":"cleared","tool":"Run","arguments":{"command":"cleared command"}})
        };
        let event = append(&mut log, kind, payload);
        fold(&mut ui, &mut reference, &event);
    }
    ui.clear_cards();
    reference.clear_cards();
    assert_eq!(ui.entry_count(), 0);
    assert!(ui.has_user_card(), "clear is not a new source conversation");
    // An old request no longer has a visible card after clear. Its completion
    // follows the same unmatched-result behavior as the previous in-memory fold.
    let event = append(
        &mut log,
        ce::TOOL_EXEC_COMPLETED,
        json!({"call":"cleared","status":"ok","result":{"stdout":"after clear"}}),
    );
    fold(&mut ui, &mut reference, &event);
    ui.push_local_card(Entry::Error("after clear notice".into()));
    reference.push_local_card(Entry::Error("after clear notice".into()));
    for index in 0..260 {
        let event = append(
            &mut log,
            ce::USER_MESSAGE,
            json!({"text":format!("tail {index}")}),
        );
        fold(&mut ui, &mut reference, &event);
    }
    let mut restored = Ui::replayed(&[]);
    restored
        .replay_prefix(&log.reader(), log.reader().snapshot_end())
        .unwrap();
    let mut canonical = Vec::new();
    log.reader()
        .visit_prefix(log.reader().snapshot_end(), |events| {
            for event in events {
                lattice::view::ingest(&mut canonical, event);
            }
            Ok(())
        })
        .unwrap();
    assert_eq!(
        materialize(&restored),
        canonical,
        "local clear must not overwrite the canonical checkpoint"
    );
}

#[test]
fn viewport_uses_paged_cards_and_source_read_failure_is_not_empty_success() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("cards.ledger");
    let mut log = log(&root);
    for index in 0..150 {
        append(
            &mut log,
            ce::OUTPUT_REPLY,
            json!({"text":format!("answer {index}")}),
        );
    }
    let through = log.reader().snapshot_end();
    let mut ui = Ui::replayed(&[]);
    ui.replay_prefix(&log.reader(), through).unwrap();
    assert!(ui.entries.is_empty());
    assert_eq!(ui.entry_count(), 150);
    let terminal = ratatui::backend::TestBackend::new(60, 20);
    let mut terminal = ratatui::Terminal::new(terminal).unwrap();
    crate::terminal_host::draw_ui(&mut terminal, &mut ui).unwrap();
    let text = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(
        text.contains("answer 149"),
        "the real tail must be rendered without resident entries"
    );
    // Clear reader caches by opening a second reader, then remove source access.
    drop(ui);
    drop(log);
    let log = log_at(&root);
    let mut ui = Ui::replayed(&[]);
    ui.bind_cards(&log.reader(), through).unwrap();
    let volume = std::fs::read_dir(&root)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "jsonl"))
        .min()
        .unwrap();
    let bytes = std::fs::read(&volume).unwrap();
    std::fs::write(&volume, b"invalid original\n").unwrap();
    let failed = ui.transcript_group(0);
    assert!(
        failed.is_err(),
        "source damage must not become an empty transcript"
    );
    std::fs::write(volume, bytes).unwrap();
}

fn log_at(root: &std::path::Path) -> EventLog {
    log(root)
}
