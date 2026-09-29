use super::*;

fn art_text(art: &[Line]) -> String {
    art.iter()
        .flat_map(|l| l.spans.iter().map(|s| s.content.to_string()))
        .collect()
}

#[test]
fn the_welcome_scene_has_stars_the_wordmark_and_the_endpoint() {
    let art = brand_art("dsk · ./ws", 80, 12);
    let text = art_text(&art);
    assert!(
        text.contains("Lattice"),
        "the wordmark rides in the space field"
    );
    assert!(text.contains("dsk · ./ws"), "the endpoint rides with it");
    assert!(
        text.contains('✦') || text.contains('∙'),
        "the star field is drawn"
    );
    assert!(text.contains('0'), "the Earth's disc is drawn as glyphs");
    assert!(
        text.contains(lattice::VERSION),
        "and the build this binary is, whole: {}",
        lattice::VERSION
    );
}

#[test]
fn the_build_id_is_separated_from_the_wordmark_at_every_width() {
    let joined = format!("Lattice  {} ", lattice::VERSION);
    for w in 28..100 {
        let text = art_text(&brand_art("m · w", w, 12));
        if text.contains(lattice::VERSION) {
            assert!(
                text.contains(&joined),
                "clean on both sides at width {w}, got: {}",
                text.lines().next().unwrap_or(&text)
            );
        }
    }
}

#[test]
fn the_build_id_is_shown_whole_or_not_at_all() {
    let full = lattice::VERSION.chars().count();
    for height in [4, 12] {
        for width in 9..(full + 40) {
            let plain = width < 28 || height < 6;
            // The full scene also reserves a trailing gap against globe digits.
            let needed = 2 + 7 + 2 + full + usize::from(!plain);
            let art = art_text(&brand_art("m · w", width, height));
            assert!(
                art.contains("Lattice"),
                "the wordmark still shows at {width}"
            );
            assert_eq!(
                art.contains(lattice::VERSION),
                width >= needed,
                "whole build id at width {width}, height {height}, needs {needed}"
            );
            if width < needed {
                for cut in (4..full).map(|n| &lattice::VERSION[..n]) {
                    assert!(
                        !art.contains(cut),
                        "no piece of the build id survives at width {width}: found {cut}"
                    );
                }
            }
        }
    }
}

#[test]
fn the_aspect_override_parses_and_guards_its_range() {
    assert_eq!(parse_aspect(Some("2.5")), 2.5, "a valid ratio is used");
    assert_eq!(
        parse_aspect(Some("  2.0 ")),
        2.0,
        "surrounding space is fine"
    );
    for value in [None, Some("nope"), Some("99"), Some("0")] {
        assert_eq!(parse_aspect(value), DEFAULT_ASPECT, "{value:?}");
    }
}

#[test]
fn the_binary_ink_layers_depth_by_weight_and_glyph() {
    assert_eq!(earth_ink(0.05, Surface::Sea, BrandStyle::Binary, 1).0, ' ');
    let (g, _) = earth_ink(0.5, Surface::Land, BrandStyle::Binary, 2);
    assert!(g == '0' || g == '1', "the disc is drawn in 0/1");
    let hi = earth_ink(0.95, Surface::Sea, BrandStyle::Binary, 3).1;
    assert!(
        hi.add_modifier.contains(Modifier::BOLD),
        "highlights carry bold, thicker ink"
    );
    let lo = earth_ink(0.3, Surface::Sea, BrandStyle::Binary, 4).1;
    assert!(
        !lo.add_modifier.contains(Modifier::BOLD),
        "shadows are thin, not bold"
    );
    let (gg, _) = earth_ink(0.5, Surface::Land, BrandStyle::Globe, 2);
    assert!(
        gg != '0' && gg != '1',
        "the Globe style keeps its density ramp"
    );
}

#[test]
fn the_land_mask_places_the_continents_and_oceans() {
    let d = |x: f64| x.to_radians();
    assert!(earth_land(d(2.0), d(20.0)), "equatorial Africa is land");
    assert!(earth_land(d(-10.0), d(-60.0)), "the Amazon is land");
    assert!(earth_land(d(-80.0), d(0.0)), "Antarctica is land");
    assert!(earth_land(d(60.0), d(100.0)), "Siberia is land");
    assert!(!earth_land(d(0.0), d(-140.0)), "the mid-Pacific is ocean");
    assert!(
        !earth_land(d(-40.0), d(-20.0)),
        "the South Atlantic is ocean"
    );
    assert!(!earth_land(d(20.0), d(65.0)), "the Arabian Sea is ocean");
}

