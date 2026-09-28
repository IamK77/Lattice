//! The in-process frontend's conversation collection. Each seat keeps its own
//! Session and Ui. Switching seats never interrupts a kernel or shares input.
use super::*;
use lattice::contracts::event::StreamRef;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
#[path = "tabs/settings.rs"]
mod settings;
use settings::Settings;
#[path = "tabs/launch.rs"]
mod launch;
pub(crate) use launch::Shutdown;

#[derive(Debug, PartialEq, Eq)]
pub(super) enum Navigation {
    Open(String),
    Select(usize),
    Next,
    Previous,
    Parent,
}

enum SeatSession<'a> {
    Main(&'a Session),
    Owned(Option<Box<Session>>),
}
impl SeatSession<'_> {
    fn get(&self) -> &Session {
        match self {
            Self::Main(session) => session,
            Self::Owned(session) => session.as_ref().expect("live seat"),
        }
    }
}
impl Drop for SeatSession<'_> {
    fn drop(&mut self) {
        if let Self::Owned(session) = self {
            if let Some(session) = session.take() {
                session.shutdown();
            }
        }
    }
}

struct Seat<'a> {
    session: SeatSession<'a>,
    // The active Ui is held by the terminal loop; every other seat owns one.
    ui: Option<Ui>,
    parent: Option<usize>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    file: String,
    parent: usize,
    origin: StreamRef,
    #[serde(default)]
    settings: Option<Settings>,
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Index {
    v: u8,
    tabs: Vec<Record>,
}

pub(super) struct Tabs<'a> {
    seats: Vec<Seat<'a>>,
    active: usize,
    config: PresetConfig,
    host_services: bool,
    directory: PathBuf,
    records: Vec<Record>,
    // Never overwrite an unreadable index or silently drop a failed restore.
    restore_error: Option<String>,
    nonblocking: bool,
    focus_epoch: u64,
    pending: Option<launch::Pending>,
    restore_queue: std::collections::VecDeque<Record>,
    workers: Vec<std::thread::JoinHandle<()>>,
    retired: Vec<Session>,
}

impl Drop for Tabs<'_> {
    fn drop(&mut self) {
        self.release_sessions().finish();
    }
}

impl<'a> Tabs<'a> {
    pub fn interactive(
        session: &'a Session,
        config: PresetConfig,
        ledger: &Path,
        ui: &mut Ui,
        host_services: bool,
    ) -> Self {
        Self::configured(session, config, ledger, ui, host_services, true)
    }

    #[cfg(test)]
    fn new(
        session: &'a Session,
        config: PresetConfig,
        ledger: &Path,
        ui: &mut Ui,
        host_services: bool,
    ) -> Self {
        Self::configured(session, config, ledger, ui, host_services, false)
    }

    fn configured(
        session: &'a Session,
        config: PresetConfig,
        ledger: &Path,
        ui: &mut Ui,
        host_services: bool,
        nonblocking: bool,
    ) -> Self {
        ui.domain.stream_id = session.log_reader().stream().to_string();
        let mut tabs = Self {
            seats: vec![Seat {
                session: SeatSession::Main(session),
                ui: None,
                parent: None,
            }],
            active: 0,
            config,
            host_services,
            directory: lattice::contracts::document::documents_dir(ledger).join("btw"),
            records: Vec::new(),
            restore_error: None,
            nonblocking,
            focus_epoch: 0,
            pending: None,
            restore_queue: Default::default(),
            workers: Vec::new(),
            retired: Vec::new(),
        };
        if let Err(error) = tabs.restore(ui) {
            ui.flash = Some(format!(
                "Could not restore side conversations: {error}. Their index was not changed."
            ));
            tabs.restore_error = Some(error);
        }
        tabs.refresh_label(ui);
        tabs
    }

    pub fn session(&self) -> &Session {
        self.seats[self.active].session.get()
    }

    /// The terminal owner restores its modes before joining these sessions.
    pub fn release_sessions(&mut self) -> Shutdown {
        let mut sessions: Vec<_> = self
            .seats
            .iter_mut()
            .filter_map(|seat| {
                if let SeatSession::Owned(session) = &mut seat.session {
                    let session = session.take()?;
                    session.request_shutdown();
                    Some(*session)
                } else {
                    None
                }
            })
            .collect();
        sessions.append(&mut self.retired);
        for session in &sessions {
            session.request_shutdown();
        }
        Shutdown {
            sessions,
            pending: self.pending.take(),
            workers: std::mem::take(&mut self.workers),
        }
    }

    pub fn drain(&mut self, active: &mut Ui) -> std::io::Result<bool> {
        let changed = self.poll_launch(active) | drain_render(active, self.session())?;
        for seat in &mut self.seats {
            if let Some(ui) = seat.ui.as_mut() {
                ui.tick = ui.tick.wrapping_add(1);
                drain_render(ui, seat.session.get())?;
                drain_link_open_results(ui);
            }
        }
        // Invisible streaming text is not a reason to redraw the active screen.
        Ok(changed | self.refresh_label(active))
    }

