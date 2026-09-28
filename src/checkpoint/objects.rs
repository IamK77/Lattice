//! Immutable, private, content-addressed bytes. These are backups, not caches:
//! corrupt or missing objects must never be reconstructed from today's files.

use std::io::{self, Read, Write};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;
use serde::Serialize;
use sha2::{Digest, Sha256};

use super::fs::{invalid, Directory, Kind};
use super::permissions::private;
use super::ObjectRef;

const FORMAT: &[u8] = b"lattice-file-archive-v1\n";
const RECORD_LIMIT: u64 = 16 * 1024 * 1024;

pub(super) struct Objects {
    pub path: PathBuf,
    directory: Directory,
}

pub(super) struct Location {
    pub path: PathBuf,
    parent: Directory,
    leaf: String,
}

pub(super) fn location(path: &Path) -> io::Result<Location> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let leaf = path
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| invalid("archive location requires a UTF-8 directory name"))?;
    super::fs::relative(leaf)?;
    let (parent_path, parent) = Directory::open(parent)?;
    Ok(Location {
        path: parent_path.join(leaf),
        parent,
        leaf: leaf.to_owned(),
    })
}

impl Objects {
    #[cfg(test)]
    pub fn open(path: &Path) -> io::Result<Self> {
        Self::open_location(location(path)?)
    }

    pub fn open_location(location: Location) -> io::Result<Self> {
        let Location { path, parent, leaf } = location;
        parent.verify_path(path.parent().expect("resolved archive parent"))?;
        let (directory, created) = match parent.create_child(&leaf, 0o700) {
            Ok(directory) => (directory, true),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                (parent.child(&leaf)?, false)
            }
            Err(error) => return Err(error),
        };
        directory.verify_private()?;
        if created {
            let mut temporary = directory.temporary()?;
            private(&temporary.file)?;
            temporary.file.write_all(FORMAT)?;
            temporary.publish_new(&directory, "format")?;
        } else {
            let mut file = directory.file("format")?;
            private(&file)?;
            let mut contents = Vec::new();
            Read::by_ref(&mut file)
                .take(FORMAT.len() as u64 + 1)
                .read_to_end(&mut contents)?;
            if contents != FORMAT {
                return Err(invalid("unsupported or damaged file archive format"));
            }
        }
        let objects = Self { path, directory };
        objects.validate_location()?;
        Ok(objects)
    }

    pub fn validate_location(&self) -> io::Result<()> {
        self.directory.verify_path(&self.path)?;
        self.directory.verify_private()
    }

    pub fn put(&self, input: &mut impl Read, limit: u64) -> io::Result<ObjectRef> {
        self.validate_location()?;
        let mut temporary = self.directory.temporary()?;
        private(&temporary.file)?;
        let reference = transfer(input, &mut temporary.file, limit)?;
        match temporary.publish_new(&self.directory, &reference.sha256) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                // An existing name is not proof that its bytes are trustworthy.
                // Never overwrite a corrupt object and silently "repair" history.
                self.verify(&reference)?;
            }
            Err(error) => return Err(error),
        }
        self.validate_location()?;
        Ok(reference)
    }

    pub fn copy(&self, reference: &ObjectRef, output: &mut impl Write) -> io::Result<()> {
        reference.validate()?;
        self.validate_location()?;
        if self.directory.kind(&reference.sha256)? != Some(Kind::File) {
            return Err(invalid(
                "file archive object is missing or is not a regular file",
            ));
        }
        let mut file = self.directory.file(&reference.sha256)?;
        let before = file.metadata()?;
        private(&file)?;
        if before.len() != reference.bytes || before.nlink() != 1 {
            return Err(invalid(
                "file archive object length or link count is invalid",
            ));
        }
        let actual = transfer(&mut file, output, reference.bytes)?;
        if actual != *reference
            || super::fs::Stamp::of(&before) != super::fs::Stamp::of(&file.metadata()?)
        {
            return Err(invalid(
                "file archive object failed its content or version check",
            ));
        }
        self.validate_location()
    }

    pub fn verify(&self, reference: &ObjectRef) -> io::Result<()> {
        self.copy(reference, &mut io::sink())
    }

    pub fn put_record(&self, record: &impl Serialize) -> io::Result<ObjectRef> {
        let mut bytes = RecordBuffer(Vec::new());
        serde_json::to_writer(&mut bytes, record).map_err(invalid_json)?;
        self.put(&mut bytes.0.as_slice(), RECORD_LIMIT)
    }

    pub fn record<T: DeserializeOwned>(&self, reference: &ObjectRef) -> io::Result<T> {
        if reference.bytes > RECORD_LIMIT {
            return Err(invalid("file archive record exceeds the size limit"));
        }
        let mut bytes = Vec::new();
        self.copy(reference, &mut bytes)?;
        serde_json::from_slice(&bytes).map_err(invalid_json)
    }
}

struct RecordBuffer(Vec<u8>);

impl Write for RecordBuffer {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() as u64 > RECORD_LIMIT.saturating_sub(self.0.len() as u64) {
            return Err(invalid("file archive record exceeds the size limit"));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn invalid_json(error: serde_json::Error) -> io::Error {
    invalid(format!("invalid file archive record: {error}"))
}

pub(super) fn transfer(
    input: &mut impl Read,
    output: &mut impl Write,
    limit: u64,
) -> io::Result<ObjectRef> {
    let mut buffer = [0u8; 64 * 1024];
    let mut digest = Sha256::new();
    let mut bytes = 0u64;
    loop {
        // Read at most one byte past the boundary, including limit=0. This
        // detects an over-limit stream without buffering or draining its tail.
        let room =
            (limit.saturating_sub(bytes).saturating_add(1)).min(buffer.len() as u64) as usize;
        let read = input.read(&mut buffer[..room])?;
        if read == 0 {
            break;
        }
        bytes = bytes
            .checked_add(read as u64)
            .ok_or_else(|| invalid("file archive byte count overflow"))?;
        if bytes > limit {
            return Err(invalid("file exceeds the snapshot byte limit"));
        }
        output.write_all(&buffer[..read])?;
        digest.update(&buffer[..read]);
    }
    Ok(ObjectRef {
        sha256: format!("{:x}", digest.finalize()),
        bytes,
    })
}
