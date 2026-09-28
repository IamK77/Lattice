//! Archive bytes may contain secrets; checking only mode bits is not enough
//! on platforms whose extended access rules can grant independent permissions.

use std::fs::File;
use std::io;
use std::os::unix::fs::MetadataExt;

use super::fs::invalid;

pub(super) fn private(file: &File) -> io::Result<()> {
    let meta = file.metadata()?;
    // SAFETY: geteuid has no arguments or memory preconditions.
    let owner = unsafe { libc::geteuid() };
    if meta.uid() != owner || meta.mode() & 0o077 != 0 || meta.nlink() == 0 {
        return Err(invalid("file archive must be private to the current user"));
    }
    #[cfg(target_os = "macos")]
    no_extended_access(file)?;
    Ok(())
}

#[cfg(target_os = "macos")]
fn no_extended_access(file: &File) -> io::Result<()> {
    use std::os::fd::AsRawFd;

    // Darwin sys/acl.h. These functions are not exposed by the pinned libc.
    unsafe extern "C" {
        fn acl_get_fd_np(fd: libc::c_int, kind: libc::c_int) -> *mut libc::c_void;
        fn acl_free(object: *mut libc::c_void) -> libc::c_int;
    }
    const ACL_TYPE_EXTENDED: libc::c_int = 0x100;

    // SAFETY: the descriptor stays live; a non-null result is an owned ACL
    // allocation, released exactly once below using its designated allocator.
    let acl = unsafe { acl_get_fd_np(file.as_raw_fd(), ACL_TYPE_EXTENDED) };
    if !acl.is_null() {
        // SAFETY: this is the allocation returned by acl_get_fd_np above.
        unsafe { acl_free(acl) };
        // Do not guess whether an explicit or inherited ACL is harmless, and
        // do not rewrite permissions on a caller-provided archive directory.
        return Err(invalid(
            "file archive extended access rules are unsupported",
        ));
    }
    let error = io::Error::last_os_error();
    // Darwin reports no extended ACL as ENOENT even for a live, linked file
    // descriptor. The ordinary-file and inherited-ACL tests retain this case.
    if error.raw_os_error() == Some(libc::ENOENT) {
        Ok(())
    } else {
        Err(error)
    }
}
