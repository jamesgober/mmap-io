//! Disk space reservation for file-backed mappings (since 1.2.0).
//!
//! `File::set_len` makes a sparse file on most Unix file systems: no
//! disk block exists for a page until the page is first written. A
//! write through a mapping that faults on such a page while the file
//! system is full cannot return an error; the kernel kills the process
//! with `SIGBUS`. Reserving the blocks when the file is created or
//! grown turns that crash into an `ENOSPC` error from `create_rw` or
//! `resize`.
//!
//! - Linux and Android: `fallocate(fd, 0, offset, len)`.
//! - macOS and iOS: `fcntl(F_PREALLOCATE)`.
//! - Windows: nothing to do. NTFS allocates clusters for the whole
//!   length when `SetEndOfFile` extends a file that is not marked
//!   sparse, so a full disk already fails `set_len`.
//! - Other targets: nothing is reserved and the file stays sparse.
//!
//! A file system that cannot reserve space (for example one without
//! `fallocate` support) leaves the file sparse; [`reserve`] reports
//! that with `Ok(false)` instead of an error.

use std::fs::File;
use std::io;

/// Reserve disk blocks for `[offset, offset + len)` of `file`. Callers
/// extend the file with `set_len` first and then reserve the new range.
///
/// Returns `Ok(true)` if the space is reserved, `Ok(false)` if this file
/// system or platform cannot reserve space (the range stays sparse).
///
/// # Errors
///
/// The OS error, `ENOSPC` included, if the file system refuses the
/// reservation for any other reason.
#[cfg(any(target_os = "linux", target_os = "android"))]
pub(crate) fn reserve(file: &File, offset: u64, len: u64) -> io::Result<bool> {
    use std::os::unix::io::AsRawFd;

    // Same split as the mmap entry points in `raw::unix`: 32-bit glibc
    // and bionic only take a 64-bit offset through the `*64` symbol.
    #[cfg(not(any(target_os = "android", all(target_os = "linux", target_env = "gnu"))))]
    use libc::{fallocate as sys_fallocate, off_t as SysOff};
    #[cfg(any(target_os = "android", all(target_os = "linux", target_env = "gnu")))]
    use libc::{fallocate64 as sys_fallocate, off64_t as SysOff};

    if len == 0 {
        return Ok(true);
    }
    let (Ok(off), Ok(count)) = (SysOff::try_from(offset), SysOff::try_from(len)) else {
        // Not expressible on this target; the mapping layer rejects
        // such sizes anyway.
        return Ok(false);
    };
    let fd = file.as_raw_fd();
    loop {
        // SAFETY: `fallocate(2)` takes a file descriptor and plain
        // integers and touches no caller memory. `fd` is owned by
        // `file`, which is borrowed for the call. Mode 0 allocates the
        // range and extends the file size if the range ends past it; it
        // never changes existing data.
        // Reference: https://man7.org/linux/man-pages/man2/fallocate.2.html
        let ret = unsafe { sys_fallocate(fd, 0, off, count) };
        if ret == 0 {
            return Ok(true);
        }
        let err = io::Error::last_os_error();
        match err.raw_os_error() {
            Some(libc::EINTR) => continue,
            // The file system has no fallocate (EOPNOTSUPP), the kernel
            // has no syscall (ENOSYS), or the file type does not support
            // it (EINVAL is also returned by some FUSE file systems).
            Some(libc::EOPNOTSUPP | libc::ENOSYS | libc::EINVAL) => return Ok(false),
            _ => return Err(err),
        }
    }
}

/// See the Linux version. On Apple platforms `F_PREALLOCATE` reserves
/// `len` bytes past the physical end of the file (its last allocated
/// block), which a sparse `set_len` does not move, so `offset` is not
/// needed.
#[cfg(any(target_os = "macos", target_os = "ios"))]
pub(crate) fn reserve(file: &File, offset: u64, len: u64) -> io::Result<bool> {
    use std::os::unix::io::AsRawFd;

    let _ = offset;
    if len == 0 {
        return Ok(true);
    }
    let Ok(count) = libc::off_t::try_from(len) else {
        return Ok(false);
    };
    let fd = file.as_raw_fd();
    // Contiguous first (cheaper to map), then any blocks.
    for flags in [
        libc::F_ALLOCATECONTIG | libc::F_ALLOCATEALL,
        libc::F_ALLOCATEALL,
    ] {
        let mut store = libc::fstore_t {
            fst_flags: flags,
            fst_posmode: libc::F_PEOFPOSMODE,
            fst_offset: 0,
            fst_length: count,
            fst_bytesalloc: 0,
        };
        // SAFETY: `fcntl(F_PREALLOCATE)` reads and writes the `fstore_t`
        // it is given; `store` is a valid, writable `fstore_t` that lives
        // for the call. `fd` is owned by `file`, borrowed for the call.
        // The call only reserves blocks; it never changes file data or
        // the file size.
        // Reference: `man 2 fcntl` (macOS), F_PREALLOCATE.
        let ret = unsafe { libc::fcntl(fd, libc::F_PREALLOCATE, &mut store) };
        if ret != -1 {
            return Ok(true);
        }
        let err = io::Error::last_os_error();
        match err.raw_os_error() {
            Some(libc::ENOSPC | libc::EDQUOT | libc::EFBIG) => {
                if flags == libc::F_ALLOCATEALL {
                    return Err(err);
                }
                // No contiguous run; try again without that constraint.
            }
            Some(libc::ENOTSUP | libc::EINVAL) => return Ok(false),
            _ => return Err(err),
        }
    }
    Ok(false)
}

/// Windows allocates the whole length on `set_len` (see the module
/// docs); other targets cannot reserve space. Nothing to do.
#[cfg(not(any(
    target_os = "linux",
    target_os = "android",
    target_os = "macos",
    target_os = "ios"
)))]
pub(crate) fn reserve(file: &File, offset: u64, len: u64) -> io::Result<bool> {
    let _ = (file, offset, len);
    Ok(cfg!(windows))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg_attr(miri, ignore = "fallocate / fcntl are not supported by Miri")]
    #[test]
    fn reserving_space_keeps_data_and_size() {
        use std::io::{Read, Seek, SeekFrom, Write};
        let mut file = tempfile::tempfile().expect("tempfile");
        file.write_all(b"keep").expect("write");
        file.set_len(1 << 20).expect("set_len");
        let _reserved = reserve(&file, 4, (1 << 20) - 4).expect("reserve");
        assert_eq!(file.metadata().expect("metadata").len(), 1 << 20);
        file.seek(SeekFrom::Start(0)).expect("seek");
        let mut head = [0u8; 8];
        file.read_exact(&mut head).expect("read");
        assert_eq!(&head, b"keep\0\0\0\0");
        assert!(reserve(&file, 0, 0).expect("empty range"));
    }
}
