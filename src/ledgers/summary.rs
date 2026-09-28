use std::path::Path;

use serde_json::{json, Value};

/// Index metadata only: never retain a historical payload.
#[derive(Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Summary {
    stream: String,
    events: u64,
    turns: u64,
    opened: Option<String>,
    last: Option<String>,
    model: Option<String>,
    host: Option<String>,
    cwd: Option<String>,
    title: Option<String>,
}

impl Summary {
    /// Fold the next actual event while another consumer is already reading
    /// history. This avoids a second full pass just to publish the exit index.
    pub fn observe_event(&mut self, event: &crate::EventEnvelope) -> std::io::Result<()> {
        if event.seq != self.events + 1 || (!self.stream.is_empty() && self.stream != event.stream)
        {
            return Err(std::io::Error::other(
                "summary event is outside its next prefix",
            ));
        }
        self.observe(
            &event.stream,
            Some(&event.time),
            &event.event_type,
            &event.payload,
            event.causes.is_empty(),
        );
        Ok(())
    }

    pub fn save_checkpoint(&self, reader: &crate::LogReader) -> std::io::Result<bool> {
        if self.events > 0 && self.stream != reader.stream() {
            return Err(std::io::Error::other(
                "summary belongs to a different stream",
            ));
        }
        reader.save_checkpoint("ledger-summary", 1, self.events, self)
    }

    pub fn invalid_line(&mut self) {
        self.events += 1;
    }

    pub fn observe(
        &mut self,
        stream: &str,
        at: Option<&str>,
        kind: &str,
        payload: &Value,
        causeless: bool,
    ) {
        self.events += 1;
        if self.stream.is_empty() {
            self.stream = stream.to_string();
        }
        if self.opened.is_none() {
            self.opened = at.map(str::to_string);
        }
        if let Some(at) = at {
            self.last = Some(at.to_string());
        }
        match kind {
            "core.stream.opened" | "core.stream.resumed" => {
                if let Some(model) = payload["model"].as_str() {
                    self.model = Some(model.to_string());
                }
                if let Some(host) = payload["host"].as_str() {
                    self.host = Some(host.to_string());
                }
                if let Some(cwd) = payload["cwd"].as_str().filter(|d| !d.is_empty()) {
                    self.cwd = Some(cwd.to_string());
                }
            }
            "core.control.turn_completed" => self.turns += 1,
            "core.input.user_message" if self.title.is_none() && causeless => {
                self.title = payload["text"]
                    .as_str()
                    .map(|t| t.chars().take(120).collect());
            }
            _ => {}
        }
    }

    pub fn finish(self, path: &Path) -> Option<Value> {
        if self.events == 0 {
            return None;
        }
        Some(json!({
            "stream": self.stream,
            "file": path.file_name()?.to_string_lossy(),
            "dir": path.parent()?.to_string_lossy(),
            "host": self.host,
            "cwd": self.cwd,
            "model": self.model,
            "opened": self.opened,
            "last": self.last,
            "events": self.events,
            "turns": self.turns,
            "bytes": super::source::bytes(path).ok()?,
            "title": self.title,
        }))
    }
}
