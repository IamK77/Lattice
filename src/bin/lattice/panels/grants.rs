//! Human-readable grant scope, separate from the authority and local controls.
use crate::terminal_host::{
    text::clip,
    theme::{ACCENT, DIM, ERR, FG, WARM},
};
use lattice::{
    components::operation_policy::GrantMatcher,
    view::{GrantPanel, View},
    wrap,
};
use ratatui::{
    style::{Color, Style},
    text::{Line, Span},
};

pub(crate) fn lines(view: &dyn View, width: usize) -> Vec<Line<'static>> {
    render(view.grant_panel(), width)
}

fn render(panel: Option<&GrantPanel>, width: usize) -> Vec<Line<'static>> {
    let mut rows = Vec::new();
    let room = width.saturating_sub(4).max(1);
    let mut row = |text: String, color: Color| {
        for line in text.lines() {
            for runs in wrap::wrap(&[wrap::Run::new(line, 0)], room) {
                rows.push(Line::from(vec![
                    Span::raw("  "),
                    Span::styled(
                        runs.into_iter().map(|r| r.text).collect::<String>(),
                        Style::default().fg(color),
                    ),
                ]));
            }
        }
    };
    row("Conversation grants".into(), ACCENT);
    let Some(panel) = panel else {
        row("Grants are unavailable in this view.".into(), ERR);
        return rows;
    };
    if let Some(error) = &panel.problem {
        row(format!("Cannot read or manage grants: {error}"), ERR);
        row("Press r to refresh; Esc to close.".into(), DIM);
        return rows;
    }
    if let Some(id) = &panel.confirming {
        row(format!("Revoke grant {id}?"), WARM);
        row(
            "Enter confirms; Esc cancels. Completed effects and permanent trust are unchanged."
                .into(),
            WARM,
        );
    } else if let Some(id) = &panel.pending {
        row(
            format!("Revocation requested for {id}; waiting for recorded state."),
            WARM,
        );
    }
    let grants = &panel.state.grants;
    if grants.is_empty() {
        row("No grants in this conversation.".into(), FG);
        return rows;
    }
    let selected = panel
        .selected
        .as_deref()
        .or_else(|| grants.keys().next().map(String::as_str));
    let at = grants
        .keys()
        .position(|id| Some(id.as_str()) == selected)
        .unwrap_or(0);
    let start = at.saturating_sub(3).min(grants.len().saturating_sub(7));
    row(
        format!(
            "Grants {}-{} of {}",
            start + 1,
            (start + 7).min(grants.len()),
            grants.len()
        ),
        DIM,
    );
    for (id, grant) in grants.iter().skip(start).take(7) {
        let summary = match grant.matchers.first() {
            Some(GrantMatcher::CommandPrefix { tool, prefix }) => format!(
                "{tool}: {}",
                serde_json::to_string(prefix).expect("serializable prefix")
            ),
            Some(GrantMatcher::ExactArguments { tool, .. }) => format!("{tool}: exact arguments"),
            None => "No matchers".into(),
        };
        let chosen = Some(id.as_str()) == selected;
        row(
            clip(
                &format!("{} {summary} [{id}]", if chosen { ">" } else { " " }),
                room,
            ),
            if chosen { ACCENT } else { DIM },
        );
    }
    if let Some((id, grant)) = selected.and_then(|id| grants.get_key_value(id)) {
        row(format!("Selected grant: {id}"), ACCENT);
        row("Only this conversation; survives reopening. Not permanent trust or interface permission.".into(), DIM);
        row(format!("Approved question: {}", grant.question), DIM);
        row(format!("Matching rules ({})", grant.matchers.len()), FG);
        for (i, matcher) in grant.matchers.iter().enumerate() {
            match matcher {
                GrantMatcher::CommandPrefix { tool, prefix } => {
                    row(format!("{}. {tool}: command argument prefix", i + 1), FG);
                    for (index, arg) in prefix.iter().enumerate() {
                        row(
                            format!(
                                "   [{index}] {}",
                                serde_json::to_string(arg).expect("serializable argument")
                            ),
                            FG,
                        );
                    }
                    row("Trailing arguments are not restricted. This does not pin the directory, remote identity, or executable contents.".into(), DIM);
                }
                GrantMatcher::ExactArguments {
                    tool,
                    arguments,
                    effects,
                } => {
                    row(format!("{}. {tool}: exact arguments", i + 1), FG);
                    row(
                        serde_json::to_string_pretty(arguments).expect("serializable arguments"),
                        FG,
                    );
                    if let Some(effects) = effects {
                        row("Declared effects must also match:".into(), DIM);
                        row(
                            serde_json::to_string_pretty(effects).expect("serializable effects"),
                            FG,
                        );
                    }
                }
            }
        }
        row(
            "Revoking affects future authorization, not work already performed.".into(),
            DIM,
        );
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use lattice::components::operation_policy::FlowGrant;
    fn text(panel: Option<&GrantPanel>, width: usize) -> String {
        render(panel, width)
            .into_iter()
            .map(|line| line.to_string())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn long_lists_keep_the_selected_row_in_a_bounded_window() {
        let mut panel = GrantPanel::default();
        for i in 0..40 {
            panel.state.grants.insert(
                format!("grant-{i:02}"),
                FlowGrant {
                    matchers: vec![],
                    question: "question".into(),
                    interface: None,
                },
            );
        }
        panel.selected = Some("grant-20".into());
        for width in [24, 80] {
            let rows = render(Some(&panel), width);
            let selected = rows
                .iter()
                .position(|line| line.to_string().trim_start().starts_with('>'))
                .unwrap();
            assert!(
                selected <= 5,
                "selected row must stay near the top: {selected}"
            );
            assert!(!text(Some(&panel), width).contains("grant-00"));
            assert!(text(Some(&panel), width).contains("grant-20"));
        }
    }

    #[test]
    fn grants_panel_distinguishes_empty_from_unavailable_and_keeps_full_scope() {
        assert!(text(Some(&GrantPanel::default()), 80).contains("No grants"));
        assert!(text(None, 80).contains("unavailable"));
        let broken = GrantPanel {
            problem: Some("fixture read failure".into()),
            ..GrantPanel::default()
        };
        assert!(text(Some(&broken), 80).contains("fixture read failure"));
        assert!(!text(Some(&broken), 80).contains("No grants"));
        let mut panel = GrantPanel::default();
        panel.state.grants.insert(
            "grant".into(),
            FlowGrant {
                matchers: vec![
                    GrantMatcher::CommandPrefix {
                        tool: "Run".into(),
                        prefix: vec!["git".into(), "push".into(), "origin".into()],
                    },
                    GrantMatcher::ExactArguments {
                        tool: "Browser".into(),
                        arguments: serde_json::json!({"action":"click", "target":"FULL-SCOPE-END"}),
                        effects: Some(serde_json::json!({"network":true})),
                    },
                ],
                question: "question".into(),
                interface: None,
            },
        );
        panel.selected = Some("grant".into());
        for width in [24, 80, 120] {
            let shown = text(Some(&panel), width);
            assert!(shown.contains("FULL-SCOPE-END"));
            assert!(shown.contains("\"origin\""));
            assert!(shown.contains("network"));
        }
        panel.confirming = Some("grant".into());
        assert!(text(Some(&panel), 100).contains("Enter confirms; Esc cancels"));
    }
}