#[test]
fn a_tiny_terminal_falls_back_to_a_plain_brand() {
    assert!(
        art_text(&brand_art("x", 20, 4)).contains("Lattice"),
        "still shows the wordmark"
    );
}

fn reads_as_white(c: Color) -> bool {
    let Color::Rgb(r, g, b) = c else {
        panic!("the globe palette is spelled in RGB");
    };
    let spread = r.max(g).max(b) - r.min(g).min(b);
    r.min(g).min(b) >= 190 && spread <= 34
}

/// White means ice, not sun-bleached land or water. Walk the terrain hash too.
#[test]
fn nothing_but_ice_is_ever_drawn_white() {
    for style in [BrandStyle::Globe, BrandStyle::Binary] {
        for surface in [Surface::Land, Surface::Sea] {
            for step in 0..=100 {
                let lit = f64::from(step) / 100.0;
                for hash in 0..12u64 {
                    let ink = earth_ink(lit, surface, style, hash).1;
                    let Some(c) = ink.fg else { continue };
                    assert!(
                        !reads_as_white(c),
                        "{style:?} {surface:?} at lit {lit:.2}: {c:?} reads as white"
                    );
                }
            }
        }
        let cap = earth_ink(1.0, Surface::Ice, style, 0).1.fg.unwrap();
        assert!(reads_as_white(cap), "{style:?}: lit ice should be white");
    }
}

#[test]
fn full_sun_reaches_the_top_of_the_ramp() {
    for (surface, wanted) in [
        (Surface::Sea, vec![SEA_HI]),
        (Surface::Land, vec![GRN_HI, TAN_HI]),
    ] {
        let got = earth_ink(1.0, surface, BrandStyle::Binary, 0).1.fg.unwrap();
        assert!(
            wanted.contains(&got),
            "{surface:?} in full sun is {got:?}, not one of {wanted:?}"
        );
    }
}

#[test]
fn each_ramp_climbs_to_its_brightest_stop() {
    let lum = |c: Color| {
        let Color::Rgb(r, g, b) = c else {
            panic!("the globe palette is spelled in RGB");
        };
        0.2126 * f64::from(r) + 0.7152 * f64::from(g) + 0.0722 * f64::from(b)
    };
    for (lt, hi, name) in [
        (GRN_LT, GRN_HI, "green"),
        (TAN_LT, TAN_HI, "tan"),
        (SEA_LT, SEA_HI, "sea"),
    ] {
        assert!(
            lum(hi) > lum(lt),
            "{name}: {hi:?} is no brighter than {lt:?}"
        );
    }
}

#[test]
fn the_north_cap_is_in_view_and_the_south_one_is_behind() {
    let (lat0, lon0) = (20.0_f64.to_radians(), (-30.0_f64).to_radians());
    let north = project(90.0_f64.to_radians(), 0.0, lat0, lon0);
    let south = project((-90.0_f64).to_radians(), 0.0, lat0, lon0);
    assert!(north.2 > 0.0, "the north pole should face us: {north:?}");
    assert!(
        north.1 > 0.9,
        "and ride near the top of the disc: {north:?}"
    );
    assert!(south.2 < 0.0, "the south pole should be hidden: {south:?}");
}

#[test]
fn the_cap_still_reads_as_ice_under_a_low_sun() {
    let polar = 0.16 + 0.72 * TROPIC.to_radians().sin();
    for style in [BrandStyle::Globe, BrandStyle::Binary] {
        let ink = earth_ink(polar, Surface::Ice, style, 0).1.fg.unwrap();
        assert_eq!(
            ink, ICE_LT,
            "{style:?}: at the June pole (lit {polar:.3}) the cap draws {ink:?}"
        );
    }
}

#[test]
fn the_caps_start_at_the_ice_line() {
    let d = |x: f64| x.to_radians();
    assert_eq!(surface_at(d(88.0), d(0.0)), Surface::Ice, "the Arctic");
    assert_eq!(surface_at(d(-85.0), d(0.0)), Surface::Ice, "Antarctica");
    assert_eq!(surface_at(d(60.0), d(100.0)), Surface::Land, "Siberia");
    assert_eq!(surface_at(d(0.0), d(-140.0)), Surface::Sea, "the Pacific");
}

fn subsolar_latitude(light: (f64, f64, f64), view_lat: f64) -> f64 {
    let lat0 = view_lat.to_radians();
    (lat0.cos() * light.1 + lat0.sin() * light.2).asin()
}

