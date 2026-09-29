use super::*;
use lattice::core_events as ce;
use ratatui::backend::{Backend, TestBackend, WindowSize};
use ratatui::layout::{Position, Size};
use std::io;

fn config() -> PresetConfig {
    PresetConfig {
        adapter: "scripted".into(),
        model: "launch-model".into(),
        base_url: String::new(),
        key_env: String::new(),
        workspace: None,
        context_window: 64000,
        usage_input_field: "input_tokens".into(),
        profile: Some(json!({"contextWindow":12345})),
        catalog_problems: vec![],
        system: "Fixture".into(),
        thinking: Some(json!("low")),
        scripted: Some(json!({"script":[]})),
        overlay: None,
        assembly: None,
    }
}
struct Live(Option<Session>);
impl Drop for Live {
    fn drop(&mut self) {
        if let Some(session) = self.0.take() {
            session.shutdown();
        }
    }
}
fn start(path: &std::path::Path, cfg: &PresetConfig) -> Live {
    let path = path.to_path_buf();
    let cfg = cfg.clone();
    Live(Some(
        Session::spawn("ui", move |tx| session_build::build(tx, &cfg, path)).unwrap(),
    ))
}
fn history() -> Vec<EventEnvelope> {
    [
        (ce::COMPONENT_REPLACED, json!({"instance":"model","to":"scripted-model","config":{"model":"past-model","entryId":"past-entry","profile":{"contextWindow":54321}}})),
        (ce::EXTERNAL_INPUT, json!({"channel":lattice::components::context_gate::EFFORT_CHANNEL,"value":"max"})),
        (ce::EXTERNAL_INPUT, json!({"channel":lattice::components::context_gate::MODEL_CHANNEL,"contextWindow":99999})),
    ].into_iter().enumerate().map(|(i,(kind,payload))| serde_json::from_value(json!({
        "v":1,"id":format!("fixture-{}",i+1),"seq":i+1,"stream":"fixture","time":"2026-09-21T00:00:00Z","type":kind,"source":"fixture","causes":[],"payload":payload
    })).unwrap()).collect()
}
#[test]
fn main_and_debug_keep_distinct_conflicting_history_precedence() {
    test_support::isolated(|| {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("main.jsonl");
        let past = history();
        let mut next = past[0].clone();
        next.seq = 4;
        next.id = "later".into();
        next.payload["config"]["model"] = json!("later-model");
        std::fs::write(
            &path,
            past.iter()
                .chain(std::iter::once(&next))
                .map(|e| serde_json::to_string(e).unwrap() + "\n")
                .collect::<String>(),
        )
        .unwrap();
        let cfg = config();
        let running = preset::running_entry(&cfg);
        let live = start(&path, &cfg);
        let session = live.0.as_ref().unwrap();
        let (mut main, _, _) = Main {
            title: "launch-model · fixture".into(),
            workspace: "fixture".into(),
            effort: EffortView {
                rungs: running.effort_rungs(),
                now: Some("low".into()),
            },
            models: model_catalog::from_config(&cfg),
            running: running.clone(),
            history_end: 3,
            documents: None,
            parts: vec![],
            expert_dir: None,
            stream_id: "fixture".into(),
            tab_config: cfg.clone(),
            main_ledger: path,
        }
        .install(session, &mut None)
        .unwrap();
        assert_eq!(main.domain.model.running().id, "past-entry");
        assert_eq!(main.domain.model.running().model, "past-model");
        assert!(main.domain.model.catalog().current().is_none());
        assert_eq!(main.domain.model.effort().now.as_deref(), Some("low"));
        assert_eq!(
            main.domain.model.effort().rungs,
            main.domain.model.running().effort_rungs()
        );
        assert_eq!(main.domain.model.effective_window(), Some(99999));
        assert_eq!(main.domain.title, "past-model · fixture");
        assert_eq!(main.parts, session.initial_parts());
        assert!(
            !main.parts.is_empty(),
            "actual assembly replaces empty preview"
        );
        let mut debug = Debug {
            models: model_catalog::from_config(&cfg),
            config: cfg,
            parts: vec![],
            workspace: dir.path().into(),
            documents: dir.path().join("documents"),
            vision: true,
        }
        .install(&past);
        assert_eq!(debug.domain.model.running(), &running);
        let current = debug.domain.model.catalog().current().unwrap();
        assert_eq!(current.model, "launch-model");
        assert!(current.accepts_images);
        assert_eq!(current.window, Some(12345));
        assert_eq!(debug.domain.model.effort().now, None);
        assert_eq!(
            debug.domain.model.effort().rungs,
            main.domain.model.effort().rungs
        );
        assert_eq!(debug.domain.model.effective_window(), Some(99999));
        assert_eq!(debug.domain.title, "debug-tui");
        assert!(debug.parts.is_empty());
        // Both paths still accept later changes through the same actual live fold.
        for ui in [&mut main, &mut debug] {
            ui.try_absorb_profiled(&next, SETTLED_TICK * 2, None)
                .unwrap();
            assert_eq!(ui.domain.model.running().model, "later-model");
            assert_eq!(ui.domain.model.effective_window(), Some(54321));
        }
    });
}

