//! Derived sealed-segment metadata. The catalog binds every cache by digest.
//! A bad cache is rebuilt from verified source; it never makes an event absent.

use super::*;
use std::io::Read;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct IndexSeal {
    pub(super) version: u32,
    pub(super) bytes: u64,
    pub(super) digest: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct IndexRecord {
    pub stream: String,
    pub header: Header,
    pub location: Location,
}

/// A scan pins one sealed record (or a bounded active-volume window), so
/// reading its body reuses the descriptor already decoded for its header.
/// Neighboring sealed records reuse physical pages through the bounded cache.
#[derive(Debug)]
pub(crate) struct IndexWindow(Arc<Vec<IndexRecord>>);

impl IndexWindow {
    fn record(&self, seq: u64) -> Option<&IndexRecord> {
        let first = self.0.first()?.header.seq;
        let offset = usize::try_from(seq.checked_sub(first)?).ok()?;
        self.0.get(offset)
    }

    pub fn metadata(&self, seq: u64) -> Option<(Header, u64)> {
        self.record(seq)
            .map(|record| (record.header.clone(), record.location.bytes as u64 + 1))
    }

    pub fn read(&self, ledger: &Ledger, seq: u64) -> io::Result<EventEnvelope> {
        let record = self
            .record(seq)
            .ok_or_else(|| invalid("index window omitted committed position"))?;
        ledger.read_record(record.clone())
    }
}

fn native_sequence(id: &str) -> Option<u64> {
    let (seq, suffix) = id.strip_prefix("ev_")?.split_once('_')?;
    if suffix.is_empty() {
        return None;
    }
    seq.parse().ok()
}

#[cfg(test)]
fn index_path(root: &Path, number: u64) -> PathBuf {
    root.join(format!("{number:020}.index"))
}

#[cfg(test)]
pub(super) fn write_index(
    root: &Path,
    number: u64,
    records: &[IndexRecord],
) -> io::Result<IndexSeal> {
    let temporary = root.join(format!("{number:020}.index.next"));
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&temporary)?;
    let mut hash = Sha256::new();
    let mut bytes = 0;
    for record in records {
        let mut line = serde_json::to_vec(record)?;
        line.push(b'\n');
        file.write_all(&line)?;
        hash.update(&line);
        bytes += line.len() as u64;
    }
    file.sync_all()?;
    fs::rename(temporary, index_path(root, number))?;
    sync_directory(root)?;
    Ok(IndexSeal {
        version: 1,
        bytes,
        digest: format!("{:x}", hash.finalize()),
    })
}

impl Ledger {
    pub(super) fn try_index(&mut self, segment: &Segment) -> io::Result<bool> {
        let Some(seal) = &segment.seal else {
            return Ok(false);
        };
        // Pin the observed identity without reading the sealed body. A missing
        // source or a changed committed length is damage, not a cache miss.
        let file = open_segment(&self.root, segment.number, false)?;
        let meta = file.metadata()?;
        if meta.len() != seal.bytes {
            return Err(invalid("sealed segment length mismatch"));
        }
        let Some(stamp) = &seal.index else {
            return Ok(false);
        };
        if stamp.version > 2 {
            return Err(invalid("unsupported future derived index version"));
        }
        // Version one is migrated from verified source, never merely wrapped
        // in a new directory and labelled validated.
        if stamp.version != 2 {
            return Ok(false);
        }
        let Ok(directory) = pages::Directory::load(
            &self.root,
            &self.catalog.stream,
            segment,
            stamp,
            self.prefix_hash,
        ) else {
            return Ok(false);
        };
        self.identities
            .insert(segment.number, (meta.dev(), meta.ino()));
        self.prefix_hash = directory.after();
        self.native_ids = false;
        self.next_seq = seal
            .through
            .checked_add(1)
            .ok_or_else(|| invalid("sequence overflow"))?;
        self.directories.insert(segment.number, directory);
        Ok(true)
    }

    pub(super) fn rebuild_index(&mut self, number: u64) -> io::Result<()> {
        // This sealed volume has passed source verification. Publish its derived
        // cache independently; a later rejected volume never changes this source.
        let mut catalog = self.catalog.clone();
        catalog.version = 2;
        let segment = &mut catalog.segments[number as usize];
        let stamp = pages::write(
            &self.root,
            &catalog.stream,
            segment,
            &self.active_index,
            self.segment_prefixes[number as usize],
        )?;
        segment.seal.as_mut().unwrap().index = Some(stamp.clone());
        let directory = pages::Directory::load(
            &self.root,
            &catalog.stream,
            segment,
            &stamp,
            self.segment_prefixes[number as usize],
        )?;
        prepare_catalog(&self.root, &catalog)?;
        publish_catalog(&self.root)?;
        self.catalog = catalog;
        self.directories.insert(number, directory);
        self.recovery.rebuilt_indexes += 1;
        Ok(())
    }

    pub(super) fn remember(&mut self, record: IndexRecord) {
        self.prefix_hash = extend_prefix(self.prefix_hash, record.location.digest);
        self.native_ids &= native_sequence(&record.header.id) == Some(record.header.seq);
        if let Some(ids) = &mut self.verification_ids {
            ids.insert(record.header.id.clone());
        }
        self.active_locations
            .insert(record.header.id.clone(), self.active_index.len());
        self.active_index.push(record);
    }

