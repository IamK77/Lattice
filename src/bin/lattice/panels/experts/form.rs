use super::*;

pub(super) fn draw(
    c: &mut Canvas,
    values: &[String],
    active: usize,
    cursor: usize,
    editing: bool,
    access_at: usize,
) {
    c.title(
        if editing {
            "Edit expert"
        } else {
            "Create expert"
        },
        "Define a specialist. Save, review, then activate.",
    );
    for (i, label) in ["Scope", "ID", "Name", "Purpose", "Model", "Access"]
        .iter()
        .enumerate()
    {
        let on = i == active;
        let mut spans = vec![Span::styled(
            format!("{} {label:<12}", if on { "▸" } else { " " }),
            Style::default().fg(if on { ACCENT } else { DIM }),
        )];
        match i {
            0 => {
                for (scope, name) in [("project", "This project"), ("personal", "Personal")] {
                    let chosen = values[0] == scope;
                    spans.push(Span::styled(
                        format!("{} {name}   ", if chosen { "●" } else { "○" }),
                        Style::default()
                            .fg(if chosen { FG } else { DIM })
                            .bg(if on && chosen { CODE_BG } else { Color::Reset }),
                    ));
                }
            }
            5 => {
                let chosen: Vec<_> = values[5].split(',').map(str::trim).collect();
                for (index, (id, name)) in ACCESS.iter().enumerate() {
                    let focused = on && index == access_at;
                    spans.push(Span::styled(
                        format!(
                            "{} {name}  ",
                            if chosen.contains(id) { "[x]" } else { "[ ]" }
                        ),
                        Style::default()
                            .fg(if focused { ACCENT } else { FG })
                            .bg(if focused { CODE_BG } else { Color::Reset }),
                    ));
                }
            }
            _ => {
                let mut text = values[i].clone();
                if on && i != 4 {
                    text.insert(cursor, '▏');
                }
                if text.is_empty() {
                    text = match i {
                        1 => "e.g. code-reviewer",
                        2 => "e.g. Code reviewer",
                        3 => "When should this expert be used?",
                        4 => "← → Choose a model",
                        _ => "",
                    }
                    .into();
                }
                spans.push(Span::styled(
                    text,
                    Style::default()
                        .fg(if values[i].is_empty() { DIM } else { FG })
                        .bg(if on { CODE_BG } else { Color::Reset }),
                ));
            }
        }
        c.row(spans);
        if on {
            let hint = match i {
                0 => "← → or Space to switch scope",
                1 => "Lowercase letters, digits, - and _",
                4 => "← → Choose from configured models",
                5 => "← → Choose capability · Space toggle",
                _ => "",
            };
            if !hint.is_empty() {
                c.text(format!("              {hint}"), DIM);
            }
        }
        c.blank();
    }
    c.row(vec![
        Span::styled(
            format!("{} Instructions", if active == 6 { "▸" } else { " " }),
            Style::default().fg(if active == 6 { ACCENT } else { DIM }),
        ),
        Span::styled("    Enter newline · ↑ ↓ move", Style::default().fg(DIM)),
    ]);
    c.rule();
    let mut offset = 0;
    let rows: Vec<_> = values[6]
        .split('\n')
        .flat_map(|line| {
            let runs = if active == 6 && (offset..=offset + line.len()).contains(&cursor) {
                let column = cursor - offset;
                vec![
                    wrap::Run::new(&line[..column], 0),
                    wrap::Run::new("▏", 1),
                    wrap::Run::new(&line[column..], 0),
                ]
            } else {
                vec![wrap::Run::new(line, 0)]
            };
            offset += line.len() + 1;
            wrap::wrap(&runs, c.width.max(1))
        })
        .collect();
    let caret_row = rows
        .iter()
        .position(|runs| runs.iter().any(|run| run.style == 1))
        .unwrap_or(0);
    let start = caret_row
        .saturating_sub(3)
        .min(rows.len().saturating_sub(7));
    for runs in rows.iter().skip(start).take(7) {
        c.row(
            runs.iter()
                .map(|run| {
                    Span::styled(
                        run.text.clone(),
                        Style::default().fg(if run.style == 1 { ACCENT } else { FG }),
                    )
                })
                .collect(),
        );
    }
    for _ in rows.len().min(7)..7 {
        c.blank();
    }
    c.rule();
    if rows.len() > 7 {
        c.text(
            format!(
                "Lines {}–{} of {} · follows the editing cursor",
                start + 1,
                (start + 7).min(rows.len()),
                rows.len()
            ),
            DIM,
        );
    }
    c.text(
        "Tab Next   Shift-Tab Previous   F2 Save…   Esc Discard",
        DIM,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expert_form_tracks_the_actual_cursor_not_a_similar_character_in_the_draft() {
        let instructions = format!(
            "A literal ▏ is not the cursor.\n{}",
            (0..30)
                .map(|i| format!("Instruction {i}\n"))
                .collect::<String>()
        );
        let values = vec![
            "project".into(),
            "reviewer".into(),
            "Reviewer".into(),
            "Review changes".into(),
            "preview".into(),
            "read".into(),
            instructions.clone(),
        ];
        for width in [26, 56, 96] {
            let mut c = Canvas {
                lines: Vec::new(),
                width,
            };
            draw(&mut c, &values, 6, instructions.len(), false, 0);
            let text = c
                .lines
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("\n");
            assert!(
                text.contains("Instruction 29"),
                "the editing cursor must remain visible: {text}"
            );
            assert!(
                !text.contains("Instruction 0\n"),
                "long instructions must use a bounded viewport"
            );
            assert!(c
                .lines
                .iter()
                .all(|line| wrap::str_cols(&line.to_string()) <= width + 2));
            assert!(c
                .lines
                .iter()
                .flat_map(|line| &line.spans)
                .any(|span| span.content == "▏" && span.style.fg == Some(ACCENT)));
            if width == 96 {
                assert!(
                    c.lines.len() <= 31,
                    "the whole form must fit the standard 40-row terminal"
                );
            }
        }
    }
}
