//! Fixed-size physical index pages. The small directory is bound by the
//! catalog; every page is independently checked when it is actually read.

use super::*;
use std::collections::VecDeque;
use std::io::Read;

const PAGE: usize = 64 * 1024;
const DESCRIPTOR: usize = 64;
const ID_ENTRY: usize = 40;
const IDS_PER_PAGE: usize = PAGE / ID_ENTRY;
const CACHE_BYTES: usize = 8 * 1024 * 1024;
type Hash = [u8; 32];

#[cfg(test)]
#[path = "segmented_pages_tests.rs"]
mod tests;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PageSeal {
    used: u32,
    digest: Hash,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Pages {
    file: String,
    pages: Vec<PageSeal>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Prefix {
    before: Hash,
    after: Hash,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Directory {
    version: u32,
    stream: String,
    number: u64,
    first: u64,
    through: u64,
    body_bytes: u64,
    body_digest: String,
    descriptors: Pages,
    headers: Pages,
    ids: Pages,
    prefixes: Vec<Prefix>,
    id_ranges: Vec<(Hash, Hash)>,
}

#[derive(Debug)]
struct PageKey {
    file: String,
    page: usize,
    digest: Hash,
    used: u32,
}

type CacheEntry = (PageKey, Arc<Vec<u8>>);

#[derive(Debug, Default)]
pub(super) struct PageCache {
    entries: VecDeque<CacheEntry>,
    pub loads: u64,
    pub bytes: usize,
    #[cfg(test)]
    identity_comparisons: usize,
}

impl PageCache {
    fn get(&mut self, file: &str, page: usize, seal: &PageSeal) -> Option<Arc<Vec<u8>>> {
        let at = self.entries.iter().position(|(key, _)| {
            key.file == file
                && key.page == page
                && key.digest == seal.digest
                && key.used == seal.used
        })?;
        let entry = self.entries.remove(at)?;
        let bytes = Arc::clone(&entry.1);
        self.entries.push_back(entry);
        Some(bytes)
    }

    fn put(&mut self, file: &str, page: usize, seal: &PageSeal, bytes: Arc<Vec<u8>>) {
        self.loads += 1;
        let key = PageKey {
            file: file.to_owned(),
            page,
            digest: seal.digest,
            used: seal.used,
        };
        let size = bytes.capacity() + key.file.capacity();
        self.entries.reserve(1);
        let slots = self.entries.capacity() * std::mem::size_of::<CacheEntry>();
        while self.bytes.saturating_add(size).saturating_add(slots) > CACHE_BYTES {
            let Some((key, old)) = self.entries.pop_front() else {
                break;
            };
            self.bytes -= old.capacity() + key.file.capacity();
        }
        self.bytes += size;
        self.entries.push_back((key, bytes));
    }
}

struct Writer<'a> {
    root: &'a Path,
    temporary: PathBuf,
    file: File,
    hash: Sha256,
    seals: Vec<PageSeal>,
    buffer: Vec<u8>,
    logical: u64,
}

impl<'a> Writer<'a> {
    fn new(root: &'a Path, number: u64, kind: &str) -> io::Result<Self> {
        let temporary = root.join(format!("{number:020}.{kind}.pages.next"));
        let file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(&temporary)?;
        if !file.metadata()?.is_file() {
            return Err(invalid("index temporary is not a file"));
        }
        Ok(Self {
            root,
            temporary,
            file,
            hash: Sha256::new(),
            seals: Vec::new(),
            buffer: Vec::with_capacity(PAGE),
            logical: 0,
        })
    }

    fn append(&mut self, mut bytes: &[u8]) -> io::Result<()> {
        self.logical = self
            .logical
            .checked_add(bytes.len() as u64)
            .ok_or_else(|| invalid("index size overflow"))?;
        while !bytes.is_empty() {
            let count = bytes.len().min(PAGE - self.buffer.len());
            self.buffer.extend_from_slice(&bytes[..count]);
            bytes = &bytes[count..];
            if self.buffer.len() == PAGE {
                self.flush()?;
            }
        }
        Ok(())
    }

    fn flush(&mut self) -> io::Result<()> {
        if self.buffer.is_empty() {
            return Ok(());
        }
        let used = self.buffer.len() as u32;
        self.buffer.resize(PAGE, 0);
        self.file.write_all(&self.buffer)?;
        self.hash.update(&self.buffer);
        self.seals.push(PageSeal {
            used,
            digest: Sha256::digest(&self.buffer).into(),
        });
        self.buffer.clear();
        Ok(())
    }

    fn finish(mut self) -> io::Result<Pages> {
        self.flush()?;
        self.file.sync_all()?;
        let name = format!("index-data-{:x}.pages", self.hash.finalize());
        fs::rename(&self.temporary, self.root.join(&name))?;
        Ok(Pages {
            file: name,
            pages: self.seals,
        })
    }
}

fn directory_path(root: &Path, number: u64) -> PathBuf {
    root.join(format!("{number:020}.index"))
}

fn u64_at(bytes: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap())
}

pub(super) fn write(
    root: &Path,
    stream: &str,
    segment: &Segment,
    records: &[index::IndexRecord],
    before: Hash,
) -> io::Result<index::IndexSeal> {
    let seal = segment
        .seal
        .as_ref()
        .ok_or_else(|| invalid("cannot index an unsealed segment"))?;
    let mut descriptors = Writer::new(root, segment.number, "descriptors")?;
    let mut headers = Writer::new(root, segment.number, "headers")?;
    let mut ids = Vec::with_capacity(records.len());
    let mut prefixes = Vec::new();
    let mut hash = before;
    for chunk in records.chunks(PAGE / DESCRIPTOR) {
        let before = hash;
        for record in chunk {
            let header = serde_json::to_vec(&record.header)?;
            let mut descriptor = [0u8; DESCRIPTOR];
            descriptor[0..8].copy_from_slice(&record.location.offset.to_le_bytes());
            descriptor[8..16].copy_from_slice(&(record.location.bytes as u64).to_le_bytes());
            descriptor[16..24].copy_from_slice(&headers.logical.to_le_bytes());
            descriptor[24..32].copy_from_slice(&(header.len() as u64).to_le_bytes());
            descriptor[32..64].copy_from_slice(&record.location.digest);
            descriptors.append(&descriptor)?;
            headers.append(&header)?;
            hash = extend_prefix(hash, record.location.digest);
            let digest: Hash = Sha256::digest(record.header.id.as_bytes()).into();
            ids.push((digest, record.header.seq));
        }
        prefixes.push(Prefix {
            before,
            after: hash,
        });
    }
    ids.sort_unstable();
    let mut id_writer = Writer::new(root, segment.number, "ids")?;
    let mut id_ranges = Vec::new();
    for chunk in ids.chunks(IDS_PER_PAGE) {
        id_ranges.push((chunk.first().unwrap().0, chunk.last().unwrap().0));
        for (hash, seq) in chunk {
            id_writer.append(hash)?;
            id_writer.append(&seq.to_le_bytes())?;
        }
        id_writer.flush()?;
    }
    let directory = Directory {
        version: 2,
        stream: stream.into(),
        number: segment.number,
        first: segment.first,
        through: seal.through,
        body_bytes: seal.bytes,
        body_digest: seal.digest.clone(),
        descriptors: descriptors.finish()?,
        headers: headers.finish()?,
        ids: id_writer.finish()?,
        prefixes,
        id_ranges,
    };
    directory.validate(stream, segment, before)?;
    let bytes = serde_json::to_vec(&directory)?;
    let temporary = root.join(format!("{:020}.index.next", segment.number));
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(&temporary)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    fs::rename(temporary, directory_path(root, segment.number))?;
    sync_directory(root)?;
    Ok(index::IndexSeal {
        version: 2,
        bytes: bytes.len() as u64,
        digest: format!("{:x}", Sha256::digest(&bytes)),
    })
}

impl Pages {
    fn validate(&self, full_before_last: bool) -> io::Result<()> {
        let Some(hash) = self
            .file
            .strip_prefix("index-data-")
            .and_then(|name| name.strip_suffix(".pages"))
        else {
            return Err(invalid("invalid index page file name"));
        };
        if hash.len() != 64 || !hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(invalid("invalid index page content name"));
        }
        for (at, page) in self.pages.iter().enumerate() {
            if page.used == 0
                || page.used as usize > PAGE
                || (full_before_last && at + 1 < self.pages.len() && page.used as usize != PAGE)
            {
                return Err(invalid("invalid index page length"));
            }
        }
        Ok(())
    }

