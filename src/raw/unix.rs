//! Unix backend for [`crate::raw`]: `mmap(2)`, `msync(2)`, `munmap(2)`,
//! `mprotect(2)`, and the optional `madvise(2)` / `mlock(2)` calls.
//!
//! References:
//! - POSIX `mmap`: <https://pubs.opengroup.org/onlinepubs/9799919799/functions/mmap.html>
//! - POSIX `msync`: <https://pubs.opengroup.org/onlinepubs/9799919799/functions/msync.html>
//! - POSIX `munmap`: <https://pubs.opengroup.org/onlinepubs/9799919799/functions/munmap.html>
//! - POSIX `mprotect`: <https://pubs.opengroup.org/onlinepubs/9799919799/functions/mprotect.html>
//! - POSIX `mlock`: <https://pubs.opengroup.org/onlinepubs/9799919799/functions/mlock.html>
//! - Linux: <https://man7.org/linux/man-pages/man2/mmap.2.html>,
//!   <https://man7.org/linux/man-pages/man2/madvise.2.html>,
//!   <https://man7.org/linux/man-pages/man2/sync_file_range.2.html>
//! - macOS / FreeBSD: `man 2 mmap`, `man 2 msync`, `man 2 madvise`.

use std::fs::File;
use std::io;
use std::os::unix::io::AsRawFd;
use std::ptr::{self, NonNull};
use std::sync::atomic::{AtomicUsize, Ordering};

use super::range::Layout;
use super::{Access, FlushMode, MapFlags, Protection};

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

impl Backing {
    /// Called after a read-only `MAP_SHARED` file mapping was made
    /// writable with `mprotect`: from now on flush must call `msync`.
    pub(crate) fn mark_shared_writable(&mut self) -> io::Result<()> {
        *self = Backing::Shared;
        Ok(())
    }
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
    extra: MapFlags,
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
    let flags = flags | populate_flag(extra);
    // SAFETY: `mmap` with a null address hint and without MAP_FIXED
    // never replaces an existing mapping, so it cannot invalidate any
    // memory Rust code holds a reference to. `map_len` is non-zero
    // (POSIX: a zero length fails with EINVAL) and at most
    // `isize::MAX` (checked by `range::layout`). `offset` is a multiple
    // of the page size (POSIX: otherwise EINVAL) because `layout`
    // rounded it down to `offset_granularity()`. The fd is borrowed
    // from a live `File` for the duration of the call; POSIX states the
    // mapping holds its own reference to the file, so closing the fd
    // later does not unmap it. MAP_POPULATE (Linux) only pre-faults the
    // pages. Errors are reported as MAP_FAILED and read from errno
    // immediately below.
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

/// Create a private anonymous read-write mapping of at least
/// `map_len` bytes.
///
/// Returns the base address, the backing, and the length of the OS
/// mapping: `map_len` rounded up to the huge page size when
/// `extra.huge` is set on Linux / Android (`munmap` of a `MAP_HUGETLB`
/// mapping needs a huge-page multiple), `map_len` otherwise.
///
/// # Safety
///
/// `map_len` must be non-zero and at most `isize::MAX`. The caller
/// owns the mapping and must release it with [`unmap`] exactly once,
/// passing the returned length.
pub(crate) unsafe fn map_anon(
    map_len: usize,
    extra: MapFlags,
) -> io::Result<(NonNull<u8>, Backing, usize)> {
    let (os_len, huge_flag) = huge_layout(map_len, extra)?;
    let flags = libc::MAP_PRIVATE | libc::MAP_ANON | populate_flag(extra) | huge_flag;
    // SAFETY: same reasoning as `map_file`: null hint, no MAP_FIXED,
    // non-zero length (`os_len >= map_len > 0`, at most isize::MAX as
    // checked by `huge_layout`). With MAP_ANON the fd must be -1 for
    // portability (required on macOS and the BSDs, ignored on Linux)
    // and the offset 0. The kernel zero-fills anonymous pages, so the
    // memory is initialised before Rust reads it. MAP_HUGETLB and
    // MAP_POPULATE only change how and when the pages are backed; a
    // kernel that cannot satisfy them fails the call with an errno,
    // which is reported below.
    let addr = unsafe {
        sys_mmap(
            ptr::null_mut(),
            os_len,
            libc::PROT_READ | libc::PROT_WRITE,
            flags,
            -1,
            0,
        )
    };
    if addr == libc::MAP_FAILED {
        return Err(io::Error::last_os_error());
    }
    finish(addr, os_len).map(|base| (base, Backing::Private, os_len))
}

/// `MAP_POPULATE` when requested on Linux / Android, otherwise no flag.
fn populate_flag(extra: MapFlags) -> libc::c_int {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        if extra.populate {
            return libc::MAP_POPULATE;
        }
    }
    let _ = extra;
    0
}

/// OS length and extra `mmap` flag for an anonymous mapping of
/// `map_len` bytes. With `extra.huge` on Linux / Android the length is
/// rounded up to the huge page size and `MAP_HUGETLB` is returned;
/// elsewhere the request is ignored.
fn huge_layout(map_len: usize, extra: MapFlags) -> io::Result<(usize, libc::c_int)> {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        if extra.huge {
            let huge = huge_page_size();
            let rounded = map_len
                .checked_add(huge - 1)
                .map(|n| n & !(huge - 1))
                .filter(|&n| n <= super::range::MAX_LEN)
                .ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidInput,
                        format!(
                            "anonymous mapping length {map_len} rounded up to the huge \
                             page size ({huge} bytes) exceeds the address space"
                        ),
                    )
                })?;
            return Ok((rounded, libc::MAP_HUGETLB));
        }
    }
    let _ = extra;
    Ok((map_len, 0))
}

