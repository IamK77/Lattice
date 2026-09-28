//! Pure activity-line animation and pacing. The event loop supplies ticks;
//! this module neither owns a turn nor advances the clock.
use crate::terminal_host::theme::{ACCENT, DIM, RULE};
use lattice::view::View;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

pub(super) fn spinner(tick: usize) -> char {
    SPINNER[(tick / SPIN_DIV) % SPINNER.len()]
}

/// Select and style the activity text, without allocating screen space or
/// changing the turn. Waiting text takes precedence over the working phrase.
pub(super) fn line(view: &dyn View) -> Line<'static> {
    let working = !view.streaming().is_empty() || view.last_tool_running();
    let phrase = state_phrase(view.turn(), working);
    let (g, word) = if view.waiting() {
        ('·', "Waiting for results — you can still type".to_string())
    } else if view.busy() {
        let g = THINK_SPIN[(view.tick() / THINK_DIV) % THINK_SPIN.len()];
        (g, phrase.to_string())
    } else {
        let end_phrase = state_phrase(view.turn(), true);
        let since = view.tick().wrapping_sub(view.done_at().unwrap_or(0));
        done_line(since / TYPE_DIV, end_phrase)
    };
    if view.busy() {
        let mut spans = vec![Span::styled(format!(" {g} "), Style::default().fg(ACCENT))];
        spans.extend(sweep_arc(&word, view.tick(), RULE, DIM));
        Line::from(spans)
    } else {
        Line::from(vec![
            Span::styled(format!(" {g} "), Style::default().fg(ACCENT)),
            Span::styled(word, Style::default().fg(RULE).add_modifier(Modifier::BOLD)),
        ])
    }
}

pub(super) const SPINNER: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
const SPIN_DIV: usize = 2;
pub(super) const THINK_SPIN: [char; 6] = ['·', '∘', '○', '◯', '○', '∘'];
pub(super) const THINK_DIV: usize = 3;
pub(super) const TYPE_DIV: usize = 2;

/// Step 12 completes Done; at two loop ticks per step, 26 leaves a margin.
pub(super) const DONE_SETTLE: usize = 26;

// One phrase per turn, with separate casts for thinking and working.
const THINKING_PHRASES: &[&str] = &[
    "Nothing is good or bad but thinking makes it so",
    "The soul becomes dyed with the color of its thoughts",
    "We become what we think",
    "Sapere aude",
    "L'homme est un roseau pensant",
    "Penser, c'est dire non",
    "Es gibt keine Tatsachen, nur Interpretationen",
    "The limits of my language are the limits of my world",
    "心外无物",
    "明鏡止水",
];
const WORKING_PHRASES: &[&str] = &[
    "We are what we repeatedly do",
    "Se hace camino al andar",
    "Il faut cultiver notre jardin",
    "Es ist nicht genug zu wollen, man muss auch tun",
    "継続は力なり",
    "The obstacle is the way",
    "Age quod agis",
    "千里之行，始于足下",
    "案ずるより産むが易し",
    "Well begun is half done",
];

pub(super) fn state_phrase(turn: usize, working: bool) -> &'static str {
    let pool = if working {
        WORKING_PHRASES
    } else {
        THINKING_PHRASES
    };
    pool[turn % pool.len()]
}

/// Shrink the glyph, erase the phrase in a fixed beat, then type and hold Done.
pub(super) fn done_line(step: usize, word: &str) -> (char, String) {
    const SHRINK: [char; 3] = ['○', '∘', '·'];
    const DONE: &str = "Done";
    const ERASE: usize = 6;
    if step < SHRINK.len() {
        return (SHRINK[step], word.to_string());
    }
    let s = step - SHRINK.len();
    let wn = word.chars().count();
    if s < ERASE {
        let keep = wn.saturating_sub(wn * (s + 1) / ERASE);
        return ('·', word.chars().take(keep).collect());
    }
    let take = (s - ERASE + 1).min(DONE.chars().count());
    ('·', DONE.chars().take(take).collect())
}

/// A shadow crosses the phrase in 24 ticks and rests for 36; colour changes,
/// not weight. The caller supplies both ends of the colour ramp.
pub(super) fn sweep_arc(
    word: &str,
    tick: usize,
    bright: Color,
    shadow: Color,
) -> Vec<Span<'static>> {
    if word.is_empty() {
        return vec![];
    }
    const SWEEP_DUR: usize = 24;
    const SWEEP_GAP: usize = 36;
    const CYCLE: usize = SWEEP_DUR + SWEEP_GAP;
    let chars: Vec<char> = word.chars().collect();
    let len = chars.len();
    let phase = tick % CYCLE;
    let solid = Style::default().fg(bright).add_modifier(Modifier::BOLD);
    if phase >= SWEEP_DUR {
        return vec![Span::styled(word.to_string(), solid)];
    }
    // Divide by the last phase, not the count, so the band exits completely
    // before the next frame returns to the solid resting colour.
    let t = phase as f64 / (SWEEP_DUR - 1) as f64;
    let eased = (1.0 - (t * std::f64::consts::PI).cos()) / 2.0;
    let half = match len {
        1 => 1.0,
        n if n <= 6 => 2.0,
        n if n <= 15 => 3.5,
        _ => 5.0,
    };
    let center_f = if len <= 1 {
        0.0
    } else {
        let span = (len - 1) as f64 + 2.0 * half;
        eased * span - half
    };
    let mut spans = Vec::with_capacity(len);
    for (i, &ch) in chars.iter().enumerate() {
        let dist = (i as f64 - center_f).abs();
        let depth = (1.0 - (dist / half).min(1.0)).clamp(0.0, 1.0);
        let depth = depth * depth * (3.0 - 2.0 * depth);
        spans.push(Span::styled(
            ch.to_string(),
            Style::default()
                .fg(shade(bright, shadow, depth))
                .add_modifier(Modifier::BOLD),
        ));
    }
    spans
}

fn shade(bright: Color, shadow: Color, depth: f64) -> Color {
    let (Color::Rgb(r1, g1, b1), Color::Rgb(r2, g2, b2)) = (bright, shadow) else {
        return bright;
    };
    let mix = |a: u8, b: u8| (a as f64 + (b as f64 - a as f64) * depth).round() as u8;
    Color::Rgb(mix(r1, r2), mix(g1, g2), mix(b1, b2))
}

#[cfg(test)]
#[path = "activity/tests.rs"]
mod tests;
