use super::*;
use serde_json::json;

fn row(key: &str, standing: bool, label: &str) -> HistoricalRow {
    HistoricalRow {
        kind: "timer",
        key: key.into(),
        label: label.into(),
        standing,
        fires: 3,
        ledger: Some(format!("/fixture/{label}")),
    }
}
#[test]
fn historical_background_can_fold_without_a_display_cache_or_clock() {
    let mut history = History::default();
    history.apply(Mutation::Start(row("timer:1", true, "persistent")));
    history.apply(Mutation::Wake("timer:1".into()));
    assert_eq!(history.rows()[0].fires, 4);
    assert_eq!(
        history.rows()[0].ledger.as_deref(),
        Some("/fixture/persistent")
    );
    history.apply(Mutation::Cancel("timer:1".into()));
    assert!(history.rows().is_empty());
}

fn sync(background: &Background) {
    assert_eq!(
        background.history().rows(),
        background
            .rows()
            .iter()
            .map(HistoricalRow::from_display)
            .collect::<Vec<_>>()
    );
}

#[test]
fn replacing_nonadjacent_duplicate_keys_preserves_survivor_progress_and_order() {
    let mut background = Background::default();
    background.restore(
        History::new(vec![
            row("same", true, "first"),
            row("other", true, "middle"),
            row("same", true, "last"),
        ]),
        77,
    );
    background.fixture_edit(1, |row| {
        row.tools = 11;
        row.tokens = 22;
        row.read_len = 33;
    });
    background.apply(Mutation::Start(row("same", false, "new")), 9);
    sync(&background);
    assert_eq!(
        background
            .rows()
            .iter()
            .map(|row| row.label.as_str())
            .collect::<Vec<_>>(),
        ["middle", "new"]
    );
    let survivor = &background.rows()[0];
    assert_eq!(
        (
            survivor.since,
            survivor.tools,
            survivor.tokens,
            survivor.read_len
        ),
        (77, 11, 22, 33)
    );
    let new = &background.rows()[1];
    assert_eq!(
        (new.since, new.tools, new.tokens, new.read_len),
        (9, 0, 0, 0)
    );
}

#[test]
fn wake_keeps_distinct_duplicate_progress_but_one_finished_row_removes_the_whole_key() {
    let mut background = Background::default();
    background.restore(
        History::new(vec![
            row("same", true, "first"),
            row("other", true, "middle"),
            row("same", true, "last"),
        ]),
        77,
    );
    background.fixture_edit(0, |row| {
        row.tools = 1;
        row.tokens = 2;
        row.read_len = 3;
    });
    background.fixture_edit(2, |row| {
        row.tools = 4;
        row.tokens = 5;
        row.read_len = 6;
    });
    let before = background.rows().to_vec();
    background.apply(Mutation::Wake("same".into()), 9);
    sync(&background);
    let mut expected = before;
    expected[0].fires += 1;
    expected[2].fires += 1;
    assert_eq!(background.rows(), expected);
    background.fixture_edit(2, |row| row.standing = false);
    background.apply(Mutation::Wake("same".into()), 10);
    sync(&background);
    assert_eq!(background.rows().len(), 1);
    assert_eq!(background.rows()[0].label, "middle");
}

#[test]
fn synchronous_expert_outcomes_never_become_background_work() {
    for result in [
        json!({"job":18,"text":"review complete","failed":null,"interrupted":null}),
        json!({"job":19,"text":"","failed":null,"interrupted":null}),
        json!({"job":20,"text":null,"failed":"failed"}),
        json!({"job":21,"text":null,"interrupted":"cancelled"}),
    ] {
        assert!(started("ask", &json!({"background":false}), &result).is_none());
    }
    for args in [json!({}), json!({"background":true})] {
        assert_eq!(
            started("ask", &args, &json!({"job":22})).unwrap().key,
            "expert:22"
        );
    }
}

#[test]
fn receipt_shape_precedes_tool_name_and_job_values_keep_their_legacy_domain() {
    let timer = started(
        "unknown",
        &json!({"interval_ms":0}),
        &json!({"timer":0,"watch":2,"job":3}),
    )
    .unwrap();
    assert_eq!(timer.kind, "timer");
    assert_eq!(timer.key, "timer:0");
    assert_eq!(timer.label, "every 0s");
    assert!(timer.standing);
    let watch = started(
        "unknown",
        &json!({}),
        &json!({"timer":-1,"watch":0,"job":3}),
    )
    .unwrap();
    assert_eq!(watch.kind, "watch");
    assert_eq!(watch.label, "");
    for job in [json!(0), json!("0"), json!("left:7")] {
        assert!(started("ask", &json!({}), &json!({"job":job})).is_some());
    }
    for job in [json!(""), json!(-1), json!(null), json!(1.5), json!(true)] {
        assert!(started("ask", &json!({}), &json!({"job":job})).is_none());
    }
    assert!(started("CancelExpert", &json!({}), &json!({"job":7})).is_none());
    assert!(started("Run", &json!({}), &json!({"job":7})).is_none());
    assert!(started("Run", &json!({}), &json!({"job":7,"background":"true"})).is_none());
    let command = started(
        "Run",
        &json!({"command":"one two three four five six seven eight nine"}),
        &json!({"job":7,"background":true}),
    )
    .unwrap();
    assert_eq!(command.label, "one two three four five six seven eight");
}