    fn refresh_label(&self, active: &mut Ui) -> bool {
        let mut labels = Vec::new();
        let mut background = Vec::new();
        let mut approvals = 0;
        for (index, seat) in self.seats.iter().enumerate() {
            let ui = if index == self.active {
                &*active
            } else {
                seat.ui.as_ref().expect("inactive seat")
            };
            approvals += ui.domain.authorizations.answerable_count();
            background.extend(ui.domain.background.rows().iter().cloned().map(|mut live| {
                live.label = format!("tab {} · {}", index + 1, live.label);
                // A display namespace, not an event or a cancellation target.
                live.key = format!("tab:{}:{}", index + 1, live.key);
                live
            }));
            let name = if index == 0 {
                "main".to_string()
            } else {
                format!("btw {index}")
            };
            let state = if ui.domain.authorizations.next().is_some() {
                " !approval"
            } else if ui.domain.turns.busy() {
                " working"
            } else if ui.domain.turns.waiting() {
                " waiting"
            } else if !ui.domain.background.rows().is_empty() {
                " background"
            } else {
                ""
            };
            let label = format!("{}:{name}{state}", index + 1);
            labels.push(if index == self.active {
                format!("[{label}]")
            } else {
                label
            });
        }
        let mut label = conversation_label(labels, self.active, approvals);
        if self.pending.is_some() || !self.restore_queue.is_empty() {
            label = format!("opening · {label}");
        }
        if let Some(parent) = self.seats[self.active].parent {
            label.push_str(&format!("  | observing tab {} · /back", parent + 1));
        }
        label.push_str("  | /btw · /tab N");
        let changed =
            active.tab_line != label || active.background_view.as_ref() != Some(&background);
        active.tab_line = label;
        active.background_view = Some(background);
        changed
    }

    pub fn navigate(&mut self, active: &mut Ui, navigation: Navigation) -> bool {
        self.focus_epoch = self.focus_epoch.wrapping_add(1);
        let result = match navigation {
            Navigation::Open(question) => self.open(active, &question),
            Navigation::Select(number) => number
                .checked_sub(1)
                .ok_or_else(|| "Tab numbers start at 1".to_string())
                .and_then(|index| self.select(active, index)),
            Navigation::Next => self.select(active, (self.active + 1) % self.seats.len()),
            Navigation::Previous => self.select(
                active,
                (self.active + self.seats.len() - 1) % self.seats.len(),
            ),
            Navigation::Parent => self.seats[self.active]
                .parent
                .ok_or_else(|| "This is the main conversation".to_string())
                .and_then(|index| self.select(active, index)),
        };
        match result {
            Ok(()) => {
                self.refresh_label(active);
                true
            }
            Err(error) => {
                active.flash = Some(error);
                false
            }
        }
    }

    fn select(&mut self, active: &mut Ui, index: usize) -> Result<(), String> {
        if index >= self.seats.len() {
            return Err(format!("No tab {}", index + 1));
        }
        if index == self.active {
            return Ok(());
        }
        let next = self.seats[index].ui.take().expect("inactive seat");
        self.seats[self.active].ui = Some(std::mem::replace(active, next));
        self.active = index;
        Ok(())
    }

    fn open(&mut self, active: &mut Ui, question: &str) -> Result<(), String> {
        if self.pending.is_some() || !self.restore_queue.is_empty() {
            return Err(
                "A side conversation is still opening; existing tabs remain available".into(),
            );
        }
        if let Some(error) = &self.restore_error {
            return Err(format!(
                "Side conversation index needs repair before adding tabs: {error}"
            ));
        }
        let parent = self.active;
        let reader = self.session().log_reader();
        let event = reader
            .latest_id()
            .ok_or_else(|| "The parent has no recorded event".to_string())?;
        let origin = StreamRef {
            stream: reader.stream().to_string(),
            event,
        };
        std::fs::create_dir_all(&self.directory).map_err(|e| e.to_string())?;
        let reservation = tempfile::Builder::new()
            .prefix("btw-")
            .suffix(".ledger")
            .tempfile_in(&self.directory)
            .map_err(|e| e.to_string())?;
        let path = reservation.path().to_owned();
        let stream = format!(
            "{}-{}",
            reader.stream(),
            path.file_stem().unwrap().to_string_lossy()
        );
        reservation.close().map_err(|error| error.to_string())?;
        // Creation is exclusive: a collision after releasing the temporary
        // name is an error, not permission to resume an unrelated ledger.
        lattice::EventLog::initialize_segmented(&path, &stream)
            .map_err(|error| error.to_string())?;
        let record = Record {
            file: path
                .file_name()
                .and_then(|n| n.to_str())
                .ok_or("Invalid ledger filename")?
                .to_string(),
            parent,
            origin,
            settings: Some(Settings::capture(&self.config_for(active))),
        };
        if self.nonblocking {
            return self.start_launch(self.prepare(&record, active)?, Some(question.to_string()));
        }
        let seat = self.build_seat(&record, active)?;
        let mut records = self.records.clone();
        records.push(record);
        self.save(&records)?;
        self.records = records;
        self.seats.push(seat);
        self.select(active, self.seats.len() - 1)?;
        if !question.trim().is_empty() {
            active.domain.turns.activate();
            self.session()
                .send_with_origin(question, Vec::new(), active.initial_origin.take());
        }
        Ok(())
    }

