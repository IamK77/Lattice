use super::*;
use lattice::view::{Entry, Material, ModelRow, ModelView, UsageReport};
use std::borrow::Cow;

struct Context {
    window: Option<u64>,
    usage: Option<UsageReport>,
    parts: Material,
    compaction: Option<lattice::components::context_gate::CompactionStatus>,
}

impl Default for Context {
    fn default() -> Self {
        let call = lattice::Usage {
            prompt: 400,
            ..Default::default()
        };
        Self {
            window: Some(1000),
            usage: Some(UsageReport {
                call,
                turn: call,
                session: call,
            }),
            parts: vec![("system prompt", 25), ("tool results", 75)],
            compaction: None,
        }
    }
}

impl View for Context {
    fn title(&self) -> &str {
        "context"
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
    fn models(&self) -> ModelView {
        ModelView {
            rows: vec![ModelRow {
                id: "model".into(),
                model: "model".into(),
                endpoint: String::new(),
                dialect: String::new(),
                window: self.window,
                rungs: vec![],
                accepts_images: false,
                key_env: String::new(),
                key_present: true,
            }],
            now: Some(0),
        }
    }
    fn usage(&self) -> Option<UsageReport> {
        self.usage
    }
    fn composition(&self) -> Material {
        self.parts.clone()
    }
    fn growth(&self) -> (Material, Material) {
        (self.parts.clone(), self.parts.clone())
    }
    fn compaction_status(&self) -> Option<&lattice::components::context_gate::CompactionStatus> {
        self.compaction.as_ref()
    }
}

#[test]
fn compaction_panel_explains_the_pause_and_retry_without_usage_data() {
    use lattice::components::context_gate::{CompactionFailure, CompactionStatus};
    let mut view = Context {
        window: None,
        usage: None,
        compaction: Some(CompactionStatus {
            in_flight: false,
            failure: Some(CompactionFailure {
                event: "failed-call".into(),
                code: "stream_read_error".into(),
                message: "fixture transport failure".into(),
            }),
        }),
        ..Default::default()
    };
    let output = rows(&view);
    assert!(output[0].1.contains("paused"));
    assert!(output
        .iter()
        .any(|(key, value)| key == "failure"
            && value == "stream_read_error: fixture transport failure"));
    assert!(output
        .iter()
        .any(|(key, value)| key == "recorded at" && value == "failed-call"));
    assert!(output.iter().any(|(_, value)| value.contains("/compact")));
    assert!(output
        .iter()
        .any(|(_, value)| value.contains("nothing sent yet")));
    view.compaction.as_mut().unwrap().in_flight = true;
    assert!(rows(&view)[0].1.contains("remains paused"));
    view.compaction.as_mut().unwrap().failure = None;
    assert_eq!(rows(&view)[0].1, "Compaction in progress");
    view.compaction.as_mut().unwrap().in_flight = false;
    assert_eq!(rows(&view)[0].1, "No recorded compaction pause");
}

#[test]
fn every_kind_of_material_is_drawn_in_its_own_colour() {
    let kinds = [
        "tool results",
        "model replies",
        "tool declarations",
        "system prompt",
        "what you said",
        "thinking",
        "condensed summaries",
    ];
    let mut seen = std::collections::HashSet::new();
    for kind in kinds {
        assert!(
            seen.insert(format!("{:?}", material_colour(kind))),
            "{kind} reuses another kind's colour"
        );
    }
}

#[test]
fn context_cells_follow_measured_occupancy_and_bounded_column_counts() {
    let view = Context::default();
    let lines = picture(&view, 26);
    let cells: Vec<_> = lines[2..7]
        .iter()
        .flat_map(|line| line.spans.iter().skip(1))
        .collect();
    assert_eq!(cells.len(), 100);
    assert_eq!(cells.iter().filter(|span| span.content == "█").count(), 40);
    for (width, columns) in [(0, 20), (26, 20), (102, 96), (usize::MAX, 96)] {
        let lines = picture(&view, width);
        assert!(lines[2..7]
            .iter()
            .all(|line| line.spans.len() == columns + 1));
    }
    let text = lines
        .iter()
        .map(Line::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text.contains("40% full · 400 of 1,000 · 600 free · 40 cells"));
    assert!(text
        .lines()
        .find(|line| line.contains("system prompt"))
        .unwrap()
        .contains("re-sent"));
    assert!(!text
        .lines()
        .find(|line| line.contains("tool results"))
        .unwrap()
        .contains("re-sent"));
    assert!(text.contains("split by size"));
}

#[test]
fn missing_measurement_or_denominator_never_invents_a_proportion() {
    let mut view = Context {
        usage: None,
        ..Default::default()
    };
    assert!(picture(&view, 80).is_empty());
    assert!(rows(&view)[0].1.contains("nothing sent yet"));
    view.usage = Context::default().usage;
    for window in [None, Some(0)] {
        view.window = window;
        assert!(picture(&view, 80).is_empty());
        assert!(rows(&view)[0].1.contains("declares no window"));
    }
    view.window = Some(1000);
    view.parts.clear();
    assert!(picture(&view, 80).is_empty());
    assert!(rows(&view)[0].1.contains("40%"));
    view.parts = vec![("tool results", 0)];
    assert!(picture(&view, 80).is_empty());
}
