//! Correlate display updates by witnessed request copies, never a reused model
//! call ID alone. Histories without causal evidence retain their legacy fallback.
use crate::{core_events as ce, EventEnvelope, LogReader};
use std::collections::HashSet;
use std::io;

pub(super) fn request_chain(reader: &LogReader, id: &str) -> io::Result<HashSet<String>> {
    let first = reader
        .header(id)?
        .ok_or_else(|| invalid("card request is absent"))?;
    if first.event_type != ce::TOOL_EXEC_STARTED {
        return Err(invalid("card creation is not a tool request"));
    }
    let call = first.call.clone();
    let tool = first.tool.clone();
    let mut pending = vec![first];
    let mut chain = HashSet::new();
    while let Some(request) = pending.pop() {
        if !chain.insert(request.id.clone()) {
            continue;
        }
        for cause in &request.causes {
            let parent = reader
                .header(cause)?
                .ok_or_else(|| invalid("card request cause is absent"))?;
            // Stop at independent work, including the model call that caused
            // this request. Approval metadata is not a forwarding predecessor.
            if parent.event_type == ce::TOOL_EXEC_STARTED
                && parent.call == call
                && parent.tool == tool
            {
                if parent.seq >= request.seq {
                    return Err(invalid("card forwarding cause is not earlier"));
                }
                pending.push(parent);
            }
        }
    }
    Ok(chain)
}

pub(super) fn update_chain(
    reader: &LogReader,
    event: &EventEnvelope,
) -> io::Result<Option<HashSet<String>>> {
    if event.event_type == ce::WAKE {
        return match crate::background_aliases::identity_for_event(reader, event)? {
            Some(identity) if identity.kind == crate::background_aliases::Kind::Run => {
                request_chain(reader, &identity.created_by).map(Some)
            }
            _ => Ok(None),
        };
    }
    if event.event_type != ce::TOOL_EXEC_COMPLETED {
        return Ok(None);
    }
    let mut chain: Option<HashSet<String>> = None;
    for cause in &event.causes {
        let header = reader
            .header(cause)?
            .ok_or_else(|| invalid("card completion cause is absent"))?;
        if header.event_type != ce::TOOL_EXEC_STARTED {
            continue;
        }
        if let Some(call) = event.payload["call"].as_str() {
            if header.call.as_deref() != Some(call) {
                continue;
            }
        }
        let found = request_chain(reader, cause)?;
        match &mut chain {
            Some(chain) => {
                if chain.is_disjoint(&found) {
                    return Err(invalid("card completion addresses independent requests"));
                }
                chain.extend(found);
            }
            None => chain = Some(found),
        }
    }
    Ok(chain)
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
