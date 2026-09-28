//! Completed shutdown observations. The host owns the outer boundary; the
//! kernel only measures its own cleanup and reports unfinished components.

use crate::startup::Timings;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KernelShutdownCost {
    pub timings: Timings,
    pub lingering: Vec<String>,
}

/// Returned to the host that owns the session. The log remains the sole
/// writer so the frontend can record its final cleanup without reopening it.
pub struct SessionShutdown {
    pub log: crate::EventLog,
    pub timings: Timings,
    pub kernel: KernelShutdownCost,
}
