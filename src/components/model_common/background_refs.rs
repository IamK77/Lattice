//! Presentation only: background references never change execution identifiers.

use crate::background_aliases::{reference_for_event, Projection};
use crate::{EventEnvelope, LogReader};

/// One lazy projection for a materialization, shared by all background parts.
/// Allocation follows the committed ledger, never the order of model material.
pub(in crate::components) struct BackgroundReferences<'a> {
    reader: &'a LogReader,
    projection: Option<Projection>,
}

impl<'a> BackgroundReferences<'a> {
    pub(in crate::components) fn new(reader: &'a LogReader) -> Self {
        Self {
            reader,
            projection: None,
        }
    }

    fn reference(&mut self, event: &EventEnvelope) -> Result<Option<String>, String> {
        let initial = self.projection.is_none();
        let reference = reference_for_event(
            &mut self.projection,
            self.reader,
            event,
            self.reader.snapshot_end(),
        )
        .map_err(|e| e.to_string())?;
        if initial {
            if let Some(projection) = &self.projection {
                projection.save(self.reader).map_err(|e| e.to_string())?;
            }
        }
        Ok(reference.map(|reference| reference.to_string()))
    }

    /// Preserve structured tool results as JSON: image extraction reads those
    /// fields after materialization. Never overwrite a provider's own field.
    pub(in crate::components) fn annotate(
        &mut self,
        event: &EventEnvelope,
        text: String,
    ) -> Result<String, String> {
        let Some(reference) = self.reference(event)? else {
            return Ok(text);
        };
        if let Ok(serde_json::Value::Object(mut result)) = serde_json::from_str(&text) {
            let mut key = "_backgroundReference".to_string();
            while result.contains_key(&key) {
                key.insert(0, '_');
            }
            result.insert(key, serde_json::json!(reference));
            return Ok(serde_json::Value::Object(result).to_string());
        }
        Ok(format!("{text}\n[background reference: {reference}]"))
    }

    pub(in crate::components) fn wake(&mut self, event: &EventEnvelope) -> Result<String, String> {
        let Some(reference) = self.reference(event)? else {
            return Ok(super::wake_text(&event.payload));
        };
        let mut payload = event.payload.clone();
        // Only the rendered source changes. Keep the original source and body
        // available without rewriting the event or inventing cancellation state.
        payload["source"] = serde_json::json!(reference);
        let mut text = super::wake_text(&payload);
        if let Some(source) = event.payload["source"].as_str() {
            text.push_str(&format!("\nOriginal source: {source}"));
        }
        Ok(text)
    }
}
