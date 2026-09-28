//! A committed-prefix projection of background receipts still awaiting wakes.
//! Checkpoints contain references, never command output or execution state.

use std::collections::HashSet;
use std::io;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{core_events as ce, LogReader};

const CONSUMER: &str = "shell-background";
const VERSION: u32 = 1;

#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct State {
    pending: Vec<Receipt>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Receipt {
    event: String,
    started: String,
}

#[derive(Debug, PartialEq)]
pub(super) struct Job {
    pub started: String,
    pub job: Value,
    pub pid: Value,
    pub command: Value,
}

pub(super) struct Recovery {
    pub jobs: Vec<Job>,
    pub cold_reason: Option<String>,
    #[cfg(test)]
    headers: usize,
    #[cfg(test)]
    bodies: usize,
}

#[derive(Default)]
struct Reads {
    #[cfg(test)]
    headers: usize,
    #[cfg(test)]
    bodies: usize,
}

impl Reads {
    fn get(&mut self, reader: &LogReader, id: &str) -> io::Result<Option<crate::EventEnvelope>> {
        #[cfg(test)]
        {
            self.bodies += 1;
        }
        reader.get(id)
    }
}

/// Fold only the suffix. Wakes may arrive before their receipts; collect their
/// causes for the whole suffix before discarding finished work. No closed-job
/// identities survive into the checkpoint.
fn fold(
    reader: &LogReader,
    previous: u64,
    through: u64,
    state: &mut State,
    reads: &mut Reads,
) -> io::Result<bool> {
    let mut ended = HashSet::new();
    let mut late_receipt = false;
    reader.try_visit_header_range(previous + 1, through, |batch| {
        for header in batch {
            #[cfg(test)]
            {
                reads.headers += 1;
            }
            match header.event_type.as_str() {
                ce::WAKE => ended.extend(header.causes.iter().cloned()),
                ce::TOOL_EXEC_COMPLETED => {
                    let Some(started) = header.causes.first() else {
                        continue;
                    };
                    let event = reads
                        .get(reader, &header.id)?
                        .ok_or_else(|| io::Error::other("background receipt source is absent"))?;
                    if event.payload["result"]["background"] != true {
                        continue;
                    }
                    // A new receipt normally belongs to a post-checkpoint
                    // request. Imported duplicates or delayed replies can
                    // refer to an older request whose wake was already folded.
                    // Rebuild explicitly rather than retain every closed job
                    // forever or invent an unfinished job from incomplete data.
                    if previous > 0
                        && reader
                            .header(started)?
                            .is_none_or(|source| source.seq <= previous)
                    {
                        late_receipt = true;
                    }
                    state.pending.push(Receipt {
                        event: header.id.clone(),
                        started: started.clone(),
                    });
                }
                _ => {}
            }
        }
        Ok(())
    })?;
    state
        .pending
        .retain(|receipt| !ended.contains(&receipt.started));
    Ok(late_receipt)
}

pub(super) fn recover(reader: &LogReader) -> io::Result<Recovery> {
    let through = reader.snapshot_end();
    let checkpoint = reader.load_checkpoint::<State>(CONSUMER, VERSION, through)?;
    let mut cold_reason = checkpoint.cold_reason;
    let mut state = checkpoint.state.unwrap_or_default();
    let mut reads = Reads::default();
    if fold(reader, checkpoint.through, through, &mut state, &mut reads)? {
        cold_reason = Some("background receipt refers to a pre-checkpoint request".into());
        state = State::default();
        fold(reader, 0, through, &mut state, &mut reads)?;
    }
    // Resolve every remaining reference before publishing a checkpoint or
    // emitting any settlements. Read failures are never treated as absence.
    let mut jobs = Vec::new();
    for receipt in &state.pending {
        let event = reads
            .get(reader, &receipt.event)?
            .ok_or_else(|| io::Error::other("background receipt source is absent"))?;
        let command = reads
            .get(reader, &receipt.started)?
            .map_or(Value::Null, |event| {
                event.payload["arguments"]["command"].clone()
            });
        jobs.push(Job {
            started: receipt.started.clone(),
            job: event.payload["result"]["job"].clone(),
            pid: event.payload["result"]["pid"].clone(),
            command,
        });
    }
    if let Err(error) = reader.save_checkpoint(CONSUMER, VERSION, through, &state) {
        eprintln!("cannot save background command recovery state: {error}");
    }
    Ok(Recovery {
        jobs,
        cold_reason,
        #[cfg(test)]
        headers: reads.headers,
        #[cfg(test)]
        bodies: reads.bodies,
    })
}

#[cfg(test)]
mod tests;
