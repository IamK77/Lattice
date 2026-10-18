//! Read-only turn progress for a compact activity line. Counts are provider
//! measurements, not estimates from response bytes or animation frames.

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
    pub running: bool,
    pub model_pending: bool,
}
