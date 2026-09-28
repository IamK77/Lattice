//! Offline comparison using the production Responses materializer; no HTTP.
use lattice::components::{responses_model, responses_wire};
use lattice::contracts::document::{documents_dir, resolve};
use lattice::kernel::log::EventLog;
use lattice::EventEnvelope;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::path::PathBuf;

fn shared_prefix(left: &[Value], right: &[Value]) -> usize {
    left.iter().zip(right).take_while(|(a, b)| a == b).count()
}

fn fingerprint(value: &Value) -> String {
    format!("{:x}", Sha256::digest(serde_json::to_vec(value).unwrap()))
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let path = PathBuf::from(
        args.next()
            .ok_or("expected ledger path and completion sequences")?,
    );
    let sequences: Vec<u64> = args.map(|s| s.parse()).collect::<Result<_, _>>()?;
    let last = *sequences
        .iter()
        .max()
        .ok_or("expected completion sequences")?;
    // EventLog can repair a torn tail when opening. Never open the real ledger
    // through that writer: parse a bounded snapshot and open a temporary copy.
    let snapshot: Vec<EventEnvelope> = std::fs::read_to_string(&path)?
        .lines()
        .take(last as usize)
        .map(serde_json::from_str)
        .collect::<Result<_, _>>()?;
    let temp = tempfile::tempdir()?;
    let copy = temp.path().join("snapshot.jsonl");
    let mut text = String::new();
    for event in &snapshot {
        text.push_str(&serde_json::to_string(event)?);
        text.push('\n');
    }
    std::fs::write(&copy, text)?;
    let log = EventLog::open(vec![], &snapshot[0].stream, Some(copy))?;
    let reader = log.reader();
    let docs = documents_dir(&path);
    let mut previous: Option<(u64, Vec<Value>)> = None;
    for seq in sequences {
        let done = snapshot
            .iter()
            .find(|e| e.seq == seq)
            .ok_or("unknown sequence")?;
        let request = reader.get(&done.causes[0])?.ok_or("missing request")?;
        let parts = request.payload["input"]["parts"]
            .as_array()
            .ok_or("missing parts")?;
        let mut native = None;
        for part in parts {
            if let Some(id) = part["digest"]["of"].as_str() {
                native = reader
                    .get(id)?
                    .and_then(|event| event.payload.get("nativeCompaction").cloned());
                if native.is_some() {
                    break;
                }
            }
        }
        let model = native
            .as_ref()
            .and_then(|v| v["model"].as_str())
            .or_else(|| request.payload["model"].as_str())
            .ok_or("missing model")?;
        // The endpoint is used only to validate native compaction ownership.
        // No network operation is available in this program.
        let endpoint = native
            .as_ref()
            .and_then(|v| v["baseUrl"].as_str())
            .unwrap_or("");
        let input = responses_model::materialize(parts, &reader, Some(&docs), model, endpoint)?;
        let tools = resolve(&request.payload["tools"], Some(&docs))?;
        let system = resolve(&request.payload["system"], Some(&docs))?;
        let normalized_tools =
            responses_wire::request("audit", vec![], tools.as_array().ok_or("missing tools")?, 1)
                ["tools"]
                .clone();
        let usage = &done.payload["usage"];
        println!(
            "{}",
            json!({
                "completion": done.id, "request":request.id,
                "material_parts":parts.len(), "wire_items":input.len(),
                "input_sha256":fingerprint(&json!(input)),
                "system_sha256":fingerprint(&system), "function_tools_sha256":fingerprint(&normalized_tools),
                "input_tokens":usage["input_tokens"], "cached_tokens":usage["input_tokens_details"]["cached_tokens"],
                "cache_request_fields":usage["attribution"]["request_fields"],
            })
        );
        if let Some((previous_seq, old)) = previous {
            let common = shared_prefix(&old, &input);
            println!(
                "{}",
                json!({"from":previous_seq,"to":seq,"old_items":old.len(),
                "new_items":input.len(),"common_items":common,"old_prefix_preserved":common==old.len(),
                "common_serialized_bytes":serde_json::to_vec(&old[..common])?.len(),
                "first_difference_types": if common<old.len() && common<input.len() {
                    json!([old[common]["type"],input[common]["type"]])
                } else {Value::Null}})
            );
        }
        previous = Some((seq, input));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn distinguishes_appending_from_changing_an_old_answer() {
        let old = vec![json!({"type":"function_call_output","output":"first"})];
        let appended = vec![old[0].clone(), json!({"type":"message","content":"next"})];
        assert_eq!(shared_prefix(&old, &appended), old.len());
        let replaced = vec![
            json!({"type":"function_call_output","output":"late answer"}),
            appended[1].clone(),
        ];
        assert_eq!(shared_prefix(&old, &replaced), 0);
    }

    #[test]
    fn detects_reordering_and_shortening() {
        let old = vec![json!("a"), json!("b")];
        assert_eq!(shared_prefix(&old, &[json!("b"), json!("a")]), 0);
        assert_eq!(shared_prefix(&old, &[json!("a")]), 1);
    }
}