    fn open(&self, root: &Path) -> io::Result<File> {
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(root.join(&self.file))?;
        let metadata = file.metadata()?;
        if !metadata.is_file()
            || metadata.len()
                != (self.pages.len() as u64)
                    .checked_mul(PAGE as u64)
                    .ok_or_else(|| invalid("index page size overflow"))?
        {
            return Err(invalid("index page file length mismatch"));
        }
        Ok(file)
    }

    fn read(
        &self,
        root: &Path,
        at: usize,
        cache: &std::cell::RefCell<PageCache>,
    ) -> io::Result<Arc<Vec<u8>>> {
        let seal = self
            .pages
            .get(at)
            .ok_or_else(|| invalid("missing index page descriptor"))?;
        if let Some(bytes) = cache.borrow_mut().get(&self.file, at, seal) {
            return Ok(bytes);
        }
        let file = self.open(root)?;
        let mut bytes = vec![0u8; PAGE];
        file.read_exact_at(
            &mut bytes,
            (at as u64)
                .checked_mul(PAGE as u64)
                .ok_or_else(|| invalid("index page offset overflow"))?,
        )?;
        let digest: Hash = Sha256::digest(&bytes).into();
        if digest != seal.digest || bytes[seal.used as usize..].iter().any(|byte| *byte != 0) {
            return Err(invalid("index page digest or padding mismatch"));
        }
        let bytes = Arc::new(bytes);
        cache
            .borrow_mut()
            .put(&self.file, at, seal, Arc::clone(&bytes));
        Ok(bytes)
    }

