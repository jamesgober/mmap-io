//! Unix backend for [`crate::raw`]: `mmap(2)`, `msync(2)`, `munmap(2)`.
//!
//! References:
//! - POSIX `mmap`: <https://pubs.opengroup.org/onlinepubs/9799919799/functions/mmap.html>
//! - POSIX `msync`: <https://pubs.opengroup.org/onlinepubs/9799919799/functions/msync.html>
//! - POSIX `munmap`: <https://pubs.opengroup.org/onlinepubs/9799919799/functions/munmap.html>
//! - Linux: <https://man7.org/linux/man-pages/man2/mmap.2.html>
//! - macOS / FreeBSD: `man 2 mmap`, `man 2 msync`.

use std::fs::File;
use std::io;
use std::os::unix::io::AsRawFd;
use std::ptr::{self, NonNull};
use std::sync::atomic::{AtomicUsize, Ordering};

use super::range::Layout;
use super::{Access, FlushMode};

// glibc and bionic (Android) expose a 64-bit file offset only through
// the `*64` entry points on 32-bit targets; on 64-bit targets the two
// are the same symbol. musl, macOS and the BSDs always use a 64-bit
// `off_t`. Without this split a 32-bit glibc build could not map
// offsets past 2 GiB. (Approach follows memmap2, MIT/Apache-2.0.)
#[cfg(any(target_os = "android", all(target_os = "linux", target_env = "gnu")))]
use libc::{mmap64 as sys_mmap, off64_t as SysOff};

#[cfg(not(any(target_os = "android", all(target_os = "linux", target_env = "gnu"))))]
use libc::{mmap as sys_mmap, off_t as SysOff};

/// How a mapping is backed, which decides what `flush` must do.
#[derive(Debug)]
pub(crate) enum Backing {
    /// Read-only, private copy-on-write, or anonymous: there is no
    /// shared file data to write back, so flush is a validated no-op.
    Private,
    /// `MAP_SHARED` with write access: flush calls `msync`.
    Shared,
}

/// Cached page size. Zero means "not yet queried".
static PAGE_SIZE: AtomicUsize = AtomicUsize::new(0);

/// System page size, validated to be a non-zero power of two.
pub(crate) fn page_size() -> io::Result<usize> {
    let cached = PAGE_SIZE.load(Ordering::Relaxed);
    if cached != 0 {
        return Ok(cached);
    }
    // SAFETY: `sysconf(_SC_PAGESIZE)` takes no pointers and only reads
    // a kernel-provided constant. POSIX allows `-1` for an unsupported
    // name, which the checked conversion below turns into an error.
    // Reference: https://pubs.opengroup.org/onlinepubs/9799919799/functions/sysconf.html
    let raw = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    let size = usize::try_from(raw)
        .ok()
        .filter(|s| s.is_power_of_two())
        .ok_or_else(|| {
            io::Error::other(format!(
                "sysconf(_SC_PAGESIZE) returned an invalid page size ({raw})"
            ))
        })?;
    // Racing initialisers store the same value, so Relaxed is enough.
    PAGE_SIZE.store(size, Ordering::Relaxed);
    Ok(size)
}

/// Offset granularity for `mmap`: the page size on every Unix.
#[inline]
pub(crate) fn offset_granularity() -> io::Result<usize> {
    page_size()
}

/// Map `layout` of `file` with the requested access.
///
/// Returns the start of the OS mapping (page aligned, `layout.map_len`
/// bytes long) and how it is backed.
///
/// # Safety
///
/// `layout.map_len` must be non-zero and `layout` must have come from
/// [`super::range::layout`] for a window that lies inside the file
/// (checked by [`super::range::resolve_len`]). The caller owns the
/// returned mapping and must release it with [`unmap`] exactly once.
pub(crate) unsafe fn map_file(
    file: &File,
    access: Access,
    layout: &Layout,
) -> io::Result<(NonNull<u8>, Backing)> {
    let offset = SysOff::try_from(layout.aligned_offset).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "mapping offset {} does not fit in this target's off_t",
                layout.aligned_offset
            ),
        )
    })?;
    let (prot, flags, backing) = match access {
        Access::Read => (libc::PROT_READ, libc::MAP_SHARED, Backing::Private),
        Access::Write => (
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_SHARED,
            Backing::Shared,
        ),
        Access::Copy => (
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_PRIVATE,
            Backing::Private,
        ),
    };
    // SAFETY: `mmap` with a null address hint and without MAP_FIXED
    // never replaces an existing mapping, so it cannot invalidate any
    // memory Rust code holds a reference to. `map_len` is non-zero
    // (POSIX: a zero length fails with EINVAL) and at most
    // `isize::MAX` (checked by `range::layout`). `offset` is a multiple
    // of the page size (POSIX: otherwise EINVAL) because `layout`
    // rounded it down to `offset_granularity()`. The fd is borrowed
    // from a live `File` for the duration of the call; POSIX states the
    // mapping holds its own reference to the file, so closing the fd
    // later does not unmap it. Errors are reported as MAP_FAILED and
    // read from errno immediately below.
    let addr = unsafe {
        sys_mmap(
            ptr::null_mut(),
            layout.map_len,
            prot,
            flags,
            file.as_raw_fd(),
            offset,
        )
    };
    if addr == libc::MAP_FAILED {
        return Err(io::Error::last_os_error());
    }
    finish(addr, layout.map_len).map(|base| (base, backing))
}

