//! Slow startup lives outside the terminal loop. No question is sent here:
//! the host must commit its index before admitting the first input.
use super::*;

pub(super) struct Prepared {
    pub record: Record,
    pub cfg: PresetConfig,
    pub reader: lattice::kernel::log::LogReader,
    pub directory: PathBuf,
    pub workspace: String,
    pub bar: Vec<String>,
    pub host_services: bool,
    pub active_streams: Vec<String>,
}

pub(super) struct Ready {
    pub session: Option<Session>,
    pub ui: Ui,
    pub parent: usize,
}

impl Drop for Ready {
    fn drop(&mut self) {
        if let Some(problem) = self.collect() {
            report(&problem);
        }
    }
}

impl Ready {
    /// Own the returned runtime before any fallible UI recovery. The initializer
    /// can borrow the runtime, but cannot take it away from this unpublished owner.
    fn initialize(
        session: Session,
        ui: Ui,
        parent: usize,
        initialize: impl FnOnce(&Session, &mut Ui) -> Result<(), String>,
    ) -> Result<Self, String> {
        let mut ready = Self {
            session: Some(session),
            ui,
            parent,
        };
        if let Err(primary) = initialize(
            ready.session.as_ref().expect("owned startup"),
            &mut ready.ui,
        ) {
            return Err(with_cleanup(primary, ready.collect()));
        }
        Ok(ready)
    }

    fn collect(&mut self) -> Option<String> {
        let session = self.session.take()?;
        session.request_shutdown();
        let result = session.finish_shutdown();
        #[cfg(test)]
        cleanup_tests::observe_collection(&result);
        cleanup_problem(result.map(|closed| closed.kernel.lingering))
    }

    pub fn seat<'a>(mut self) -> Seat<'a> {
        Seat {
            session: SeatSession::Owned(self.session.take().map(Box::new)),
            ui: Some(std::mem::replace(&mut self.ui, Ui::replayed(&[]))),
            parent: Some(self.parent),
        }
    }
}

fn cleanup_problem(result: Result<Vec<String>, String>) -> Option<String> {
    match result {
        Err(error) => Some(format!(
            "Side startup cleanup could not join its session: {error}"
        )),
        Ok(lingering) if !lingering.is_empty() => Some(format!(
            "Side startup session joined, but components are still running: {}",
            lingering.join(", ")
        )),
        Ok(_) => None,
    }
}

fn with_cleanup(primary: String, cleanup: Option<String>) -> String {
    match cleanup {
        Some(problem) => format!("{primary}\n{problem}"),
        None => primary,
    }
}

fn report_to(writer: &mut dyn std::io::Write, message: &str) {
    // Diagnostics are best effort, including during unwinding. A failed write
    // must not replace a primary error or turn a cleanup warning into a panic.
    let _ = writeln!(writer, "{message}");
}

fn report(message: &str) {
    #[cfg(test)]
    if cleanup_tests::report_if_configured(message) {
        return;
    }
    report_to(&mut std::io::stderr().lock(), message);
}

fn deliver(sender: std::sync::mpsc::Sender<Result<Ready, String>>, result: Result<Ready, String>) {
    if let Err(rejected) = sender.send(result) {
        match rejected.0 {
            Err(error) => report(&format!("Side startup result receiver closed: {error}")),
            Ok(ready) => drop(ready),
        }
    }
}

