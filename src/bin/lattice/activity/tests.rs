use super::*;
use crate::terminal_host::theme::{DIM, RULE};
use std::borrow::Cow;

#[derive(Default)]
struct Activity {
    busy: bool,
    waiting: bool,
    tick: usize,
    working: bool,
    streaming: String,
    done_at: Option<usize>,
}
impl View for Activity {
    fn title(&self) -> &str {
        ""
    }
    fn entries(&self) -> &[lattice::view::Entry] {
        &[]
    }
    fn streaming(&self) -> &str {
        &self.streaming
    }
    fn input(&self) -> Cow<'_, str> {
        Cow::Borrowed("")
    }
    fn busy(&self) -> bool {
        self.busy
    }
    fn tick(&self) -> usize {
        self.tick
    }
    fn waiting(&self) -> bool {
        self.waiting
    }
    fn last_tool_running(&self) -> bool {
        self.working
    }
    fn done_at(&self) -> Option<usize> {
        self.done_at
    }
}

#[test]
fn waiting_wins_over_working_text_and_is_static_when_not_busy() {
    let mut view = Activity {
        waiting: true,
        working: true,
        ..Default::default()
    };
    let before = line(&view);
    assert_eq!(
        before.to_string(),
        " · Waiting for results — you can still type"
    );
    view.tick = 17;
    assert_eq!(line(&view), before);
    assert_eq!(before.spans[1].style.fg, Some(RULE));
    assert!(before.spans[1].style.add_modifier.contains(Modifier::BOLD));
    view.busy = true;
    assert!(line(&view).to_string().contains("Waiting for results"));
}

#[test]
fn running_work_and_streamed_output_select_working_while_done_uses_elapsed_ticks() {
    let mut view = Activity {
        busy: true,
        ..Default::default()
    };
    assert!(line(&view).to_string().contains(state_phrase(0, false)));
    view.working = true;
    assert!(line(&view).to_string().contains(state_phrase(0, true)));
    view.working = false;
    view.streaming = "answer".into();
    assert!(line(&view).to_string().contains(state_phrase(0, true)));
    view.busy = false;
    view.done_at = Some(100);
    view.tick = 100;
    assert!(line(&view).to_string().contains(state_phrase(0, true)));
    view.tick += DONE_SETTLE;
    assert_eq!(line(&view).to_string(), " · Done");
    assert_eq!(spinner(0), spinner(1));
    assert_ne!(spinner(1), spinner(2));
    assert_eq!(spinner(0), spinner(SPINNER.len() * SPIN_DIV));
}

#[test]
fn the_done_line_shrinks_wipes_then_types_done() {
    let w = "abcdef";
    assert_eq!(
        done_line(0, w),
        ('○', "abcdef".to_string()),
        "shrink, phrase intact"
    );
    assert_eq!(
        done_line(2, w),
        ('·', "abcdef".to_string()),
        "dot at smallest"
    );
    assert_eq!(done_line(3, w), ('·', "abcde".to_string()), "wipe begins");
    assert_eq!(done_line(8, w), ('·', String::new()), "phrase fully wiped");
    assert_eq!(done_line(9, w), ('·', "D".to_string()), "typing begins");
    assert_eq!(done_line(12, w), ('·', "Done".to_string()), "typed out");
    assert_eq!(
        done_line(999, w),
        ('·', "Done".to_string()),
        "holds at Done"
    );
    assert_eq!(
        done_line(8, "A journey of a thousand miles begins with one step").1,
        "",
        "any length clears by the same tick"
    );
}

#[test]
fn loop_settling_allowance_covers_every_phrase_and_unicode_wipe() {
    for word in THINKING_PHRASES.iter().chain(WORKING_PHRASES) {
        assert_eq!(done_line(8, word).1, "");
        assert_eq!(done_line(DONE_SETTLE / TYPE_DIV, word).1, "Done");
    }
    assert_eq!(
        state_phrase(THINKING_PHRASES.len(), false),
        state_phrase(0, false)
    );
    assert_eq!(
        state_phrase(WORKING_PHRASES.len(), true),
        state_phrase(0, true)
    );
    assert_ne!(state_phrase(0, false), state_phrase(0, true));
}