struct Failing {
    inner: TestBackend,
    fail: bool,
    draws: usize,
}
impl Backend for Failing {
    type Error = io::Error;

    fn draw<'a, I>(&mut self, content: I) -> io::Result<()>
    where
        I: Iterator<Item = (u16, u16, &'a ratatui::buffer::Cell)>,
    {
        self.draws += 1;
        if self.fail {
            return Err(io::Error::other("injected draw failure"));
        }
        self.inner.draw(content).map_err(|never| match never {})
    }
    fn hide_cursor(&mut self) -> io::Result<()> {
        self.inner.hide_cursor().map_err(|never| match never {})
    }
    fn show_cursor(&mut self) -> io::Result<()> {
        self.inner.show_cursor().map_err(|never| match never {})
    }
    fn get_cursor_position(&mut self) -> io::Result<Position> {
        self.inner
            .get_cursor_position()
            .map_err(|never| match never {})
    }
    fn set_cursor_position<P: Into<Position>>(&mut self, p: P) -> io::Result<()> {
        self.inner
            .set_cursor_position(p)
            .map_err(|never| match never {})
    }
    fn clear(&mut self) -> io::Result<()> {
        self.inner.clear().map_err(|never| match never {})
    }
    fn clear_region(&mut self, clear_type: ratatui::backend::ClearType) -> io::Result<()> {
        self.inner
            .clear_region(clear_type)
            .map_err(|never| match never {})
    }
    fn size(&self) -> io::Result<Size> {
        self.inner.size().map_err(|never| match never {})
    }
    fn window_size(&mut self) -> io::Result<WindowSize> {
        self.inner.window_size().map_err(|never| match never {})
    }
    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush().map_err(|never| match never {})
    }
}
#[test]
fn only_a_successfully_drawn_first_frame_consumes_startup_observation() {
    test_support::isolated(|| {
        let dir = tempfile::tempdir().unwrap();
        let live = start(&dir.path().join("main.ledger"), &config());
        let session = live.0.as_ref().unwrap();
        let mut ui = Ui::replayed(&[]);
        let mut term = Terminal::new(Failing {
            inner: TestBackend::new(100, 40),
            fail: true,
            draws: 0,
        })
        .unwrap();
        let mut startup = Some(StartupTrace::start());
        let mut cost = RenderCost::default();
        let error = draw_observed(&mut term, &mut ui, session, &mut startup, &mut cost)
            .err()
            .unwrap();
        assert!(error.to_string().contains("injected draw failure"));
        assert_eq!(term.backend().draws, 1);
        assert!(startup.is_some());
        term.backend_mut().fail = false;
        draw_observed(&mut term, &mut ui, session, &mut startup, &mut cost).unwrap();
        assert!(startup.is_none());
        term.backend_mut().fail = true;
        assert!(draw_observed(&mut term, &mut ui, session, &mut startup, &mut cost).is_err());
        assert!(startup.is_none());
        // Live joins the session on drop; no real host service or credential is used.
    });
}
