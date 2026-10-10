use super::*;

// Keep a join-owning guard separate from Tabs' borrowed main seat.
struct Live(Option<Session>);
impl Live {
    fn get(&self) -> &Session {
        self.0.as_ref().unwrap()
    }
}
impl Drop for Live {
    fn drop(&mut self) {
        if let Some(session) = self.0.take() {
            session.shutdown();
        }
    }
}

fn record(main: &Session) -> Record {
    Record {
        file: "fixture.ledger".into(),
        parent: 0,
        settings: None,
        origin: StreamRef {
            stream: main.log_reader().stream().into(),
            event: main
                .log_reader()
                .scan_back(|event, _| Ok(Some(event.id.clone())))
                .unwrap()
                .unwrap(),
        },
    }
}

#[test]
fn index_rejection_retires_without_admitting_the_first_question() {
    let (_hooks, collected) = observe();
    let root = tempfile::tempdir().unwrap();
    let main = Live(Some(fixture().0));
    let mut ui = Ui::replayed(&[]);
    let cfg = super::super::super::tests::config();
    let mut tabs = Tabs::interactive(
        main.get(),
        cfg,
        &root.path().join("main.jsonl"),
        &mut ui,
        false,
    );
    std::fs::create_dir_all(tabs.directory.parent().unwrap()).unwrap();
    std::fs::write(&tabs.directory, b"index parent is not a directory").unwrap();
    let (session, dropped) = fixture();
    let reader = session.log_reader();
    let (_, receiver) = mpsc::channel();
    let pending = Pending {
        record: record(main.get()),
        question: Some("must not be sent".into()),
        focus_epoch: 0,
        receiver,
    };
    tabs.admit_launch(&mut ui, pending, Ok(blank(session)));
    let counts = (tabs.seats.len(), tabs.records.len(), tabs.retired.len());
    let diagnostic = ui.flash.clone();
    let unchanged = std::fs::read(&tabs.directory).unwrap();
    let closed = tabs.release_sessions().finish();
    drop(tabs);
    assert_eq!(counts, (1, 0, 1));
    assert!(diagnostic.unwrap().contains("The index was not changed"));
    assert_eq!(unchanged, b"index parent is not a directory");
    assert_eq!(closed.sessions.len(), 1);
    assert!(closed.errors.is_empty());
    assert_eq!(closed.sessions[0]["kernel"]["lingering"], json!([]));
    assert!(
        collected.try_iter().next().is_none(),
        "retirement, not blocking Ready collection, owns this exit"
    );
    dropped.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(!reader.replay(1).unwrap().iter().any(|event| matches!(
        event.event_type.as_str(),
        core_events::USER_MESSAGE | core_events::MODEL_CALL_STARTED
    )));
}

struct Release(Option<mpsc::Sender<()>>);
impl Release {
    fn now(&mut self) {
        if let Some(sender) = self.0.take() {
            let _ = sender.send(());
        }
    }
}
impl Drop for Release {
    fn drop(&mut self) {
        self.now();
    }
}

#[test]
fn shutdown_with_pending_builder_rejects_and_collects_its_late_ready() {
    let root = tempfile::tempdir().unwrap();
    let main = Live(Some(fixture().0));
    let mut ui = Ui::replayed(&[]);
    let cfg = super::super::super::tests::config();
    let mut tabs = Tabs::interactive(
        main.get(),
        cfg,
        &root.path().join("main.jsonl"),
        &mut ui,
        false,
    );
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let (collected_tx, collected_rx) = mpsc::channel();
    let (child_tx, child_rx) = mpsc::channel();
    let mut release = Release(Some(release_tx));
    tabs.launch(
        record(main.get()),
        Some("must not be sent".into()),
        move || {
            HOOKS.with(|hooks| hooks.borrow_mut().collected = Some(collected_tx));
            entered_tx.send(()).unwrap();
            release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            let (session, dropped) = fixture();
            let reader = session.log_reader();
            let ready = blank(session);
            child_tx.send((dropped, reader)).unwrap();
            Ok(ready)
        },
    )
    .unwrap();
    entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    let mut shutdown = tabs.release_sessions();
    drop(shutdown.pending.take());
    release.now();
    let closed = shutdown.finish();
    let admitted = tabs.seats.len();
    let index_created = tabs.directory.join("tabs.json").exists();
    drop(tabs);
    let (dropped, reader) = child_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(closed.errors.is_empty());
    assert_eq!(admitted, 1);
    assert!(!index_created);
    assert_eq!(
        collected_rx.recv_timeout(Duration::from_secs(5)).unwrap(),
        Ok(vec![])
    );
    dropped.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(!reader.replay(1).unwrap().iter().any(|event| matches!(
        event.event_type.as_str(),
        core_events::USER_MESSAGE | core_events::MODEL_CALL_STARTED
    )));
}
