//! Material accounting rules shared by growth tracking and its display.

/// The preamble is re-sent, not accumulated. Counting it as new on every call
/// falsely inflates lifetime growth, especially when some records omit it.
pub(super) fn accumulates(kind: &str) -> bool {
    !matches!(kind, "system prompt" | "tool declarations")
}
