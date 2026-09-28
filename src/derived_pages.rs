//! Copy-on-write pages of derived records. Opening the directory
//! reads no old records or event bodies; one bounded page is decoded at a time.

use std::cell::{Cell, RefCell};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Runtime-local storage contract; persisted rows and directories are pure data.
pub(super) trait Item: Clone + Serialize + serde::de::DeserializeOwned {
    fn lookup_key(&self) -> Option<&str>;

    fn same_identity(&self, other: &Self) -> bool {
        self.lookup_key() == other.lookup_key()
    }
}

#[cfg(test)]
#[path = "view/pages/tests.rs"]
mod tests;

const PAGE_RECORDS: usize = 128;
const PAGE_BYTES: usize = 64 * 1024;
static TEMPORARY: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Slot {
    first: usize,
    count: usize,
    bytes: usize,
    digest: [u8; 32],
    keys: [u64; 4],
}

struct Cached<T> {
    index: usize,
    records: Vec<T>,
    dirty: bool,
}

pub(super) struct Pages<T: Item> {
    root: Option<PathBuf>,
    slots: RefCell<Vec<Slot>>,
    len: usize,
    cache: RefCell<Option<Cached<T>>>,
    // Legacy and memory ledgers have no persistent recovery cache.
    memory: Vec<T>,
    reads: Cell<usize>,
}

impl<T: Item> Pages<T> {
    pub fn open(root: Option<PathBuf>, slots: Vec<Slot>, len: usize) -> io::Result<Self> {
        let mut next = 0usize;
        for slot in &slots {
            if slot.first != next
                || slot.count == 0
                || slot.count > PAGE_RECORDS
                || slot.bytes == 0
                || (slot.bytes > PAGE_BYTES && slot.count > 1)
            {
                return Err(invalid("invalid card page directory"));
            }
            next = next
                .checked_add(slot.count)
                .ok_or_else(|| invalid("card page range overflow"))?;
        }
        if next != len || (root.is_none() && len != 0) {
            return Err(invalid("card page directory boundary mismatch"));
        }
        Ok(Self {
            root,
            slots: RefCell::new(slots),
            len,
            cache: RefCell::new(None),
            memory: Vec::new(),
            reads: Cell::new(0),
        })
    }

    pub fn len(&self) -> usize {
        self.len
    }

    #[cfg(test)]
    pub fn read_count(&self) -> usize {
        self.reads.get()
    }

    #[cfg(test)]
    pub fn resident_records(&self) -> usize {
        self.memory.len()
            + self
                .cache
                .borrow()
                .as_ref()
                .map_or(0, |page| page.records.len())
    }

    fn page_index(&self, index: usize) -> io::Result<usize> {
        if index >= self.len {
            return Err(invalid("card ordinal outside history"));
        }
        self.slots
            .borrow()
            .partition_point(|slot| slot.first <= index)
            .checked_sub(1)
            .ok_or_else(|| invalid("card ordinal has no page"))
    }

    fn cache_page(&self, index: usize) -> io::Result<()> {
        if self
            .cache
            .borrow()
            .as_ref()
            .is_some_and(|page| page.index == index)
        {
            return Ok(());
        }
        self.flush()?;
        let root = self
            .root
            .as_ref()
            .ok_or_else(|| invalid("card page has no root"))?;
        let slot = self
            .slots
            .borrow()
            .get(index)
            .cloned()
            .ok_or_else(|| invalid("card page is absent"))?;
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(page_path(root, &slot.digest))?;
        if !file.metadata()?.is_file() || file.metadata()?.len() != slot.bytes as u64 {
            return Err(invalid("card page size or file type mismatch"));
        }
        let mut bytes = Vec::new();
        file.take(slot.bytes as u64 + 1).read_to_end(&mut bytes)?;
        if bytes.len() != slot.bytes || <[u8; 32]>::from(Sha256::digest(&bytes)) != slot.digest {
            return Err(invalid("card page checksum mismatch"));
        }
        let records: Vec<T> = serde_json::from_slice(&bytes)?;
        if records.len() != slot.count || key_bits(&records) != slot.keys {
            return Err(invalid("card page contents disagree with its directory"));
        }
        self.reads.set(self.reads.get().saturating_add(1));
        *self.cache.borrow_mut() = Some(Cached {
            index,
            records,
            dirty: false,
        });
        Ok(())
    }

