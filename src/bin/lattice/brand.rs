//! The welcome scene: deterministic stars and a shaded Earth, independent of
//! live UI state. Shared colors and text clipping are sibling responsibilities.

use super::text::clip;
use super::theme::{
    ACCENT, ATMO, DIM, FG, LAND_DK, LAND_LT, LAND_MD, OCEAN_DK, OCEAN_LT, OCEAN_MD, STAR_BRIGHT,
};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

#[cfg(test)]
#[path = "brand/tests.rs"]
mod tests;

/// A stable per-cell hash — keeps the star field and the Earth's glyph texture
/// steady across redraws (no flicker) without a random-number dependency.
fn cell_hash(x: usize, y: usize) -> u64 {
    (x as u64).wrapping_mul(0x9E37_79B1) ^ (y as u64).wrapping_mul(0x85EB_CA77)
}

/// A deterministic sparse star: `Some(true)` = bright, `Some(false)` = faint.
fn star(x: usize, y: usize) -> Option<bool> {
    match cell_hash(x, y) % 29 {
        0 => Some(true),
        1 | 2 => Some(false),
        _ => None,
    }
}

/// Faint stars are nearly colourless: rods rather than cones do the seeing.
/// Only about one cell in twenty-nine carries a bright star's full colour.
const STAR_FAINT: [Color; 7] = [
    Color::Rgb(108, 116, 138),
    Color::Rgb(110, 116, 133),
    Color::Rgb(113, 116, 126),
    Color::Rgb(115, 116, 119),
    Color::Rgb(123, 115, 107),
    Color::Rgb(128, 114, 99),
    Color::Rgb(132, 113, 92),
];

/// Cumulative share of naked-eye stars by class, in 256ths — O 0.4%, B 19.5%,
/// A 21.9%, F 9.0%, G 12.1%, K 28.9%, M 8.2%.
/// Weighted like the sky rather than evenly: the O+B figure is the Bright Star
/// Catalogue's own (1808 of 9095); the rest follow magnitude 6.5 and brighter.
const STAR_MIX: [u16; 7] = [1, 51, 107, 130, 161, 235, 256];

/// Mix before class selection: the raw cell hash is linear, and its low bits
/// already decide presence. Without mixing, vertical neighbours share a class
/// 2.6% of the time instead of chance's 19.9%: correct shares, but a woven sky.
fn star_class(x: usize, y: usize) -> usize {
    let mut m = cell_hash(x, y) ^ 0x517c_c1b7_2722_0a95;
    m ^= m >> 33;
    m = m.wrapping_mul(0xff51_afd7_ed55_8ccd);
    m ^= m >> 29;
    let roll = (m % 256) as u16;
    STAR_MIX.iter().position(|&edge| roll < edge).unwrap_or(6)
}

fn star_cell(x: usize, y: usize) -> (char, Style) {
    match star(x, y) {
        Some(bright) => {
            let class = star_class(x, y);
            let ink = if bright {
                STAR_BRIGHT[class]
            } else {
                STAR_FAINT[class]
            };
            (if bright { '✦' } else { '∙' }, Style::default().fg(ink))
        }
        None => (' ', Style::default()),
    }
}

/// The embedded land/ocean bitmap: 360×180 cells (1° each), row-major, MSB
/// first, a set bit = land. Derived from NASA's public-domain Blue Marble
/// imagery (see assets/README.md).
const MASK_W: usize = 360;
const MASK_H: usize = 180;
static EARTH_MASK: &[u8] = include_bytes!("../../../assets/earth_mask.bin");

fn earth_land(lat: f64, lon: f64) -> bool {
    let mx = (((lon.to_degrees() + 180.0) / 360.0) * MASK_W as f64) as isize;
    let mx = mx.rem_euclid(MASK_W as isize) as usize;
    let my = ((((90.0 - lat.to_degrees()) / 180.0) * MASK_H as f64) as isize)
        .clamp(0, MASK_H as isize - 1) as usize;
    let idx = my * MASK_W + mx;
    (EARTH_MASK[idx / 8] >> (7 - idx % 8)) & 1 == 1
}

fn ramp_glyph(b: f64) -> char {
    const RAMP: &[u8] = b" .:-=+*oO0#%@";
    let i = (b.clamp(0.0, 1.0) * (RAMP.len() - 1) as f64).round() as usize;
    RAMP[i] as char
}