#[test]
fn sweep_arc_says_nothing_about_nothing() {
    assert!(sweep_arc("", 0, RULE, DIM).is_empty());
    assert!(sweep_arc("", 99, RULE, DIM).is_empty());
}

fn gap_starts_at() -> usize {
    (1..200)
        .find(|t| sweep_arc("abcdefgh", *t, RULE, DIM).len() == 1)
        .expect("the sweep ends somewhere")
}

#[test]
fn the_phrase_survives_every_tick() {
    for word in ["x", "心外无物", "Sapere aude", state_phrase(0, false)] {
        for tick in 0..(gap_starts_at() * 3) {
            let spans = sweep_arc(word, tick, RULE, DIM);
            let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
            assert_eq!(text, word, "tick {tick}");
        }
    }
}

#[test]
fn it_costs_spans_only_while_something_is_moving() {
    let gap = gap_starts_at();
    assert_eq!(sweep_arc("cats", gap / 2, RULE, DIM).len(), 4, "mid-sweep");
    assert_eq!(sweep_arc("cats", gap + 5, RULE, DIM).len(), 1, "at rest");
}

#[test]
fn nothing_ever_changes_hue_or_weight() {
    let (Color::Rgb(br, bg, bb), Color::Rgb(dr, dg, db)) = (RULE, DIM) else {
        panic!("the ramp needs two RGB ends");
    };
    for word in ["x", "心外无物", state_phrase(0, false)] {
        for tick in 0..(gap_starts_at() * 2) {
            for span in sweep_arc(word, tick, RULE, DIM) {
                assert!(
                    span.style.add_modifier.contains(Modifier::BOLD),
                    "{word:?} at {tick}: always heavy"
                );
                assert!(
                    !span.style.add_modifier.contains(Modifier::ITALIC),
                    "{word:?} at {tick}: never italic"
                );
                let Some(Color::Rgb(r, g, b)) = span.style.fg else {
                    panic!("{word:?} at {tick}: off the ramp");
                };
                for (v, lo, hi) in [(r, dr, br), (g, dg, bg), (b, db, bb)] {
                    assert!(
                        v >= lo && v <= hi,
                        "{word:?} at {tick}: {v} outside {lo}..={hi}"
                    );
                }
            }
        }
    }
}

#[test]
fn the_shadow_actually_falls_across_the_words() {
    let Color::Rgb(br, ..) = RULE else {
        unreachable!()
    };
    let Color::Rgb(dr, ..) = DIM else {
        unreachable!()
    };
    let midpoint = dr + (br - dr) / 2;
    for word in ["心外无物", "Sapere aude", state_phrase(0, false)] {
        let darkest = (0..gap_starts_at())
            .flat_map(|t| sweep_arc(word, t, RULE, DIM))
            .filter_map(|s| match s.style.fg {
                Some(Color::Rgb(r, ..)) => Some(r),
                _ => None,
            })
            .min()
            .expect("some character is styled");
        assert!(
            darkest < midpoint,
            "{word:?}: darkest {darkest} never past {midpoint}"
        );
    }
}

#[test]
fn the_shadow_has_left_before_the_sweep_ends() {
    let last = gap_starts_at() - 1;
    for word in ["心外无物", "Sapere aude", state_phrase(0, false)] {
        for span in sweep_arc(word, last, RULE, DIM) {
            assert_eq!(
                span.style.fg,
                Some(RULE),
                "{word:?}: still shadowed on the last frame"
            );
        }
    }
}

#[test]
fn the_shadow_has_not_arrived_when_the_sweep_begins() {
    for word in ["Sapere aude", state_phrase(0, false)] {
        for tick in [0, gap_starts_at()] {
            for span in sweep_arc(word, tick, RULE, DIM) {
                assert_eq!(
                    span.style.fg,
                    Some(RULE),
                    "{word:?}: dark at entry or rest, tick {tick}"
                );
            }
        }
    }
}

#[test]
fn a_non_rgb_ramp_keeps_the_supplied_bright_colour() {
    assert_eq!(shade(Color::White, Color::Black, 0.5), Color::White);
    assert_eq!(
        shade(Color::Rgb(100, 100, 100), Color::Black, 0.5),
        Color::Rgb(100, 100, 100)
    );
}
