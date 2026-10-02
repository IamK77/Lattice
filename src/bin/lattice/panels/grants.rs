//! Responsive grant list and scope detail. Authorization semantics live elsewhere.
use crate::terminal_host::{
    text::{clip, ELLIPSIS},
    theme::{ACCENT, CODE_BG, DIM, ERR, FG, WARM},
};
use lattice::{
    components::operation_policy::{FlowGrant, GrantMatcher},
    view::{GrantPanel, View},
    wrap,
};
use ratatui::{
    style::{Modifier, Style},
    text::{Line, Span},
};

const WIDE_AT: usize = 92;
const LIST_WIDTH: usize = 30;
const MAX_WIDTH: usize = 116;
const VISIBLE_GRANTS: usize = 5;

pub(crate) fn lines(view: &dyn View, width: usize) -> Vec<Line<'static>> {
    render(view.grant_panel(), width)
}

struct Canvas {
    width: usize,
    rows: Vec<Line<'static>>,
}
impl Canvas {
    fn new(width: usize) -> Self {
        Self {
            width: width.max(1),
            rows: Vec::new(),
        }
    }
    fn blank(&mut self) {
        self.rows.push(Line::default());
    }
    fn text(&mut self, text: impl AsRef<str>, style: Style) {
        for line in text.as_ref().lines() {
            for runs in wrap::wrap(&[wrap::Run::new(line, 0)], self.width) {
                self.rows.push(Line::from(Span::styled(
                    runs.into_iter().map(|run| run.text).collect::<String>(),
                    style,
                )));
            }
        }
    }
    fn single(&mut self, text: impl AsRef<str>, style: Style) {
        let safe: String = text.as_ref().chars().filter(|c| !c.is_control()).collect();
        let clipped = if self.width < wrap::char_cols(ELLIPSIS) {
            safe.chars()
                .next()
                .filter(|c| wrap::char_cols(*c) <= self.width)
                .map(|c| c.to_string())
                .unwrap_or_default()
        } else {
            clip(&safe, self.width)
        };
        self.rows.push(Line::from(Span::styled(clipped, style)));
    }
    fn heading(&mut self, text: impl AsRef<str>) {
        self.text(text, Style::default().fg(FG).add_modifier(Modifier::BOLD));
    }
    fn note(&mut self, text: impl AsRef<str>) {
        self.text(text, Style::default().fg(DIM));
    }
    fn code(&mut self, text: impl AsRef<str>) {
        self.text(text, Style::default().fg(FG).bg(CODE_BG));
    }
}

// Readable argument notation, not executable shell source. Preserve boundaries
// and escapes instead of joining raw strings into a misleading command.
fn argument(value: &str) -> String {
    if !value.is_empty()
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "_./:-".contains(c))
    {
        value.to_owned()
    } else {
        serde_json::to_string(value).expect("serializable argument")
    }
}
fn prefix_text(prefix: &[String]) -> String {
    prefix
        .iter()
        .map(|value| argument(value))
        .collect::<Vec<_>>()
        .join(" ")
}
fn summary(grant: &FlowGrant) -> (String, String) {
    match grant.matchers.first() {
        Some(GrantMatcher::CommandPrefix { tool, prefix }) => (tool.clone(), prefix_text(prefix)),
        Some(GrantMatcher::ExactArguments { tool, .. }) => (tool.clone(), "Exact arguments".into()),
        None => ("Empty grant".into(), "No matching rules".into()),
    }
}

fn list(panel: &GrantPanel, selected: &str, width: usize, wide: bool) -> Canvas {
    let mut out = Canvas::new(width);
    let at = panel
        .state
        .grants
        .keys()
        .position(|id| id == selected)
        .unwrap_or(0);
    let start = at.saturating_sub(1);
    for (id, grant) in panel.state.grants.iter().skip(start).take(VISIBLE_GRANTS) {
        let chosen = id == selected;
        let (tool, preview) = summary(grant);
        let style = if chosen {
            Style::default()
                .fg(ACCENT)
                .bg(CODE_BG)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(DIM)
        };
        let marker = if chosen { ">" } else { " " };
        if wide {
            out.single(format!("{marker} {tool}"), style);
            out.single(
                format!("  {preview}"),
                Style::default().fg(if chosen { FG } else { DIM }),
            );
            out.blank();
        } else {
            out.single(format!("{marker} {tool} / {preview}"), style);
        }
    }
    out
}