impl Prepared {
    pub fn build(self) -> Result<Ready, String> {
        let Self {
            record,
            mut cfg,
            reader,
            directory,
            workspace,
            bar,
            host_services,
            active_streams,
        } = self;
        if reader.stream() != record.origin.stream
            || !reader
                .contains_id(&record.origin.event)
                .map_err(|error| error.to_string())?
        {
            return Err(
                "The side conversation's parent provenance does not match its ledger".into(),
            );
        }
        let path = directory.join(&record.file);
        let metadata = std::fs::symlink_metadata(&path).map_err(|e| e.to_string())?;
        if !metadata.is_file() && !metadata.is_dir() {
            return Err(
                "A side conversation ledger must be a regular file or directory, not a link".into(),
            );
        }
        if lattice::EventLog::stream_of(&path).is_some_and(|id| active_streams.contains(&id)) {
            return Err("A side conversation cannot reopen another active tab's ledger".into());
        }
        let settings_note = Settings::restore(record.settings.as_ref(), &path, &mut cfg)?;
        let running = preset::running_entry(&cfg);
        let effective_window = running.context_window().unwrap_or(cfg.context_window);
        let models = crate::terminal_host::model_catalog::from_config(&cfg);
        let effort = EffortView {
            rungs: running.effort_rungs(),
            now: cfg
                .thinking
                .as_ref()
                .map(|v| v.as_str().unwrap_or("off").to_string()),
        };
        let mut ui = Ui::replayed(&[]);
        ui.domain.title = format!("{} · {}", running.model, workspace);
        ui.workspace = workspace;
        ui.bar = bar;
        ui.documents = Some(lattice::contracts::document::documents_dir(&path));
        ui.domain.expert_dir = Some(directory.join("experts"));
        let foreign = std::sync::Arc::new([(reader.stream().to_string(), reader)].into());
        let expert_cfg = cfg.clone();
        let expert_path = directory.join("experts");
        let build =
            move |tx| crate::terminal_host::session_build::observing(tx, &cfg, path, foreign);
        let session = if host_services {
            Session::spawn_with_subagents(
                "ui",
                Some(lattice::workshop::Workshop::standard()),
                Some((
                    String::new(),
                    Box::new(move || {
                        preset::expert_host(&expert_cfg).with_ledger_path(move |stream| {
                            std::fs::create_dir_all(&expert_path).ok()?;
                            Some(lattice::ledgers::named_path(&expert_path, stream))
                        })
                    }),
                )),
                build,
            )
        } else {
            Session::spawn("ui", build)
        }?;
        Ready::initialize(session, ui, record.parent, |session, ui| {
            ui.parts = session.initial_parts().to_vec();
            let reader = session.log_reader();
            ui.domain.stream_id = reader.stream().to_string();
            let through = reader.snapshot_end();
            ui.replay_prefix(&reader, through)
                .map_err(|e| e.to_string())?;
            let has_user_message = ui.has_user_card();
            // Only the configuration used to start the actual session owns the
            // current controls, not historical UI changes encountered in replay.
            ui.domain.model = crate::terminal_host::ModelState::new(
                models,
                running,
                effort,
                Some(effective_window),
            );
            ui.flash = settings_note;
            if !has_user_message {
                ui.initial_origin = Some(record.origin.clone());
            }
            Ok(())
        })
    }
}

#[cfg(test)]
#[path = "launch/tests.rs"]
mod tests;

#[cfg(test)]
#[path = "launch/cleanup_tests.rs"]
mod cleanup_tests;

pub(super) struct Pending {
    pub record: Record,
    pub question: Option<String>, // None = restore, Some = a newly created side
    pub focus_epoch: u64,
    pub receiver: std::sync::mpsc::Receiver<Result<Ready, String>>,
}

#[derive(Default)]
pub(crate) struct Shutdown {
    pub(super) sessions: Vec<Session>,
    pub(super) pending: Option<Pending>,
    pub(super) workers: Vec<std::thread::JoinHandle<()>>,
}

impl<'a> Tabs<'a> {
    pub(super) fn start_launch(
        &mut self,
        prepared: Prepared,
        question: Option<String>,
    ) -> Result<(), String> {
        let record = prepared.record.clone();
        self.launch(record, question, move || prepared.build())
    }

    pub(super) fn launch(
        &mut self,
        record: Record,
        question: Option<String>,
        build: impl FnOnce() -> Result<Ready, String> + Send + 'static,
    ) -> Result<(), String> {
        let (sender, receiver) = std::sync::mpsc::channel();
        let worker = std::thread::Builder::new()
            .name("side-startup".into())
            .spawn(move || {
                // A rejected Ready still owns its runtime. A rejected error is
                // reported locally rather than silently losing the diagnosis.
                deliver(sender, build());
            })
            .map_err(|error| error.to_string())?;
        self.workers.push(worker);
        self.pending = Some(Pending {
            record,
            question,
            focus_epoch: self.focus_epoch,
            receiver,
        });
        Ok(())
    }

    pub(super) fn poll_launch(&mut self, active: &mut Ui) -> bool {
        let mut changed = false;
        if let Some(pending) = self.pending.take() {
            match pending.receiver.try_recv() {
                Ok(result) => {
                    self.admit_launch(active, pending, result);
                    changed = true;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => self.pending = Some(pending),
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.admit_launch(
                        active,
                        pending,
                        Err("Side startup worker stopped without a result".into()),
                    );
                    changed = true;
                }
            }
        }
        if self.pending.is_none() && self.restore_error.is_none() {
            if let Some(record) = self.restore_queue.pop_front() {
                let parent_ui = if record.parent == self.active {
                    &*active
                } else {
                    self.seats[record.parent]
                        .ui
                        .as_ref()
                        .expect("restored parent")
                };
                let result = self
                    .prepare(&record, parent_ui)
                    .and_then(|prepared| self.start_launch(prepared, None));
                if let Err(error) = result {
                    self.restore_error = Some(error.clone());
                    active.flash = Some(format!("Could not restore side conversations: {error}. Their index was not changed."));
                }
                changed = true;
            }
        }
        changed
    }

