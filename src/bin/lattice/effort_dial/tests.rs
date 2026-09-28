use super::*;

fn deepseek() -> EffortView {
    EffortView {
        rungs: vec!["high".into(), "max".into()],
        now: Some("high".into()),
    }
}

#[test]
fn the_dial_is_two_lines() {
    assert_eq!(dial_lines(&deepseek(), 4).len(), 2);
}

#[test]
fn the_scale_holds_its_columns_as_the_cursor_travels() {
    let effort = deepseek();
    let width = |cursor| {
        dial_lines(&effort, cursor)[0]
            .spans
            .iter()
            .map(|span| span.content.chars().count())
            .sum::<usize>()
    };
    let first = width(0);
    for cursor in 1..dial_positions().len() {
        assert_eq!(
            width(cursor),
            first,
            "the row changed width with the cursor at {cursor}"
        );
    }
}

#[test]
fn the_readout_names_the_substitution_and_the_cost() {
    let effort = deepseek();
    let at = |word: &str| {
        let i = dial_positions().iter().position(|w| *w == word).unwrap();
        dial_readout(&effort, i)
    };
    assert!(at("xhigh").contains("sends max"), "{}", at("xhigh"));
    assert!(at("low").contains("sends high"), "{}", at("low"));
    assert!(
        at("high").contains("current") && !at("high").contains("cold cache"),
        "standing still costs nothing: {}",
        at("high")
    );
    assert!(
        at("max").contains("cold cache"),
        "a change says what it costs BEFORE it is made: {}",
        at("max")
    );
    assert_eq!(at("off"), "no thinking at all");
}

#[test]
fn the_cursor_opens_on_the_setting_in_force() {
    assert_eq!(dial_positions()[dial_home(&deepseek())], "high");
    let never_set = EffortView {
        rungs: vec!["high".into()],
        now: None,
    };
    assert_eq!(dial_positions()[dial_home(&never_set)], "off");
}

#[test]
fn absent_and_explicit_off_remain_distinct_and_aliases_are_dim() {
    let mut effort = deepseek();
    effort.now = None;
    assert!(dial_readout(&effort, 0).contains("no parameter is sent"));
    effort.now = Some("off".into());
    assert_eq!(dial_readout(&effort, 0), "no thinking at all");
    let lines = dial_lines(&effort, 0);
    for (name, color) in [("off", FG), ("low", DIM), ("high", FG), ("max", FG)] {
        let span = lines[0]
            .spans
            .iter()
            .find(|span| span.content == name)
            .unwrap();
        assert_eq!(span.style.fg, Some(color), "{name}");
    }
    effort.rungs.clear();
    let lines = dial_lines(&effort, usize::MAX);
    assert_eq!(lines, dial_lines(&effort, dial_positions().len() - 1));
    assert_eq!(
        lines[0]
            .spans
            .iter()
            .find(|span| span.content == "low")
            .unwrap()
            .style
            .fg,
        Some(FG)
    );
}
