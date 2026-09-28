//! Directory-relative I/O for file archives and workspace restoration.
//!
//! A checked string path is not a directory capability: an ancestor can be
//! replaced with a symlink before the next open. Descendants are opened one
//! component at a time, relative to held directory descriptors, without
//! following links. This does not freeze other writers or make a sequence of
//! file replacements an atomic directory transaction.

use std::ffi::{CStr, CString};
use std::fs::{File, Metadata, OpenOptions};
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);
pub(super) const MAX_DEPTH: usize = 256;

pub(super) fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

pub(super) fn relative(path: &str) -> io::Result<()> {
    if path.split('/').count() > MAX_DEPTH {
        return Err(invalid("snapshot path exceeds the directory nesting limit"));
    }
    if path.is_empty()
        || path.contains('\0')
        || path
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
        || Path::new(path)
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
    {
        return Err(invalid(
            "snapshot path must be a nonempty normalized relative path",
        ));
    }
    Ok(())
}

fn name(value: &str) -> io::Result<CString> {
    relative(value)?;
    if value.contains('/') {
        return Err(invalid("directory entry must be a single path component"));
    }
    CString::new(value).map_err(|_| invalid("directory entry contains a null byte"))
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct Identity {
    pub device: u64,
    pub inode: u64,
}

impl Identity {
    pub fn of(meta: &Metadata) -> Self {
        Self {
            device: meta.dev(),
            inode: meta.ino(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct Stamp {
    pub identity: Identity,
    pub bytes: u64,
    pub modified: i64,
    pub modified_nanos: i64,
    pub changed: i64,
    pub changed_nanos: i64,
    pub mode: u32,
    pub links: u64,
}

impl Stamp {
    pub fn of(meta: &Metadata) -> Self {
        Self {
            identity: Identity::of(meta),
            bytes: meta.len(),
            modified: meta.mtime(),
            modified_nanos: meta.mtime_nsec(),
            changed: meta.ctime(),
            changed_nanos: meta.ctime_nsec(),
            mode: meta.mode(),
            links: meta.nlink(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Kind {
    File,
    Directory,
    Other,
}

pub(super) struct Directory {
    file: File,
}

impl Directory {
    pub fn open(path: &Path) -> io::Result<(PathBuf, Self)> {
        let canonical = path.canonicalize()?;
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&canonical)?;
        let directory = Self { file };
        directory.verify_path(&canonical)?;
        Ok((canonical, directory))
    }

    pub fn metadata(&self) -> io::Result<Metadata> {
        self.file.metadata()
    }

    pub fn verify_private(&self) -> io::Result<()> {
        super::permissions::private(&self.file)
    }

    pub fn verify_path(&self, path: &Path) -> io::Result<()> {
        let current = std::fs::symlink_metadata(path)?;
        if !current.is_dir() || Identity::of(&current) != Identity::of(&self.metadata()?) {
            return Err(invalid("snapshot directory was replaced or moved"));
        }
        Ok(())
    }

    pub fn sync(&self) -> io::Result<()> {
        self.file.sync_all()
    }

    pub fn clone_handle(&self) -> io::Result<Self> {
        Ok(Self {
            file: self.file.try_clone()?,
        })
    }

    fn open_entry(&self, entry: &str, flags: libc::c_int, mode: u32) -> io::Result<File> {
        let entry = name(entry)?;
        // SAFETY: the directory descriptor and nul-terminated entry stay live
        // through the call. A successful descriptor has exactly one owner.
        let fd = unsafe {
            libc::openat(
                self.file.as_raw_fd(),
                entry.as_ptr(),
                flags | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                mode as libc::c_uint,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: openat returned a new, owned descriptor.
        Ok(unsafe { File::from_raw_fd(fd) })
    }

    pub fn child(&self, entry: &str) -> io::Result<Self> {
        Ok(Self {
            file: self.open_entry(entry, libc::O_RDONLY | libc::O_DIRECTORY, 0)?,
        })
    }

    pub fn create_child(&self, entry: &str, mode: u32) -> io::Result<Self> {
        let leaf = name(entry)?;
        // SAFETY: both arguments remain valid for the duration of mkdirat.
        if unsafe { libc::mkdirat(self.file.as_raw_fd(), leaf.as_ptr(), mode as libc::mode_t) } != 0
        {
            return Err(io::Error::last_os_error());
        }
        self.sync()?;
        self.child(entry)
    }

    pub fn parent(&self, path: &str) -> io::Result<(Self, String)> {
        relative(path)?;
        let mut parts: Vec<_> = path.split('/').collect();
        let leaf = parts.pop().expect("validated nonempty relative path");
        let mut current = self.clone_handle()?;
        for part in parts {
            current = current.child(part)?;
        }
        Ok((current, leaf.to_owned()))
    }

    pub fn file(&self, entry: &str) -> io::Result<File> {
        // NONBLOCK prevents a swapped-in FIFO from blocking before fstat can
        // reject it. It has no effect on ordinary disk-file reads.
        let file = self.open_entry(entry, libc::O_RDONLY | libc::O_NONBLOCK, 0)?;
        if !file.metadata()?.is_file() {
            return Err(invalid("snapshot entry is not a regular file"));
        }
        Ok(file)
    }

    pub fn kind(&self, entry: &str) -> io::Result<Option<Kind>> {
        let entry = name(entry)?;
        let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
        // SAFETY: stat points to writable storage; assume_init is used only
        // after fstatat reports success. The final link is never followed.
        let result = unsafe {
            libc::fstatat(
                self.file.as_raw_fd(),
                entry.as_ptr(),
                stat.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        };
        if result != 0 {
            let error = io::Error::last_os_error();
            return if error.kind() == io::ErrorKind::NotFound {
                Ok(None)
            } else {
                Err(error)
            };
        }
        // SAFETY: fstatat initialized the structure on success.
        let mode = unsafe { stat.assume_init() }.st_mode;
        Ok(Some(match mode & libc::S_IFMT {
            libc::S_IFREG => Kind::File,
            libc::S_IFDIR => Kind::Directory,
            _ => Kind::Other,
        }))
    }

    pub fn entries(&self, limit: usize) -> io::Result<Vec<String>> {
        // A new open-file description avoids sharing a directory offset with
        // this handle or another enumeration. fdopendir owns the new fd.
        let dot = CString::new(".").expect("literal has no null byte");
        // SAFETY: arguments are valid; a successful fd is transferred below.
        let fd = unsafe {
            libc::openat(
                self.file.as_raw_fd(),
                dot.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: fd is an owned directory descriptor.
        let file = unsafe { File::from_raw_fd(fd) };
        // SAFETY: fdopendir takes ownership only on success.
        let stream = unsafe { libc::fdopendir(file.as_raw_fd()) };
        if stream.is_null() {
            return Err(io::Error::last_os_error());
        }
        let _owned_by_stream = file.into_raw_fd();
        struct Stream(*mut libc::DIR);
        impl Drop for Stream {
            fn drop(&mut self) {
                // SAFETY: this wrapper exclusively owns the live DIR pointer.
                unsafe { libc::closedir(self.0) };
            }
        }
        let stream = Stream(stream);
        let mut entries = Vec::new();
        loop {
            clear_errno();
            // SAFETY: stream remains live; the entry is copied before the next
            // readdir call can invalidate its storage.
            let entry = unsafe { libc::readdir(stream.0) };
            if entry.is_null() {
                let error = io::Error::last_os_error();
                if error.raw_os_error().unwrap_or(0) != 0 {
                    return Err(error);
                }
                break;
            }
            // SAFETY: successful readdir returns a nul-terminated d_name.
            let value = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) };
            if value.to_bytes() == b"." || value.to_bytes() == b".." {
                continue;
            }
            if entries.len() >= limit {
                return Err(invalid("directory exceeds the snapshot entry limit"));
            }
            entries.push(
                value
                    .to_str()
                    .map_err(|_| invalid("file snapshots require UTF-8 path names"))?
                    .to_owned(),
            );
        }
        entries.sort();
        Ok(entries)
    }

    pub fn remove_file(&self, entry: &str) -> io::Result<()> {
        let entry = name(entry)?;
        // SAFETY: unlinkat receives a live directory and one validated leaf.
        // Flags=0 never recursively removes a directory or follows a link.
        if unsafe { libc::unlinkat(self.file.as_raw_fd(), entry.as_ptr(), 0) } != 0 {
            return Err(io::Error::last_os_error());
        }
        self.sync()
    }

    pub fn temporary(&self) -> io::Result<Temporary> {
        let directory = self.clone_handle()?;
        for _ in 0..128 {
            let serial = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
            let name = format!(".snapshot-stage-{}-{serial}", std::process::id());
            match self.open_entry(&name, libc::O_RDWR | libc::O_CREAT | libc::O_EXCL, 0o600) {
                Ok(file) => {
                    return Ok(Temporary {
                        file,
                        directory,
                        name: Some(name),
                    })
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            }
        }
        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "could not allocate a unique snapshot staging file",
        ))
    }
}

pub(super) struct Temporary {
    pub file: File,
    directory: Directory,
    name: Option<String>,
}

impl Temporary {
    pub fn publish_new(mut self, destination: &Directory, entry: &str) -> io::Result<()> {
        self.file.sync_all()?;
        let source = name(self.name.as_deref().expect("unpublished staging file"))?;
        let target = name(entry)?;
        // SAFETY: both directories and names remain live. linkat with no flags
        // publishes a regular-file name without replacing an existing entry.
        if unsafe {
            libc::linkat(
                self.directory.file.as_raw_fd(),
                source.as_ptr(),
                destination.file.as_raw_fd(),
                target.as_ptr(),
                0,
            )
        } != 0
        {
            return Err(io::Error::last_os_error());
        }
        self.discard()?;
        destination.sync()
    }

    fn discard(&mut self) -> io::Result<()> {
        if let Some(entry) = self.name.as_deref() {
            self.directory.remove_file(entry)?;
            self.name = None;
        }
        Ok(())
    }
}

impl Drop for Temporary {
    fn drop(&mut self) {
        // Best-effort cleanup only. Published files are never removed here;
        // interrupted operations must be inspected, not silently replayed.
        let _ = self.discard();
    }
}

fn clear_errno() {
    #[cfg(target_os = "macos")]
    // SAFETY: __error returns this thread's errno location.
    unsafe {
        *libc::__error() = 0;
    }
    #[cfg(target_os = "linux")]
    // SAFETY: __errno_location returns this thread's errno location.
    unsafe {
        *libc::__errno_location() = 0;
    }
}