    fn prepare(&self, record: &Record, parent_ui: &Ui) -> Result<launch::Prepared, String> {
        let parent = self.seats.get(record.parent).ok_or("Missing parent tab")?;
        Ok(launch::Prepared {
            record: record.clone(),
            cfg: self.config_for(parent_ui),
            reader: parent.session.get().log_reader(),
            directory: self.directory.clone(),
            workspace: parent_ui.workspace.clone(),
            bar: parent_ui.bar.clone(),
            host_services: self.host_services,
            active_streams: self
                .seats
                .iter()
                .map(|seat| seat.session.get().log_reader().stream().to_string())
                .collect(),
        })
    }

    fn build_seat(&self, record: &Record, parent_ui: &Ui) -> Result<Seat<'a>, String> {
        self.prepare(record, parent_ui)?
            .build()
            .map(launch::Ready::seat)
    }

    fn config_for(&self, ui: &Ui) -> PresetConfig {
        let mut cfg = self.config.clone();
        let running = ui.domain.model.running();
        cfg.adapter = running.adapter.clone();
        cfg.model = running.model.clone();
        cfg.base_url = running.base_url.clone();
        cfg.key_env = running.key_env.clone();
        cfg.profile = running.profile.clone();
        cfg.context_window = ui
            .domain
            .model
            .effective_window()
            .or_else(|| running.context_window())
            .unwrap_or(cfg.context_window);
        cfg.thinking = ui.domain.model.effort().now.as_ref().map(|word| {
            if word == "off" {
                json!(false)
            } else {
                json!(word)
            }
        });
        cfg
    }

    fn restore(&mut self, main_ui: &Ui) -> Result<(), String> {
        let path = self.directory.join("tabs.json");
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.to_string()),
        };
        let index: Index = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
        if index.v != 1 {
            return Err("Unsupported side conversation index version".to_string());
        }
        let mut files = HashSet::new();
        for (i, record) in index.tabs.iter().enumerate() {
            if record.parent > i
                || !safe_filename(&record.file)
                || !files.insert(record.file.clone())
            {
                return Err("Invalid side conversation index".to_string());
            }
            if !self
                .directory
                .join(&record.file)
                .symlink_metadata()
                .is_ok_and(|metadata| metadata.is_file() || metadata.is_dir())
            {
                return Err(format!(
                    "Side conversation ledger is missing: {}",
                    record.file
                ));
            }
        }
        if self.nonblocking {
            self.restore_queue = index.tabs.into();
            return Ok(());
        }
        for record in &index.tabs {
            let parent_ui = if record.parent == 0 {
                main_ui
            } else {
                self.seats[record.parent]
                    .ui
                    .as_ref()
                    .expect("restored parent")
            };
            let seat = self.build_seat(record, parent_ui)?;
            self.seats.push(seat);
            self.records.push(record.clone());
        }
        Ok(())
    }

    fn save(&self, records: &[Record]) -> Result<(), String> {
        use std::io::Write;
        let bytes = serde_json::to_vec_pretty(&Index {
            v: 1,
            tabs: records.to_vec(),
        })
        .map_err(|e| e.to_string())?;
        let mut file =
            tempfile::NamedTempFile::new_in(&self.directory).map_err(|e| e.to_string())?;
        file.write_all(&bytes).map_err(|e| e.to_string())?;
        file.as_file().sync_all().map_err(|e| e.to_string())?;
        file.persist(self.directory.join("tabs.json"))
            .map_err(|e| e.to_string())?;
        Ok(())
    }
}

/// A fresh headless run resets its main ledger. Keep the old tab index as an
/// artifact rather than trying to attach its children to the new parent.
pub(super) fn archive_index(ledger: &Path) -> std::io::Result<Option<PathBuf>> {
    let directory = lattice::contracts::document::documents_dir(ledger).join("btw");
    let index = directory.join("tabs.json");
    match std::fs::symlink_metadata(&index) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
        Ok(_) => {}
    }
    let archived = tempfile::Builder::new()
        .prefix("tabs-previous-")
        .suffix(".json")
        .tempfile_in(&directory)?;
    let (file, path) = archived.keep().map_err(std::io::Error::other)?;
    drop(file);
    std::fs::rename(index, &path)?;
    Ok(Some(path))
}

/// Identity and global approvals precede the potentially clipped list.
fn conversation_label(mut labels: Vec<String>, active: usize, approvals: usize) -> String {
    let total = labels.len();
    let current = labels.remove(active);
    let attention = if approvals == 0 {
        String::new()
    } else {
        format!("!{approvals} approval · ")
    };
    format!(
        "#{}/{total} {attention}{current} · {} other tabs | {}",
        active + 1,
        total - 1,
        labels.join("  ")
    )
}