fn shade3(b: f64, dk: Color, md: Color, lt: Color) -> Color {
    if b < 0.42 {
        dk
    } else if b < 0.72 {
        md
    } else {
        lt
    }
}

fn shade4(b: f64, dk: Color, md: Color, lt: Color, hi: Color) -> Color {
    if b < 0.40 {
        dk
    } else if b < 0.62 {
        md
    } else if b < 0.82 {
        lt
    } else {
        hi
    }
}

/// Permanent ice is a latitude line; the bitmap only knows land from sea.
const ICE_LAT: f64 = 72.0;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Surface {
    Land,
    Sea,
    Ice,
}

fn surface_at(lat: f64, lon: f64) -> Surface {
    if lat.to_degrees().abs() >= ICE_LAT {
        Surface::Ice
    } else if earth_land(lat, lon) {
        Surface::Land
    } else {
        Surface::Sea
    }
}

fn overlay(grid: &mut [Vec<(char, Style)>], row: usize, col: usize, text: &str, style: Style) {
    if let Some(r) = grid.get_mut(row) {
        for (i, ch) in text.chars().enumerate() {
            if let Some(cell) = r.get_mut(col + i) {
                *cell = (ch, style);
            }
        }
    }
}

/// The Earth's axial tilt: the furthest north the sun stands directly overhead.
const TROPIC: f64 = 23.44;

/// Forward orthographic projection: x east, y north, z toward the viewer.
/// All angles are radians; (lat0, lon0) is the centre of the view.
fn project(lat: f64, lon: f64, lat0: f64, lon0: f64) -> (f64, f64, f64) {
    let d = lon - lon0;
    (
        lat.cos() * d.sin(),
        lat0.cos() * lat.sin() - lat0.sin() * lat.cos() * d.cos(),
        lat0.sin() * lat.sin() + lat0.cos() * lat.cos() * d.cos(),
    )
}

/// Specify the sun as a place, not an arbitrary light vector. The old vector
/// put the sun overhead at 61°N, outside the tropics, losing the southern day.
/// Both scenes use the June solstice; only the longitude offset sets how much
/// visible disc falls into night. View coordinates are in degrees.
fn brand_light(style: BrandStyle, view_lat: f64, view_lon: f64) -> (f64, f64, f64) {
    let sun_lon = match style {
        BrandStyle::Globe => view_lon - 23.0,
        BrandStyle::Binary => view_lon + 40.0,
    };
    project(
        TROPIC.to_radians(),
        sun_lon.to_radians(),
        view_lat.to_radians(),
        view_lon.to_radians(),
    )
}