/// Default huge page size from `/proc/meminfo` (`Hugepagesize:`),
/// falling back to 2 MiB when it cannot be read. Always a non-zero
/// power of two.
#[cfg(any(target_os = "linux", target_os = "android"))]
fn huge_page_size() -> usize {
    static HUGE: AtomicUsize = AtomicUsize::new(0);
    let cached = HUGE.load(Ordering::Relaxed);
    if cached != 0 {
        return cached;
    }
    let size = std::fs::read_to_string("/proc/meminfo")
        .ok()
        .and_then(|text| {
            text.lines()
                .find_map(|line| line.strip_prefix("Hugepagesize:"))
                .and_then(|rest| rest.split_whitespace().next())
                .and_then(|kb| kb.parse::<usize>().ok())
        })
        .and_then(|kb| kb.checked_mul(1024))
        .filter(|n| n.is_power_of_two())
        .unwrap_or(2 * 1024 * 1024);
    HUGE.store(size, Ordering::Relaxed);
    size
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

/// Change the protection of a whole OS mapping.
///
/// # Safety
///
/// `base` / `os_len` must describe exactly one live mapping created by
/// [`map_file`] or [`map_anon`]. The caller must own the mapping by
/// value with no Rust reference into it alive, because removing write
/// access while a `&mut [u8]` exists, or adding it while other code
/// relies on the bytes being read-only, would break those references.
pub(crate) unsafe fn protect(base: *mut u8, os_len: usize, prot: Protection) -> io::Result<()> {
    let flags = match prot {
        Protection::ReadOnly => libc::PROT_READ,
        Protection::ReadWrite | Protection::WriteCopy => libc::PROT_READ | libc::PROT_WRITE,
    };
    // SAFETY: `base` is the page-aligned start of a live mapping of
    // `os_len` bytes (caller contract), which is what POSIX `mprotect`
    // requires (EINVAL for an unaligned address, ENOMEM for a range
    // that is not mapped). Adding PROT_WRITE to a MAP_SHARED mapping of
    // a file opened read-only fails with EACCES instead of granting
    // access. No Rust reference into the mapping is alive (caller
    // contract), so the change cannot invalidate one.
    let rc = unsafe { libc::mprotect(base.cast::<libc::c_void>(), os_len, flags) };
    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// Apply `advice` to `count` bytes starting at `addr`.
///
/// # Safety
///
/// `addr` must be page aligned and `[addr, addr + count)` must lie
/// inside a live mapping created by this module, with `count > 0`. The
/// caller must not pass `DontNeed` for a private (copy-on-write or
/// anonymous) mapping, since that discards the private pages under any
/// live reference.
#[cfg(feature = "advise")]
pub(crate) unsafe fn advise(
    addr: *mut u8,
    count: usize,
    advice: crate::advise::MmapAdvice,
) -> io::Result<()> {
    use crate::advise::MmapAdvice;
    let flag = match advice {
        MmapAdvice::Normal => libc::MADV_NORMAL,
        MmapAdvice::Random => libc::MADV_RANDOM,
        MmapAdvice::Sequential => libc::MADV_SEQUENTIAL,
        MmapAdvice::WillNeed => libc::MADV_WILLNEED,
        MmapAdvice::DontNeed => libc::MADV_DONTNEED,
    };
    // SAFETY: the caller guarantees a page-aligned, non-empty range
    // inside a live mapping, which is `madvise`'s contract (EINVAL /
    // ENOMEM otherwise). These hints only change paging policy; the
    // one that can change contents (MADV_DONTNEED on private memory)
    // is excluded by the caller contract. On shared file mappings
    // MADV_DONTNEED only drops page table entries and the next access
    // reads the same bytes back from the page cache.
    let rc = unsafe { libc::madvise(addr.cast::<libc::c_void>(), count, flag) };
    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// Lock (`lock == true`) or unlock `count` bytes at `addr` in RAM.
///
/// # Safety
///
/// `addr` must be page aligned and `[addr, addr + count)` must lie
/// inside a live mapping created by this module, with `count > 0`.
#[cfg(feature = "locking")]
pub(crate) unsafe fn lock(addr: *mut u8, count: usize, lock: bool) -> io::Result<()> {
    let addr = addr.cast::<libc::c_void>().cast_const();
    // SAFETY: page-aligned, non-empty range inside a live mapping
    // (caller contract), as POSIX `mlock` / `munlock` require. Neither
    // call reads or writes the bytes; they only pin or unpin the pages.
    // Failure (EPERM without CAP_IPC_LOCK, ENOMEM over RLIMIT_MEMLOCK)
    // is reported through errno.
    let rc = unsafe {
        if lock {
            libc::mlock(addr, count)
        } else {
            libc::munlock(addr, count)
        }
    };
    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// Release a mapping. Errors are ignored: this runs from `Drop`, and
/// `munmap` can only fail on arguments this module never produces.
///
/// # Safety
///
/// `base`/`map_len` must describe exactly one live mapping created by
/// [`map_file`] or [`map_anon`] (for `map_anon`, the length it
/// returned), and no reference into it may outlive this call.
pub(crate) unsafe fn unmap(base: *mut u8, map_len: usize, _backing: &Backing) {
    // SAFETY: forwarded from the caller's contract above. `base` is the
    // page-aligned address `mmap` returned and `map_len` the length
    // passed to it (a huge-page multiple for MAP_HUGETLB mappings, as
    // `munmap` requires there), so the whole mapping is removed.
    unsafe { libc::munmap(base.cast::<libc::c_void>(), map_len) };
}