    pub fn get(&self, index: usize) -> io::Result<T> {
        if self.root.is_none() {
            return self
                .memory
                .get(index)
                .cloned()
                .ok_or_else(|| invalid("card ordinal outside history"));
        }
        let page = self.page_index(index)?;
        self.cache_page(page)?;
        let cache = self.cache.borrow();
        cache
            .as_ref()
            .and_then(|cached| cached.records.get(index - self.slots.borrow()[page].first))
            .cloned()
            .ok_or_else(|| invalid("card missing from its page"))
    }

    pub fn replace(&mut self, index: usize, record: T) -> io::Result<()> {
        if self.root.is_none() {
            let target = self
                .memory
                .get_mut(index)
                .ok_or_else(|| invalid("card ordinal outside history"))?;
            *target = record;
            return Ok(());
        }
        let page = self.page_index(index)?;
        self.cache_page(page)?;
        let slot = &mut self.slots.get_mut()[page];
        let cached = self
            .cache
            .get_mut()
            .as_mut()
            .ok_or_else(|| invalid("card cache is absent"))?;
        let target = &mut cached.records[index - slot.first];
        if !target.same_identity(&record) {
            return Err(invalid("card replacement changed its immutable identity"));
        }
        slot.bytes =
            slot.bytes - serde_json::to_vec(target)?.len() + serde_json::to_vec(&record)?.len();
        *target = record;
        cached.dirty = true;
        if slot.bytes > PAGE_BYTES && slot.count > 1 {
            self.split_page(page)?;
        }
        Ok(())
    }

    fn split_page(&mut self, index: usize) -> io::Result<()> {
        let cached = self
            .cache
            .get_mut()
            .take()
            .ok_or_else(|| invalid("card cache is absent"))?;
        let root = self
            .root
            .as_ref()
            .ok_or_else(|| invalid("card page has no root"))?;
        let mut first = self.slots.get_mut()[index].first;
        let mut slots = Vec::new();
        let mut records = Vec::new();
        let mut bytes = 2usize;
        for record in cached.records {
            let size = serde_json::to_vec(&record)?.len();
            if !records.is_empty()
                && (records.len() == PAGE_RECORDS || bytes.saturating_add(size + 1) > PAGE_BYTES)
            {
                slots.push(publish(root, first, &records)?);
                first += records.len();
                records.clear();
                bytes = 2;
            }
            bytes += size + usize::from(!records.is_empty());
            records.push(record);
        }
        if !records.is_empty() {
            slots.push(publish(root, first, &records)?);
        }
        self.slots.get_mut().splice(index..index + 1, slots);
        Ok(())
    }

    pub fn push(&mut self, record: T) -> io::Result<usize> {
        let ordinal = self.len;
        if self.root.is_none() {
            self.memory.push(record);
            self.len += 1;
            return Ok(ordinal);
        }
        let size = serde_json::to_vec(&record)?.len();
        let keys = key_bits(std::slice::from_ref(&record));
        let append = self.slots.borrow().last().is_some_and(|slot| {
            slot.count < PAGE_RECORDS && slot.bytes.saturating_add(size + 1) <= PAGE_BYTES
        });
        if append {
            let index = self.slots.get_mut().len() - 1;
            self.cache_page(index)?;
            let cached = self
                .cache
                .get_mut()
                .as_mut()
                .ok_or_else(|| invalid("card cache is absent"))?;
            cached.records.push(record);
            cached.dirty = true;
            let slot = &mut self.slots.get_mut()[index];
            slot.count += 1;
            slot.bytes += size + 1;
            for (bits, mask) in slot.keys.iter_mut().zip(keys) {
                *bits |= mask;
            }
        } else {
            self.flush()?;
            let index = self.slots.get_mut().len();
            self.slots.get_mut().push(Slot {
                first: ordinal,
                count: 1,
                bytes: size + 2,
                digest: [0; 32],
                keys,
            });
            *self.cache.get_mut() = Some(Cached {
                index,
                records: vec![record],
                dirty: true,
            });
        }
        self.len += 1;
        Ok(ordinal)
    }