    fn length(&self) -> u64 {
        self.pages.last().map_or(0, |last| {
            (self.pages.len() as u64 - 1) * PAGE as u64 + last.used as u64
        })
    }

    fn range(
        &self,
        root: &Path,
        offset: u64,
        length: u64,
        cache: &std::cell::RefCell<PageCache>,
    ) -> io::Result<Vec<u8>> {
        let end = offset
            .checked_add(length)
            .ok_or_else(|| invalid("header range overflow"))?;
        if length == 0 || end > self.length() {
            return Err(invalid("header range outside index"));
        }
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(
                usize::try_from(length)
                    .map_err(|_| invalid("header too large for this machine"))?,
            )
            .map_err(|_| invalid("cannot allocate indexed header"))?;
        let mut pos = offset;
        while pos < end {
            let at =
                usize::try_from(pos / PAGE as u64).map_err(|_| invalid("header page overflow"))?;
            let page = self.read(root, at, cache)?;
            let from = (pos % PAGE as u64) as usize;
            let count = (end - pos).min((PAGE - from) as u64) as usize;
            bytes.extend_from_slice(&page[from..from + count]);
            pos += count as u64;
        }
        Ok(bytes)
    }
}

impl Directory {
    pub fn load(
        root: &Path,
        stream: &str,
        segment: &Segment,
        stamp: &index::IndexSeal,
        before: Hash,
    ) -> io::Result<Self> {
        if stamp.version != 2 {
            return Err(invalid("unsupported paged index version"));
        }
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(directory_path(root, segment.number))?;
        if !file.metadata()?.is_file() || file.metadata()?.len() != stamp.bytes {
            return Err(invalid("index directory length mismatch"));
        }
        let mut bytes = Vec::new();
        std::io::Read::take(
            file,
            stamp
                .bytes
                .checked_add(1)
                .ok_or_else(|| invalid("index directory size overflow"))?,
        )
        .read_to_end(&mut bytes)?;
        if bytes.len() as u64 != stamp.bytes
            || format!("{:x}", Sha256::digest(&bytes)) != stamp.digest
        {
            return Err(invalid("index directory digest mismatch"));
        }
        let directory: Self =
            serde_json::from_slice(&bytes).map_err(|error| invalid(error.to_string()))?;
        directory.validate(stream, segment, before)?;
        directory.descriptors.open(root)?;
        directory.headers.open(root)?;
        directory.ids.open(root)?;
        Ok(directory)
    }

    fn validate(&self, stream: &str, segment: &Segment, before: Hash) -> io::Result<()> {
        let seal = segment
            .seal
            .as_ref()
            .ok_or_else(|| invalid("unsealed directory source"))?;
        if self.version != 2
            || self.stream != stream
            || self.number != segment.number
            || self.first != segment.first
            || self.through != seal.through
            || self.body_bytes != seal.bytes
            || self.body_digest != seal.digest
            || self.first == 0
            || self.through < self.first
        {
            return Err(invalid("index directory source binding mismatch"));
        }
        self.descriptors.validate(true)?;
        self.headers.validate(true)?;
        self.ids.validate(false)?;
        let count = self.through - self.first + 1;
        if self.descriptors.length()
            != count
                .checked_mul(DESCRIPTOR as u64)
                .ok_or_else(|| invalid("descriptor count overflow"))?
            || self.prefixes.len() != self.descriptors.pages.len()
            || self.id_ranges.len() != self.ids.pages.len()
            || self.headers.pages.is_empty()
        {
            return Err(invalid("index directory coverage mismatch"));
        }
        let mut hash = before;
        for prefix in &self.prefixes {
            if prefix.before != hash {
                return Err(invalid("index prefix discontinuity"));
            }
            hash = prefix.after;
        }
        let mut entries = 0u64;
        let mut previous = None;
        for (at, ((low, high), page)) in self.id_ranges.iter().zip(&self.ids.pages).enumerate() {
            if low > high
                || previous.is_some_and(|previous| previous > *low)
                || !(page.used as usize).is_multiple_of(ID_ENTRY)
                || (at + 1 < self.ids.pages.len() && page.used as usize != IDS_PER_PAGE * ID_ENTRY)
            {
                return Err(invalid("invalid index identity range"));
            }
            previous = Some(*high);
            entries += page.used as u64 / ID_ENTRY as u64;
        }
        if entries != count {
            return Err(invalid("index identity count mismatch"));
        }
        Ok(())
    }