/// Light only the sunward arc of the Binary globe's rim.
fn limb_facing(ex: f64, ey: f64, rho: f64, light: (f64, f64, f64)) -> f64 {
    if rho < 1e-6 {
        return 1.0;
    }
    ((ex * light.0 + ey * light.1) / rho).clamp(0.0, 1.0)
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum BrandStyle {
    Globe,
    Binary,
}

/// Terminal cell height ÷ width, including typical line spacing.
const DEFAULT_ASPECT: f64 = 2.35;

fn parse_aspect(s: Option<&str>) -> f64 {
    s.and_then(|s| s.trim().parse::<f64>().ok())
        .filter(|a| (0.5..=6.0).contains(a))
        .unwrap_or(DEFAULT_ASPECT)
}

fn cell_aspect() -> f64 {
    static ASPECT: std::sync::OnceLock<f64> = std::sync::OnceLock::new();
    *ASPECT.get_or_init(|| parse_aspect(std::env::var("LATTICE_ASPECT").ok().as_deref()))
}

/// Read once: Binary is the default, globe/earth select the shaded-glyph Earth.
fn brand_style() -> BrandStyle {
    static STYLE: std::sync::OnceLock<BrandStyle> = std::sync::OnceLock::new();
    *STYLE.get_or_init(
        || match std::env::var("LATTICE_BRAND").as_deref().map(str::trim) {
            Ok("globe") | Ok("earth") => BrandStyle::Globe,
            _ => BrandStyle::Binary,
        },
    )
}

fn earth_ink(lit: f64, surface: Surface, style: BrandStyle, hash: u64) -> (char, Style) {
    // Ice reflects much more than water. At the June pole the sun is only
    // 23.44° high; shading by incoming light alone incorrectly makes ice grey.
    let albedo = match surface {
        Surface::Ice => 0.30,
        Surface::Land => 0.07,
        Surface::Sea => 0.0,
    };
    let b = (lit + albedo).min(1.0);
    match style {
        BrandStyle::Globe => {
            let c = match surface {
                Surface::Ice => shade3(b, ICE_DK, ICE_MD, ICE_LT),
                Surface::Land => shade3(b, LAND_DK, LAND_MD, LAND_LT),
                Surface::Sea => shade3(b, OCEAN_DK, OCEAN_MD, OCEAN_LT),
            };
            (ramp_glyph(b), Style::default().fg(c))
        }
        BrandStyle::Binary => {
            if lit < 0.14 {
                return (' ', Style::default());
            }
            // Mix glyph and terrain choices separately to avoid diagonal stripes.
            let mut m = hash;
            m ^= m >> 13;
            m = m.wrapping_mul(0xff51_afd7_ed55_8ccd);
            m ^= m >> 15;
            let g = if m.is_multiple_of(2) { '0' } else { '1' };
            let c = match surface {
                Surface::Ice => shade3(b, ICE_DK, ICE_MD, ICE_LT),
                Surface::Land if (m >> 8).is_multiple_of(3) => {
                    shade4(b, TAN_DK, TAN_MD, TAN_LT, TAN_HI)
                }
                Surface::Land => shade4(b, GRN_DK, GRN_MD, GRN_LT, GRN_HI),
                Surface::Sea => shade4(b, SEA_DK, SEA_MD, SEA_LT, SEA_HI),
            };
            let mut st = Style::default().fg(c);
            if b > 0.58 {
                st = st.add_modifier(Modifier::BOLD);
            }
            (g, st)
        }
    }
}

fn cells_to_line(cells: &[(char, Style)]) -> Line<'static> {
    let mut spans: Vec<Span> = Vec::new();
    for &(ch, st) in cells {
        match spans.last_mut() {
            Some(s) if s.style == st => s.content.to_mut().push(ch),
            _ => spans.push(Span::styled(ch.to_string(), st)),
        }
    }
    Line::from(spans)
}

