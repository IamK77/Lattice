//! Distinguish auxiliary calls from conversation material without tool-name rules.
#[cfg(test)]
mod tests;
use crate::kernel::log::Header;
use crate::{core_events as ce, EventEnvelope, LogReader};
use std::collections::HashSet;
use std::io;

pub(crate) fn auxiliary(reader: &LogReader, event: &EventEnvelope) -> io::Result<bool> {
    auxiliary_header(reader, &Header::from_event(event))
}

pub(crate) fn auxiliary_header(reader: &LogReader, event: &Header) -> io::Result<bool> {
    match event.event_type.as_str() {
        ce::TOOL_EXEC_STARTED => tool_request(reader, event),
        ce::MODEL_CALL_STARTED => Ok(event.has_purpose),
        ce::TOOL_EXEC_COMPLETED | ce::MODEL_CALL_COMPLETED | ce::INTERRUPTED => {
            let mut found = false;
            for id in &event.causes {
                let Some(request) = reader.header(id)? else {
                    continue;
                };
                let matches = match event.event_type.as_str() {
                    ce::TOOL_EXEC_COMPLETED => {
                        request.event_type == ce::TOOL_EXEC_STARTED && request.call == event.call
                    }
                    ce::MODEL_CALL_COMPLETED => request.event_type == ce::MODEL_CALL_STARTED,
                    _ => matches!(
                        request.event_type.as_str(),
                        ce::TOOL_EXEC_STARTED | ce::MODEL_CALL_STARTED
                    ),
                };
                if !matches {
                    continue;
                }
                found = true;
                let auxiliary = if request.event_type == ce::TOOL_EXEC_STARTED {
                    tool_request(reader, &request)?
                } else {
                    request.has_purpose
                };
                // A mixed interruption still carries a conversation outcome.
                if !auxiliary {
                    return Ok(false);
                }
            }
            Ok(found)
        }
        _ => Ok(false),
    }
}

fn tool_request(reader: &LogReader, request: &Header) -> io::Result<bool> {
    if request.has_purpose {
        return Ok(true);
    }
    let mut seen = HashSet::new();
    let mut pending = request.causes.clone();
    while let Some(id) = pending.pop() {
        if !seen.insert(id.clone()) {
            continue;
        }
        let Some(parent) = reader.header(&id)? else {
            continue;
        };
        // Follow copies, not the model call or unrelated work that caused them.
        if parent.event_type != ce::TOOL_EXEC_STARTED
            || parent.call != request.call
            || parent.tool != request.tool
        {
            continue;
        }
        if parent.has_purpose {
            return Ok(true);
        }
        pending.extend(parent.causes);
    }
    Ok(false)
}
