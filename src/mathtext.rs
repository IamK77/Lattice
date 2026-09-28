//! A LaTeX subset rendered into Unicode text — one line in, one line out.
//!
//! Agent replies carry mathematics, and `\frac{a}{b}` on screen helps nobody.
//! A terminal cannot typeset, but Unicode already contains most of what a
//! sentence of mathematics needs: the Greek letters, the operators, the
//! arrows, and (partially) superscripts and subscripts. This module maps the
//! LaTeX that actually shows up in conversation onto those characters.
//!
//! It stays deliberately one-dimensional. Fractions become `a/b`, never a
//! stacked pair of lines; sums keep their limits beside them, not above and
//! below. Two-dimensional layout is a typesetting engine, and this is not one.
//!
//! **Anything it does not understand it leaves alone.** An unknown command
//! stays as written — visibly unhandled, which is honest, rather than silently
//! swallowed. The same goes for a superscript Unicode has no character for:
//! `x^{(n)}` keeps its `^(...)` form instead of being mangled into something
//! that reads as multiplication.

/// Greek letters, operators, relations, arrows and set notation — the symbols
/// a sentence of mathematics is actually made of. Order does not matter:
/// lookup is by whole command name, never by prefix.
const SYMBOLS: &[(&str, &str)] = &[
    // Greek, lower case
    ("alpha", "α"),
    ("beta", "β"),
    ("gamma", "γ"),
    ("delta", "δ"),
    ("epsilon", "ε"),
    ("varepsilon", "ε"),
    ("zeta", "ζ"),
    ("eta", "η"),
    ("theta", "θ"),
    ("vartheta", "ϑ"),
    ("iota", "ι"),
    ("kappa", "κ"),
    ("lambda", "λ"),
    ("mu", "μ"),
    ("nu", "ν"),
    ("xi", "ξ"),
    ("pi", "π"),
    ("varpi", "ϖ"),
    ("rho", "ρ"),
    ("varrho", "ϱ"),
    ("sigma", "σ"),
    ("varsigma", "ς"),
    ("tau", "τ"),
    ("upsilon", "υ"),
    ("phi", "φ"),
    ("varphi", "ϕ"),
    ("chi", "χ"),
    ("psi", "ψ"),
    ("omega", "ω"),
    // Greek, upper case
    ("Gamma", "Γ"),
    ("Delta", "Δ"),
    ("Theta", "Θ"),
    ("Lambda", "Λ"),
    ("Xi", "Ξ"),
    ("Pi", "Π"),
    ("Sigma", "Σ"),
    ("Upsilon", "Υ"),
    ("Phi", "Φ"),
    ("Psi", "Ψ"),
    ("Omega", "Ω"),
    // Binary operators
    ("times", "×"),
    ("div", "÷"),
    ("pm", "±"),
    ("mp", "∓"),
    ("cdot", "·"),
    ("ast", "∗"),
    ("star", "⋆"),
    ("circ", "∘"),
    ("bullet", "∙"),
    ("oplus", "⊕"),
    ("ominus", "⊖"),
    ("otimes", "⊗"),
    ("odot", "⊙"),
    // Relations
    ("leq", "≤"),
    ("le", "≤"),
    ("geq", "≥"),
    ("ge", "≥"),
    ("neq", "≠"),
    ("ne", "≠"),
    ("approx", "≈"),
    ("sim", "∼"),
    ("simeq", "≃"),
    ("equiv", "≡"),
    ("cong", "≅"),
    ("propto", "∝"),
    ("ll", "≪"),
    ("gg", "≫"),
    ("prec", "≺"),
    ("succ", "≻"),
    // Big operators and calculus
    ("sum", "∑"),
    ("prod", "∏"),
    ("coprod", "∐"),
    ("int", "∫"),
    ("iint", "∬"),
    ("iiint", "∭"),
    ("oint", "∮"),
    ("partial", "∂"),
    ("nabla", "∇"),
    ("infty", "∞"),
    // Operator NAMES: set upright in LaTeX, so the name IS its own rendering.
    // Listed rather than left unknown, because `\deg P` showing up on screen
    // as `\deg P` helps nobody.
    ("lim", "lim"),
    ("limsup", "limsup"),
    ("liminf", "liminf"),
    ("log", "log"),
    ("lg", "lg"),
    ("ln", "ln"),
    ("exp", "exp"),
    ("sin", "sin"),
    ("cos", "cos"),
    ("tan", "tan"),
    ("sec", "sec"),
    ("csc", "csc"),
    ("cot", "cot"),
    ("arcsin", "arcsin"),
    ("arccos", "arccos"),
    ("arctan", "arctan"),
    ("sinh", "sinh"),
    ("cosh", "cosh"),
    ("tanh", "tanh"),
    ("coth", "coth"),
    ("max", "max"),
    ("min", "min"),
    ("sup", "sup"),
    ("inf", "inf"),
    ("deg", "deg"),
    ("gcd", "gcd"),
    ("det", "det"),
    ("dim", "dim"),
    ("ker", "ker"),
    ("arg", "arg"),
    ("hom", "hom"),
    ("Pr", "Pr"),
    ("bmod", "mod"),
    ("pmod", "mod"),
    // Set theory and logic
    ("in", "∈"),
    ("notin", "∉"),
    ("ni", "∋"),
    ("subset", "⊂"),
    ("subseteq", "⊆"),
    ("supset", "⊃"),
    ("supseteq", "⊇"),
    ("cup", "∪"),
    ("cap", "∩"),
    ("setminus", "∖"),
    ("emptyset", "∅"),
    ("varnothing", "∅"),
    ("forall", "∀"),
    ("exists", "∃"),
    ("nexists", "∄"),
    ("neg", "¬"),
    ("lnot", "¬"),
    ("land", "∧"),
    ("lor", "∨"),
    ("wedge", "∧"),
    ("vee", "∨"),
    ("therefore", "∴"),
    ("because", "∵"),
    // Arrows
    ("to", "→"),
    ("rightarrow", "→"),
    ("leftarrow", "←"),
    ("Rightarrow", "⇒"),
    ("Leftarrow", "⇐"),
    ("leftrightarrow", "↔"),
    ("Leftrightarrow", "⇔"),
    ("mapsto", "↦"),
    ("uparrow", "↑"),
    ("downarrow", "↓"),
    ("implies", "⟹"),
    ("iff", "⟺"),
    // Dots and spacing-ish
    ("ldots", "…"),
    ("dots", "…"),
    ("cdots", "⋯"),
    ("vdots", "⋮"),
    ("ddots", "⋱"),
    ("prime", "′"),
    // Not core LaTeX (it comes from the gensymb package) but common enough to
    // accept; the STANDARD way to write a degree is `^\circ`, in `script`.
    ("degree", "°"),
    ("angle", "∠"),
    ("perp", "⊥"),
    ("parallel", "∥"),
    ("aleph", "ℵ"),
    ("hbar", "ℏ"),
    ("ell", "ℓ"),
    ("Re", "ℜ"),
    ("Im", "ℑ"),
    ("checkmark", "✓"),
    ("dagger", "†"),
];

