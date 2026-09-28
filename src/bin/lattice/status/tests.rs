use super::*;
use lattice::view::{EffortView, Entry, Live, ModelRow, ModelView, Usage, UsageReport};
use serde_json::json;
use std::borrow::Cow;

const DIM: Color = Color::Rgb(10, 20, 30);
const WARM: Color = Color::Rgb(40, 50, 60);

#[derive(Default)]
struct StatusView {
    names: Vec<String>,
    workspace: String,
    models: ModelView,
    effort: EffortView,
    usage: Option<UsageReport>,
    background: Vec<Live>,
    compaction: Option<lattice::components::context_gate::CompactionStatus>,
}

impl View for StatusView {
    fn title(&self) -> &str {
        ""
    }
    fn entries(&self) -> &[Entry] {
        &[]
    }
    fn streaming(&self) -> &str {
        ""
    }
    fn input(&self) -> Cow<'_, str> {
        Cow::Borrowed("")
    }
    fn busy(&self) -> bool {
        false
    }
    fn tick(&self) -> usize {
        0
    }
    fn status_bar(&self) -> &[String] {
        &self.names
    }
    fn workspace(&self) -> &str {
        &self.workspace
    }
    fn models(&self) -> ModelView {
        self.models.clone()
    }
    fn effort(&self) -> EffortView {
        self.effort.clone()
    }
    fn usage(&self) -> Option<UsageReport> {
        self.usage
    }
    fn background(&self) -> &[Live] {
        &self.background
    }
    fn compaction_status(&self) -> Option<&lattice::components::context_gate::CompactionStatus> {
        self.compaction.as_ref()
    }
}

#[test]
fn compaction_pause_and_progress_remain_visible_without_an_occupancy_measurement() {
    use lattice::components::context_gate::{CompactionFailure, CompactionStatus};
    let failure = CompactionFailure {
        event: "failed-call".into(),
        code: "transport.failed".into(),
        message: "fixture failure".into(),
    };
    let mut view = StatusView::default();
    for (in_flight, failed, label) in [
        (false, true, "compact paused · /compact"),
        (true, true, "compacting · auto paused"),
        (true, false, "compacting"),
    ] {
        view.compaction = Some(CompactionStatus {
            in_flight,
            failure: failed.then(|| failure.clone()),
        });
        let segments = shown(&view);
        assert_eq!(segments.len(), 1);
        assert_eq!(segments[0].text, label);
        assert_eq!(segments[0].style.fg, Some(WARM));
        assert_eq!(segments[0].opens, Opens::Context);
    }
    view.compaction = Some(CompactionStatus {
        in_flight: false,
        failure: None,
    });
    assert!(shown(&view).is_empty());
    view.models = full_bar().models;
    view.usage = full_bar().usage;
    assert!(shown(&view).iter().any(|segment| segment.text == "ctx 90%"));
}

