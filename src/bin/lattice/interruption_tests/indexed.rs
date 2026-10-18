use super::*;
use lattice::{EventDraft, EventLog};
use std::collections::HashMap;

#[test]
fn interruption_status_survives_indexed_recovery_at_every_boundary_without_saving_fragments() {
    for split in 0..=5 {
        let temp = tempfile::tempdir().unwrap();
        let mut log = EventLog::open_segmented(
            core_events::core_event_decls(),
            "interruption",
            temp.path().join("test.ledger"),
            4096,
        )
        .unwrap();
        let mut ids = HashMap::new();
        let mut events = Vec::new();
        for original in interrupted("deadline") {
            let causes: Vec<&str> = original
                .causes
                .iter()
                .map(|id| ids.get(id).map(String::as_str).unwrap())
                .collect();
            let mut draft = EventDraft::new(&original.event_type, &causes, original.payload);
            draft.reason = Some("Synthetic test outcome".into());
            let event = log.append(draft, "fixture").unwrap();
            ids.insert(original.id, event.id.clone());
            events.push(event);
        }
        let reader = log.reader();
        let mut cold = Ui::replayed(&[]);
        cold.replay_prefix(&reader, split as u64).unwrap();
        let mut live = Ui::replayed(&[]);
        live.replay_prefix(&reader, split as u64).unwrap();
        assert_eq!(live.recovery.as_ref().unwrap().replayed, 0);
        if split == 2 {
            live.live_output
                .seed_reply("Ephemeral partial answer".into());
        }
        for event in &events[split..] {
            live.try_absorb_profiled(event, event.seq as usize, None)
                .unwrap();
        }
        let screen = frame(&mut live);
        assert!(screen.contains("Timed out"), "split {split}: {screen}");
        assert!(!screen.contains("[interrupted]"), "split {split}: {screen}");
        assert_eq!(
            screen.matches("Ephemeral partial answer").count(),
            usize::from(split == 2)
        );
        assert_eq!(
            reader.snapshot_end(),
            5,
            "display must not append audit events"
        );
        let mut restored = Ui::replayed(&[]);
        restored.replay_prefix(&reader, 5).unwrap();
        let screen = frame(&mut restored);
        assert!(screen.contains("Timed out"), "split {split}: {screen}");
        assert!(
            !screen.contains("Ephemeral partial answer"),
            "fragments must not enter recovery checkpoints"
        );
        assert!(!screen.contains("[interrupted]"));
        let mut warm = Ui::replayed(&[]);
        warm.replay_prefix(&reader, 5).unwrap();
        assert_eq!(warm.recovery.as_ref().unwrap().replayed, 0);
        assert_eq!(warm.turn_status(), "Timed out");
    }
}
