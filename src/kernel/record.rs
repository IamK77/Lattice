//! Tail repair recognizes incomplete bytes, not incompatible event schemas.

pub(super) fn incomplete(bytes: &[u8], error: &serde_json::Error) -> bool {
    error.is_eof()
        || std::str::from_utf8(bytes)
            .err()
            .is_some_and(|error| error.error_len().is_none())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EventDraft, EventEnvelope, EventLog, EventTypeDecl};
    use serde_json::json;

    #[test]
    fn every_partial_serialized_record_is_repairable_but_schema_errors_are_not() {
        let mut log = EventLog::in_memory(vec![EventTypeDecl::new("fixture", "record")], "fixture");
        let event = log
            .append(
                EventDraft::new(
                    "fixture",
                    &[],
                    json!({"text": "中𠀀", "items": [1, false, null]}),
                ),
                "fixture",
            )
            .unwrap();
        let bytes = serde_json::to_vec(&event).unwrap();
        for cut in 0..bytes.len() {
            let error = serde_json::from_slice::<EventEnvelope>(&bytes[..cut]).unwrap_err();
            assert!(incomplete(&bytes[..cut], &error), "cut {cut}: {error}");
        }
        for bytes in [br#"{"v":2}"#.as_slice(), b"null", b"{broken", b"\xff"] {
            let error = serde_json::from_slice::<EventEnvelope>(bytes).unwrap_err();
            assert!(!incomplete(bytes, &error), "not a torn record: {bytes:?}");
        }
    }
}
