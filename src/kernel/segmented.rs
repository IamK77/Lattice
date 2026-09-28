//! Segmented append storage. Audit/schema/witness enforcement stays in EventLog.
//! A catalog publication precedes the first event in a new segment.

#[cfg(test)]
#[path = "segmented_tests.rs"]
mod tests;

#[path = "segmented_index.rs"]
mod index;
#[path = "segmented_pages.rs"]
mod pages;
pub(crate) use index::IndexWindow;
use index::{IndexRecord, IndexSeal};

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{FileExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::history::Header;
use crate::EventEnvelope;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Seal {
    through: u64,
    bytes: u64,
    digest: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    index: Option<IndexSeal>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Segment {
    number: u64,
    first: u64,
    seal: Option<Seal>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Catalog {
    format: String,
    version: u32,
    stream: String,
    segments: Vec<Segment>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Location {
    pub segment: u64,
    pub offset: u64,
    pub bytes: usize,
    digest: [u8; 32],
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RotationStep {
    NextFileSynced,
    CatalogPrepared,
    CatalogPublished,
}

#[derive(Default, Debug, PartialEq, Eq)]
pub struct Recovery {
    pub discarded_tail_bytes: u64,
    pub repaired_newline: bool,
    pub rebuilt_indexes: usize,
}

#[derive(Default, Debug)]
pub struct OpenStats {
    pub sealed_body_bytes: u64,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum OpenMode {
    Append,
    Verify,
    Snapshot,
}

#[derive(Debug)]
pub struct Ledger {
    root: PathBuf,
    lease: Option<File>,
    catalog: Catalog,
    active: File,
    active_bytes: u64,
    active_hash: Sha256,
    prefix_hash: [u8; 32],
    segment_prefixes: Vec<[u8; 32]>,
    active_index: Vec<IndexRecord>,
    pub open_stats: OpenStats,
    next_seq: u64,
    active_locations: HashMap<String, usize>,
    page_cache: RefCell<pages::PageCache>,
    directories: HashMap<u64, pages::Directory>,
    native_ids: bool,
    verification_ids: Option<HashSet<String>>,
    identities: HashMap<u64, (u64, u64)>,
    limit: u64,
    failed: bool,
    repair: bool,
    preview: bool,
    #[cfg(test)]
    fail_at: Option<RotationStep>,
    pub recovery: Recovery,
}

impl Drop for Ledger {
    fn drop(&mut self) {
        self.release_writer();
    }
}

fn read_catalog(root: &Path) -> io::Result<Catalog> {
    let input = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(root.join("catalog.json"))?;
    if !input.metadata()?.is_file() {
        return Err(invalid("catalog is not a regular file"));
    }
    let catalog: Catalog = serde_json::from_reader(input).map_err(|e| invalid(e.to_string()))?;
    if catalog.format != "lattice-segmented"
        || !matches!(catalog.version, 1 | 2)
        || catalog.stream.is_empty()
        || catalog.segments.is_empty()
    {
        return Err(invalid("unsupported or incomplete segment catalog"));
    }
    let mut expected_first = 1;
    for (index, segment) in catalog.segments.iter().enumerate() {
        if segment.number != index as u64 || segment.first != expected_first {
            return Err(invalid("discontinuous segment catalog"));
        }
        match &segment.seal {
            Some(seal)
                if index + 1 < catalog.segments.len()
                    && seal.through >= segment.first
                    && seal.bytes > 0 =>
            {
                expected_first = seal
                    .through
                    .checked_add(1)
                    .ok_or_else(|| invalid("sequence overflow"))?;
            }
            None if index + 1 == catalog.segments.len() => {}
            _ => return Err(invalid("only the last segment can be active")),
        }
    }
    Ok(catalog)
}

fn prefix_seed(stream: &str) -> [u8; 32] {
    Sha256::digest(format!("lattice-record-prefix-v1:{}:{stream}", stream.len()).as_bytes()).into()
}

fn extend_prefix(previous: [u8; 32], record: [u8; 32]) -> [u8; 32] {
    let mut hash = Sha256::new();
    hash.update(previous);
    hash.update(record);
    hash.finalize().into()
}

fn segment_path(root: &Path, number: u64) -> PathBuf {
    root.join(format!("{number:020}.jsonl"))
}

fn lock(root: &Path) -> io::Result<File> {
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(root.join(".writer.lock"))?;
    // The descriptor owns the advisory lock; no stale-lock-file deletion.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(file)
}

fn open_segment(root: &Path, number: u64, append: bool) -> io::Result<File> {
    OpenOptions::new()
        .read(true)
        .append(append)
        .custom_flags(libc::O_NOFOLLOW)
        .open(segment_path(root, number))
}

fn sync_directory(root: &Path) -> io::Result<()> {
    File::open(root)?.sync_all()
}

fn prepare_catalog(root: &Path, catalog: &Catalog) -> io::Result<()> {
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(root.join("catalog.next"))?;
    serde_json::to_writer(&mut file, catalog)?;
    file.write_all(b"\n")?;
    file.sync_all()
}

fn publish_catalog(root: &Path) -> io::Result<()> {
    fs::rename(root.join("catalog.next"), root.join("catalog.json"))?;
    sync_directory(root)
}

impl Ledger {
    pub fn create(root: &Path, stream: &str, limit: u64) -> io::Result<Self> {
        if limit == 0 || stream.is_empty() {
            return Err(invalid("invalid ledger options"));
        }
        // Never reinterpret an existing directory as a new empty ledger.
        fs::create_dir(root)?;
        let parent = root
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        sync_directory(parent)?;
        let guard = lock(root)?;
        let active = OpenOptions::new()
            .create_new(true)
            .read(true)
            .append(true)
            .open(segment_path(root, 0))?;
        active.sync_all()?;
        sync_directory(root)?;
        let catalog = Catalog {
            format: "lattice-segmented".into(),
            version: 2,
            stream: stream.into(),
            segments: vec![Segment {
                number: 0,
                first: 1,
                seal: None,
            }],
        };
        prepare_catalog(root, &catalog)?;
        publish_catalog(root)?;
        let prefix_hash = prefix_seed(stream);
        Ok(Self {
            root: root.into(),
            lease: Some(guard),
            catalog,
            active,
            active_bytes: 0,
            active_hash: Sha256::new(),
            prefix_hash,
            segment_prefixes: vec![prefix_hash],
            active_index: Vec::new(),
            open_stats: OpenStats::default(),
            next_seq: 1,
            active_locations: HashMap::new(),
            page_cache: RefCell::new(pages::PageCache::default()),
            directories: HashMap::new(),
            native_ids: true,
            verification_ids: None,
            identities: HashMap::new(),
            limit,
            failed: false,
            repair: true,
            preview: false,
            #[cfg(test)]
            fail_at: None,
            recovery: Recovery::default(),
        })
    }

    #[cfg(test)]
    pub fn open(root: &Path, limit: u64) -> io::Result<Self> {
        Self::open_for(root, limit, None)
    }

    pub fn open_for(root: &Path, limit: u64, expected_stream: Option<&str>) -> io::Result<Self> {
        Self::open_mode(root, limit, expected_stream, OpenMode::Append)
    }

    /// Offline full verification never creates a lock, repairs a tail, writes
    /// an index, or starts components. An active writer makes it refuse.
    pub fn verify_path(root: &Path) -> io::Result<()> {
        Self::open_mode(root, u64::MAX, None, OpenMode::Verify).map(|_| ())
    }

    /// A closed-ledger snapshot may rebuild derived indexes but never repairs
    /// source records. Its exclusive lease prevents concurrent cache writers.
    pub fn snapshot(root: &Path) -> io::Result<Self> {
        Self::open_mode(root, u64::MAX, None, OpenMode::Snapshot)
    }

    pub fn stream(&self) -> &str {
        &self.catalog.stream
    }

    pub fn release_writer(&mut self) {
        if let Some(lease) = self.lease.take() {
            // A concurrent fork can retain the same open file description until
            // exec. Closing only our descriptor would leave its flock alive.
            if unsafe { libc::flock(lease.as_raw_fd(), libc::LOCK_UN) } != 0 {
                eprintln!(
                    "cannot release ledger lease: {}",
                    io::Error::last_os_error()
                );
            }
        }
    }

    fn open_mode(
        root: &Path,
        limit: u64,
        expected_stream: Option<&str>,
        mode: OpenMode,
    ) -> io::Result<Self> {
        let read_only = mode != OpenMode::Append;
        let verify = mode == OpenMode::Verify;
        if limit == 0 {
            return Err(invalid("zero segment limit"));
        }
        let guard = if read_only {
            let file = OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW)
                .open(root.join(".writer.lock"))?;
            let lock_mode = if verify { libc::LOCK_SH } else { libc::LOCK_EX };
            if unsafe { libc::flock(file.as_raw_fd(), lock_mode | libc::LOCK_NB) } != 0 {
                return Err(io::Error::last_os_error());
            }
            file
        } else {
            lock(root)?
        };
        let catalog = read_catalog(root)?;
        if expected_stream.is_some_and(|stream| stream != catalog.stream) {
            return Err(invalid("segmented ledger belongs to a different stream"));
        }
        // An unlisted empty next file is a prepared rotation. Nonempty data
        // is never adopted, truncated, or overwritten based on a guess.
        for entry in fs::read_dir(root)? {
            let entry = entry?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if let Some(stem) = name.strip_suffix(".jsonl") {
                let number: u64 = stem
                    .parse()
                    .map_err(|_| invalid("unexpected segment file"))?;
                if name != format!("{number:020}.jsonl") {
                    return Err(invalid("noncanonical segment name"));
                }
                let meta = entry.path().symlink_metadata()?;
                if !meta.is_file() {
                    return Err(invalid("segment is not a regular file"));
                }
                if number >= catalog.segments.len() as u64
                    && (number != catalog.segments.len() as u64 || meta.len() != 0)
                {
                    return Err(invalid("unlisted segment contains unexplained data"));
                }
            }
        }
        let last = catalog.segments.last().unwrap().number;
        let active = open_segment(root, last, !read_only)?;
        let prefix_hash = prefix_seed(&catalog.stream);
        let mut ledger = Self {
            root: root.into(),
            lease: Some(guard),
            catalog,
            active,
            active_bytes: 0,
            active_hash: Sha256::new(),
            prefix_hash,
            segment_prefixes: vec![prefix_hash],
            active_index: Vec::new(),
            open_stats: OpenStats::default(),
            next_seq: 1,
            active_locations: HashMap::new(),
            page_cache: RefCell::new(pages::PageCache::default()),
            directories: HashMap::new(),
            native_ids: true,
            verification_ids: None,
            identities: HashMap::new(),
            limit,
            failed: false,
            repair: true,
            preview: false,
            #[cfg(test)]
            fail_at: None,
            recovery: Recovery::default(),
        };
        ledger.repair = !read_only;
        ledger.preview = mode == OpenMode::Snapshot;
        ledger.verification_ids = verify.then(HashSet::new);
        for segment in ledger.catalog.segments.clone() {
            if segment.number > 0 {
                ledger.segment_prefixes.push(ledger.prefix_hash);
            }
            ledger.active_index.clear();
            ledger.active_locations.clear();
            if verify || !ledger.try_index(&segment)? {
                ledger.scan(&segment)?;
                if segment.seal.is_some() && !verify {
                    ledger.rebuild_index(segment.number)?;
                }
            }
        }
        Ok(ledger)
    }

    fn validate_event(&self, event: &EventEnvelope) -> io::Result<()> {
        if event.v != 1
            || event.stream != self.catalog.stream
            || event.seq != self.next_seq
            || event.id.is_empty()
            || self.exists(&event.id)?
        {
            return Err(invalid(
                "invalid event version, stream, identity or sequence",
            ));
        }
        for id in &event.causes {
            if !self.exists(id)? {
                return Err(invalid("unknown event cause"));
            }
        }
        event
            .seq
            .checked_add(1)
            .ok_or_else(|| invalid("sequence overflow"))?;
        Ok(())
    }

    fn scan(&mut self, segment: &Segment) -> io::Result<()> {
        let writable = segment.seal.is_none() && self.repair;
        let mut file = open_segment(&self.root, segment.number, writable)?;
        let meta = file.metadata()?;
        self.identities
            .insert(segment.number, (meta.dev(), meta.ino()));
        let original_len = meta.len();
        let mut input = BufReader::new(file.try_clone()?);
        let mut bytes = Vec::new();
        let mut offset = 0;
        let mut hash = Sha256::new();
        loop {
            bytes.clear();
            if input.read_until(b'\n', &mut bytes)? == 0 {
                break;
            }
            let terminated = bytes.ends_with(b"\n");
            let raw = bytes.strip_suffix(b"\n").unwrap_or(&bytes);
            let event: EventEnvelope = match serde_json::from_slice(raw) {
                Ok(event) => event,
                Err(error)
                    if (writable || (self.preview && segment.seal.is_none()))
                        && !terminated
                        && super::record::incomplete(raw, &error) =>
                {
                    if writable {
                        self.recovery.discarded_tail_bytes = original_len - offset;
                        file.set_len(offset)?;
                        file.sync_all()?;
                    }
                    break;
                }
                Err(error) => return Err(invalid(format!("invalid committed event: {error}"))),
            };
            self.validate_event(&event)?;
            if !(terminated || writable || self.preview && segment.seal.is_none()) {
                return Err(invalid("sealed segment has an incomplete boundary"));
            }
            let header = Header::from_event(&event);
            let location = Location {
                segment: segment.number,
                offset,
                bytes: raw.len(),
                digest: Sha256::digest(raw).into(),
            };
            self.remember(IndexRecord {
                stream: event.stream.clone(),
                header,
                location,
            });
            self.next_seq += 1;
            hash.update(&bytes);
            offset += bytes.len() as u64;
            if !terminated && writable {
                file.write_all(b"\n")?;
                file.sync_all()?;
                hash.update(b"\n");
                offset += 1;
                self.recovery.repaired_newline = true;
            }
        }
        if let Some(seal) = &segment.seal {
            self.open_stats.sealed_body_bytes += offset;
            if offset != seal.bytes
                || self.next_seq - 1 != seal.through
                || format!("{:x}", hash.finalize()) != seal.digest
            {
                return Err(invalid("sealed segment integrity mismatch"));
            }
        } else {
            self.active_bytes = offset;
            self.active_hash = hash;
        }
        Ok(())
    }

    pub fn count(&self) -> u64 {
        self.next_seq - 1
    }

    pub fn byte_len(&self) -> u64 {
        self.catalog
            .segments
            .iter()
            .filter_map(|segment| segment.seal.as_ref())
            .map(|seal| seal.bytes)
            .sum::<u64>()
            + self.active_bytes
    }

    pub fn metadata_at(&self, seq: u64) -> io::Result<Option<(Header, u64)>> {
        Ok(self
            .record_at(seq)?
            .map(|record| (record.header, record.location.bytes as u64 + 1)))
    }

    pub fn sequence_of(&self, id: &str) -> io::Result<Option<u64>> {
        Ok(self.record_id(id)?.map(|record| record.header.seq))
    }

    pub fn stream_of(root: &Path) -> io::Result<String> {
        Ok(read_catalog(root)?.stream)
    }

    /// A read-only catalog snapshot, not a writer or a full source verifier.
    pub fn source_paths(root: &Path) -> io::Result<Vec<PathBuf>> {
        Ok(read_catalog(root)?
            .segments
            .iter()
            .map(|segment| segment_path(root, segment.number))
            .collect())
    }

    #[cfg(test)]
    pub fn location(&self, id: &str) -> Option<Location> {
        self.record_id(id).unwrap().map(|record| record.location)
    }
    #[cfg(test)]
    pub fn next_seq(&self) -> u64 {
        self.next_seq
    }
    #[cfg(test)]
    pub fn segment_count(&self) -> usize {
        self.catalog.segments.len()
    }

    #[cfg(test)]
    pub fn get(&self, id: &str) -> io::Result<Option<EventEnvelope>> {
        self.record_id(id)?
            .map(|record| self.read_record(record))
            .transpose()
    }

    pub fn get_at(&self, seq: u64) -> io::Result<Option<EventEnvelope>> {
        self.record_at(seq)?
            .map(|record| self.read_record(record))
            .transpose()
    }

    fn read_record(&self, record: IndexRecord) -> io::Result<EventEnvelope> {
        let location = record.location;
        let file = open_segment(&self.root, location.segment, false)?;
        let meta = file.metadata()?;
        let expected = if location.segment == self.catalog.segments.last().unwrap().number {
            let active = self.active.metadata()?;
            (active.dev(), active.ino())
        } else {
            *self
                .identities
                .get(&location.segment)
                .ok_or_else(|| invalid("missing segment identity"))?
        };
        if (meta.dev(), meta.ino()) != expected {
            return Err(invalid("indexed segment identity changed"));
        }
        let length = location
            .bytes
            .checked_add(1)
            .ok_or_else(|| invalid("record length overflow"))?;
        let mut bytes = vec![0; length];
        file.read_exact_at(&mut bytes, location.offset)?;
        if bytes.pop() != Some(b'\n') {
            return Err(invalid("indexed record delimiter changed"));
        }
        let digest: [u8; 32] = Sha256::digest(&bytes).into();
        if digest != location.digest {
            return Err(invalid("indexed event bytes changed"));
        }
        serde_json::from_slice(&bytes).map_err(|e| invalid(e.to_string()))
    }

    fn check_active(&self) -> io::Result<()> {
        let number = self.catalog.segments.last().unwrap().number;
        let path = segment_path(&self.root, number).symlink_metadata()?;
        let opened = self.active.metadata()?;
        if !path.is_file()
            || path.dev() != opened.dev()
            || path.ino() != opened.ino()
            || opened.len() != self.active_bytes
        {
            return Err(invalid("active segment identity or length changed"));
        }
        Ok(())
    }

    #[cfg(test)]
    pub fn fail_rotation_at(&mut self, step: RotationStep) {
        self.fail_at = Some(step);
    }
    #[cfg(test)]
    fn boundary(&mut self, step: RotationStep) -> io::Result<()> {
        if self.fail_at == Some(step) {
            self.fail_at = None;
            return Err(io::Error::other(format!(
                "injected rotation failure at {step:?}"
            )));
        }
        Ok(())
    }

    fn rotate(&mut self) -> io::Result<()> {
        self.check_active()?;
        self.active.sync_all()?;
        let mut hash = Sha256::new();
        let mut offset = 0;
        let mut buffer = [0u8; 8192];
        while offset < self.active_bytes {
            let count = (self.active_bytes - offset).min(buffer.len() as u64) as usize;
            self.active.read_exact_at(&mut buffer[..count], offset)?;
            hash.update(&buffer[..count]);
            offset += count as u64;
        }
        if hash.finalize() != self.active_hash.clone().finalize() {
            return Err(invalid("active segment changed before sealing"));
        }
        self.check_active()?;
        let number = self.catalog.segments.len() as u64;
        let next_path = segment_path(&self.root, number);
        let next = match OpenOptions::new()
            .create_new(true)
            .read(true)
            .append(true)
            .open(&next_path)
        {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                let file = open_segment(&self.root, number, true)?;
                if file.metadata()?.len() != 0 {
                    return Err(invalid("prepared segment is not empty"));
                }
                file
            }
            Err(error) => return Err(error),
        };
        next.sync_all()?;
        sync_directory(&self.root)?;
        #[cfg(test)]
        self.boundary(RotationStep::NextFileSynced)?;
        let mut catalog = self.catalog.clone();
        catalog.version = 2;
        let sealed = catalog.segments.last_mut().unwrap();
        sealed.seal = Some(Seal {
            through: self.next_seq - 1,
            bytes: self.active_bytes,
            digest: format!("{:x}", self.active_hash.clone().finalize()),
            index: None,
        });
        let before = self.segment_prefixes[(number - 1) as usize];
        let stamp = pages::write(
            &self.root,
            &catalog.stream,
            sealed,
            &self.active_index,
            before,
        )?;
        sealed.seal.as_mut().unwrap().index = Some(stamp.clone());
        let directory =
            pages::Directory::load(&self.root, &catalog.stream, sealed, &stamp, before)?;
        catalog.segments.push(Segment {
            number,
            first: self.next_seq,
            seal: None,
        });
        prepare_catalog(&self.root, &catalog)?;
        #[cfg(test)]
        self.boundary(RotationStep::CatalogPrepared)?;
        publish_catalog(&self.root)?;
        #[cfg(test)]
        self.boundary(RotationStep::CatalogPublished)?;
        let old = self.active.metadata()?;
        self.identities.insert(number - 1, (old.dev(), old.ino()));
        self.directories.insert(number - 1, directory);
        self.catalog = catalog;
        self.active = next;
        self.active_bytes = 0;
        self.active_hash = Sha256::new();
        self.segment_prefixes.push(self.prefix_hash);
        self.active_index.clear();
        self.active_locations.clear();
        Ok(())
    }

    #[cfg(test)]
    pub fn append(&mut self, event: &EventEnvelope) -> io::Result<()> {
        self.append_line(event, &serde_json::to_vec(event)?)
    }

    #[cfg(test)]
    fn append_line(&mut self, event: &EventEnvelope, line: &[u8]) -> io::Result<()> {
        self.validate_append(event)?;
        self.append_validated_line(event, line)
    }

    pub fn validate_append(&self, event: &EventEnvelope) -> io::Result<()> {
        if self.failed {
            return Err(io::Error::other("writer failed; reopen before continuing"));
        }
        self.validate_event(event)
    }

    /// The caller holds the ledger lock continuously from validate_append.
    /// No historical lookup may happen after crossing this write boundary.
    pub fn append_validated_line(&mut self, event: &EventEnvelope, line: &[u8]) -> io::Result<()> {
        let mut bytes = line.to_vec();
        let result = (|| {
            self.check_active()?;
            if self.active_bytes > 0 && self.active_bytes >= self.limit {
                self.rotate()?;
            }
            let location = Location {
                segment: self.catalog.segments.last().unwrap().number,
                offset: self.active_bytes,
                bytes: bytes.len(),
                digest: Sha256::digest(&bytes).into(),
            };
            bytes.push(b'\n');
            self.active.write_all(&bytes)?;
            self.active.sync_all()?;
            self.active_hash.update(&bytes);
            self.active_bytes += bytes.len() as u64;
            self.remember(IndexRecord {
                stream: event.stream.clone(),
                header: Header::from_event(event),
                location,
            });
            self.next_seq += 1;
            Ok(())
        })();
        if result.is_err() {
            self.failed = true;
        }
        result
    }
}