    pub fn after(&self) -> Hash {
        self.prefixes.last().unwrap().after
    }

    pub fn record(
        &self,
        root: &Path,
        seq: u64,
        cache: &std::cell::RefCell<PageCache>,
    ) -> io::Result<index::IndexRecord> {
        if seq < self.first || seq > self.through {
            return Err(invalid("sequence outside index directory"));
        }
        let pos = (seq - self.first) * DESCRIPTOR as u64;
        let page = self
            .descriptors
            .read(root, (pos / PAGE as u64) as usize, cache)?;
        let at = (pos % PAGE as u64) as usize;
        let descriptor = &page[at..at + DESCRIPTOR];
        let offset = u64_at(descriptor, 0);
        let length = u64_at(descriptor, 8);
        if length == 0
            || offset
                .checked_add(length)
                .and_then(|n| n.checked_add(1))
                .is_none_or(|end| end > self.body_bytes)
        {
            return Err(invalid("indexed body outside sealed source"));
        }
        let bytes =
            self.headers
                .range(root, u64_at(descriptor, 16), u64_at(descriptor, 24), cache)?;
        let header: Header =
            serde_json::from_slice(&bytes).map_err(|error| invalid(error.to_string()))?;
        if header.seq != seq || header.v != 1 || header.id.is_empty() {
            return Err(invalid("indexed header identity mismatch"));
        }
        Ok(index::IndexRecord {
            stream: self.stream.clone(),
            header,
            location: Location {
                segment: self.number,
                offset,
                bytes: usize::try_from(length)
                    .map_err(|_| invalid("body too large for this machine"))?,
                digest: descriptor[32..64].try_into().unwrap(),
            },
        })
    }

    pub fn lookup(
        &self,
        root: &Path,
        id: &str,
        cache: &std::cell::RefCell<PageCache>,
    ) -> io::Result<Option<index::IndexRecord>> {
        self.lookup_digest(root, id, Sha256::digest(id.as_bytes()).into(), cache)
    }

    fn lookup_digest(
        &self,
        root: &Path,
        id: &str,
        digest: Hash,
        cache: &std::cell::RefCell<PageCache>,
    ) -> io::Result<Option<index::IndexRecord>> {
        for (at, (low, high)) in self.id_ranges.iter().enumerate() {
            if digest < *low || digest > *high {
                continue;
            }
            let page = self.ids.read(root, at, cache)?;
            let (entries, _) = page[..self.ids.pages[at].used as usize].as_chunks::<ID_ENTRY>();
            // Identity pages are sorted by digest. A missing ID is common
            // during recovery; scanning the whole page for it makes every
            // material lookup proportional to the entire ledger.
            let first = entries.partition_point(|entry| {
                #[cfg(test)]
                {
                    cache.borrow_mut().identity_comparisons += 1;
                }
                entry[..32] < digest[..]
            });
            for entry in &entries[first..] {
                #[cfg(test)]
                {
                    cache.borrow_mut().identity_comparisons += 1;
                }
                if entry[..32] != digest {
                    break;
                }
                let record = self.record(root, u64_at(entry, 32), cache)?;
                if record.header.id == id {
                    return Ok(Some(record));
                }
            }
        }
        Ok(None)
    }

    pub fn prefix(
        &self,
        root: &Path,
        through: u64,
        cache: &std::cell::RefCell<PageCache>,
    ) -> io::Result<Hash> {
        if through < self.first || through > self.through {
            return Err(invalid("prefix outside index directory"));
        }
        let at = ((through - self.first) / (PAGE / DESCRIPTOR) as u64) as usize;
        let before = self.prefixes[at].before;
        let page = self.descriptors.read(root, at, cache)?;
        let mut hash = before;
        let mut answer = None;
        for (slot, entry) in page[..self.descriptors.pages[at].used as usize]
            .as_chunks::<DESCRIPTOR>()
            .0
            .iter()
            .enumerate()
        {
            hash = extend_prefix(hash, entry[32..64].try_into().unwrap());
            if self.first + (at * (PAGE / DESCRIPTOR) + slot) as u64 == through {
                answer = Some(hash);
            }
        }
        if hash != self.prefixes[at].after {
            return Err(invalid("index page prefix mismatch"));
        }
        answer.ok_or_else(|| invalid("missing prefix position"))
    }
}