#[test]
fn the_sun_never_stands_outside_the_tropics() {
    for style in [BrandStyle::Globe, BrandStyle::Binary] {
        for (view_lat, view_lon) in [(20.0, -30.0), (0.0, 0.0), (48.0, 137.0)] {
            let at =
                subsolar_latitude(brand_light(style, view_lat, view_lon), view_lat).to_degrees();
            assert!(
                at.abs() <= TROPIC + 1e-9,
                "{style:?} seen from ({view_lat}, {view_lon}): sun overhead at {at:.1}°"
            );
        }
    }
}

/// Equal-area sampling checks the consequence of the old invalid sun vector.
#[test]
fn daylight_still_reaches_deep_into_the_south() {
    for style in [BrandStyle::Globe, BrandStyle::Binary] {
        let phi = subsolar_latitude(brand_light(style, 20.0, -30.0), 20.0);
        let (mut lit, mut all) = (0usize, 0usize);
        for i in 0..600 {
            let lat = (-1.0 + (f64::from(i) + 0.5) / 600.0).asin();
            for j in 0..360 {
                let dlon = (f64::from(j) + 0.5 - 180.0).to_radians();
                all += 1;
                if lat.sin() * phi.sin() + lat.cos() * phi.cos() * dlon.cos() > 0.0 {
                    lit += 1;
                }
            }
        }
        let share = lit as f64 / all as f64;
        assert!(
            share > 0.33,
            "{style:?}: only {:.1}% of the southern hemisphere is in daylight",
            share * 100.0
        );
    }
}

#[test]
fn the_spectral_mix_accounts_for_every_roll() {
    assert!(
        STAR_MIX.windows(2).all(|p| p[0] < p[1]),
        "the shares are cumulative, so each edge is past the one before it"
    );
    assert_eq!(
        *STAR_MIX.last().unwrap(),
        256,
        "the last edge is the whole sky"
    );
    assert_eq!(STAR_MIX.len(), STAR_BRIGHT.len());
    assert_eq!(STAR_MIX.len(), STAR_FAINT.len());
}

#[test]
fn the_sky_is_mixed_in_the_proportions_the_table_states() {
    let mut seen = [0usize; STAR_MIX.len()];
    for y in 0..400 {
        for x in 0..400 {
            seen[star_class(x, y)] += 1;
        }
    }
    let total = 400.0 * 400.0;
    let mut want_from = 0u16;
    for (class, &edge) in STAR_MIX.iter().enumerate() {
        let want = f64::from(edge - want_from) / 256.0;
        want_from = edge;
        let got = seen[class] as f64 / total;
        assert!(
            (got - want).abs() < 0.02,
            "class {class}: the table says {want:.3} of the sky, the field gives {got:.3}"
        );
    }
}

/// Bound correlation on both sides: the unmixed hash matched too rarely.
#[test]
fn neighbouring_stars_are_no_more_alike_than_chance_and_no_less() {
    let mut share = [0f64; STAR_MIX.len()];
    let mut from = 0u16;
    for (class, &edge) in STAR_MIX.iter().enumerate() {
        share[class] = f64::from(edge - from) / 256.0;
        from = edge;
    }
    let chance: f64 = share.iter().map(|p| p * p).sum();
    for (dx, dy) in [(1, 0), (0, 1), (1, 1)] {
        let (mut same, mut total) = (0usize, 0usize);
        for y in 0..300 {
            for x in 0..300 {
                total += 1;
                if star_class(x, y) == star_class(x + dx, y + dy) {
                    same += 1;
                }
            }
        }
        let got = same as f64 / total as f64;
        assert!(
            (got - chance).abs() < 0.03,
            "offset ({dx},{dy}): neighbours agree {got:.4} of the time, chance is {chance:.4}"
        );
    }
}

#[test]
fn a_faint_star_carries_less_colour_than_a_bright_one() {
    let spread = |c: Color| {
        let Color::Rgb(r, g, b) = c else {
            panic!("the star palette is spelled in RGB");
        };
        u32::from(r.max(g).max(b) - r.min(g).min(b))
    };
    for class in 0..STAR_MIX.len() {
        assert!(
            spread(STAR_FAINT[class]) < spread(STAR_BRIGHT[class]),
            "class {class}: faint {:?} is no greyer than bright {:?}",
            STAR_FAINT[class],
            STAR_BRIGHT[class]
        );
    }
}