fn safe_filename(file: &str) -> bool {
    let mut components = Path::new(file).components();
    matches!(components.next(), Some(std::path::Component::Normal(_)))
        && components.next().is_none()
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(super) fn config() -> PresetConfig {
        PresetConfig {
            adapter: "scripted".into(),
            model: "scripted".into(),
            base_url: String::new(),
            key_env: String::new(),
            workspace: None,
            context_window: 64_000,
            usage_input_field: "input_tokens".into(),
            profile: None,
            catalog_problems: Vec::new(),
            system: "You are a test model.".into(),
            thinking: None,
            scripted: Some(json!({"script":[{"status":"ok","text":"test answer"}]})),
            overlay: None,
            assembly: None,
        }
    }

    fn start(path: &Path, cfg: &PresetConfig) -> Session {
        let path = path.to_path_buf();
        let cfg = cfg.clone();
        Session::spawn("ui", move |tx| {
            crate::terminal_host::session_build::build(tx, &cfg, path)
        })
        .unwrap()
    }

    fn ui_for(cfg: &PresetConfig) -> Ui {
        let mut ui = Ui::replayed(&[]);
        *ui.domain.model.fixture_running() = preset::running_entry(cfg);
        ui.workspace = "test-workspace".into();
        ui
    }

    fn wait_for(session: &Session, ui: &mut Ui, wanted: &str) {
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            let render = session
                .next_render_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
                .unwrap_or_else(|error| {
                    panic!(
                        "Waiting for {wanted} ended: {error}; ledger {}",
                        session.log_reader().stream()
                    )
                });
            if let RenderEvent::Appended(event) = &render {
                assert!(
                    event.event_type != core_events::ERROR
                        && !(event.event_type == core_events::MODEL_CALL_COMPLETED
                            && event.payload["status"] == "error"),
                    "Waiting for {wanted} encountered {}: {}",
                    event.event_type,
                    event.payload
                );
                assert!(
                    event.event_type != core_events::TURN_COMPLETED
                        || wanted == core_events::TURN_COMPLETED,
                    "Turn ended before {wanted}: {}",
                    event.payload
                );
            }
            let done =
                matches!(&render, RenderEvent::Appended(event) if event.event_type == wanted);
            fold_render(ui, render).unwrap();
            if done {
                return;
            }
        }
    }

    fn finish(session: &Session, ui: &mut Ui) {
        wait_for(session, ui, core_events::TURN_COMPLETED);
    }

    #[test]
    fn replay_overlap_is_not_counted_twice_and_model_controls_do_not_keep_old_profiles() {
        let event = |seq, kind: &str, payload| {
            serde_json::from_value::<EventEnvelope>(json!({"v":1,"id":format!("ev_{seq}_test"),"seq":seq,"stream":"side","time":"2026-09-12T00:00:00Z","type":kind,"source":"core","causes":[],"payload":payload})).unwrap()
        };
        let auth = event(1, trust_policy::AUTH_REQUESTED, json!({"held":"call"}));
        let mut ui = Ui::replayed(std::slice::from_ref(&auth));
        fold_render(&mut ui, RenderEvent::Appended(Box::new(auth))).unwrap();
        assert_eq!(ui.domain.authorizations.answerable_count(), 1);
        ui.absorb(
            &event(2, trust_policy::AUTH_REQUESTED, json!({"held":"next"})),
            1,
        );
        assert_eq!(ui.domain.authorizations.answerable_count(), 2);
        ui.domain.model.fixture_running().profile = Some(json!({"contextWindow":32000}));
        *ui.domain.model.fixture_window() = Some(32000);
        ui.domain.model.fixture_effort().rungs = vec!["old-only".into()];
        ui.note_model_swap(&event(3, core_events::COMPONENT_REPLACED, json!({"instance":"model","to":"scripted-model","config":{"model":"no-profile","profile":null}})));
        assert!(ui.domain.model.running().profile.is_none());
        assert!(!ui
            .domain
            .model
            .effort()
            .rungs
            .iter()
            .any(|rung| rung == "old-only"));
        assert_eq!(ui.domain.model.effective_window(), Some(32000));
        ui.note_model_swap(&event(4, core_events::COMPONENT_REPLACED, json!({"instance":"model","to":"scripted-model","config":{"model":"new-profile","profile":{"contextWindow":48000}}})));
        assert_eq!(ui.domain.model.running().context_window(), Some(48000));
        assert_eq!(ui.domain.model.effective_window(), Some(48000));
    }

    #[test]
    fn live_assembly_notices_replace_the_panel_snapshot() {
        let mut ui = Ui::replayed(&[]);
        let row = (
            "installed".to_string(),
            "tool-provider".to_string(),
            "subprocess",
            "Tool".to_string(),
            true,
            vec!["agent.tools -> installed.call".to_string()],
        );
        fold_render(
            &mut ui,
            RenderEvent::Notice {
                source: "assembly".into(),
                payload: json!({"parts":[row]}),
            },
        )
        .unwrap();
        assert_eq!(ui.parts, vec![row]);
        fold_render(
            &mut ui,
            RenderEvent::Notice {
                source: "assembly".into(),
                payload: json!({"parts":[]}),
            },
        )
        .unwrap();
        assert!(ui.parts.is_empty(), "removed instances must disappear too");
    }

    #[test]
    fn held_startup_keeps_navigation_live_and_commits_before_sending() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = dir.path().join("main.jsonl");
        let cfg = config();
        let main = start(&ledger, &cfg);
        let mut ui = ui_for(&cfg);
        ui.parts = vec![(
            "parent-only".into(),
            "fake".into(),
            "in-process",
            "".into(),
            false,
            vec![],
        )];
        let mut tabs = Tabs::interactive(&main, cfg.clone(), &ledger, &mut ui, false);
        std::fs::create_dir_all(&tabs.directory).unwrap();
        std::fs::write(tabs.directory.join("held.jsonl"), "").unwrap();
        let record = Record {
            file: "held.jsonl".into(),
            parent: 0,
            origin: StreamRef {
                stream: main.log_reader().stream().into(),
                event: main
                    .log_reader()
                    .scan_back(|e, _| Ok(Some(e.id.clone())))
                    .unwrap()
                    .unwrap(),
            },
            settings: Some(Settings::capture(&cfg)),
        };
        let prepared = tabs.prepare(&record, &ui).unwrap();
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        tabs.launch(record, Some("first question".into()), move || {
            entered_tx.send(()).unwrap();
            release_rx.recv_timeout(Duration::from_secs(10)).unwrap();
            prepared.build()
        })
        .unwrap();
        entered_rx.recv_timeout(Duration::from_secs(10)).unwrap();
        tabs.drain(&mut ui).unwrap();
        assert!(ui.tab_line.contains("opening"));
        assert_eq!(tabs.seats.len(), 1);
        assert!(!tabs.directory.join("tabs.json").exists());
        ui.draft.edit().insert_str("still editable");
        assert!(tabs.navigate(&mut ui, Navigation::Select(1)));
        release_tx.send(()).unwrap();
        // The worker may build a session, but cannot persist or send input.
        let pending = tabs.pending.take().unwrap();
        let ready = pending
            .receiver
            .recv_timeout(Duration::from_secs(10))
            .unwrap()
            .unwrap();
        assert!(!ready
            .session
            .as_ref()
            .unwrap()
            .log_reader()
            .replay(1)
            .unwrap()
            .iter()
            .any(|e| e.event_type == core_events::USER_MESSAGE));
        assert!(!tabs.directory.join("tabs.json").exists());
        tabs.admit_launch(&mut ui, pending, Ok(ready));
        assert_eq!(
            tabs.active, 0,
            "startup must not steal a later focus choice"
        );
        assert_eq!(ui.draft.editor().text(), "still editable");
        assert!(tabs.directory.join("tabs.json").is_file());
        assert!(tabs.navigate(&mut ui, Navigation::Select(2)));
        finish(tabs.session(), &mut ui);
        assert!(!ui.parts.iter().any(|part| part.0 == "parent-only"));
        assert_eq!(ui.parts, tabs.session().initial_parts());
        assert!(tabs
            .session()
            .log_reader()
            .replay(1)
            .unwrap()
            .iter()
            .any(|e| e.event_type == core_events::USER_MESSAGE
                && e.payload["text"] == "first question"));
        tabs.release_sessions().finish();
        drop(tabs);
        main.shutdown();
        let main = start(&ledger, &cfg);
        let mut ui = ui_for(&cfg);
        let mut restored = Tabs::interactive(&main, cfg, &ledger, &mut ui, false);
        assert_eq!(restored.seats.len(), 1);
        restored.wait_startups(&mut ui).unwrap();
        assert_eq!(restored.seats.len(), 2);
        assert_eq!(restored.active, 0);
        drop(restored);
        main.shutdown();
    }

    #[test]
    fn image_only_and_full_skill_messages_send_images_but_skill_candidates_keep_references() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = config();
        cfg.scripted = Some(
            json!({"script":(0..3).map(|_| json!({"status":"ok","text":"answer"})).collect::<Vec<_>>()}),
        );
        let session = start(&dir.path().join("images.jsonl"), &cfg);
        let mut ui = ui_for(&cfg);
        for (case, text) in ["", "/research argument", "/res"].into_iter().enumerate() {
            ui.domain.skills = vec![("research".into(), "research".into())];
            if case != 2 {
                ui.draft.edit().set(text);
            }
            ui.draft.attach(
                lattice::contracts::document::DocRef {
                    file: format!("{case}.png"),
                    bytes: 10,
                    lines: None,
                    preview: None,
                },
                "image/png",
                "picture",
            );
            if case == 2 {
                ui.draft.edit().set(text);
            }
            on_key(
                &mut ui,
                Some(&session),
                ratatui::crossterm::event::KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
                &Hit::default(),
            );
            assert_eq!(ui.draft.references().len(), usize::from(case == 2));
            finish(&session, &mut ui);
            let event = session
                .log_reader()
                .replay(1)
                .unwrap()
                .into_iter()
                .rev()
                .find(|e| e.event_type == core_events::USER_MESSAGE && e.causes.is_empty())
                .unwrap();
            assert_eq!(
                event.payload["text"],
                if case == 2 { "/research" } else { text }
            );
            if case == 2 {
                assert!(event
                    .payload
                    .get("images")
                    .is_none_or(|v| v.as_array().unwrap().is_empty()));
            } else {
                assert_eq!(event.payload["images"][0]["file"], format!("{case}.png"));
            }
        }
        session.shutdown();
    }

    #[test]
    fn btw_is_an_observing_private_conversation_and_switching_preserves_each_ui() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = dir.path().join("main.jsonl");
        let mut cfg = config();
        cfg.scripted = Some(json!({"script":[
            {"status":"ok","text":"test answer"},
            {"status":"ok","text":"draft answer"}
        ]}));
        let main = start(&ledger, &cfg);
        let mut ui = ui_for(&cfg);
        main.send_text("parent context");
        finish(&main, &mut ui);
        let parent_events = main.log_reader().replay(1).unwrap();
        let mut tabs = Tabs::new(&main, cfg, &ledger, &mut ui, false);
        ui.draft.edit().insert_str("parent draft");
        ui.draft.attach(
            lattice::contracts::document::DocRef {
                file: "parent.png".into(),
                bytes: 10,
                lines: None,
                preview: None,
            },
            "image/png",
            "parent picture",
        );
        ui.draft.next_hint(3);
        ui.browsing.set_offset(9);
        ui.domain.authorizations.restore(vec![
            ("answered-parent-approval".into(), "answered-ask".into()),
            ("parent-approval".into(), "ask".into()),
        ]);
        assert_eq!(
            ui.domain.authorizations.answer_oldest().as_deref(),
            Some("answered-parent-approval")
        );
        tabs.refresh_label(&mut ui);
        assert!(ui.tab_line.contains("!1 approval"));
        assert!(tabs.navigate(&mut ui, Navigation::Open("side question".into())));
        finish(tabs.session(), &mut ui);
        assert_eq!(tabs.active, 1);
        assert!(ui.draft.editor().is_empty());
        assert!(ui.draft.references().is_empty());
        assert_eq!(ui.draft.selected(), 0);
        assert!(ui.domain.authorizations.next().is_none());
        let side_events = tabs.session().log_reader().replay(1).unwrap();
        let root = side_events
            .iter()
            .find(|e| e.event_type == core_events::USER_MESSAGE && e.causes.is_empty())
            .unwrap();
        assert_eq!(
            root.origin.as_ref().unwrap().stream,
            main.log_reader().stream()
        );
        assert_eq!(root.payload["text"], "side question");
        assert!(side_events
            .iter()
            .any(|e| e.event_type == core_events::MODEL_CALL_STARTED
                && e.payload["input"]["parts"]
                    .as_array()
                    .is_some_and(|parts| parts.iter().any(|part| part["digest"]["text"]
                        .as_str()
                        .is_some_and(|text| text.contains("parent context"))))));
        assert_eq!(
            main.log_reader().replay(1).unwrap().len(),
            parent_events.len(),
            "the side question must not enter the parent"
        );
        ui.draft.edit().insert_str("side draft");
        ui.draft.attach(
            lattice::contracts::document::DocRef {
                file: "side.png".into(),
                bytes: 10,
                lines: None,
                preview: None,
            },
            "image/png",
            "side picture",
        );
        ui.draft.next_hint(3);
        ui.draft.next_hint(3);
        ui.browsing.set_offset(4);
        ui.domain.model.sent_effort("low".into());
        assert!(tabs.navigate(&mut ui, Navigation::Parent));
        assert_eq!(ui.draft.editor().expanded(), "parent draft");
        assert_eq!(ui.draft.references()[0]["file"], "parent.png");
        assert_eq!(ui.draft.editor().images(), vec![0]);
        assert_eq!(ui.draft.selected(), 1);
        assert_eq!(ui.browsing.offset(), 9);
        assert_eq!(ui.domain.authorizations.next(), Some("parent-approval"));
        assert_eq!(ui.domain.authorizations.history().len(), 2);
        assert_eq!(ui.domain.authorizations.answerable_count(), 1);
        assert_eq!(ui.domain.model.effort().now, None);
        assert!(tabs.navigate(&mut ui, Navigation::Select(2)));
        assert_eq!(ui.draft.editor().expanded(), "side draft");
        assert_eq!(ui.draft.references()[0]["file"], "side.png");
        assert_eq!(ui.draft.editor().images(), vec![0]);
        assert_eq!(ui.draft.selected(), 2);
        assert_eq!(ui.browsing.offset(), 4);
        assert_eq!(ui.domain.model.effort().now.as_deref(), Some("low"));
        assert!(ui.tab_line.contains("1:main !approval"));
        assert!(ui.tab_line.contains("[2:btw 1]"));
        let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(100, 30)).unwrap();
        draw(&mut terminal, &ui).unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(text.contains("[2:btw 1]"), "the selector is rendered");
        assert!(!tabs.navigate(&mut ui, Navigation::Select(99)));
        assert_eq!(tabs.active, 1);
        assert!(tabs.navigate(&mut ui, Navigation::Parent));
        on_key(
            &mut ui,
            Some(tabs.session()),
            ratatui::crossterm::event::KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            &Hit::default(),
        );
        assert!(ui.draft.references().is_empty());
        finish(tabs.session(), &mut ui);
        let sent = main
            .log_reader()
            .replay(1)
            .unwrap()
            .into_iter()
            .rev()
            .find(|event| event.event_type == core_events::USER_MESSAGE && event.causes.is_empty())
            .unwrap();
        assert_eq!(sent.payload["text"], "parent draft");
        assert_eq!(sent.payload["images"][0]["file"], "parent.png");
        drop(tabs);
        main.shutdown();
    }

    #[test]
    fn a_blank_btw_waits_for_input_and_nested_parents_survive_restart() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = dir.path().join("main.jsonl");
        let cfg = config();
        let main = start(&ledger, &cfg);
        let mut ui = ui_for(&cfg);
        let mut tabs = Tabs::new(&main, cfg.clone(), &ledger, &mut ui, false);
        assert!(tabs.navigate(&mut ui, Navigation::Open(String::new())));
        assert!(!tabs
            .session()
            .log_reader()
            .replay(1)
            .unwrap()
            .iter()
            .any(|e| e.event_type == core_events::MODEL_CALL_STARTED));
        assert!(ui.initial_origin.is_some());
        tabs.session()
            .send_with_origin("first side input", Vec::new(), ui.initial_origin.take());
        finish(tabs.session(), &mut ui);
        assert!(tabs.navigate(&mut ui, Navigation::Open(String::new())));
        let nested_stream = tabs.session().log_reader().stream().to_string();
        drop(tabs);
        main.shutdown();

        let main = start(&ledger, &cfg);
        let mut ui = ui_for(&cfg);
        let mut tabs = Tabs::new(&main, cfg, &ledger, &mut ui, false);
        assert!(tabs.restore_error.is_none(), "{:?}", tabs.restore_error);
        assert_eq!(tabs.seats.len(), 3);
        assert!(tabs.navigate(&mut ui, Navigation::Select(3)));
        assert_eq!(ui.domain.stream_id, nested_stream);
        assert!(ui.initial_origin.is_some());
        tabs.session()
            .send_with_origin("after restart", Vec::new(), ui.initial_origin.take());
        finish(tabs.session(), &mut ui);
        let events = tabs.session().log_reader().replay(1).unwrap();
        assert!(events
            .iter()
            .any(|e| e.event_type == core_events::MODEL_CALL_STARTED
                && e.payload["input"]["parts"]
                    .as_array()
                    .is_some_and(|parts| parts.iter().any(|part| part["digest"]["text"]
                        .as_str()
                        .is_some_and(|text| text.contains("first side input"))))));
        assert!(tabs.navigate(&mut ui, Navigation::Parent));
        assert_eq!(tabs.active, 1);
        drop(tabs);
        main.shutdown();
    }

    #[cfg(unix)]
    #[test]
    fn a_side_ledger_cannot_alias_an_active_parent() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = dir.path().join("main.jsonl");
        let cfg = config();
        let main = start(&ledger, &cfg);
        let mut ui = ui_for(&cfg);
        let tabs = Tabs::new(&main, cfg, &ledger, &mut ui, false);
        std::fs::create_dir_all(&tabs.directory).unwrap();
        let reader = main.log_reader();
        let mut record = Record {
            file: "symbolic.jsonl".into(),
            settings: None,
            parent: 0,
            origin: StreamRef {
                stream: reader.stream().into(),
                event: reader
                    .scan_back(|event, _| Ok(Some(event.id.clone())))
                    .unwrap()
                    .unwrap(),
            },
        };
        std::os::unix::fs::symlink(&ledger, tabs.directory.join(&record.file)).unwrap();
        assert!(tabs
            .build_seat(&record, &ui)
            .err()
            .unwrap()
            .contains("regular file"));
        record.file = "hard.jsonl".into();
        std::fs::hard_link(&ledger, tabs.directory.join(&record.file)).unwrap();
        assert!(tabs
            .build_seat(&record, &ui)
            .err()
            .unwrap()
            .contains("another active tab"));
        drop(tabs);
        main.shutdown();
    }

    #[test]
    fn a_broken_index_is_reported_and_never_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = dir.path().join("main.jsonl");
        let directory = lattice::contracts::document::documents_dir(&ledger).join("btw");
        std::fs::create_dir_all(&directory).unwrap();
        let index = directory.join("tabs.json");
        std::fs::write(&index, "not valid json").unwrap();
        let cfg = config();
        let main = start(&ledger, &cfg);
        let mut ui = ui_for(&cfg);
        let mut tabs = Tabs::new(&main, cfg, &ledger, &mut ui, false);
        assert!(tabs.restore_error.is_some());
        assert!(ui.flash.as_deref().unwrap().contains("not changed"));
        assert!(!tabs.navigate(&mut ui, Navigation::Open("question".into())));
        assert_eq!(std::fs::read_to_string(index).unwrap(), "not valid json");
        for name in ["", ".", "..", "../main.jsonl", "/tmp/main.jsonl", "a/b"] {
            assert!(!safe_filename(name), "{name}");
        }
        assert!(safe_filename("btw-123.jsonl"));
        drop(tabs);
        main.shutdown();
    }

    #[test]
    #[cfg(unix)]
    fn a_side_answer_finishes_while_the_parent_process_is_still_held() {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        struct Release(std::fs::File);
        impl Drop for Release {
            fn drop(&mut self) {
                let _ = self.0.write_all(b"go\n");
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let gate = dir.path().join("gate");
        assert!(std::process::Command::new("mkfifo")
            .arg(&gate)
            .status()
            .unwrap()
            .success());
        let mut release = Release(
            std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .custom_flags(libc::O_NONBLOCK)
                .open(&gate)
                .unwrap(),
        );
        let mut parent_cfg = config();
        parent_cfg.scripted = Some(json!({"script":[
            {"status":"ok", "toolCalls":[{"id":"blocked", "tool":"Run", "arguments":{"command":format!("read -r line < '{}'; printf 'parent released\\n'", gate.display())}}]},
            {"status":"ok","text":"parent finished"}
        ]}));
        let ledger = dir.path().join("main.jsonl");
        let main = start(&ledger, &parent_cfg);
        let mut ui = ui_for(&parent_cfg);
        main.send_text("parent work");
        wait_for(&main, &mut ui, lattice::components::minimal_loop::WAITING);
        assert!(ui.domain.turns.waiting());
        let mut side_cfg = config();
        side_cfg.scripted = Some(json!({"script":[
            {"status":"ok","text":"test answer"},
            {"status":"ok","text":"updated answer"}
        ]}));
        let mut tabs = Tabs::new(&main, side_cfg, &ledger, &mut ui, false);
        assert!(tabs.navigate(&mut ui, Navigation::Open("independent question".into())));
        assert_eq!(
            ui.background().len(),
            1,
            "the side panel includes the parent's live command"
        );
        assert!(ui.background()[0].label.starts_with("tab 1 ·"));
        assert!(snapshot(&ui, 160, 30)
            .rows
            .iter()
            .any(|row| row.contains("1 background")));
        finish(tabs.session(), &mut ui);
        assert!(crate::terminal_host::cards::tests::materialize(&ui)
            .iter()
            .any(|entry| matches!(entry, Entry::Agent(text) if text == "test answer")));
        assert!(!main
            .log_reader()
            .replay(1)
            .unwrap()
            .iter()
            .any(|event| event.event_type == core_events::WAKE));
        assert!(tabs.navigate(&mut ui, Navigation::Parent));
        assert!(
            ui.domain.turns.waiting(),
            "switching away did not finish or cancel the parent"
        );
        release.0.write_all(b"go\n").unwrap();
        finish(&main, &mut ui);
        assert!(ui
            .entries
            .iter()
            .any(|entry| matches!(entry, Entry::Agent(text) if text == "parent finished")));
        assert!(tabs.navigate(&mut ui, Navigation::Select(2)));
        assert!(
            ui.background().is_empty(),
            "completion removes the parent's task from every tab's display"
        );
        tabs.session().send_text("what changed in the parent?");
        finish(tabs.session(), &mut ui);
        let events = tabs.session().log_reader().replay(1).unwrap();
        assert!(events
            .iter()
            .any(|event| event.event_type == core_events::MODEL_CALL_STARTED
                && event.payload["input"]["parts"]
                    .as_array()
                    .is_some_and(|parts| parts.iter().any(|part| part["digest"]["text"]
                        .as_str()
                        .is_some_and(|text| text.contains("parent finished"))))));
        drop(tabs);
        main.shutdown();
    }

    #[test]
    fn a_fresh_debug_run_archives_the_index_without_deleting_side_ledgers() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = dir.path().join("main.jsonl");
        assert!(archive_index(&ledger).unwrap().is_none());
        let directory = lattice::contracts::document::documents_dir(&ledger).join("btw");
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(directory.join("tabs.json"), "old index").unwrap();
        std::fs::write(directory.join("side.jsonl"), "old ledger").unwrap();
        let archived = archive_index(&ledger).unwrap().unwrap();
        assert_eq!(std::fs::read_to_string(archived).unwrap(), "old index");
        assert_eq!(
            std::fs::read_to_string(directory.join("side.jsonl")).unwrap(),
            "old ledger"
        );
        assert!(!directory.join("tabs.json").exists());
    }

    #[test]
    fn a_narrow_header_keeps_the_active_number_and_global_approval_count() {
        let mut ui = ui_for(&config());
        ui.tab_line = conversation_label((1..=12).map(|i| format!("[{i}:side]")).collect(), 11, 3);
        let frame = snapshot(&ui, 24, 12);
        assert!(frame.rows[0].contains("#12/12"), "{}", frame.rows[0]);
        assert!(frame.rows[0].contains("!3 approval"), "{}", frame.rows[0]);
    }

    #[test]
    fn navigation_keys_preserve_drafts_and_are_not_model_messages() {
        use ratatui::crossterm::event::KeyEvent;
        let mut ui = ui_for(&config());
        ui.draft.edit().insert_str("unfinished draft");
        on_key(
            &mut ui,
            None,
            KeyEvent::new(KeyCode::PageDown, KeyModifiers::CONTROL),
            &Hit::default(),
        );
        assert_eq!(ui.navigation.take(), Some(Navigation::Next));
        assert_eq!(ui.draft.editor().text(), "unfinished draft");
        on_key(
            &mut ui,
            None,
            KeyEvent::new(KeyCode::PageUp, KeyModifiers::CONTROL),
            &Hit::default(),
        );
        assert_eq!(ui.navigation.take(), Some(Navigation::Previous));
        run_slash(&mut ui, "/btw what does this mean?", None);
        assert_eq!(
            ui.navigation.take(),
            Some(Navigation::Open("what does this mean?".into()))
        );
        run_slash(&mut ui, "/back", None);
        assert_eq!(ui.navigation.take(), Some(Navigation::Parent));
        assert!(!ui.domain.turns.busy());
    }
}