    /// Bloom hints only skip definite misses; every candidate compares the full
    /// lookup identity, and a failed page read is never treated as a miss.
    pub fn find_last(&self, call: &str) -> io::Result<Option<usize>> {
        if self.root.is_none() {
            return Ok(self
                .memory
                .iter()
                .rposition(|record| record.lookup_key() == Some(call)));
        }
        let mask = key_mask(call);
        let count = self.slots.borrow().len();
        for index in (0..count).rev() {
            let slot = self.slots.borrow()[index].clone();
            if !slot
                .keys
                .iter()
                .zip(mask)
                .all(|(bits, required)| bits & required == required)
            {
                continue;
            }
            self.cache_page(index)?;
            let cache = self.cache.borrow();
            let cached = cache
                .as_ref()
                .ok_or_else(|| invalid("card cache is absent"))?;
            if let Some(offset) = cached
                .records
                .iter()
                .rposition(|record| record.lookup_key() == Some(call))
            {
                return Ok(Some(slot.first + offset));
            }
        }
        Ok(None)
    }

    pub fn directory(&self) -> io::Result<Vec<Slot>> {
        self.flush()?;
        Ok(self.slots.borrow().clone())
    }

    fn flush(&self) -> io::Result<()> {
        let mut cache = self.cache.borrow_mut();
        let Some(cached) = cache.as_mut().filter(|cached| cached.dirty) else {
            return Ok(());
        };
        let root = self
            .root
            .as_ref()
            .ok_or_else(|| invalid("dirty card page has no root"))?;
        let mut slots = self.slots.borrow_mut();
        let slot = &mut slots[cached.index];
        *slot = publish(root, slot.first, &cached.records)?;
        cached.dirty = false;
        Ok(())
    }
}

fn publish<T: Item>(root: &Path, first: usize, records: &[T]) -> io::Result<Slot> {
    let bytes = serde_json::to_vec(records)?;
    if records.is_empty()
        || records.len() > PAGE_RECORDS
        || (bytes.len() > PAGE_BYTES && records.len() > 1)
    {
        return Err(invalid("card page exceeds its record or byte budget"));
    }
    let digest = Sha256::digest(&bytes).into();
    let (temporary, mut file) = loop {
        let temporary = root.join(format!(
            "cards-{}-{}.next",
            std::process::id(),
            TEMPORARY.fetch_add(1, Ordering::Relaxed)
        ));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(&temporary)
        {
            Ok(file) => break (temporary, file),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    };
    file.write_all(&bytes)?;
    file.sync_all()?;
    fs::rename(&temporary, page_path(root, &digest))?;
    File::open(root)?.sync_all()?;
    Ok(Slot {
        first,
        count: records.len(),
        bytes: bytes.len(),
        digest,
        keys: key_bits(records),
    })
}

fn page_path(root: &Path, digest: &[u8; 32]) -> PathBuf {
    let hex: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
    root.join(format!("cards-{hex}.json"))
}

fn key_mask(call: &str) -> [u64; 4] {
    let digest = Sha256::digest(call.as_bytes());
    std::array::from_fn(|index| 1 << (digest[index] % 64))
}

fn key_bits<T: Item>(records: &[T]) -> [u64; 4] {
    let mut bits = [0; 4];
    for record in records {
        if let Some(call) = record.lookup_key() {
            for (target, mask) in bits.iter_mut().zip(key_mask(call)) {
                *target |= mask;
            }
        }
    }
    bits
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
