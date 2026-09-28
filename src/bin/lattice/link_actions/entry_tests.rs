//! Link completion notices belong to one frontend instance, never the ledger.
use super::*;
use std::sync::mpsc::{channel, Sender};

fn pending(ui: &mut Ui) -> Sender<Result<(), String>> {
    let (sender, receiver) = channel();
    ui.links.fixture_results().push(receiver);
    sender
}

#[test]
fn link_pending_success_and_disconnect_do_not_replace_the_current_notice() {
    let mut ui = Ui::replayed(&[]);
    ui.flash = Some("existing notice".into());
    let waiting = pending(&mut ui);
    let succeeded = pending(&mut ui);
    let disconnected = pending(&mut ui);
    succeeded.send(Ok(())).unwrap();
    drop(disconnected);
    assert!(!drain_link_open_results(&mut ui));
    assert_eq!(ui.links.fixture_results().len(), 1);
    assert_eq!(ui.flash.as_deref(), Some("existing notice"));
    assert!(ui.draft.editor().is_empty());
    assert_eq!(ui.entry_count(), 0);
    assert!(!ui.busy());
    waiting.send(Err("late failure".into())).unwrap();
    assert!(drain_link_open_results(&mut ui));
    assert_eq!(ui.flash.as_deref(), Some("late failure"));
    assert!(ui.links.fixture_results().is_empty());
    assert!(!drain_link_open_results(&mut ui));
}

#[test]
fn link_failures_preserve_queue_order_and_do_not_cross_frontends() {
    let mut first = Ui::replayed(&[]);
    let mut second = Ui::replayed(&[]);
    let first_early = pending(&mut first);
    let second_failure = pending(&mut second);
    let first_late = pending(&mut first);
    // Arrival order differs from launch order; draining retains launch order.
    first_late
        .send(Err("first frontend last launch".into()))
        .unwrap();
    second_failure
        .send(Err("second frontend failure".into()))
        .unwrap();
    first_early
        .send(Err("first frontend first launch".into()))
        .unwrap();
    second.flash = Some("second frontend unchanged".into());
    assert!(drain_link_open_results(&mut first));
    assert_eq!(first.flash.as_deref(), Some("first frontend last launch"));
    assert_eq!(second.flash.as_deref(), Some("second frontend unchanged"));
    assert_eq!(second.links.fixture_results().len(), 1);
    assert!(drain_link_open_results(&mut second));
    assert_eq!(second.flash.as_deref(), Some("second frontend failure"));
    assert_eq!(first.flash.as_deref(), Some("first frontend last launch"));
    assert_eq!(first.entry_count(), 0);
    assert_eq!(second.entry_count(), 0);
}

#[test]
fn invalid_links_report_locally_without_starting_a_launcher() {
    let mut ui = Ui::replayed(&[]);
    for url in [
        "file:///tmp/private",
        "https://user:secret@example.com/",
        "--help",
    ] {
        open_link(&mut ui, url);
        assert_eq!(
            ui.flash.as_deref(),
            Some("Only HTTP(S) links without credentials can be opened")
        );
        assert!(ui.links.fixture_results().is_empty());
        assert_eq!(ui.entry_count(), 0);
        assert!(!ui.busy());
    }
}
