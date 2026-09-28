//! Rewriting a ledger into the shape it would have been written in today.
//!
//! Two jobs, one operation. A record written before documents moved out still
//! holds a 27 KB system prompt inside a JSON line, where it has no lines to
//! search; compacting it puts the prompt in a file beside the ledger and
//! leaves a reference. And a record written by an older Lattice is migrated by
//! exactly the same pass, so there is one piece of code to get right rather
//! than two.
//!
//! What it must not do is change the record. Only the CONTENTS of declared
//! document fields move; no line is added, removed or reordered, so an event's
//! id keeps naming its own line — which is the address the agent is given.

use std::io::{BufRead, BufReader, Write};
use std::path::Path;

use serde_json::Value;

use crate::contracts::event::EventTypeDecl;

/// What one pass did, in the terms a person would ask about.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Compacted {
    pub events: usize,
    /// Events whose payload actually changed
    pub rewritten: usize,
    pub documents: usize,
    pub bytes_before: u64,
    pub bytes_after: u64,
}

/// Rewrite one ledger in place, moving its documents out beside it.
///
/// Idempotent: a field already moved out is a small reference, under the
/// threshold, so a second pass finds nothing to do. Safe to interrupt: the new
/// ledger is built beside the old one and put in its place by a rename, so the
/// file at that path is always a whole ledger — the previous one or the new
/// one, never half of either.
pub fn compact(ledger: &Path, types: &[EventTypeDecl]) -> std::io::Result<Compacted> {
    if ledger.is_dir() {
        return Err(std::io::Error::other(
            "segmented source records are immutable; compaction is not supported",
        ));
    }
    super::migrate::refuse_original_write(ledger)?;
    let documents: std::collections::HashMap<&str, &[String]> = types
        .iter()
        .filter(|t| !t.documents.is_empty())
        .map(|t| (t.event_type.as_str(), t.documents.as_slice()))
        .collect();

    let mut store = crate::kernel::log::Documents::beside(ledger);
    let mut report = Compacted::default();
    let source = BufReader::new(std::fs::File::open(ledger)?);
    let beside = ledger.with_file_name(format!(
        "{}.compacting",
        ledger
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default()
    ));
    let mut out = std::fs::File::create(&beside)?;

    for line in source.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        report.events += 1;
        report.bytes_before += line.len() as u64 + 1;
        // A line this pass cannot parse is copied through untouched. The
        // ledger is the audit record: not understanding an event is never a
        // reason to drop it.
        let rewritten = match serde_json::from_str::<Value>(&line) {
            Ok(mut event) => {
                let fields = event["type"]
                    .as_str()
                    .and_then(|t| documents.get(t))
                    .copied()
                    .unwrap_or(&[]);
                let seq = event["seq"].as_u64().unwrap_or(0);
                let moved = crate::kernel::log::move_documents(
                    &mut event["payload"],
                    fields,
                    seq,
                    &mut store,
                );
                if moved > 0 {
                    report.rewritten += 1;
                    report.documents += moved;
                    serde_json::to_string(&event).unwrap_or(line)
                } else {
                    line
                }
            }
            Err(_) => line,
        };
        report.bytes_after += rewritten.len() as u64 + 1;
        writeln!(out, "{rewritten}")?;
    }
    out.sync_all()?;
    drop(out);
    std::fs::rename(&beside, ledger)?;
    Ok(report)
}

/// What one export produced.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Exported {
    pub events: usize,
    pub inlined: usize,
    /// References whose document could not be read — named, never silently
    /// dropped: an export that quietly loses content is worse than one that
    /// says what is missing.
    pub missing: Vec<String>,
    pub bytes: u64,
}

/// Write a ledger with its documents folded back in, as one self-sufficient
/// file.
///
/// Moving documents out makes a conversation a directory rather than a file.
/// That is the right trade for reading it — a document beside the ledger has
/// lines, and inside a JSON string it has none — but it is the wrong shape for
/// handing one to somebody or putting it in an archive. This turns it back.
pub fn export(
    ledger: &Path,
    out: &mut impl Write,
    types: &[EventTypeDecl],
) -> std::io::Result<Exported> {
    let documents: std::collections::HashMap<&str, &[String]> = types
        .iter()
        .filter(|t| !t.documents.is_empty())
        .map(|t| (t.event_type.as_str(), t.documents.as_slice()))
        .collect();
    let dir = crate::contracts::document::documents_dir(ledger);

    let mut report = Exported::default();
    for line in crate::ledgers::lines(ledger)? {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        report.events += 1;
        let restored = match serde_json::from_str::<Value>(&line) {
            Ok(mut event) => {
                let fields = event["type"]
                    .as_str()
                    .and_then(|t| documents.get(t))
                    .copied()
                    .unwrap_or(&[]);
                let mut changed = false;
                for field in fields {
                    let carried = event["payload"][field.as_str()].clone();
                    let Some(reference) = crate::contracts::document::DocRef::of(&carried) else {
                        continue;
                    };
                    match crate::contracts::document::resolve(&carried, Some(&dir)) {
                        Ok(whole) => {
                            event["payload"][field.as_str()] = whole;
                            report.inlined += 1;
                            changed = true;
                        }
                        Err(_) => report.missing.push(reference.file),
                    }
                }
                if changed {
                    serde_json::to_string(&event).unwrap_or(line)
                } else {
                    line
                }
            }
            // Unreadable to this pass is still part of the record
            Err(_) => line,
        };
        report.bytes += restored.len() as u64 + 1;
        writeln!(out, "{restored}")?;
    }
    Ok(report)
}
