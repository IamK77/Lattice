use super::*;

#[test]
fn every_neutral_is_the_same_white_as_the_stars() {
    for (name, c) in [
        ("FG", FG),
        ("DIM", DIM),
        ("CODE_FG", CODE_FG),
        ("RULE", RULE),
        ("USER_BG", USER_BG),
        ("CODE_BG", CODE_BG),
        ("star white", STAR_BRIGHT[3]),
    ] {
        let Color::Rgb(r, g, b) = c else {
            panic!("{name} should be spelled in RGB");
        };
        assert!(
            r < g && g < b,
            "{name} {c:?}: the star white leans cool, in that order"
        );
        let spread = u32::from(b - r);
        assert!(
            (4..=16).contains(&spread),
            "{name} {c:?}: channel spread {spread} is outside the shared band"
        );
    }
}

#[test]
fn the_accents_are_the_hottest_and_coolest_stars() {
    assert_eq!(ACCENT, STAR_BRIGHT[0], "the cool accent is the O class");
    assert_eq!(
        WARM,
        STAR_BRIGHT[STAR_BRIGHT.len() - 1],
        "the warm accent is the M class"
    );
}

#[test]
fn failure_is_not_just_a_colder_star() {
    let ch = |c: Color| {
        let Color::Rgb(r, g, b) = c else {
            panic!("RGB");
        };
        (i32::from(r), i32::from(g), i32::from(b))
    };
    let (_, wg, wb) = ch(WARM);
    let (_, eg, eb) = ch(ERR);
    assert!(wg > wb, "the warm accent is amber: {WARM:?}");
    assert!(eg < eb, "failure is not amber: {ERR:?}");
}

#[test]
fn the_classes_run_blue_to_orange_in_order() {
    let warmth = |c: Color| {
        let Color::Rgb(r, _, b) = c else {
            panic!("the star palette is spelled in RGB");
        };
        i32::from(r) - i32::from(b)
    };
    for pair in STAR_BRIGHT.windows(2) {
        assert!(
            warmth(pair[0]) < warmth(pair[1]),
            "{:?} should be cooler than {:?}",
            pair[0],
            pair[1]
        );
    }
}