/// Blackboard bold — the number sets, which are the only `\mathbb` anyone
/// writes in conversation.
const BLACKBOARD: &[(char, &str)] = &[
    ('R', "ℝ"),
    ('N', "ℕ"),
    ('Z', "ℤ"),
    ('Q', "ℚ"),
    ('C', "ℂ"),
    ('P', "ℙ"),
    ('E', "𝔼"),
    ('H', "ℍ"),
    ('F', "𝔽"),
];

/// Superscript forms. Unicode has no `q`, and only some capitals — a group
/// containing anything missing keeps its `^(...)` form rather than half
/// converting.
const SUPERSCRIPT: &[(char, char)] = &[
    ('0', '⁰'),
    ('1', '¹'),
    ('2', '²'),
    ('3', '³'),
    ('4', '⁴'),
    ('5', '⁵'),
    ('6', '⁶'),
    ('7', '⁷'),
    ('8', '⁸'),
    ('9', '⁹'),
    ('+', '⁺'),
    ('-', '⁻'),
    ('−', '⁻'),
    ('=', '⁼'),
    ('(', '⁽'),
    (')', '⁾'),
    ('a', 'ᵃ'),
    ('b', 'ᵇ'),
    ('c', 'ᶜ'),
    ('d', 'ᵈ'),
    ('e', 'ᵉ'),
    ('f', 'ᶠ'),
    ('g', 'ᵍ'),
    ('h', 'ʰ'),
    ('i', 'ⁱ'),
    ('j', 'ʲ'),
    ('k', 'ᵏ'),
    ('l', 'ˡ'),
    ('m', 'ᵐ'),
    ('n', 'ⁿ'),
    ('o', 'ᵒ'),
    ('p', 'ᵖ'),
    ('r', 'ʳ'),
    ('s', 'ˢ'),
    ('t', 'ᵗ'),
    ('u', 'ᵘ'),
    ('v', 'ᵛ'),
    ('w', 'ʷ'),
    ('x', 'ˣ'),
    ('y', 'ʸ'),
    ('z', 'ᶻ'),
    ('A', 'ᴬ'),
    ('B', 'ᴮ'),
    ('D', 'ᴰ'),
    ('E', 'ᴱ'),
    ('G', 'ᴳ'),
    ('H', 'ᴴ'),
    ('I', 'ᴵ'),
    ('J', 'ᴶ'),
    ('K', 'ᴷ'),
    ('L', 'ᴸ'),
    ('M', 'ᴹ'),
    ('N', 'ᴺ'),
    ('O', 'ᴼ'),
    ('P', 'ᴾ'),
    ('R', 'ᴿ'),
    ('T', 'ᵀ'),
    ('U', 'ᵁ'),
    ('W', 'ᵂ'),
];