    pub(super) fn exists(&self, id: &str) -> io::Result<bool> {
        if let Some(ids) = &self.verification_ids {
            return Ok(ids.contains(id));
        }
        Ok(self.record_id(id)?.is_some())
    }

    pub(super) fn record_id(&self, id: &str) -> io::Result<Option<IndexRecord>> {
        if let Some(&at) = self.active_locations.get(id) {
            return Ok(Some(self.active_index[at].clone()));
        }
        if let Some(seq) = native_sequence(id) {
            if let Some(record) = self.record_at(seq)? {
                if record.header.id == id {
                    return Ok(Some(record));
                }
            }
            if self.native_ids {
                return Ok(None);
            }
        }
        let active_number = self
            .active_index
            .first()
            .map(|record| record.location.segment);
        for segment in self.catalog.segments.iter().rev() {
            if segment.first >= self.next_seq
                || Some(segment.number) == active_number
                || segment.seal.is_none()
            {
                continue;
            }
            if let Some(directory) = self.directories.get(&segment.number) {
                if let Some(record) = directory.lookup(&self.root, id, &self.page_cache)? {
                    return Ok(Some(record));
                }
            } else {
                return Err(invalid("missing validated index directory"));
            }
        }
        Ok(None)
    }

    pub(super) fn record_at(&self, seq: u64) -> io::Result<Option<IndexRecord>> {
        if seq == 0 || seq >= self.next_seq {
            return Ok(None);
        }
        if let Some(first) = self.active_index.first() {
            if seq >= first.header.seq {
                return Ok(self
                    .active_index
                    .get((seq - first.header.seq) as usize)
                    .cloned());
            }
        }
        let at = self
            .catalog
            .segments
            .partition_point(|segment| segment.first <= seq);
        let segment = self
            .catalog
            .segments
            .get(at.saturating_sub(1))
            .ok_or_else(|| invalid("missing sequence segment"))?;
        if let Some(directory) = self.directories.get(&segment.number) {
            return directory
                .record(&self.root, seq, &self.page_cache)
                .map(Some);
        }
        Err(invalid("missing validated index directory"))
    }

    /// Binds a recovery state to the complete committed prefix, not merely
    /// its last identity. Rotation and appending a suffix do not change it.
    pub fn prefix_digest(&self, through: u64) -> io::Result<[u8; 32]> {
        if through >= self.next_seq {
            return Err(invalid("checkpoint boundary exceeds committed history"));
        }
        if through == self.next_seq - 1 {
            return Ok(self.prefix_hash);
        }
        if through == 0 {
            return Ok(prefix_seed(&self.catalog.stream));
        }
        let at = self
            .catalog
            .segments
            .partition_point(|segment| segment.first <= through)
            - 1;
        let segment = &self.catalog.segments[at];
        let mut hash = self.segment_prefixes[at];
        if segment.seal.is_none() {
            for record in &self.active_index {
                if record.header.seq > through {
                    break;
                }
                hash = extend_prefix(hash, record.location.digest);
            }
        } else if let Some(directory) = self.directories.get(&segment.number) {
            return directory.prefix(&self.root, through, &self.page_cache);
        } else {
            return Err(invalid("missing validated index directory"));
        }
        Ok(hash)
    }

    pub fn index_window(&self, seq: u64) -> io::Result<IndexWindow> {
        if seq == 0 || seq >= self.next_seq {
            return Err(invalid("index window outside committed history"));
        }
        if let Some(first) = self.active_index.first() {
            if seq >= first.header.seq {
                let at = ((seq - first.header.seq) as usize / 64) * 64;
                let end = at.saturating_add(64).min(self.active_index.len());
                return Ok(IndexWindow(Arc::new(self.active_index[at..end].to_vec())));
            }
        }
        let at = self
            .catalog
            .segments
            .partition_point(|segment| segment.first <= seq);
        let segment = self
            .catalog
            .segments
            .get(at.saturating_sub(1))
            .ok_or_else(|| invalid("missing sequence segment"))?;
        if let Some(directory) = self.directories.get(&segment.number) {
            return Ok(IndexWindow(Arc::new(vec![directory.record(
                &self.root,
                seq,
                &self.page_cache,
            )?])));
        }
        Err(invalid("missing validated index directory"))
    }

    /// Explicit full verification. No repair, cache rewrite, or event emission.
    /// Normal reads instead verify only the selected record's committed digest.
    pub fn verify(&self) -> io::Result<()> {
        self.check_active()?;
        for segment in &self.catalog.segments {
            let mut file = open_segment(&self.root, segment.number, false)?;
            let (bytes, digest) = match &segment.seal {
                Some(seal) => (seal.bytes, seal.digest.clone()),
                None => (
                    self.active_bytes,
                    format!("{:x}", self.active_hash.clone().finalize()),
                ),
            };
            if file.metadata()?.len() != bytes {
                return Err(invalid("segment length mismatch"));
            }
            let mut hash = Sha256::new();
            let mut buffer = [0u8; 8192];
            loop {
                let count = file.read(&mut buffer)?;
                if count == 0 {
                    break;
                }
                hash.update(&buffer[..count]);
            }
            if format!("{:x}", hash.finalize()) != digest {
                return Err(invalid("segment integrity mismatch"));
            }
        }
        Ok(())
    }
}
