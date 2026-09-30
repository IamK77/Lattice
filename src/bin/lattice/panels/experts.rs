//! Expert manager: bounded reading width, distinct hierarchy and explicit focus.
#[path = "experts/form.rs"]
mod form;
use crate::terminal_host::{
    text::clip,
    theme::{ACCENT, CODE_BG, DIM, FG, WARM},
};
use lattice::{
    view::{
        expert_panel::{Mode, ACCESS},
        View,
    },
    wrap,
};
use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};
use serde_json::Value;

struct Canvas {
    lines: Vec<Line<'static>>,
    width: usize,
}
impl Canvas {
    fn row(&mut self, spans: Vec<Span<'static>>) {
        let styles: Vec<_> = spans.iter().map(|span| span.style).collect();
        let runs: Vec<_> = spans
            .into_iter()
            .enumerate()
            .map(|(i, span)| wrap::Run::new(span.content.into_owned(), i))
            .collect();
        for runs in wrap::wrap(&runs, self.width.max(1)) {
            let mut row = vec![Span::raw("  ")];
            row.extend(
                runs.into_iter()
                    .map(|run| Span::styled(run.text, styles[run.style])),
            );
            self.lines.push(Line::from(row));
        }
    }
    fn text(&mut self, text: impl Into<String>, color: Color) {
        for runs in wrap::wrap(&[wrap::Run::new(text.into(), 0)], self.width.max(1)) {
            self.row(vec![Span::styled(
                runs.into_iter().map(|run| run.text).collect::<String>(),
                Style::default().fg(color),
            )]);
        }
    }
    fn blank(&mut self) {
        self.lines.push(Line::default());
    }
    fn rule(&mut self) {
        self.row(vec![Span::styled(
            "─".repeat(self.width),
            Style::default().fg(DIM),
        )]);
    }
    fn title(&mut self, title: &str, subtitle: &str) {
        self.row(vec![Span::styled(
            clip(title, self.width),
            Style::default().fg(FG).add_modifier(Modifier::BOLD),
        )]);
        self.text(subtitle, DIM);
        self.blank();
    }
    fn field(&mut self, label: &str, value: &str) {
        self.text(format!("{label:<14}{value}"), FG);
    }
    fn help(&mut self, text: &str) {
        self.blank();
        self.rule();
        self.text(text, DIM);
    }
}
fn str_field<'a>(value: &'a Value, key: &str) -> &'a str {
    value[key].as_str().unwrap_or_default()
}
fn access(value: &Value) -> String {
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(|id| {
            ACCESS
                .iter()
                .find(|(key, _)| *key == id)
                .map_or(id, |(_, label)| *label)
        })
        .collect::<Vec<_>>()
        .join(" · ")
}
fn builtin_name(id: &str) -> &str {
    match id {
        "explorer" => "Explorer",
        "researcher" => "Researcher",
        "worker" => "Worker",
        _ => id,
    }
}
fn short_description(id: &str, fallback: &str) -> String {
    match id {
        "builtin:explorer" => {
            "Find answers in a codebase. Read files, trace definitions and search references."
                .into()
        }
        "builtin:researcher" => {
            "Research a question on the web and bring back supporting sources.".into()
        }
        "builtin:worker" => {
            "Carry out a focused task with file editing and command execution.".into()
        }
        _ => fallback.into(),
    }
}
fn badge(state: &str) -> (&str, Color) {
    match state {
        "ready" => ("Ready", ACCENT),
        "builtin" => ("Built-in", DIM),
        "pending" => ("Needs activation", WARM),
        _ => ("Unavailable", WARM),
    }
}