/// Subscript forms. Far sparser than superscripts — most consonants simply do
/// not exist, which is why the fallback matters more here.
const SUBSCRIPT: &[(char, char)] = &[
    ('0', '₀'),
    ('1', '₁'),
    ('2', '₂'),
    ('3', '₃'),
    ('4', '₄'),
    ('5', '₅'),
    ('6', '₆'),
    ('7', '₇'),
    ('8', '₈'),
    ('9', '₉'),
    ('+', '₊'),
    ('-', '₋'),
    ('−', '₋'),
    ('=', '₌'),
    ('(', '₍'),
    (')', '₎'),
    ('a', 'ₐ'),
    ('e', 'ₑ'),
    ('h', 'ₕ'),
    ('i', 'ᵢ'),
    ('j', 'ⱼ'),
    ('k', 'ₖ'),
    ('l', 'ₗ'),
    ('m', 'ₘ'),
    ('n', 'ₙ'),
    ('o', 'ₒ'),
    ('p', 'ₚ'),
    ('r', 'ᵣ'),
    ('s', 'ₛ'),
    ('t', 'ₜ'),
    ('u', 'ᵤ'),
    ('v', 'ᵥ'),
    ('x', 'ₓ'),
];

/// Render a LaTeX fragment (the content BETWEEN delimiters — no `\(`, no `$$`)
/// as one line of Unicode text.
pub fn to_unicode(latex: &str) -> String {
    let chars: Vec<char> = latex.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '\\' => i = command(&chars, i, &mut out),
            '^' => i = script(&chars, i, &mut out, SUPERSCRIPT, '^'),
            '_' => i = script(&chars, i, &mut out, SUBSCRIPT, '_'),
            // Grouping braces carry no meaning once their argument is placed
            '{' | '}' => i += 1,
            // Collapse the runs of whitespace LaTeX ignores anyway
            ' ' | '\t' => {
                space(&mut out);
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    out.trim_end().to_string()
}

/// Symbols that behave like a letter — an operand, something a variable can
/// sit right next to. Greek letters are alphabetic outright; the handful of
/// mathematical constants are not, but read the same way.
fn letterlike(glyph: &str) -> bool {
    glyph
        .chars()
        .next()
        .is_some_and(|c| c.is_alphabetic() || matches!(c, '∂' | '∇' | '∞' | 'ℏ' | 'ℓ' | 'ℵ'))
}

/// Skip the space that merely terminated a control word, but only when a
/// variable follows it.
fn skip_binding_space(chars: &[char], i: usize) -> usize {
    let mut j = i;
    while j < chars.len() && chars[j] == ' ' {
        j += 1;
    }
    // Only before a VARIABLE. Before another command the space usually
    // belongs to an operator (`\alpha \leq \beta` wants its air), and
    // keeping a space is always safe where dropping one can fuse two symbols.
    let follows_operand = chars.get(j).is_some_and(|c| c.is_alphanumeric());
    if j > i && follows_operand {
        j
    } else {
        i
    }
}

/// One space, never two: LaTeX's several widths of space all collapse to the
/// single one a terminal has, and a leading one is not a space at all.
fn space(out: &mut String) {
    if !out.is_empty() && !out.ends_with(' ') {
        out.push(' ');
    }
}

/// Handle one `\…` command starting at `i`; returns the index after it.
fn command(chars: &[char], i: usize, out: &mut String) -> usize {
    let start = i + 1;
    if start >= chars.len() {
        out.push('\\');
        return start;
    }
    // A non-alphabetic escape is a single character: `\{`, `\%`, `\,` …
    if !chars[start].is_ascii_alphabetic() {
        match chars[start] {
            // Thin/medium/quad spaces all become one ordinary space
            ',' | ';' | ':' | ' ' => space(out),
            // A negative space removes one, as far as text can
            '!' => {
                out.pop();
            }
            '\\' => out.push(' '),
            c => out.push(c),
        }
        return start + 1;
    }
    let mut end = start;
    while end < chars.len() && chars[end].is_ascii_alphabetic() {
        end += 1;
    }
    let name: String = chars[start..end].iter().collect();
    match name.as_str() {
        // Fences are the two-dimensional part of the notation; the delimiter
        // that follows stands on its own
        "left" | "right" | "big" | "Big" | "bigg" | "Bigg" => end,
        "quad" | "qquad" => {
            space(out);
            end
        }
        "frac" | "dfrac" | "tfrac" => fraction(chars, end, out),
        "sqrt" => root(chars, end, out),
        // A word set in upright type is just its own text
        "text" | "textrm" | "mathrm" | "mathbf" | "mathit" | "operatorname" => {
            match group(chars, end) {
                Some((body, next)) => {
                    out.push_str(&to_unicode(&body));
                    next
                }
                None => end,
            }
        }
        "mathbb" => match group(chars, end) {
            Some((body, next)) => {
                let mut it = body.chars();
                match (it.next(), it.next()) {
                    (Some(c), None) => {
                        match BLACKBOARD.iter().find(|(k, _)| *k == c) {
                            Some((_, glyph)) => out.push_str(glyph),
                            None => out.push(c),
                        }
                        next
                    }
                    _ => {
                        out.push_str(&to_unicode(&body));
                        next
                    }
                }
            }
            None => end,
        },
        _ => match SYMBOLS.iter().find(|(k, _)| *k == name) {
            Some((_, glyph)) => {
                out.push_str(glyph);
                // LaTeX's rule: the space after a control word only ends the
                // NAME, and is not a space in the output — `\partial f` is one
                // symbol applied to one variable. But only when a variable is
                // what follows: the space in `\alpha + \beta` is real, and an
                // operator glyph like ≤ wants air around it either way.
                if glyph.chars().count() == 1 && letterlike(glyph) {
                    skip_binding_space(chars, end)
                } else {
                    end
                }
            }
            // Unknown: leave it written as it was — braces and all, since a
            // command this module cannot read is one whose argument it cannot
            // read either. A gap in the tables should be visible on screen,
            // not silently eaten.
            None => {
                out.push('\\');
                out.push_str(&name);
                match group(chars, end) {
                    Some((body, next)) => {
                        out.push('{');
                        out.push_str(&body);
                        out.push('}');
                        next
                    }
                    None => end,
                }
            }
        },
    }
}

/// `\frac{a}{b}` → `a/b`, parenthesizing a part only when it would otherwise
/// bind wrong: `\frac{a+b}{2}` must not read as `a+b/2`.
fn fraction(chars: &[char], i: usize, out: &mut String) -> usize {
    let Some((num, after_num)) = group(chars, i) else {
        out.push_str("\\frac");
        return i;
    };
    let Some((den, after_den)) = group(chars, after_num) else {
        out.push_str("\\frac");
        return i;
    };
    out.push_str(&bind(&to_unicode(&num)));
    out.push('/');
    out.push_str(&bind(&to_unicode(&den)));
    after_den
}

/// `\sqrt{x}` → `√x`, `\sqrt{a+b}` → `√(a+b)`.
fn root(chars: &[char], i: usize, out: &mut String) -> usize {
    match group(chars, i) {
        Some((body, next)) => {
            let body = to_unicode(&body);
            out.push('√');
            if body.chars().count() > 1 {
                out.push('(');
                out.push_str(&body);
                out.push(')');
            } else {
                out.push_str(&body);
            }
            next
        }
        None => {
            out.push('√');
            i
        }
    }
}

/// Wrap a rendered part in parentheses when it contains a loose operator —
/// the only case where dropping to one line would change what it means.
fn bind(rendered: &str) -> String {
    let loose = rendered
        .chars()
        .any(|c| matches!(c, '+' | '-' | '−' | '±' | '∓' | '/' | '·' | '×' | '÷' | ' '));
    if loose && !rendered.is_empty() {
        format!("({rendered})")
    } else {
        rendered.to_string()
    }
}

/// A super/subscript: `x^2`, `x^{10}`, `a_{ij}`. Converts only when EVERY
/// character of the group has a raised (or lowered) form — a half-converted
/// script reads as multiplication, which is worse than not converting.
fn script(
    chars: &[char],
    i: usize,
    out: &mut String,
    table: &[(char, char)],
    marker: char,
) -> usize {
    let Some((body, next)) = atom(chars, i + 1) else {
        out.push(marker);
        return i + 1;
    };
    let rendered = to_unicode(&body);
    // `90^\circ` is how a degree is written in plain LaTeX — a raised ring,
    // which Unicode spells with one character of its own.
    if marker == '^' && rendered == "∘" {
        out.push('°');
        return next;
    }
    let converted: Option<String> = rendered
        .chars()
        .map(|c| table.iter().find(|(k, _)| *k == c).map(|(_, v)| *v))
        .collect();
    match converted {
        Some(script) if !script.is_empty() => out.push_str(&script),
        // No raised form for something in there: keep it legible instead
        _ => {
            out.push(marker);
            if rendered.chars().count() > 1 {
                out.push('(');
                out.push_str(&rendered);
                out.push(')');
            } else {
                out.push_str(&rendered);
            }
        }
    }
    next
}

/// The argument at `i`: a braced group, or the single character that follows.
fn atom(chars: &[char], i: usize) -> Option<(String, usize)> {
    if i >= chars.len() {
        return None;
    }
    if chars[i] == '{' {
        return group(chars, i);
    }
    // A command is one atom: `x^\alpha`
    if chars[i] == '\\' {
        let mut end = i + 1;
        while end < chars.len() && chars[end].is_ascii_alphabetic() {
            end += 1;
        }
        let end = if end == i + 1 { i + 2 } else { end };
        return Some((chars[i..end.min(chars.len())].iter().collect(), end));
    }
    Some((chars[i].to_string(), i + 1))
}

/// The braced group starting at `i` (which must be `{`), balanced, returning
/// its contents and the index after the closing brace.
fn group(chars: &[char], i: usize) -> Option<(String, usize)> {
    if chars.get(i) != Some(&'{') {
        return None;
    }
    let mut depth = 0usize;
    let mut j = i;
    while j < chars.len() {
        match chars[j] {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some((chars[i + 1..j].iter().collect(), j + 1));
                }
            }
            _ => {}
        }
        j += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn symbols_and_scripts_become_unicode() {
        assert_eq!(to_unicode(r"\alpha + \beta"), "α + β");
        assert_eq!(to_unicode(r"x^2"), "x²");
        assert_eq!(to_unicode(r"x^{10}"), "x¹⁰");
        assert_eq!(to_unicode(r"H_2O"), "H₂O");
        assert_eq!(to_unicode(r"a_{ij}"), "aᵢⱼ");
        assert_eq!(to_unicode(r"n \times m \leq k"), "n × m ≤ k");
        assert_eq!(to_unicode(r"x \in \mathbb{R}"), "x ∈ ℝ");
        assert_eq!(to_unicode(r"\sum_{i=1}^{n} i"), "∑ᵢ₌₁ⁿ i");
    }

    /// Unicode has no raised `q` and few lowered consonants. Half-converting
    /// would read as multiplication, so the whole group stays written out.
    #[test]
    fn a_script_unicode_cannot_spell_stays_written_out() {
        assert_eq!(to_unicode(r"x^q"), "x^q");
        assert_eq!(to_unicode(r"v_{bc}"), "v_(bc)");
        assert_eq!(to_unicode(r"x^{(n)}"), "x⁽ⁿ⁾");
    }

    /// Operator names are upright text in LaTeX, so they render as themselves
    /// — and a degree is a RAISED RING, which Unicode spells with one glyph.
    #[test]
    fn operator_names_and_degrees_render_as_themselves() {
        assert_eq!(to_unicode(r"\deg P = 3"), "deg P = 3");
        assert_eq!(to_unicode(r"\gcd(a,b)"), "gcd(a,b)");
        assert_eq!(to_unicode(r"\dim V = n"), "dim V = n");
        assert_eq!(to_unicode(r"90^\circ"), "90°");
        assert_eq!(to_unicode(r"90^{\circ}"), "90°");
        // The composition operator is still a ring when it is not raised
        assert_eq!(to_unicode(r"f \circ g"), "f ∘ g");
    }

    #[test]
    fn fractions_flatten_and_parenthesize_only_when_they_must() {
        assert_eq!(to_unicode(r"\frac{1}{2}"), "1/2");
        assert_eq!(to_unicode(r"\frac{a+b}{2}"), "(a+b)/2");
        assert_eq!(to_unicode(r"\frac{x^2}{2}"), "x²/2");
        assert_eq!(to_unicode(r"\sqrt{2}"), "√2");
        assert_eq!(to_unicode(r"\sqrt{a+b}"), "√(a+b)");
    }

    /// A gap in the tables must be VISIBLE, never silently swallowed.
    #[test]
    fn an_unknown_command_survives_unchanged() {
        assert_eq!(to_unicode(r"\bowtie x"), "\\bowtie x");
        assert_eq!(to_unicode(r"\begin{matrix}"), "\\begin{matrix}");
        assert_eq!(to_unicode(r"\vec{v}"), "\\vec{v}");
    }

    /// The space after a control word ended the NAME; it is not a space on
    /// screen. Around an operator the space is real and stays.
    #[test]
    fn a_control_words_trailing_space_binds_to_what_follows() {
        assert_eq!(to_unicode(r"\partial f"), "∂f");
        assert_eq!(to_unicode(r"\alpha x"), "αx");
        assert_eq!(to_unicode(r"\alpha + \beta"), "α + β");
        assert_eq!(to_unicode(r"x \leq \alpha"), "x ≤ α");
        assert_eq!(to_unicode(r"\alpha \leq \beta"), "α ≤ β");
        assert_eq!(to_unicode(r"\frac{\partial f}{\partial x}"), "∂f/∂x");
    }

    #[test]
    fn fences_and_spacing_commands_leave_only_their_content() {
        assert_eq!(to_unicode(r"\left( \frac{1}{2} \right)"), "( 1/2 )");
        assert_eq!(to_unicode(r"a \, b \quad c"), "a b c");
        assert_eq!(to_unicode(r"\text{if } x > 0"), "if x > 0");
    }
}