fn full_bar() -> StatusView {
    StatusView {
        workspace: "/work/project".into(),
        models: ModelView {
            rows: vec![ModelRow {
                id: "model".into(),
                window: Some(1000),
                key_present: true,
                ..Default::default()
            }],
            now: Some(0),
        },
        effort: EffortView {
            now: Some("high".into()),
            ..Default::default()
        },
        usage: Some(UsageReport {
            call: Usage {
                prompt: 900,
                ..Default::default()
            },
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn activity(standing: bool) -> Live {
    Live {
        kind: if standing { "timer" } else { "command" },
        key: "job".into(),
        label: "work".into(),
        standing,
        fires: 0,
        since: 0,
        ledger: None,
        tools: 0,
        tokens: 0,
        read_len: 0,
    }
}

fn shown(view: &StatusView) -> Vec<Segment> {
    readouts(view, DIM, WARM)
}

#[test]
fn a_readout_with_nothing_to_say_is_absent() {
    assert!(shown(&StatusView::default()).is_empty());
    let mut view = full_bar();
    assert_eq!(shown(&view).len(), 4);
    for effort in [None, Some(""), Some("off")] {
        view.effort.now = effort.map(str::to_string);
        assert!(!shown(&view).iter().any(|s| s.opens == Opens::Effort));
    }
    for prompt in [100, 499] {
        view.usage.as_mut().unwrap().call.prompt = prompt;
        assert!(!shown(&view).iter().any(|s| s.opens == Opens::Context));
    }
    view.usage.as_mut().unwrap().call.prompt = 500;
    assert!(shown(&view).iter().any(|s| s.text == "ctx 50%"));
    view.usage = None;
    assert!(!shown(&view).iter().any(|s| s.opens == Opens::Context));
    view = full_bar();
    for window in [None, Some(0)] {
        view.models.rows[0].window = window;
        assert!(!shown(&view).iter().any(|s| s.opens == Opens::Context));
    }
    view = full_bar();
    view.models.now = None;
    assert!(!shown(&view)
        .iter()
        .any(|s| matches!(s.opens, Opens::Models | Opens::Context)));
}

#[test]
fn the_readouts_that_bear_on_the_next_turn_go_warm() {
    let mut view = full_bar();
    for (prompt, percent, color) in [
        (900, 90, WARM),
        (600, 60, DIM),
        (694, 69, DIM),
        (695, 70, WARM),
        (700, 70, WARM),
    ] {
        view.usage.as_mut().unwrap().call.prompt = prompt;
        let ctx = shown(&view)
            .into_iter()
            .find(|s| s.opens == Opens::Context)
            .unwrap();
        assert_eq!(ctx.text, format!("ctx {percent}%"));
        assert_eq!(ctx.style.fg, Some(color), "prompt={prompt}");
    }
    view.models.rows[0].key_present = false;
    let model = shown(&view).remove(0);
    assert_eq!(model.style.fg, Some(WARM));
    assert!(model.text.contains("no key"));
}

#[test]
fn the_status_bar_setting_takes_names_and_forgives_the_rest() {
    for setting in [None, Some(json!("model")), Some(json!(false))] {
        assert_eq!(bar_from(setting), BAR_DEFAULT.to_vec());
    }
    assert_eq!(
        bar_from(Some(json!(["cwd", "model"]))),
        vec!["cwd", "model"]
    );
    assert_eq!(
        bar_from(Some(json!(["model", "wheather", 7]))),
        vec!["model"]
    );
    assert!(
        bar_from(Some(json!([]))).is_empty(),
        "parsing preserves the empty selection"
    );
}

#[test]
fn empty_and_filtered_selections_keep_the_existing_default_fallback() {
    let mut view = full_bar();
    let expected: Vec<_> = shown(&view).into_iter().map(|s| s.text).collect();
    for names in [json!([]), json!(["unknown", 7])] {
        view.names = bar_from(Some(names));
        assert!(view.names.is_empty());
        assert_eq!(
            shown(&view).into_iter().map(|s| s.text).collect::<Vec<_>>(),
            expected
        );
    }
}

#[test]
fn readouts_preserve_order_and_name_navigation_without_panel_numbers() {
    let mut view = full_bar();
    view.background = vec![activity(true), activity(false), activity(false)];
    view.names = bar_from(Some(json!([
        "cwd",
        "background",
        "effort",
        "context",
        "model"
    ])));
    let got: Vec<_> = shown(&view)
        .into_iter()
        .map(|s| (s.text, s.opens))
        .collect();
    assert_eq!(
        got,
        vec![
            ("project".into(), Opens::Config),
            ("2 background".into(), Opens::Background),
            ("high".into(), Opens::Effort),
            ("ctx 90%".into(), Opens::Context),
            ("model".into(), Opens::Models),
        ]
    );
    view.background.retain(|job| job.standing);
    assert!(!shown(&view).iter().any(|s| s.opens == Opens::Background));
    view.workspace.clear();
    assert!(!shown(&view).iter().any(|s| s.opens == Opens::Config));
}

#[test]
fn readouts_use_the_callers_palette() {
    let view = full_bar();
    for (dim, warm) in [(DIM, WARM), (Color::Blue, Color::Yellow)] {
        let got = readouts(&view, dim, warm);
        assert_eq!(got[0].style.fg, Some(dim));
        assert_eq!(got[1].style.fg, Some(warm));
    }
}
