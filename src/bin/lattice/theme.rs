//! Shared terminal colors. The welcome scene and ordinary UI both consume
//! this palette; neither owns the other's rendering implementation.

use ratatui::style::Color;

/// The seven spectral classes a star can be drawn as, coldest colour first:
/// O B A F G K M — the order every star chart runs them in, blue through white
/// to orange.
///
/// The HUE is physics: a blackbody spectrum at the class's temperature, through
/// the CIE 1931 colour matching functions into sRGB. Above about 7000 K the
/// Planck locus points in one direction (blue), so O, B and A share a hue and
/// differ only in how far along it they sit; below about 4500 K it points at
/// amber, which is where K and M sit.
///
/// The CHROMA is convention, and deliberately so. Physically the Sun is white
/// (its chroma is 0.018 in OKLab, which is below the level where a hue angle
/// means anything) and Sirius is very nearly white too. Star charts have drawn
/// them yellow and icy blue for two centuries because that is what tells the
/// classes apart on paper, and the same is true on a terminal. So F borrows the
/// blue hue and G borrows the amber one, and each class is given the chroma its
/// chart colour needs — held at a fixed lightness in OKLab, so no class comes
/// out brighter than its neighbours just because the eye favours yellow.
pub(super) const STAR_BRIGHT: [Color; 7] = [
    Color::Rgb(150, 174, 244), // O  ~33000 K
    Color::Rgb(178, 198, 251), // B  ~15000 K
    Color::Rgb(193, 205, 237), // A   ~9500 K — Sirius, Vega
    Color::Rgb(205, 209, 218), // F   ~6800 K — Procyon
    Color::Rgb(223, 198, 171), // G   ~5772 K — the Sun
    Color::Rgb(232, 187, 139), // K   ~4400 K — Arcturus, Aldebaran
    Color::Rgb(235, 172, 101), // M   ~3400 K — Betelgeuse, Antares
];

// A restrained, near-monochrome palette: hierarchy comes from brightness
// (FG vs DIM), weight (bold) and italics — not colour.
//
// The interface stands on the SAME axis as the sky above it. Every neutral here
// is the star field's own white — the F class, the point where the blackbody
// ramp crosses from blue to amber — held at whatever lightness the role needs.
// The two accents are the two ENDS of that ramp, taken from the star table
// itself rather than copied out of it.
//
// This is what the palette was reaching for and missing. Measured, the greys it
// replaces all sat at hue −90° to −100° in OKLab, which is the accent's own
// direction: they were not neutrals, they were the accent at low chroma, and
// the accent had to compete with a whole screen tinted its own colour. Their
// chroma is now roughly halved and, more to the point, it is one shared value
// rather than six drifting ones.
pub(super) const FG: Color = Color::Rgb(203, 207, 216); // primary text
pub(super) const DIM: Color = Color::Rgb(113, 116, 125); // secondary / muted
pub(super) const CODE_BG: Color = Color::Rgb(30, 33, 39); // inline `code` chip background
/// Fenced code with no language named, or one this build has no syntax for.
pub(super) const CODE_FG: Color = Color::Rgb(195, 199, 208);
pub(super) const USER_BG: Color = Color::Rgb(37, 40, 47); // the user's question bar
pub(super) const RULE: Color = Color::Rgb(231, 235, 244); // white input rules

/// The cool accent: an O star, the hottest class there is — and so the bluest.
/// Handles and furniture, the things you can act on.
pub(super) const ACCENT: Color = STAR_BRIGHT[0];
/// The warm accent: an M star, the coolest class — and so the orangest. Things
/// that WANT you, as against things you can act on. The two fell out of the
/// star table already matched: 0.76 against 0.79 in lightness, 0.105 against
/// 0.115 in chroma, so neither shouts over the other.
pub(super) const WARM: Color = STAR_BRIGHT[STAR_BRIGHT.len() - 1];
/// Failures only — and deliberately OFF the blackbody axis, which is why it is
/// not simply a colder star. That ramp is a continuum and everything on it is a
/// matter of degree; a failure is a different kind of thing, and it should not
/// look like the warm end pushed a little further.
pub(super) const ERR: Color = Color::Rgb(224, 108, 117);

// Shared scene/chart ramps: material composition and the usage calendar use
// the same ocean, land, and atmosphere colors as the welcome scene.
pub(super) const OCEAN_DK: Color = Color::Rgb(22, 42, 78);
pub(super) const OCEAN_MD: Color = Color::Rgb(40, 82, 140);
pub(super) const OCEAN_LT: Color = Color::Rgb(86, 146, 206);
pub(super) const LAND_DK: Color = Color::Rgb(38, 70, 44);
pub(super) const LAND_MD: Color = Color::Rgb(92, 138, 70);
pub(super) const LAND_LT: Color = Color::Rgb(168, 190, 116);
pub(super) const ATMO: Color = Color::Rgb(150, 196, 234);

#[cfg(test)]
#[path = "theme/tests.rs"]
mod tests;