    pub(super) fn admit_launch(
        &mut self,
        active: &mut Ui,
        pending: Pending,
        result: Result<Ready, String>,
    ) {
        let restoring = pending.question.is_none();
        let result = result.and_then(|mut ready| {
            let mut records = self.records.clone();
            records.push(pending.record);
            if !restoring {
                if let Err(error) = self.save(&records) {
                    if let Some(session) = ready.session.take() {
                        session.request_shutdown();
                        self.retired.push(session);
                    }
                    return Err(error);
                }
            }
            self.records = records;
            let mut seat = ready.seat();
            if let Some(question) = pending.question.filter(|q| !q.trim().is_empty()) {
                let ui = seat.ui.as_mut().expect("new side UI");
                ui.domain.turns.activate();
                seat.session.get().send_with_origin(
                    &question,
                    Vec::new(),
                    ui.initial_origin.take(),
                );
            }
            self.seats.push(seat);
            if !restoring && pending.focus_epoch == self.focus_epoch {
                self.select(active, self.seats.len() - 1)?;
            }
            Ok(())
        });
        if let Err(error) = result {
            if restoring {
                self.restore_error = Some(error.clone());
            }
            active.flash = Some(format!(
                "Could not {} side conversation: {error}. The index was not changed.",
                if restoring { "restore" } else { "open" }
            ));
        }
    }

    /// Headless actions explicitly wait for startup; the terminal never calls this.
    pub(crate) fn wait_startups(&mut self, active: &mut Ui) -> Result<(), String> {
        self.poll_launch(active);
        while let Some(pending) = self.pending.take() {
            let result = pending
                .receiver
                .recv_timeout(std::time::Duration::from_secs(30));
            match result {
                Ok(result) => self.admit_launch(active, pending, result),
                Err(error) => {
                    self.pending = Some(pending);
                    return Err(format!("Side startup did not finish: {error}"));
                }
            }
            self.poll_launch(active);
        }
        self.refresh_label(active);
        self.restore_error.clone().map_or(Ok(()), Err)
    }
}

/// Startup may be stuck inside arbitrary in-process restore code. Rust cannot
/// kill that thread safely. Bound only the host's wait, report detachment, and
/// leave the closed Ready receiver to reject input if startup eventually ends.
fn join_launches(
    workers: Vec<std::thread::JoinHandle<()>>,
    budget: std::time::Duration,
) -> Vec<String> {
    let deadline = std::time::Instant::now() + budget;
    let (sender, receiver) = std::sync::mpsc::channel();
    let mut pending = 0;
    let mut errors = Vec::new();
    for worker in workers {
        if worker.is_finished() {
            if worker.join().is_err() {
                errors.push("side startup worker panicked".into());
            }
        } else {
            pending += 1;
            let sender = sender.clone();
            std::thread::spawn(move || {
                let _ = sender.send(worker.join().is_ok());
            });
        }
    }
    drop(sender);
    while pending > 0 {
        match receiver.recv_timeout(deadline.saturating_duration_since(std::time::Instant::now())) {
            Ok(ok) => {
                pending -= 1;
                if !ok {
                    errors.push("side startup worker panicked".into());
                }
            }
            Err(_) => {
                errors.push(format!("{pending} side startup worker(s) still running at exit deadline; detached without accepting input"));
                break;
            }
        }
    }
    errors
}

impl Shutdown {
    /// Called only after the terminal is restored. Closing the receiver also
    /// makes a not-yet-finished startup close its unused session on its worker.
    pub fn finish(self) -> super::super::shutdown::Children {
        let mut timer = lattice::startup::PhaseTimer::start();
        let mut result = super::super::shutdown::Children::default();
        drop(self.pending);
        for session in self.sessions {
            match session.finish_shutdown() {
                Ok(closed) => result.sessions.push(serde_json::json!({
                    "stream": closed.log.stream(), "session": closed.timings, "kernel": closed.kernel,
                })),
                Err(error) => result.errors.push(error),
            }
        }
        timer.checkpoint("session_joins");
        result.errors.extend(join_launches(
            self.workers,
            std::time::Duration::from_secs(10),
        ));
        result.timings = timer.finish("pending_launches");
        result
    }
}

#[cfg(test)]
mod shutdown_tests {
    use super::*;

    #[test]
    fn stuck_startup_does_not_hold_the_exit_wait_forever() {
        let (release, held) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            held.recv().unwrap();
        });
        let (reported, report) = std::sync::mpsc::channel();
        let waiter = std::thread::spawn(move || {
            reported
                .send(join_launches(vec![worker], std::time::Duration::ZERO))
                .unwrap();
        });
        let before_release = report.recv_timeout(std::time::Duration::from_secs(5));
        // Even a poisoned unbounded join is released before the assertion.
        release.send(()).unwrap();
        waiter.join().unwrap();
        let errors = before_release.expect("Exit waited for uncooperative startup");
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("still running"));
    }
}
