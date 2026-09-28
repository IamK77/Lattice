//! The screen must survive whatever the model says.
//!
//! Everything here renders text the model wrote — markdown it may have got
//! wrong, mathematics it may have half-closed, a table with a missing rule,
//! a URL longer than the terminal. None of that is checked before it is
//! rendered, and none of it can be: a reply is prose, not a document format.
//!
//! So the law is not "renders correctly" — that is what the example tests in
//! each module are for. The law is that it renders AT ALL. A panic on this
//! path takes the whole terminal down while it is displaying an answer, and
//! the answer is the thing the person was waiting for.

use lattice::richtext;
use lattice::wrap::{str_cols, wrap, Run};

/// xorshift64*, seeded and fixed. Same generator as the other law files; they
/// are separate binaries, and a shared helper would have to be shipped in the
/// crate to be seen by all of them.
struct Seeded(u64);

impl Seeded {
    fn below(&mut self, n: u64) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D) % n
    }
}

/// The pieces text gets built from. Every one of them is either a markdown
/// marker that can be left unclosed, a character class that breaks a naive
/// byte or column count, or a run long enough to need splitting.
const PIECES: [&str; 34] = [
    "**",
    "*",
    "`",
    "```",
    "```rust",
    "~~",
    "#",
    "###",
    "- ",
    "1. ",
    "> ",
    "|",
    "| a | b |",
    "|---|---|",
    "$",
    "$$",
    "\\frac{1}{2}",
    "\\alpha",
    "\\notacommand",
    "_",
    "[link](http://x)",
    "![img](http://x)",
    "\n",
    "\n\n",
    " ",
    "\t",
    "hello",
    "中文字符",
    "🙂🙂",
    "e\u{0301}",
    "\u{200b}",
    "─│┌┘",
    "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    "×≤≥·→…",
];

/// The ones that are not printable text at all. Kept out of the conservation
/// check below, because dropping or replacing them is the correct behaviour —
/// a raw control byte must never reach a terminal cell.
const CONTROLS: [&str; 5] = ["\0", "\u{1}", "\u{7}", "\u{1b}[31m", "\u{feff}"];

fn text(seed: u64, pieces: usize, controls: bool) -> String {
    let mut rng = Seeded(seed | 1);
    let mut out = String::new();
    for _ in 0..pieces {
        if controls && rng.below(8) == 0 {
            out.push_str(CONTROLS[rng.below(CONTROLS.len() as u64) as usize]);
        } else {
            out.push_str(PIECES[rng.below(PIECES.len() as u64) as usize]);
        }
    }
    out
}

const SEEDS: [u64; 40] = {
    let mut seeds = [0u64; 40];
    let mut i = 0;
    while i < 40 {
        seeds[i] = (i as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
        i += 1;
    }
    seeds
};

const WIDTHS: [usize; 9] = [1, 2, 3, 5, 8, 13, 20, 79, 120];

#[test]
fn no_reply_can_be_written_that_the_renderer_cannot_draw() {
    for seed in SEEDS {
        let source = text(seed, 40, true);
        let lines = richtext::render(&source);
        for width in WIDTHS {
            for line in &lines {
                let runs: Vec<Run> = line
                    .iter()
                    .map(|span| Run::new(span.text.clone(), 0))
                    .collect();
                // Reaching here at all is the assertion: either of these
                // panicking is the defect this test exists for.
                let _ = wrap(&runs, width);
            }
        }
        // Mathematics goes through its own translator first, on text that was
        // never promised to be mathematics.
        let _ = lattice::mathtext::to_unicode(&source);
    }
}

/// The reason wrapping exists: a row that overruns the terminal is drawn over
/// the next one, and a screen of tidy text becomes interleaved nonsense. One
/// existing test states this law for one paragraph at six widths; this asks it
/// of everything, including widths narrower than a single character.
#[test]
fn no_row_is_wider_than_the_room_it_was_given() {
    for seed in SEEDS {
        let source = text(seed, 40, true);
        for line in richtext::render(&source) {
            let runs: Vec<Run> = line
                .iter()
                .map(|span| Run::new(span.text.clone(), 0))
                .collect();
            for width in WIDTHS {
                for row in wrap(&runs, width) {
                    let rendered: String = row.iter().map(|r| r.text.as_str()).collect();
                    let cols = str_cols(&rendered);
                    // A single character wider than the whole width has
                    // nowhere narrower to go — splitting it is not an option.
                    // Everything else must fit.
                    let indivisible = rendered.chars().count() <= 1;
                    assert!(
                        cols <= width || indivisible,
                        "seed {seed}, width {width}: row {rendered:?} is {cols} columns"
                    );
                }
            }
        }
    }
}

/// Wrapping moves text between rows; it does not consume it. A break eats the
/// space it broke on — that is what a break IS — so this asks after the
/// characters that are not spaces.
#[test]
fn wrapping_moves_text_between_rows_without_eating_any() {
    for seed in SEEDS {
        // No control characters here: replacing those is correct, and this
        // law is about the text a person actually wrote.
        let source = text(seed, 30, false);
        for line in richtext::render(&source) {
            let runs: Vec<Run> = line
                .iter()
                .map(|span| Run::new(span.text.clone(), 0))
                .collect();
            let before: String = runs
                .iter()
                .flat_map(|r| r.text.chars())
                .filter(|c| !c.is_whitespace())
                .collect();
            for width in WIDTHS {
                let after: String = wrap(&runs, width)
                    .iter()
                    .flat_map(|row| row.iter())
                    .flat_map(|r| r.text.chars())
                    .filter(|c| !c.is_whitespace())
                    .collect();
                assert_eq!(
                    before, after,
                    "seed {seed}, width {width}: wrapping changed the text"
                );
            }
        }
    }
}

/// Rendering is a reading. The same reply drawn twice is the same drawing —
/// the whole call-it-again obligation for this slice, which has no state to
/// advance.
#[test]
fn the_same_reply_renders_the_same_way_every_time() {
    for seed in SEEDS {
        let source = text(seed, 40, true);
        assert_eq!(richtext::render(&source), richtext::render(&source));
        assert_eq!(
            lattice::mathtext::to_unicode(&source),
            lattice::mathtext::to_unicode(&source)
        );
    }
}
