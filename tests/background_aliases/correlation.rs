use super::*;
use lattice::view::{history::History, Entry, ToolStatus};

fn start(log: &mut EventLog, command: &str) -> EventEnvelope {
    let model = append(
        log,
        ce::MODEL_CALL_COMPLETED,
        &[],
        json!({"status":"ok","toolCalls":[{"id":"same","tool":"Run","arguments":{"command":command}}]}),
    );
    let root = append(
        log,
        ce::TOOL_EXEC_STARTED,
        &[&model.id],
        json!({"call":"same","tool":"Run","arguments":{"command":command}}),
    );
    append(
        log,
        ce::TOOL_EXEC_STARTED,
        &[&root.id],
        root.payload.clone(),
    )
}
fn receipt(log: &mut EventLog, start: &EventEnvelope) -> EventEnvelope {
    append(
        log,
        ce::TOOL_EXEC_COMPLETED,
        &[&start.id],
        json!({"call":"same","status":"ok","result":{"background":true,"job":"same"}}),
    )
}
fn wake(log: &mut EventLog, start: &EventEnvelope, code: i64, text: &str) -> EventEnvelope {
    append(
        log,
        ce::WAKE,
        &[&start.id],
        json!({"source":"background:same","body":{"job":"same","exit_code":code,"stdout":text}}),
    )
}

#[test]
fn late_old_wake_updates_its_own_command_not_the_new_same_job() {
    reused_command(false);
}
#[test]
fn early_wake_then_late_receipt_cannot_settle_a_new_same_call() {
    reused_command(true);
}

fn reused_command(early_wake: bool) {
    let dir = tempfile::tempdir().unwrap();
    let mut log = EventLog::open_segmented(
        ce::core_event_decls(),
        "owner",
        dir.path().join("correlation.ledger"),
        4096,
    )
    .unwrap();
    let old = start(&mut log, "old command");
    if early_wake {
        wake(&mut log, &old, 9, "OLD OUTPUT");
    } else {
        receipt(&mut log, &old);
    }
    let new = start(&mut log, "new command");
    if early_wake {
        receipt(&mut log, &old);
    } else {
        receipt(&mut log, &new);
        wake(&mut log, &old, 9, "OLD OUTPUT");
    }
    let reader = log.reader();
    for _ in 0..2 {
        let mut history = History::recover(reader.clone(), reader.snapshot_end()).unwrap();
        let entries = history.load(0..history.len()).unwrap();
        assert_eq!(
            entries.len(),
            2,
            "forwarded requests still create one card each"
        );
        let [Entry::Tool(old_card), Entry::Tool(new_card)] = entries.as_slice() else {
            panic!("command cards missing")
        };
        assert_eq!(old_card.args["command"], "old command");
        assert_eq!(new_card.args["command"], "new command");
        assert_eq!(old_card.call.as_deref(), Some("same"));
        assert_eq!(new_card.call.as_deref(), Some("same"));
        assert_eq!(old_card.status, ToolStatus::Failed);
        assert!(old_card.output.join("\n").contains("OLD OUTPUT"));
        assert_eq!(
            new_card.status,
            if early_wake {
                ToolStatus::Running
            } else {
                ToolStatus::Background
            },
            "old evidence must not settle the new invocation"
        );
        assert!(!new_card.output.join("\n").contains("OLD OUTPUT"));
        history.save().unwrap();
    }
    if early_wake {
        receipt(&mut log, &new);
    }
    let new_wake = wake(&mut log, &new, 0, "NEW OUTPUT");
    let projection = Projection::recover(&reader, reader.snapshot_end()).unwrap();
    let new_identity = identity_for_event(&reader, &new_wake).unwrap().unwrap();
    let new_alias = projection.alias(&new_identity).unwrap();
    for _ in 0..2 {
        let mut history = History::recover(reader.clone(), reader.snapshot_end()).unwrap();
        let entries = history.load(0..history.len()).unwrap();
        let [Entry::Tool(old_card), Entry::Tool(new_card)] = entries.as_slice() else {
            panic!("command cards missing")
        };
        assert_eq!(old_card.status, ToolStatus::Failed);
        assert!(old_card.output.join("\n").contains("OLD OUTPUT"));
        assert!(!old_card.output.join("\n").contains(new_alias));
        assert_eq!(new_card.status, ToolStatus::Ok);
        let output = new_card.output.join("\n");
        assert!(output.contains("NEW OUTPUT") && output.contains(new_alias));
        assert!(!output.contains("OLD OUTPUT"));
        history.save().unwrap();
    }
}
