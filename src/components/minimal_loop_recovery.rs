//! Pure material recovery. Scanning an audited prefix never invokes the loop
//! handler, recreates a pending execution queue, or emits a model/tool request.

use std::io;

use super::{ce, material_store, MinimalLoop};
use crate::kernel::log::Header;
use crate::LogReader;

const KEY: &str = "minimal-loop-material";
const VERSION: u32 = 2;
const CHECKPOINT_INTERVAL: u64 = 256;

#[derive(Default)]
pub(super) struct Material {
    pub parts: material_store::Store,
}

impl Material {
    fn supersede(&mut self, header: &Header) -> io::Result<()> {
        if header.event_type == ce::USER_MESSAGE {
            self.parts.supersede(&header.causes)?;
        }
        Ok(())
    }

    fn observe(&mut self, reader: &LogReader, header: &Header) -> io::Result<()> {
        if !matches!(
            header.event_type.as_str(),
            ce::USER_MESSAGE
                | ce::WAKE
                | ce::MODEL_CALL_COMPLETED
                | ce::TOOL_EXEC_COMPLETED
                | ce::INTERRUPTED
        ) {
            return Ok(());
        }
        if matches!(
            header.event_type.as_str(),
            ce::MODEL_CALL_COMPLETED | ce::INTERRUPTED
        ) {
            if let Some(cause) = header.causes.first() {
                if reader
                    .header(cause)?
                    .is_some_and(|request| request.has_purpose)
                {
                    return Ok(());
                }
            }
        }
        self.supersede(header)?;
        self.parts
            .push(header.id.clone(), header.event_type == ce::USER_MESSAGE)?;
        Ok(())
    }

    fn advance(&mut self, reader: &LogReader, from: u64, through: u64) -> io::Result<()> {
        reader.try_visit_header_range(from, through, |batch| {
            for header in batch {
                self.observe(reader, header)?;
            }
            Ok(())
        })
    }
}

impl MinimalLoop {
    pub(super) fn accept_material(
        &mut self,
        reader: &LogReader,
        event: &crate::EventEnvelope,
    ) -> io::Result<()> {
        if !self.rebuilt {
            self.sync_material(reader, event.seq.saturating_sub(1))?;
            self.parts = self.material.parts.fork()?;
            if event.event_type == ce::USER_MESSAGE {
                self.parts.supersede(&event.causes)?;
            }
            self.material_base = Some(event.seq.saturating_sub(1));
            self.rebuilt = true;
        } else if self
            .material_source
            .as_ref()
            .is_none_or(|source| !source.matches(reader))
        {
            return Err(io::Error::other(
                "a running loop cannot change its history source",
            ));
        }
        // This projection is only a recovery acceleration. Committed input
        // may still be waiting behind a gate; it must not leak into live parts.
        if event.seq.saturating_sub(self.material_saved) >= CHECKPOINT_INTERVAL {
            self.sync_material(reader, event.seq)?;
        }
        self.parts
            .push(event.id.clone(), event.event_type == ce::USER_MESSAGE)?;
        Ok(())
    }

    pub(super) fn sync_material(&mut self, reader: &LogReader, through: u64) -> io::Result<()> {
        let cold_start = self
            .material_source
            .as_ref()
            .is_none_or(|source| !source.matches(reader));
        if cold_start {
            let checkpoint =
                reader.load_checkpoint::<material_store::State>(KEY, VERSION, through)?;
            if let Some(reason) = checkpoint.cold_reason {
                if reader.path().is_some() {
                    eprintln!("slow recovery for {KEY}: {reason}");
                }
            }
            match material_store::Store::open(reader, checkpoint.state.unwrap_or_default()) {
                Ok(parts) => {
                    self.material = Material { parts };
                    self.material_through = checkpoint.through;
                    self.material_saved = checkpoint.through;
                }
                Err(error) => {
                    eprintln!("slow recovery for {KEY}: {error}");
                    self.material = Material {
                        parts: material_store::Store::open(reader, Default::default())?,
                    };
                    self.material_through = 0;
                    self.material_saved = 0;
                }
            }
            self.material_source = Some(reader.identity());
        }
        if through < self.material_through {
            return Err(io::Error::other(
                "material delivery precedes its recovered prefix",
            ));
        }
        let mut rebuilt = false;
        if let Err(error) = self.advance_material(reader, through) {
            if self.material_through == 0 {
                return Err(error);
            }
            eprintln!("slow recovery for {KEY}: {error}");
            self.material = Material {
                parts: material_store::Store::open(reader, Default::default())?,
            };
            self.material_through = 0;
            self.material_saved = 0;
            self.advance_material(reader, through)?;
            rebuilt = true;
        }
        if cold_start
            || rebuilt
            || through.saturating_sub(self.material_saved) >= CHECKPOINT_INTERVAL
        {
            match self
                .material
                .parts
                .snapshot()
                .and_then(|state| reader.save_checkpoint(KEY, VERSION, through, &state))
            {
                Ok(_) => self.material_saved = through,
                Err(error) => eprintln!("cannot save derived recovery state for {KEY}: {error}"),
            }
        }
        Ok(())
    }

    fn advance_material(&mut self, reader: &LogReader, through: u64) -> io::Result<()> {
        reader.try_visit_header_range(self.material_through + 1, through, |batch| {
            for header in batch {
                self.material.observe(reader, header)?;
                self.material_through = header.seq;
            }
            Ok(())
        })
    }

    /// Republish the immutable historical seed, never replace the live view
    /// with a newer historical projection. A damaged live-only page remains
    /// an explicit error: its actual deliveries cannot be guessed from history.
    pub(super) fn repair_material_pages(&self, reader: &LogReader) -> io::Result<()> {
        let base = self
            .material_base
            .ok_or_else(|| io::Error::other("material has no recovery seed"))?;
        let mut seed = Material {
            parts: material_store::Store::open(reader, Default::default())?,
        };
        seed.advance(reader, 1, base)?;
        // Forking published this exact prefix before adding the first delivery.
        seed.parts.snapshot()?;
        reader.try_visit_header_range(base + 1, base + 1, |batch| {
            for header in batch {
                seed.supersede(header)?;
                seed.parts
                    .push(header.id.clone(), header.event_type == ce::USER_MESSAGE)?;
            }
            Ok(())
        })?;
        seed.parts.snapshot()?;
        Ok(())
    }

    #[cfg(test)]
    pub(super) fn restore_parts(reader: &LogReader, before: u64) -> io::Result<Vec<String>> {
        let mut state = Material::default();
        state.advance(reader, 1, before.saturating_sub(1))?;
        // The current forwarded input replaces its source before it is added
        // by the caller. It is not part of this checkpoint's committed prefix.
        reader.try_visit_header_range(before, before, |batch| {
            for header in batch {
                state.supersede(header)?;
            }
            Ok(())
        })?;
        Ok(state.parts.collect())
    }
}
