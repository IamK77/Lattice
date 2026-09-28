//! Frontend shutdown observation. Recording happens after cleanup, through the
//! sole ledger writer returned by the session, never by reopening a live log.

use lattice::startup::{PhaseTimer, Timings};
use serde::Serialize;
use serde_json::json;

pub(super) struct Trace {
    started_at: String,
    pub timer: PhaseTimer,
}

#[derive(Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Children {
    pub timings: Timings,
    pub sessions: Vec<serde_json::Value>,
    pub errors: Vec<String>,
}

impl Trace {
    pub fn start() -> Self {
        Self {
            started_at: chrono::Utc::now().to_rfc3339(),
            timer: PhaseTimer::start(),
        }
    }

    pub fn record(
        self,
        mut main: lattice::shutdown::SessionShutdown,
        children: Children,
        errors: Vec<String>,
    ) -> std::io::Result<lattice::EventEnvelope> {
        let stopped = main
            .log
            .reader()
            .scan_back_types(&[lattice::core_events::INTERRUPTED], |event, _| {
                Ok((event.payload["scope"] == "stream").then(|| event.id.clone()))
            })?;
        let causes: Vec<&str> = stopped.as_deref().into_iter().collect();
        let completed_at = chrono::Utc::now().to_rfc3339();
        let frontend = self.timer.finish("finalize");
        main.log
            .append(
                lattice::EventDraft::new(
                    lattice::components::silent_ui::SHUTDOWN_COST,
                    &causes,
                    json!({
                        "startedAt": self.started_at,
                        "completedAt": completed_at,
                        "frontend": frontend,
                        "session": main.timings,
                        "kernel": main.kernel,
                        "children": children,
                        "errors": errors,
                    }),
                ),
                "ui",
            )
            .map_err(std::io::Error::other)
    }
}
