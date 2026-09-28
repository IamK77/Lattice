use super::*;

fn day(prompt: u64) -> Day {
    Day {
        prompt,
        calls: 1,
        cached: prompt / 2,
        output: prompt / 4,
        reasoning: prompt / 8,
        conversations: 1,
    }
}

#[test]
fn historical_totals_use_last_recorded_day_and_month_not_the_clock() {
    let days = BTreeMap::from([
        ("2001-01-31".into(), day(100)),
        ("2001-02-01".into(), day(200)),
        ("2001-02-03".into(), day(400)),
    ]);
    let rows = rows(&days);
    let values = |key: &str| {
        rows.iter()
            .find(|(k, _)| k == key)
            .unwrap()
            .1
            .split_whitespace()
            .collect::<Vec<_>>()
    };
    assert_eq!(values("tokens in"), vec!["400", "600", "700"]);
    assert_eq!(values("conversations"), vec!["1", "2", "3"]);
    assert_eq!(values("cache hits"), vec!["50%", "50%", "50%"]);
    assert_eq!(values("thinking"), vec!["50%", "50%", "50%"]);
    assert_eq!(values("busiest day"), vec!["2001-02-03"]);
    assert!(rows
        .last()
        .unwrap()
        .1
        .contains("takes their numbers with them"));
}

#[test]
fn calendar_keeps_missing_days_and_global_peak_outside_the_visible_weeks() {
    let days = BTreeMap::from([
        ("2020-01-01".into(), day(400)),
        ("2024-03-04".into(), day(100)),
        ("2024-03-06".into(), day(200)),
    ]);
    let lines = calendar(&days, 24);
    assert_eq!(lines.len(), 12);
    assert!(lines[1].to_string().contains("Feb"));
    assert!(lines[1].to_string().contains("Mar"));
    for (weekday, glyph, color) in [
        (0, "▪ ", OCEAN_DK),
        (1, "· ", Color::Rgb(52, 58, 74)),
        (2, "▪ ", OCEAN_MD),
        (3, "· ", Color::Rgb(52, 58, 74)),
        (6, "· ", Color::Rgb(52, 58, 74)),
    ] {
        let cell = lines[weekday + 2].spans.last().unwrap();
        assert_eq!(cell.content, glyph, "weekday={weekday}");
        assert_eq!(cell.style.fg, Some(color), "weekday={weekday}");
    }
    assert!(lines[10]
        .to_string()
        .contains("busiest 2020-01-01 · 400 tokens"));
    for (width, weeks) in [(0, 8), (24, 8), (114, 53), (usize::MAX, 53)] {
        let rows = calendar(&days, width);
        assert!(rows[2..9].iter().all(|row| row.spans.len() == weeks + 1));
    }
}

#[test]
fn absent_history_and_zero_denominators_do_not_invent_rates() {
    assert!(calendar(&BTreeMap::new(), 80).is_empty());
    assert!(rows(&BTreeMap::new())[0].1.contains("no conversations"));
    let days = BTreeMap::from([("2001-01-01".into(), Day::default())]);
    for (key, value) in rows(&days) {
        if key == "cache hits" || key == "thinking" {
            assert_eq!(
                value.split_whitespace().collect::<Vec<_>>(),
                vec!["—", "—", "—"]
            );
        }
    }
    let lines = calendar(&days, 80);
    assert!(lines[2..9]
        .iter()
        .flat_map(|line| line.spans.iter().skip(1))
        .all(|span| span.content == "· "));
    let invalid = BTreeMap::from([("not-a-date".into(), day(1))]);
    assert!(calendar(&invalid, 80).is_empty());
}