fn detail(panel: &GrantPanel, id: &str, grant: &FlowGrant, width: usize) -> Canvas {
    let mut out = Canvas::new(width);
    out.heading("Scope");
    out.note("This conversation; survives reopening.");
    if panel.confirming.as_deref() == Some(id) {
        out.blank();
        out.text(
            "Revoke this grant?",
            Style::default().fg(WARM).add_modifier(Modifier::BOLD),
        );
        out.text("Enter confirms; Esc cancels", Style::default().fg(WARM));
    }
    if panel.pending.as_deref() == Some(id) {
        out.blank();
        out.text(
            "Revocation requested. Waiting for recorded state.",
            Style::default().fg(WARM),
        );
    }
    let mut has_prefix = false;
    for matcher in &grant.matchers {
        out.blank();
        match matcher {
            GrantMatcher::CommandPrefix { tool, prefix } => {
                has_prefix = true;
                out.heading(format!("{tool} / Argument prefix"));
                out.code(prefix_text(prefix));
            }
            GrantMatcher::ExactArguments {
                tool,
                arguments,
                effects,
            } => {
                out.heading(format!("{tool} / Exact arguments"));
                out.code(serde_json::to_string_pretty(arguments).expect("serializable arguments"));
                if let Some(effects) = effects {
                    out.note("Declared effects must also match:");
                    out.code(serde_json::to_string_pretty(effects).expect("serializable effects"));
                }
            }
        }
    }
    if has_prefix {
        out.blank();
        out.note("Additional arguments are allowed.");
        out.note("Directory, remote identity and executable contents are not pinned.");
    }
    out.blank();
    out.note("Revoking affects future checks, not completed work or permanent trust.");
    out.blank();
    out.note("Audit record");
    out.note(format!("Grant     {id}"));
    out.note(format!("Question  {}", grant.question));
    out
}

fn columns(left: Canvas, right: Canvas) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    for index in 0..left.rows.len().max(right.rows.len()) {
        let mut line = left.rows.get(index).cloned().unwrap_or_default();
        let used = wrap::str_cols(&line.to_string());
        line.spans
            .push(Span::raw(" ".repeat(left.width.saturating_sub(used))));
        line.spans
            .push(Span::styled(" | ", Style::default().fg(DIM)));
        if let Some(right) = right.rows.get(index) {
            line.spans.extend(right.spans.clone());
        }
        out.push(line);
    }
    out
}

