//! Offline legacy import. Original records and attachment paths are retained;
//! publication never replaces an existing destination.
use std::ffi::CString;
use std::fs::{self, File, Metadata, OpenOptions};
use std::io::{self, BufRead, BufReader, Read};
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::segmented::Ledger;
use crate::EventEnvelope;

const RECEIPT: &str = "migration.json";

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Stamp {
    device: u64,
    inode: u64,
    bytes: u64,
    modified: i64,
    nanos: i64,
}

impl Stamp {
    fn of(meta: &Metadata) -> Self {
        Self {
            device: meta.dev(),
            inode: meta.ino(),
            bytes: meta.len(),
            modified: meta.mtime(),
            nanos: meta.mtime_nsec(),
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Migrated {
    pub version: u32,
    pub source: PathBuf,
    pub destination: PathBuf,
    pub stream: String,
    pub events: u64,
    pub source_sha256: String,
    pub documents: u64,
    original: Stamp,
}

fn input(path: &Path) -> io::Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    if !file.metadata()?.is_file() {
        return Err(io::Error::other("migration source is not a regular file"));
    }
    Ok(file)
}

/// Import beside a closed legacy ledger. lsof is a prerequisite so writers
/// from older runtimes, which did not take advisory locks, are also detected.
pub fn migrate(source: &Path) -> io::Result<Migrated> {
    migrate_checked(source, 64 * 1024 * 1024, ensure_offline)
}

fn ensure_offline(path: &Path) -> io::Result<()> {
    let output = Command::new("lsof")
        .args(["-nP", "-Fpa", "--"])
        .arg(path)
        .output()
        .map_err(|error| {
            io::Error::other(format!(
                "cannot check legacy writers; install lsof before migration: {error}"
            ))
        })?;
    if !output.stderr.is_empty() || (!output.status.success() && output.status.code() != Some(1)) {
        return Err(io::Error::other(format!(
            "cannot establish that the ledger is offline: {}",
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    let fields = std::str::from_utf8(&output.stdout).map_err(io::Error::other)?;
    if fields
        .lines()
        .any(|line| line.strip_prefix('a').is_some_and(|access| access != "r"))
    {
        return Err(io::Error::other(
            "ledger has an open writer; close its runtime before migration",
        ));
    }
    Ok(())
}

fn migrate_checked(
    source: &Path,
    limit: u64,
    offline: impl Fn(&Path) -> io::Result<()>,
) -> io::Result<Migrated> {
    if source
        .extension()
        .is_none_or(|extension| extension != "jsonl")
    {
        return Err(io::Error::other(
            "migration accepts an offline .jsonl ledger",
        ));
    }
    if !source.symlink_metadata()?.is_file() {
        return Err(io::Error::other(
            "migration source must not be a symbolic link",
        ));
    }
    let source = source.canonicalize()?;
    let destination = source.with_extension("ledger");
    if destination.try_exists()? {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "migration destination already exists",
        ));
    }
    let file = input(&source)?;
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err(io::Error::last_os_error());
    }
    offline(&source)?;
    let original = Stamp::of(&file.metadata()?);
    let parent = source
        .parent()
        .ok_or_else(|| io::Error::other("ledger has no parent directory"))?;
    let staging = tempfile::Builder::new()
        .prefix(".lattice-migration-")
        .tempdir_in(parent)?;
    let root = staging.path().join("ledger");
    let mut reader = BufReader::new(&file);
    let mut line = Vec::new();
    let mut hash = Sha256::new();
    let mut ledger: Option<Ledger> = None;
    let mut events = 0;
    let mut stream = String::new();
    loop {
        line.clear();
        if reader.read_until(b'\n', &mut line)? == 0 {
            break;
        }
        hash.update(&line);
        if line.last() == Some(&b'\n') {
            line.pop();
        }
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        let event: EventEnvelope = serde_json::from_slice(&line)?;
        if ledger.is_none() {
            stream = event.stream.clone();
            ledger = Some(Ledger::create(&root, &stream, limit)?);
            fs::set_permissions(&root, fs::Permissions::from_mode(0o700))?;
        }
        let ledger = ledger.as_mut().unwrap();
        ledger.validate_append(&event)?;
        ledger.append_validated_line(&event, &line)?;
        events += 1;
    }
    if events == 0 {
        return Err(io::Error::other("cannot migrate an empty ledger"));
    }
    drop(ledger);
    let mut watched = vec![(source.clone(), original.clone())];
    let old_documents = crate::document::documents_dir(&source);
    let documents_existed = match old_documents.symlink_metadata() {
        Ok(_) => true,
        Err(error) if error.kind() == io::ErrorKind::NotFound => false,
        Err(error) => return Err(error),
    };
    let documents = if documents_existed {
        copy_tree(
            &old_documents,
            &crate::document::documents_dir(&root),
            &mut watched,
        )?
    } else {
        0
    };
    Ledger::verify_path(&root)?;
    let report = Migrated {
        version: 1,
        source: source.clone(),
        destination: destination.clone(),
        stream,
        events,
        source_sha256: format!("{:x}", hash.finalize()),
        documents,
        original,
    };
    let mut receipt = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(root.join(RECEIPT))?;
    serde_json::to_writer(&mut receipt, &report)?;
    receipt.sync_all()?;
    File::open(&root)?.sync_all()?;
    offline(&source)?;
    if !documents_existed {
        match old_documents.symlink_metadata() {
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
            Ok(_) => {
                return Err(io::Error::other(
                    "attachment directory appeared during migration",
                ))
            }
        }
    }
    for (path, stamp) in watched {
        if Stamp::of(&path.symlink_metadata()?) != stamp {
            return Err(io::Error::other(format!(
                "source changed during migration: {}",
                path.display()
            )));
        }
    }
    rename_new(&root, &destination)?;
    File::open(parent)?.sync_all()?;
    Ok(report)
}

fn copy_tree(
    source: &Path,
    destination: &Path,
    watched: &mut Vec<(PathBuf, Stamp)>,
) -> io::Result<u64> {
    let meta = source.symlink_metadata()?;
    if !meta.is_dir() {
        return Err(io::Error::other(
            "attachment directory must not be a symbolic link",
        ));
    }
    watched.push((source.to_owned(), Stamp::of(&meta)));
    fs::create_dir(destination)?;
    let mut count = 0;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let from = entry.path();
        let to = destination.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            count += copy_tree(&from, &to, watched)?;
        } else {
            let mut original = input(&from)?;
            let stamp = Stamp::of(&original.metadata()?);
            let mut copy = OpenOptions::new().write(true).create_new(true).open(&to)?;
            let bytes = io::copy(&mut original.by_ref().take(stamp.bytes), &mut copy)?;
            if bytes != stamp.bytes {
                return Err(io::Error::other("attachment changed while copying"));
            }
            copy.set_permissions(original.metadata()?.permissions())?;
            copy.sync_all()?;
            watched.push((from, stamp));
            count += 1;
        }
    }
    fs::set_permissions(destination, meta.permissions())?;
    File::open(destination)?.sync_all()?;
    Ok(count)
}

fn rename_new(from: &Path, to: &Path) -> io::Result<()> {
    let from = CString::new(from.as_os_str().as_bytes()).map_err(io::Error::other)?;
    let to = CString::new(to.as_os_str().as_bytes()).map_err(io::Error::other)?;
    #[cfg(target_os = "macos")]
    let result = unsafe { libc::renamex_np(from.as_ptr(), to.as_ptr(), libc::RENAME_EXCL) };
    #[cfg(target_os = "linux")]
    let result = unsafe {
        libc::syscall(
            libc::SYS_renameat2,
            libc::AT_FDCWD,
            from.as_ptr(),
            libc::AT_FDCWD,
            to.as_ptr(),
            libc::RENAME_NOREPLACE,
        ) as i32
    };
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    let result = {
        let _ = (from, to);
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "exclusive directory publication is unavailable",
        ));
    };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn receipt(source: &Path) -> io::Result<Option<Migrated>> {
    let path = source.with_extension("ledger").join(RECEIPT);
    let file = match input(&path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    if file.metadata()?.len() > 64 * 1024 {
        return Err(io::Error::other("oversized migration receipt"));
    }
    let receipt: Migrated = serde_json::from_reader(file.take(64 * 1024))?;
    if receipt.version != 1 || receipt.source != source.canonicalize()? {
        return Err(io::Error::other(
            "migration receipt does not identify this original",
        ));
    }
    Ok(Some(receipt))
}

pub(crate) fn retained_original(source: &Path) -> bool {
    receipt(source).ok().flatten().is_some_and(|report| {
        source
            .symlink_metadata()
            .is_ok_and(|meta| Stamp::of(&meta) == report.original)
            && Ledger::stream_of(&source.with_extension("ledger"))
                .is_ok_and(|stream| stream == report.stream)
    })
}

pub(crate) fn refuse_original_write(source: &Path) -> io::Result<()> {
    if let Some(receipt) = receipt(source)? {
        return Err(io::Error::other(format!(
            "this ledger is a retained migration original; continue {} instead",
            receipt.destination.display()
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests;
