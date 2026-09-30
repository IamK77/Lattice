//! Expert content wraps, including multiline instructions, instead of clipping drafts.
use lattice::{view::View, wrap};
use ratatui::text::Line;

pub(crate) fn lines(view: &dyn View, width: usize) -> Vec<Line<'static>> {
    let mut lines = super::compose::lines(super::AT_EXPERTS, width, Vec::new(), Vec::new());
    for (label, value) in view.expert_rows() {
        let text = format!("{label}: {value}");
        for paragraph in text.split('\n') {
            let wrapped = wrap::wrap(
                &[wrap::Run::new(paragraph, 0)],
                width.saturating_sub(4).max(1),
            );
            if wrapped.is_empty() {
                lines.push(Line::from("    "));
            }
            for runs in wrapped {
                let text: String = runs.into_iter().map(|run| run.text).collect();
                lines.push(Line::from(format!("  {text}")));
            }
        }
    }
    lines
}
