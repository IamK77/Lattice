use super::*;
use crate::terminal_host::activity::{self, DONE_SETTLE};
use lattice::view::{Entry, View};
use std::borrow::Cow;

struct Fixture {
    summary: TurnSummary,
    status: &'static str,
    approval: bool,
    waiting: bool,
    tool: bool,
}
impl View for Fixture {
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
        self.summary.running && !self.waiting
    }
    fn waiting(&self) -> bool {
        self.waiting
    }
    fn tick(&self) -> usize {
        DONE_SETTLE
    }
    fn done_at(&self) -> Option<usize> {
        Some(0)
    }
    fn turn_status(&self) -> &str {
        self.status
    }
    fn turn_summary(&self) -> Option<TurnSummary> {
        Some(self.summary)
    }
    fn pending_auth(&self) -> Option<&str> {
        self.approval.then_some("approval")
    }
    fn last_tool_running(&self) -> bool {
        self.tool
    }
}
fn fixture() -> Fixture {
    Fixture {
        summary: TurnSummary {
            elapsed_ms: Some(18_000),
            input: TokenCount {
                known: Some(12_400),
                complete: true,
            },
            output: TokenCount {
                known: Some(860),
                complete: true,
            },
            running: true,
            model_pending: true,
        },
        status: "Done",
        approval: false,
        waiting: false,
        tool: false,
    }
}

#[test]
fn turn_summary_preserves_counts_before_the_quote_at_narrow_widths() {
    let f = fixture();
    let wide = activity::line(&f, 100).to_string();
    assert!(
        wide.contains("Nothing is good or bad") && wide.contains("(18s · ↑12.4k ↓860 · model)"),
        "{wide}"
    );
    let narrow = activity::line(&f, 34).to_string();
    assert!(
        narrow.contains("18s") && narrow.contains("↑12.4k ↓860"),
        "{narrow}"
    );
    assert!(!narrow.contains("Nothing is good or bad"));
    assert_eq!(
        activity::line(&f, 100).to_string(),
        wide,
        "resize is pure layout"
    );
}

#[test]
fn turn_summary_distinguishes_unknown_partial_and_zero_without_estimates() {
    assert_eq!(count(TokenCount::default()), "—");
    assert_eq!(
        count(TokenCount {
            known: Some(0),
            complete: true
        }),
        "0"
    );
    assert_eq!(
        count(TokenCount {
            known: Some(0),
            complete: false
        }),
        "≥0"
    );
    assert_eq!(
        count(TokenCount {
            known: Some(1234),
            complete: false
        }),
        "≥1.2k"
    );
    assert_eq!(compact(999), "999");
    assert_eq!(compact(1000), "1k");
    assert_eq!(compact(999_999), "999.9k");
    assert_eq!(compact(1_000_000), "1M");
    assert_eq!(compact(u64::MAX), "18.4E");
    assert_eq!(elapsed(None), "—");
    assert_eq!(elapsed(Some(59_999)), "59s");
    assert_eq!(elapsed(Some(60_000)), "1m00s");
    assert_eq!(elapsed(Some(3_661_000)), "1h01m");
    assert_eq!(elapsed(Some(90_061_000)), "1d01h");
}

#[test]
fn turn_summary_observed_states_and_terminal_labels_remain_in_the_same_row() {
    let mut f = fixture();
    for (approval, waiting, tool, expected) in [
        (false, false, false, "model"),
        (false, false, true, "tools"),
        (false, true, false, "waiting"),
        (true, false, true, "approval"),
    ] {
        f.approval = approval;
        f.waiting = waiting;
        f.tool = tool;
        let line = activity::line(&f, 100).to_string();
        assert!(line.contains(expected), "{line}");
        assert!(!line.contains("reasoning"));
    }
    f.summary.running = false;
    f.waiting = false;
    f.approval = false;
    f.tool = false;
    for status in ["Done", "Interrupted", "Timed out"] {
        f.status = status;
        for width in [36, 45, 80] {
            let line = activity::line(&f, width).to_string();
            assert!(
                line.contains(status) && line.contains("↑12.4k ↓860"),
                "{width}: {line}"
            );
            assert!(!line.contains("model"));
        }
    }
}

#[test]
fn turn_summary_all_widths_fit_the_real_terminal_buffer_without_wrapping() {
    let mut f = fixture();
    f.summary.input = TokenCount {
        known: Some(1200),
        complete: false,
    };
    f.summary.output = TokenCount::default();
    for status in ["Done", "Interrupted", "Timed out"] {
        f.status = status;
        for running in [false, true] {
            f.summary.running = running;
            for width in 0..100 {
                let line = activity::line(&f, width);
                assert!(line.width() <= width, "{width}: {line}");
                let mut buffer = ratatui::buffer::Buffer::empty(ratatui::layout::Rect::new(
                    0,
                    0,
                    width as u16,
                    2,
                ));
                use ratatui::widgets::Widget;
                ratatui::widgets::Paragraph::new(line.clone()).render(buffer.area, &mut buffer);
                if width > 0 {
                    assert!(buffer.content[width..]
                        .iter()
                        .all(|cell| cell.symbol() == " "));
                }
                let text = line.to_string();
                if text.contains('↑') {
                    assert!(text.contains("↑≥1.2k"), "{text}");
                }
                if text.contains('↓') {
                    assert!(text.contains("↓—"), "{text}");
                }
            }
        }
    }
    for word in ["心外无物", "→→→→→", "a\u{301}bcdef"] {
        for width in 0..10 {
            assert!(cells(&clip(word, width)) <= width);
        }
    }
}

#[test]
fn turn_summary_wait_clock_requests_a_frame_only_when_the_displayed_second_changes() {
    let mut f = fixture();
    f.waiting = true;
    assert!(!f.busy());
    assert_eq!(activity::clock_second(&f), Some(18));
    f.summary.elapsed_ms = Some(18_999);
    assert_eq!(activity::clock_second(&f), Some(18));
    f.summary.elapsed_ms = Some(19_000);
    assert_eq!(activity::clock_second(&f), Some(19));
    f.summary.running = false;
    assert_eq!(activity::clock_second(&f), None);
}