pub(crate) fn lines(view: &dyn View, width: usize) -> Vec<Line<'static>> {
    let mut c = Canvas {
        lines: super::compose::lines(super::AT_EXPERTS, width, Vec::new(), Vec::new()),
        width: width.saturating_sub(4).min(96),
    };
    let Some(panel) = view.expert_panel() else {
        return c.lines;
    };
    match &panel.mode {
        Mode::List { rows, selected } => {
            c.title(
                "Experts",
                "Choose a specialist for focused work, or create your own.",
            );
            if rows.is_empty() && !panel.waiting {
                c.text("No experts to show. Press F5 to refresh.", DIM);
            }
            for (scope, heading) in [
                ("builtin", "BUILT-IN"),
                ("project", "THIS PROJECT"),
                ("personal", "PERSONAL"),
            ] {
                let group: Vec<_> = rows
                    .iter()
                    .enumerate()
                    .filter(|(_, row)| str_field(row, "name").starts_with(&format!("{scope}:")))
                    .collect();
                if group.is_empty() {
                    continue;
                }
                c.text(format!("{heading}  {}", group.len()), DIM);
                c.rule();
                for (i, row) in group {
                    let qualified = str_field(row, "name");
                    let id = qualified.split_once(':').map_or(qualified, |(_, id)| id);
                    let name = if scope == "builtin" {
                        builtin_name(id)
                    } else {
                        row["displayName"]
                            .as_str()
                            .filter(|name| !name.is_empty())
                            .unwrap_or(id)
                    };
                    let state = row["state"].as_str().unwrap_or(if row["ready"] == true {
                        "ready"
                    } else {
                        "unavailable"
                    });
                    let (status, color) = badge(state);
                    let on = i == *selected;
                    let name_room = c.width.saturating_sub(status.len() + 5);
                    let left = format!("{} {}", if on { "▸" } else { " " }, clip(name, name_room));
                    let gap = c.width.saturating_sub(wrap::str_cols(&left) + status.len());
                    let style = Style::default()
                        .fg(if on { ACCENT } else { FG })
                        .add_modifier(if on {
                            Modifier::BOLD
                        } else {
                            Modifier::empty()
                        });
                    c.row(vec![
                        Span::styled(left, style.bg(if on { CODE_BG } else { Color::Reset })),
                        Span::styled(
                            " ".repeat(gap),
                            if on {
                                Style::default().bg(CODE_BG)
                            } else {
                                Style::default()
                            },
                        ),
                        Span::styled(
                            status.to_owned(),
                            Style::default()
                                .fg(color)
                                .bg(if on { CODE_BG } else { Color::Reset }),
                        ),
                    ]);
                    let description = short_description(qualified, str_field(row, "description"));
                    c.text(format!("  {description}"), DIM);
                    c.blank();
                }
            }
            c.help("↑ ↓ Choose   Enter Details   n New expert   F5 Refresh   Esc Close");
        }
        Mode::Detail { details } => {
            let definition = details
                .get("copyTemplate")
                .unwrap_or(&details["definition"]);
            let builtin = details["builtin"] == true;
            let id = str_field(definition, "id");
            let title = if builtin {
                builtin_name(id)
            } else {
                str_field(definition, "name")
            };
            c.title(title, "Experts / Details");
            let state = str_field(details, "state");
            let (label, color) = badge(state);
            c.row(vec![
                Span::styled(format!("● {label}"), Style::default().fg(color)),
                Span::styled(
                    if builtin {
                        "   Read-only template"
                    } else {
                        "   Reusable expert"
                    },
                    Style::default().fg(DIM),
                ),
            ]);
            c.blank();
            let identity = if builtin {
                format!("builtin:{id}")
            } else {
                format!("{}:{id}", str_field(&details["target"], "scope"))
            };
            c.text(
                short_description(&identity, str_field(definition, "description")),
                FG,
            );
            c.blank();
            c.field("Identity", &identity);
            c.field(
                "Model",
                if builtin {
                    "Choose when copying"
                } else {
                    str_field(definition, "model")
                },
            );
            c.field("Access", &access(&definition["capabilities"]));
            if let Some(root) = details.get("toolRoot") {
                c.field(
                    "Tool root",
                    root.as_str().unwrap_or("Not confined by a configured root"),
                );
            }
            c.blank();
            c.text("INSTRUCTIONS", DIM);
            c.rule();
            for line in str_field(definition, "instructions").split('\n') {
                if line.is_empty() {
                    c.blank();
                } else {
                    c.text(line, FG);
                }
            }
            if let Some(error) = details["error"].as_str() {
                c.blank();
                c.text(error, WARM);
            }
            c.help(if builtin {
                "c Copy to a custom expert   Esc Back   PgUp PgDn Scroll"
            } else {
                "e Edit   c Copy   a Activate   d Delete…   Esc Back   PgUp PgDn Scroll"
            });
        }
        Mode::Form {
            values,
            active,
            cursor,
            editing,
            access_at,
        } => form::draw(&mut c, values, *active, *cursor, *editing, *access_at),
    }
    if panel.waiting
        || (!panel.notice.is_empty() && panel.notice != "Choose an expert or create your own.")
    {
        c.blank();
        c.text(&panel.notice, if panel.waiting { ACCENT } else { WARM });
    }
    c.lines
}
