//! Lazy, paged content lookup for document files. The index is disposable;
//! original documents are never overwritten to repair it.
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::derived_pages::{Item, Pages, Slot};

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    digest: String,
    name: String,
}

impl Item for Entry {
    fn lookup_key(&self) -> Option<&str> {
        Some(&self.digest)
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Directory {
    version: u32,
    pages: Vec<Slot>,
    count: usize,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Stored {
    directory: Directory,
    checksum: [u8; 32],
}

struct Index {
    root: PathBuf,
    pages: Pages<Entry>,
}

fn regular(path: &Path) -> io::Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    if !file.metadata()?.is_file() {
        return Err(io::Error::other("document or lookup is not a regular file"));
    }
    Ok(file)
}

fn digest_file(path: &Path) -> io::Result<String> {
    let mut file = regular(path)?;
    let expected = file.metadata()?.len();
    let mut remaining = expected;
    let mut hash = Sha256::new();
    let mut buffer = [0; 64 * 1024];
    while remaining > 0 {
        let take = remaining.min(buffer.len() as u64) as usize;
        let count = file.read(&mut buffer[..take])?;
        if count == 0 {
            return Err(io::Error::other("document changed while hashing"));
        }
        hash.update(&buffer[..count]);
        remaining -= count as u64;
    }
    if file.metadata()?.len() != expected {
        return Err(io::Error::other("document changed while hashing"));
    }
    Ok(format!("{:x}", hash.finalize()))
}

fn basename(name: &str) -> bool {
    Path::new(name).file_name().is_some_and(|file| file == name)
        && Path::new(name).components().count() == 1
}

impl Index {
    fn load(dir: &Path) -> io::Result<Self> {
        let root = dir.join(".document-index");
        if !root.symlink_metadata()?.is_dir() {
            return Err(io::Error::other("document lookup root is not a directory"));
        }
        let file = regular(&root.join("directory.json"))?;
        if file.metadata()?.len() > 64 * 1024 * 1024 {
            return Err(io::Error::other(
                "document lookup directory exceeds its read budget",
            ));
        }
        let stored: Stored = serde_json::from_reader(file.take(64 * 1024 * 1024))?;
        if stored.directory.version != 1
            || stored.checksum
                != <[u8; 32]>::from(Sha256::digest(serde_json::to_vec(&stored.directory)?))
        {
            return Err(io::Error::other("invalid document lookup directory"));
        }
        let pages = Pages::open(
            Some(root.clone()),
            stored.directory.pages,
            stored.directory.count,
        )?;
        Ok(Self { root, pages })
    }

    fn rebuild(dir: &Path) -> io::Result<Self> {
        let root = dir.join(".document-index");
        fs::create_dir_all(dir)?;
        match fs::create_dir(&root) {
            Ok(()) => {}
            Err(error)
                if error.kind() == io::ErrorKind::AlreadyExists
                    && root.symlink_metadata()?.is_dir() => {}
            Err(error) => return Err(error),
        }
        let mut index = Self {
            pages: Pages::open(Some(root.clone()), Vec::new(), 0)?,
            root,
        };
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let Some(name) = entry.file_name().to_str().map(str::to_string) else {
                continue;
            };
            // Directories, binary/text logs and unreadable files follow the
            // old directory lookup's best-effort rule, without whole-file RAM.
            if !entry.file_type()?.is_file() {
                continue;
            }
            if let Ok(digest) = digest_file(&entry.path()) {
                index.pages.push(Entry { digest, name })?;
            }
        }
        index.save()?;
        Ok(index)
    }

    fn save(&self) -> io::Result<()> {
        let directory = Directory {
            version: 1,
            pages: self.pages.directory()?,
            count: self.pages.len(),
        };
        let checksum = Sha256::digest(serde_json::to_vec(&directory)?).into();
        let mut temporary = tempfile::NamedTempFile::new_in(&self.root)?;
        serde_json::to_writer(
            &mut temporary,
            &Stored {
                directory,
                checksum,
            },
        )?;
        temporary.as_file().sync_all()?;
        temporary
            .persist(self.root.join("directory.json"))
            .map_err(|error| error.error)?;
        File::open(&self.root)?.sync_all()
    }

    fn find(&self, digest: &str) -> io::Result<Option<String>> {
        let Some(index) = self.pages.find_last(digest)? else {
            return Ok(None);
        };
        let entry = self.pages.get(index)?;
        if !basename(&entry.name) {
            return Err(io::Error::other("invalid indexed document name"));
        }
        Ok(Some(entry.name))
    }
}

pub(crate) struct Documents {
    dir: PathBuf,
    index: Option<Index>,
}

impl Documents {
    /// Startup does not enumerate or hash attachments. A valid lookup directory
    /// is loaded on the first put; only its selected document is then checked.
    pub(crate) fn beside(ledger: &Path) -> Self {
        Self {
            dir: crate::document::documents_dir(ledger),
            index: None,
        }
    }

    fn lookup(&mut self, digest: &str) -> io::Result<Option<String>> {
        if self.index.is_none() {
            self.index = Some(Index::load(&self.dir).or_else(|error| {
                eprintln!("rebuilding document lookup: {error}");
                Index::rebuild(&self.dir)
            })?);
        }
        let found = self.index.as_ref().unwrap().find(digest);
        match found {
            Ok(Some(name))
                if digest_file(&self.dir.join(&name)).is_ok_and(|actual| actual == digest) =>
            {
                Ok(Some(name))
            }
            Ok(None) => Ok(None),
            _ => {
                eprintln!("rebuilding document lookup: selected entry is missing or changed");
                self.index = Some(Index::rebuild(&self.dir)?);
                self.index.as_ref().unwrap().find(digest)
            }
        }
    }

    pub(super) fn put(&mut self, name: &str, text: &str) -> io::Result<String> {
        if !basename(name) {
            return Err(io::Error::other("invalid document name"));
        }
        let digest = format!("{:x}", Sha256::digest(text.as_bytes()));
        if let Some(existing) = self.lookup(&digest)? {
            return Ok(existing);
        }
        fs::create_dir_all(&self.dir)?;
        let path = self.dir.join(name);
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(mut file) => {
                file.write_all(text.as_bytes())?;
                file.sync_all()?;
            }
            Err(error)
                if error.kind() == io::ErrorKind::AlreadyExists
                    && digest_file(&path)? == digest => {}
            Err(error) => return Err(error),
        }
        File::open(&self.dir)?.sync_all()?;
        let index = self.index.as_mut().unwrap();
        index.pages.push(Entry {
            digest,
            name: name.to_string(),
        })?;
        index.save()?;
        Ok(name.to_string())
    }
}

#[cfg(test)]
mod tests;
