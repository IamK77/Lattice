//! A shared lazy lookup for model material, UI facts and historical cards.
use super::{identity_for_event, operations, Projection, Reference};
use crate::{EventEnvelope, LogReader};
use std::io;

pub(crate) fn reference_for_event(
    cache: &mut Option<Projection>,
    reader: &LogReader,
    event: &EventEnvelope,
    through: u64,
) -> io::Result<Option<Reference>> {
    if event.seq > through || through > reader.snapshot_end() || event.stream != reader.stream() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "background reference is outside its committed view",
        ));
    }
    let creation = identity_for_event(reader, event)?;
    let operation = creation.is_none();
    let identity = match creation {
        Some(identity) => Some(identity),
        None => operations::receipt_identity(reader, event)?,
    };
    let Some(identity) = identity else {
        return Ok(None);
    };
    if cache
        .as_ref()
        .is_some_and(|projection| projection.through() > through)
    {
        // UI facts may be revisited at an earlier historical boundary. Future
        // identity evidence must not decide which invocation was current then.
        *cache = None;
    }
    let projection = match cache {
        Some(projection) => {
            if projection.through() < through {
                projection.advance(reader, through)?;
            }
            projection
        }
        slot @ None => slot.insert(Projection::recover(reader, through)?),
    };
    // An operation receipt can describe a rejected target, but it cannot
    // establish an invocation or reserve an alias that was never created.
    let Some(alias) = projection.alias(&identity) else {
        return if operation {
            Ok(None)
        } else {
            Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "background identity has no committed assignment",
            ))
        };
    };
    Ok(Some(Reference {
        alias: alias.to_owned(),
        identity,
    }))
}
