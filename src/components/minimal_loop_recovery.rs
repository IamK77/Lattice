//! Pure material recovery. Scanning an audited prefix never invokes the loop
//! handler, recreates a pending execution queue, or emits a model/tool request.

use std::io;

use super::{ce, material_store, model_outcome::ModelOutcomes, MinimalLoop};
use crate::kernel::log::Header;
use crate::LogReader;

const KEY: &str = "minimal-loop-material";
const VERSION: u32 = 5;
const CHECKPOINT_INTERVAL: u64 = 256;

fn checkpoint_key(owner: &str) -> String {
    use sha2::{Digest, Sha256};
    // Instance names are data, not filesystem paths. Separate projections
    // without letting unusual names select directories or alias another key.
    format!("{KEY}-{:x}", Sha256::digest(owner.as_bytes()))
}

#[cfg(test)]
#[path = "minimal_loop_recovery_ownership_tests.rs"]
mod ownership_tests;
#[cfg(test)]
#[path = "minimal_loop_recovery_tests.rs"]
mod tests;

#[derive(Default)]
pub(super) struct Material {
    pub parts: material_store::Store,
    models: ModelOutcomes,
}

#[derive(Default, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Checkpoint {
    parts: material_store::State,
    models: ModelOutcomes,
}

impl Material {
    fn supersede(&mut self, header: &Header) -> io::Result<()> {
        if header.event_type == ce::USER_MESSAGE {
            self.parts.supersede(&header.causes)?;
        }
        Ok(())
    }

    fn observe(&mut self, reader: &LogReader, header: &Header) -> io::Result<()> {
        if crate::components::call_purpose::auxiliary_header(reader, header)?
            || !self.models.accept(reader, header)?
        {
            return Ok(());
        }
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
        owner: &str,
    ) -> io::Result<()> {
        if !self.rebuilt {
            self.sync_material(reader, event.seq.saturating_sub(1), owner)?;
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
            || self.material.models.owner() != owner
        {
            return Err(io::Error::other(
                "a running loop cannot change its history source or owner",
            ));
        }
        // This projection is only a recovery acceleration. Committed input
        // may still be waiting behind a gate; it must not leak into live parts.
        if event.seq.saturating_sub(self.material_saved) >= CHECKPOINT_INTERVAL {
            self.sync_material(reader, event.seq, owner)?;
        }
        self.parts
            .push(event.id.clone(), event.event_type == ce::USER_MESSAGE)?;
        Ok(())
    }

    pub(super) fn sync_material(
        &mut self,
        reader: &LogReader,
        through: u64,
        owner: &str,
    ) -> io::Result<()> {
        let key = checkpoint_key(owner);
        let cold_start = self
            .material_source
            .as_ref()
            .is_none_or(|source| !source.matches(reader))
            || self.material.models.owner() != owner;
        if cold_start {
            let mut checkpoint = reader.load_checkpoint::<Checkpoint>(&key, VERSION, through)?;
            if checkpoint
                .state
                .as_ref()
                .is_some_and(|state| state.models.owner() != owner)
            {
                // A cache is an acceleration, never authority over ownership.
                checkpoint.state = None;
                checkpoint.through = 0;
                eprintln!("slow recovery for {key}: checkpoint owner mismatch");
            }
            if let Some(reason) = checkpoint.cold_reason {
                if reader.path().is_some() {
                    eprintln!("slow recovery for {KEY}: {reason}");
                }
            }
            let state = checkpoint.state.unwrap_or_else(|| Checkpoint {
                parts: Default::default(),
                models: ModelOutcomes::new(owner),
            });
            match material_store::Store::open(reader, state.parts) {
                Ok(parts) => {
                    self.material = Material {
                        parts,
                        models: state.models,
                    };
                    self.material_through = checkpoint.through;
                    self.material_saved = checkpoint.through;
                }
                Err(error) => {
                    eprintln!("slow recovery for {KEY}: {error}");
                    self.material = Material {
                        parts: material_store::Store::open(reader, Default::default())?,
                        models: ModelOutcomes::new(owner),
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
                models: ModelOutcomes::new(owner),
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
            match self.material.parts.snapshot().and_then(|parts| {
                reader.save_checkpoint(
                    &key,
                    VERSION,
                    through,
                    &Checkpoint {
                        parts,
                        models: self.material.models.clone(),
                    },
                )
            }) {
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
            models: ModelOutcomes::new(self.material.models.owner()),
        };
        seed.advance(reader, 1, base)?;
        // Forking published this exact prefix before adding the first delivery.
        seed.parts.snapshot()?;
        reader.try_visit_header_range(base + 1, base + 1, |batch| {
            for header in batch {
                seed.observe(reader, header)?;
            }
            Ok(())
        })?;
        seed.parts.snapshot()?;
        Ok(())
    }

    #[cfg(test)]
    pub(super) fn restore_parts(
        reader: &LogReader,
        before: u64,
        owner: &str,
    ) -> io::Result<Vec<String>> {
        let mut state = Material {
            models: ModelOutcomes::new(owner),
            ..Default::default()
        };
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