/// Full-height welcome scene, or a plain wordmark on tiny terminals.
/// Use the unambiguously narrow U+2219 dot, not CJK-ambiguous U+00B7.
pub(super) fn brand_art(meta: &str, width: usize, height: usize) -> Vec<Line<'static>> {
    let dim = Style::default().fg(DIM);
    if width < 28 || height < 6 {
        let mut wordmark = vec![
            Span::styled("✦ ", Style::default().fg(ACCENT)),
            Span::styled(
                "Lattice",
                Style::default().fg(FG).add_modifier(Modifier::BOLD),
            ),
        ];
        if 9 + 2 + lattice::VERSION.chars().count() <= width {
            wordmark.push(Span::styled(format!("  {}", lattice::VERSION), dim));
        }
        return vec![
            Line::from(wordmark),
            Line::from(Span::styled(clip(meta, width), dim)),
            Line::default(),
        ];
    }
    let (w, h) = (width, height);
    let cxf = (w as f64 - 1.0) / 2.0;
    let cyf = (h as f64 - 1.0) / 2.0;
    let aspect = cell_aspect();
    let ry = ((h as f64 / 2.0).min(w as f64 / (2.0 * aspect)) - 0.5).max(2.0);
    let rx = ry * aspect;
    // Blue Marble framing: Atlantic centre, Americas left, Africa/Europe right.
    let (view_lat, view_lon) = (20.0_f64, -30.0_f64);
    let lat0 = view_lat.to_radians();
    let lon0 = view_lon.to_radians();
    let (sin0, cos0) = (lat0.sin(), lat0.cos());
    let style = brand_style();
    let light = brand_light(style, view_lat, view_lon);
    let halo_hi = Style::default().fg(ATMO);
    let halo_lo = Style::default().fg(OCEAN_MD);
    let mut grid: Vec<Vec<(char, Style)>> = (0..h)
        .map(|y| {
            (0..w)
                .map(|x| {
                    let ex = (x as f64 - cxf) / rx;
                    let ey = (cyf - y as f64) / ry;
                    let rho = (ex * ex + ey * ey).sqrt();
                    if rho <= 1.0 {
                        // Invert the orthographic projection, sample the bitmap, then light.
                        let z = (1.0 - rho * rho).max(0.0).sqrt();
                        let (lat, lon) = if rho < 1e-6 {
                            (lat0, lon0)
                        } else {
                            let c = rho.asin();
                            let (sc, cc) = (c.sin(), c.cos());
                            let lat = (cc * sin0 + ey * sc * cos0 / rho).clamp(-1.0, 1.0).asin();
                            let lon = lon0 + (ex * sc).atan2(rho * cc * cos0 - ey * sc * sin0);
                            (lat, lon)
                        };
                        let lambert = (ex * light.0 + ey * light.1 + z * light.2).max(0.0);
                        let (amb, diff) = match style {
                            BrandStyle::Globe => (0.30, 0.55),
                            BrandStyle::Binary => (0.16, 0.72),
                        };
                        let core = amb + diff * lambert;
                        let rim_base = ((rho - 0.72) / 0.28).clamp(0.0, 1.0).powi(2) * 0.45;
                        let rim = match style {
                            BrandStyle::Globe => rim_base,
                            BrandStyle::Binary => rim_base * limb_facing(ex, ey, rho, light),
                        };
                        let lit = (core + rim).min(1.0);
                        earth_ink(lit, surface_at(lat, lon), style, cell_hash(x, y))
                    } else if rho < 1.0 + 2.0 / ry {
                        let t = (rho - 1.0) * ry;
                        match style {
                            BrandStyle::Globe => ('∙', if t < 0.9 { halo_hi } else { halo_lo }),
                            BrandStyle::Binary => {
                                // A continuous ring: white sunward, cyan flanks, faint blue shadow.
                                let f = limb_facing(ex, ey, rho, light);
                                let (thick, c) = if f > 0.6 {
                                    (1.5, LIMB_WHITE)
                                } else if f > 0.25 {
                                    (1.2, ATMO)
                                } else {
                                    (0.8, HALO_DIM)
                                };
                                if t < thick {
                                    ('∙', Style::default().fg(c))
                                } else {
                                    star_cell(x, y)
                                }
                            }
                        }
                    } else {
                        star_cell(x, y)
                    }
                })
                .collect()
        })
        .collect();
    const WORDMARK: &str = "Lattice";
    overlay(
        &mut grid,
        1,
        2,
        WORDMARK,
        Style::default().fg(FG).add_modifier(Modifier::BOLD),
    );
    // Whole build id or none. Write both surrounding gaps too, so globe digits
    // cannot abut the version and make it appear to name another build.
    let at = 2 + WORDMARK.chars().count() + 2;
    if at + lattice::VERSION.chars().count() < width {
        overlay(
            &mut grid,
            1,
            at - 2,
            &format!("  {} ", lattice::VERSION),
            dim,
        );
    }
    if h >= 4 {
        overlay(&mut grid, 2, 2, &clip(meta, width.saturating_sub(2)), dim);
    }
    grid.iter().map(|r| cells_to_line(r)).collect()
}

// Scene-local ramps. These are not the shared UI theme: only the Earth uses them.
const GRN_DK: Color = Color::Rgb(38, 60, 40);
const GRN_MD: Color = Color::Rgb(92, 126, 62);
const GRN_LT: Color = Color::Rgb(156, 182, 104);
const TAN_DK: Color = Color::Rgb(92, 74, 40);
const TAN_MD: Color = Color::Rgb(158, 128, 66);
const TAN_LT: Color = Color::Rgb(206, 182, 116);
const SEA_DK: Color = Color::Rgb(16, 30, 58);
const SEA_MD: Color = Color::Rgb(28, 58, 104);
const SEA_LT: Color = Color::Rgb(58, 100, 154);
/// Full sun brightens the surface's own hue; it must not bleach land/sea white.
const GRN_HI: Color = Color::Rgb(187, 214, 135);
const TAN_HI: Color = Color::Rgb(239, 214, 147);
const SEA_HI: Color = Color::Rgb(86, 130, 186);
/// White is reserved for the ice and sunlit atmosphere.
const ICE_DK: Color = Color::Rgb(105, 112, 118);
const ICE_MD: Color = Color::Rgb(170, 178, 186);
const ICE_LT: Color = Color::Rgb(232, 240, 248);
const LIMB_WHITE: Color = Color::Rgb(210, 232, 248);
const HALO_DIM: Color = Color::Rgb(52, 84, 128);