/// Create a private anonymous read-write mapping of `map_len` bytes.
///
/// # Safety
///
/// `map_len` must be non-zero and at most `isize::MAX`. The caller
/// owns the mapping and must release it with [`unmap`] exactly once.
pub(crate) unsafe fn map_anon(map_len: usize) -> io::Result<(NonNull<u8>, Backing)> {
    // SAFETY: same reasoning as `map_file`: null hint, no MAP_FIXED,
    // non-zero length. With MAP_ANON the fd must be -1 for portability
    // (required on macOS and the BSDs, ignored on Linux) and the
    // offset 0. The kernel zero-fills anonymous pages, so the memory is
    // initialised before Rust reads it.
    let addr = unsafe {
        sys_mmap(
            ptr::null_mut(),
            map_len,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_PRIVATE | libc::MAP_ANON,
            -1,
            0,
        )
    };
    if addr == libc::MAP_FAILED {
        return Err(io::Error::last_os_error());
    }
    finish(addr, map_len).map(|base| (base, Backing::Private))
}

/// Convert a successful `mmap` result into a `NonNull`, unmapping and
/// failing in the (theoretical) case of a null address.
fn finish(addr: *mut libc::c_void, map_len: usize) -> io::Result<NonNull<u8>> {
    match NonNull::new(addr.cast::<u8>()) {
        Some(base) => Ok(base),
        None => {
            // A null return is only possible if the kernel allows
            // mapping page zero (vm.mmap_min_addr = 0). Rust slices
            // cannot start at null, so release it and report failure.
            // SAFETY: `addr`/`map_len` describe the mapping `mmap` just
            // created and nothing else references it.
            unsafe { libc::munmap(addr, map_len) };
            Err(io::Error::other("mmap returned a null address"))
        }
    }
}

/// Write back `count` bytes starting at `addr`.
///
/// # Safety
///
/// `addr` must be page aligned and `[addr, addr + count)` must lie
/// inside a live mapping created by this module. Both are guaranteed
/// when `addr`/`count` come from [`super::range::flush_span`] applied
/// to that mapping.
pub(crate) unsafe fn flush(
    addr: *mut u8,
    count: usize,
    backing: &Backing,
    mode: FlushMode,
) -> io::Result<()> {
    match backing {
        Backing::Private => Ok(()),
        Backing::Shared => {
            let flags = match mode {
                FlushMode::Sync => libc::MS_SYNC,
                FlushMode::Async => libc::MS_ASYNC,
            };
            // SAFETY: the caller guarantees `addr` is page aligned
            // (POSIX: EINVAL otherwise) and the range is inside a live
            // MAP_SHARED mapping. `msync` only schedules or performs
            // write-back of the pages; it does not read or write the
            // memory from Rust's point of view. MS_SYNC blocks until
            // the write completes (Linux implements it with
            // vfs_fsync_range, so the data is durable on return);
            // MS_ASYNC only schedules it.
            let rc = unsafe { libc::msync(addr.cast::<libc::c_void>(), count, flags) };
            if rc == 0 {
                Ok(())
            } else {
                Err(io::Error::last_os_error())
            }
        }
    }
}

/// Release a mapping. Errors are ignored: this runs from `Drop`, and
/// `munmap` can only fail on arguments this module never produces.
///
/// # Safety
///
/// `base`/`map_len` must describe exactly one live mapping created by
/// [`map_file`] or [`map_anon`], and no reference into it may outlive
/// this call.
pub(crate) unsafe fn unmap(base: *mut u8, map_len: usize, _backing: &Backing) {
    // SAFETY: forwarded from the caller's contract above. `base` is the
    // page-aligned address `mmap` returned and `map_len` the length
    // passed to it, so the whole mapping is removed.
    unsafe { libc::munmap(base.cast::<libc::c_void>(), map_len) };
}
