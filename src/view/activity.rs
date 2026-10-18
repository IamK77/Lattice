//! Read-only turn progress for a compact activity line. Provider measurements
//! and provisional output estimates remain explicitly distinct.

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TokenCount {
    /// None means no measurement is available, not zero consumption.
    pub known: Option<u64>,
    /// False means `known` is only a subtotal / lower bound.
    pub complete: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TurnSummary {
    pub elapsed_ms: Option<u64>,
    pub input: TokenCount,
    pub output: TokenCount,
    /// Provisional total including streamed text, displayed with `~`, never `≥`.
    pub output_estimate: Option<u64>,
    pub running: bool,
    pub model_pending: bool,
}
