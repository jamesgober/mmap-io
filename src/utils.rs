//! Utility helpers for alignment, page size, and safe range calculations.

use crate::errors::{MmapIoError, Result};
use std::sync::OnceLock;

/// Cached page size. Initialized on first call to [`page_size`].
///
/// The page size cannot change at runtime, so caching after the first
/// syscall removes a per-call cost from every hot path that asks for
/// it (microflush optimization, page-aligned operations, touch_pages).
static PAGE_SIZE: OnceLock<usize> = OnceLock::new();

/// Get the system page size in bytes.
///
/// The value is computed once via a platform syscall (`sysconf` on
/// Unix, `GetSystemInfo` on Windows) and cached for the lifetime of
/// the process.
#[must_use]
pub fn page_size() -> usize {
    *PAGE_SIZE.get_or_init(query_page_size)
}

/// Query the platform for the current page size. Called at most once
/// per process via [`PAGE_SIZE`].
fn query_page_size() -> usize {
    cfg_if::cfg_if! {
        if #[cfg(target_os = "windows")] {
            windows_page_size()
        } else {
            unix_page_size()
        }
    }
}

#[cfg(target_os = "windows")]
fn windows_page_size() -> usize {
    use std::mem::MaybeUninit;
    #[allow(non_snake_case)]
    #[repr(C)]
    struct SYSTEM_INFO {
        wProcessorArchitecture: u16,
        wReserved: u16,
        dwPageSize: u32,
        lpMinimumApplicationAddress: *mut core::ffi::c_void,
        lpMaximumApplicationAddress: *mut core::ffi::c_void,
        dwActiveProcessorMask: usize,
        dwNumberOfProcessors: u32,
        dwProcessorType: u32,
        dwAllocationGranularity: u32,
        wProcessorLevel: u16,
        wProcessorRevision: u16,
    }
    extern "system" {
        fn GetSystemInfo(lpSystemInfo: *mut SYSTEM_INFO);
    }
    let mut sysinfo = MaybeUninit::<SYSTEM_INFO>::uninit();
    // SAFETY: `GetSystemInfo` (kernel32.dll, documented on MSDN) accepts
    // a pointer to caller-allocated SYSTEM_INFO storage. We pass a
    // pointer to our MaybeUninit slot, which has the correct size and
    // alignment for the struct. The function unconditionally populates
    // every field of the SYSTEM_INFO struct on return (no failure
    // mode), so `assume_init` is sound. The returned `dwPageSize` is a
    // u32 representing the system page size in bytes; casting to usize
    // is lossless on every supported Windows target (page sizes are
    // <= 64 KiB on every documented architecture).
    // Reference: https://learn.microsoft.com/en-us/windows/win32/api/sysinfoapi/nf-sysinfoapi-getsysteminfo
    unsafe {
        GetSystemInfo(sysinfo.as_mut_ptr());
        let s = sysinfo.assume_init();
        s.dwPageSize as usize
    }
}

#[cfg(not(target_os = "windows"))]
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn unix_page_size() -> usize {
    // SAFETY: `sysconf` (POSIX.1-2001) with `_SC_PAGESIZE` is a
    // documented query that takes no pointer arguments and has no
    // failure mode that requires inspection on supported platforms.
    // It returns a long that is always positive on the platforms we
    // support (Linux, macOS, FreeBSD, OpenBSD, illumos). On the rare
    // chance it returns -1 (POSIX permits this only for queries
    // sysconf doesn't recognize, which is impossible for _SC_PAGESIZE),
    // we clamp to 0 via `.max(0)` and the resulting `as usize` cast
    // produces 0. Callers can detect this degenerate value, but no
    // documented platform exhibits it.
    // Reference: https://man7.org/linux/man-pages/man3/sysconf.3.html
    unsafe {
        let page_size = libc::sysconf(libc::_SC_PAGESIZE);
        page_size.max(0) as usize
    }
}

/// Align a value up to the nearest multiple of `alignment`.
///
/// Returns the original value unchanged when `alignment == 0` (a
/// permissive convention rather than a panic; callers passing 0 are
/// presumed to mean "no alignment requested").
#[must_use]
pub fn align_up(value: u64, alignment: u64) -> u64 {
    if alignment == 0 {
        return value;
    }
    // Fast path for power-of-2 alignments (common case for page sizes)
    if alignment.is_power_of_two() {
        let mask = alignment - 1;
        (value + mask) & !mask
    } else {
        value.div_ceil(alignment) * alignment
    }
}

/// Ensure the requested [offset, offset+len) range is within [0, total).
/// Returns `Ok(())` if valid; otherwise an `OutOfBounds` error.
///
/// # Errors
///
/// Returns `MmapIoError::OutOfBounds` if the range exceeds bounds.
pub fn ensure_in_bounds(offset: u64, len: u64, total: u64) -> Result<()> {
    if offset > total {
        return Err(MmapIoError::OutOfBounds { offset, len, total });
    }
    let end = offset.saturating_add(len);
    if end > total {
        return Err(MmapIoError::OutOfBounds { offset, len, total });
    }
    Ok(())
}

/// Compute a safe byte slice range for a given total length, returning start..end as usize tuple.
///
/// # Errors
///
/// Returns `MmapIoError::OutOfBounds` if the requested range exceeds the total length.
#[allow(clippy::cast_possible_truncation)]
pub fn slice_range(offset: u64, len: u64, total: u64) -> Result<(usize, usize)> {
    ensure_in_bounds(offset, len, total)?;
    // Safe to cast because we've already validated bounds against total
    // which itself must fit in memory (and thus usize)
    let start = offset as usize;
    let end = (offset + len) as usize;
    Ok((start, end))
}