fn render(panel: Option<&GrantPanel>, width: usize) -> Vec<Line<'static>> {
    if width == 0 {
        return Vec::new();
    }
    let inset = if width > 2 { 2 } else { 0 };
    let room = width.saturating_sub(inset * 2).clamp(1, MAX_WIDTH);
    let mut out = Canvas::new(room);
    out.heading("Conversation Grants");
    // The common panel viewport pins the first two rows. Keep the header
    // compact; long descriptions and IDs belong to the scrollable body.
    let Some(panel) = panel else {
        out.text("Grants unavailable", Style::default().fg(ERR));
        return inset_rows(out.rows, inset);
    };
    if let Some(error) = &panel.problem {
        out.text("Grants unavailable", Style::default().fg(ERR));
        out.blank();
        out.text(error, Style::default().fg(ERR));
        out.note("Press r to refresh; Esc to close.");
        return inset_rows(out.rows, inset);
    }
    if panel.pending.is_some() {
        out.single("Revocation pending", Style::default().fg(WARM));
    } else {
        out.single(
            format!("{} saved", panel.state.grants.len()),
            Style::default().fg(DIM),
        );
    }
    out.blank();
    if panel.state.grants.is_empty() {
        out.text("No grants in this conversation.", Style::default().fg(FG));
        out.note("Saved approvals will appear here.");
        return inset_rows(out.rows, inset);
    }
    let (id, grant) = panel
        .selected
        .as_ref()
        .and_then(|id| panel.state.grants.get_key_value(id))
        .or_else(|| panel.state.grants.first_key_value())
        .expect("nonempty grants");
    if room >= WIDE_AT {
        out.rows.extend(columns(
            list(panel, id, LIST_WIDTH, true),
            detail(panel, id, grant, room - LIST_WIDTH - 3),
        ));
    } else {
        out.rows.extend(list(panel, id, room, false).rows);
        out.blank();
        out.rows.extend(detail(panel, id, grant, room).rows);
    }
    inset_rows(out.rows, inset)
}
fn inset_rows(rows: Vec<Line<'static>>, inset: usize) -> Vec<Line<'static>> {
    rows.into_iter()
        .map(|mut row| {
            row.spans.insert(0, Span::raw(" ".repeat(inset)));
            row
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    fn panel() -> GrantPanel {
        let mut panel = GrantPanel::default();
        panel.state.grants.insert(
            "grant-id".into(),
            FlowGrant {
                matchers: vec![
                    GrantMatcher::CommandPrefix {
                        tool: "Run".into(),
                        prefix: vec!["git".into(), "push".into(), "origin".into()],
                    },
                    GrantMatcher::ExactArguments {
                        tool: "Browser".into(),
                        arguments: serde_json::json!({"target":"FULL-SCOPE-END"}),
                        effects: Some(serde_json::json!({"network":true})),
                    },
                ],
                question: "question-id".into(),
                interface: None,
            },
        );
        panel.selected = Some("grant-id".into());
        panel
    }
    fn text(panel: Option<&GrantPanel>, width: usize) -> String {
        render(panel, width)
            .into_iter()
            .map(|line| line.to_string())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn wide_grants_use_columns_narrow_grants_stack_and_audit_is_secondary() {
        let panel = panel();
        let wide = render(Some(&panel), 120);
        let selected = wide
            .iter()
            .position(|line| line.to_string().trim_start().starts_with('>'))
            .unwrap();
        assert!(
            wide[selected].to_string().contains("Scope"),
            "list and scope share a row"
        );
        for width in [24, 80] {
            let narrow = text(Some(&panel), width);
            assert!(!narrow.contains(" | "));
            assert!(narrow.find("> Run").unwrap() < narrow.find("Scope").unwrap());
        }
        for width in [80, 120] {
            let shown = text(Some(&panel), width);
            assert_eq!(shown.matches("survives reopening").count(), 1);
            assert_eq!(shown.matches("grant-id").count(), 1);
            assert!(shown.find("git push origin").unwrap() < shown.find("Audit record").unwrap());
            assert!(!shown.contains("Selected grant:"));
            assert!(!shown.contains("[0]"));
        }
    }

    #[test]
    fn long_lists_keep_the_selected_row_near_the_top_without_repeating_ids() {
        let mut panel = panel();
        let grant = panel.state.grants.remove("grant-id").unwrap();
        for i in 0..40 {
            panel
                .state
                .grants
                .insert(format!("grant-{i:02}"), grant.clone());
        }
        panel.selected = Some("grant-20".into());
        for width in [24, 80, 120] {
            let rows = render(Some(&panel), width);
            let selected = rows
                .iter()
                .position(|line| line.to_string().trim_start().starts_with('>'))
                .unwrap();
            assert!(
                selected <= 6,
                "selected row must stay near the top: {selected}"
            );
            let shown = text(Some(&panel), width);
            assert!(!shown.contains("grant-00"));
            assert!(shown.contains("grant-20"));
        }
    }

    #[test]
    fn argument_notation_preserves_empty_strings_boundaries_and_controls() {
        assert_eq!(argument("printf"), "printf");
        assert_eq!(argument(""), "\"\"");
        for value in ["two words", "one\ntwo", "quote\"here", "\x1b[2J", "中文"] {
            assert_eq!(
                serde_json::from_str::<String>(&argument(value)).unwrap(),
                value
            );
        }
    }

    #[test]
    fn grants_rows_fit_the_terminal_and_keep_all_scope_fields() {
        let panel = panel();
        for width in 0..160 {
            for line in render(Some(&panel), width) {
                assert!(
                    wrap::str_cols(&line.to_string()) <= width,
                    "width {width}: {line}"
                );
                assert!(!line.to_string().contains('\x1b'));
            }
        }
        for width in [24, 80, 120] {
            let shown = text(Some(&panel), width);
            assert!(shown.contains("FULL-SCOPE-END"));
            assert!(shown.contains("origin"));
            assert!(shown.contains("network"));
        }
    }

    #[test]
    fn grants_panel_distinguishes_empty_error_confirmation_and_pending() {
        assert!(text(Some(&GrantPanel::default()), 80).contains("No grants"));
        assert!(text(None, 80).contains("unavailable"));
        let broken = GrantPanel {
            problem: Some("fixture read failure".into()),
            ..GrantPanel::default()
        };
        assert!(text(Some(&broken), 80).contains("fixture read failure"));
        assert!(!text(Some(&broken), 80).contains("No grants"));
        let mut panel = panel();
        panel.confirming = Some("grant-id".into());
        assert!(text(Some(&panel), 100).contains("Enter confirms; Esc cancels"));
        panel.confirming = None;
        panel.pending = Some("grant-id".into());
        assert!(text(Some(&panel), 100).contains("Revocation requested."));
        let other = panel.state.grants["grant-id"].clone();
        panel.state.grants.insert("other-grant".into(), other);
        panel.selected = Some("other-grant".into());
        assert!(
            text(Some(&panel), 100).contains("Revocation pending"),
            "pending revocation remains visible when selecting another grant"
        );
    }
}
