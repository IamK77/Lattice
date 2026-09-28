//! Status readouts: a read-only view becomes styled text and navigation intent.
//! Preference I/O, screen layout, hit geometry, and executing navigation belong
//! to the caller. This module neither knows nor mutates the live Ui.

use lattice::view::View;
use ratatui::style::{Color, Style};
use serde_json::Value;

#[cfg(test)]
#[path = "status/tests.rs"]
mod tests;

/// The status bar's readouts when nothing says otherwise.
const BAR_DEFAULT: [&str; 5] = ["model", "context", "effort", "background", "cwd"];

/// Parse an ordered list of known names, without reading preferences.
/// An empty result delegates the displayed selection to the frontend default,
/// just like an empty `View::status_bar()` from a static or replayed view.
pub(super) fn bar_from(setting: Option<Value>) -> Vec<String> {
    let Some(Value::Array(names)) = setting else {
        return BAR_DEFAULT.iter().map(|s| (*s).to_string()).collect();
    };
    names
        .iter()
        .filter_map(|v| v.as_str())
        .filter(|n| BAR_DEFAULT.contains(n))
        .map(str::to_string)
        .collect()
}

/// What a status readout opens, independent of the caller's panel numbering.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Opens {
    Models,
    Effort,
    Context,
    Background,
    Config,
}

/// One readout on the left of the status bar.
pub(super) struct Segment {
    pub(super) text: String,
    pub(super) style: Style,
    pub(super) opens: Opens,
}

/// Below this share of the window the context readout says nothing at all.
const CTX_QUIET: f64 = 0.50;
/// Warn at the displayed percentage, so identical labels have identical colors.
const CTX_LOUD_PERCENT: u64 = 70;

/// A readout with nothing to say says nothing. The effort readout is absent
/// at the default rung, context is absent until half the window is gone, and
/// the key warning is absent while the key is there. Keyboard hints and hiding
/// readouts in a modal screen are the caller's layout responsibilities.
pub(super) fn readouts(view: &dyn View, dim: Color, warm: Color) -> Vec<Segment> {
    let names: Vec<&str> = if view.status_bar().is_empty() {
        BAR_DEFAULT.to_vec()
    } else {
        view.status_bar().iter().map(String::as_str).collect()
    };
    let models = view.models();
    let current = models.current();
    let dim = Style::default().fg(dim);
    let mut out = Vec::new();
    for name in names {
        match name {
            "model" => {
                let Some(row) = current else { continue };
                // Warm, not red: a missing key would fail the next turn,
                // but nothing has gone wrong yet.
                let (text, style) = if row.key_present {
                    (row.id.clone(), dim)
                } else {
                    (format!("{} · no key", row.id), Style::default().fg(warm))
                };
                out.push(Segment {
                    text,
                    style,
                    opens: Opens::Models,
                });
            }
            "context" => {
                if let Some(status) = view.compaction_status() {
                    let text = match (status.in_flight, status.failure.is_some()) {
                        (true, true) => Some("compacting · auto paused"),
                        (true, false) => Some("compacting"),
                        (false, true) => Some("compact paused · /compact"),
                        (false, false) => None,
                    };
                    if let Some(text) = text {
                        out.push(Segment {
                            text: text.into(),
                            style: Style::default().fg(warm),
                            opens: Opens::Context,
                        });
                        continue;
                    }
                }
                let (Some(report), Some(window)) = (view.usage(), current.and_then(|m| m.window))
                else {
                    continue;
                };
                if window == 0 {
                    continue;
                }
                let share = report.call.prompt as f64 / window as f64;
                if share < CTX_QUIET {
                    continue;
                }
                let percent = (share * 100.0).round() as u64;
                out.push(Segment {
                    text: format!("ctx {percent}%"),
                    style: if percent >= CTX_LOUD_PERCENT {
                        Style::default().fg(warm)
                    } else {
                        dim
                    },
                    opens: Opens::Context,
                });
            }
            "effort" => {
                // "off" means no parameter is sent, not an active choice.
                let now = view.effort().now.unwrap_or_default();
                if now.is_empty() || now == "off" {
                    continue;
                }
                out.push(Segment {
                    text: now,
                    style: dim,
                    opens: Opens::Effort,
                });
            }
            "background" => {
                // Standing timers and watches are armed, not running work.
                let running = view.background().iter().filter(|l| !l.standing).count();
                if running == 0 {
                    continue;
                }
                out.push(Segment {
                    text: format!("{running} background"),
                    style: Style::default().fg(warm),
                    opens: Opens::Background,
                });
            }
            "cwd" => {
                let path = view.workspace();
                if path.is_empty() {
                    continue;
                }
                // The leaf distinguishes workspaces; shared parents do not.
                let leaf = std::path::Path::new(path)
                    .file_name()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path.to_string());
                out.push(Segment {
                    text: leaf,
                    style: dim,
                    opens: Opens::Config,
                });
            }
            _ => {}
        }
    }
    out
}
