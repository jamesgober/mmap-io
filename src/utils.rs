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
#[cfg(windows)]
fn query_page_size() -> usize {
    use std::mem::MaybeUninit;
    // Field names mirror the Win32 `SYSTEM_INFO` definition.
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

/// Query the platform for the current page size. Called at most once
/// per process via [`PAGE_SIZE`].
#[cfg(unix)]
fn query_page_size() -> usize {
    // SAFETY: `sysconf` (POSIX.1-2001) with `_SC_PAGESIZE` takes no
    // pointer arguments and only reads system configuration; the
    // `unsafe` is required solely because it is an `extern "C"`
    // function.
    // Reference: https://man7.org/linux/man-pages/man3/sysconf.3.html
    let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    // POSIX allows -1 only for names sysconf does not know, which
    // cannot happen for _SC_PAGESIZE. Fall back to 4 KiB rather than
    // returning 0, which callers would divide by.
    usize::try_from(page_size)
        .ok()
        .filter(|&p| p > 0)
        .unwrap_or(4096)
}

/// Fallback for targets that are neither Unix nor Windows.
#[cfg(not(any(unix, windows)))]
fn query_page_size() -> usize {
    4096
}

/// Align a value up to the nearest multiple of `alignment`.
///
/// Returns the original value unchanged when `alignment == 0` (a
/// permissive convention rather than a panic; callers passing 0 are
/// presumed to mean "no alignment requested").
///
/// Saturates: if the next multiple of `alignment` does not fit in a
/// `u64`, returns `u64::MAX` (which is then generally not a multiple
/// of `alignment`) instead of overflowing.
#[inline]
#[must_use]
pub fn align_up(value: u64, alignment: u64) -> u64 {
    if alignment == 0 {
        return value;
    }
    // Fast path for power-of-2 alignments (common case for page sizes)
    let aligned = if alignment.is_power_of_two() {
        let mask = alignment - 1;
        value.checked_add(mask).map(|v| v & !mask)
    } else {
        value.div_ceil(alignment).checked_mul(alignment)
    };
    aligned.unwrap_or(u64::MAX)
}

/// Ensure the requested [offset, offset+len) range is within [0, total).
/// Returns `Ok(())` if valid; otherwise an `OutOfBounds` error.
///
/// This function is called from every bounds-checked public method in
/// the crate (`as_slice`, `as_slice_mut`, `read_into`, `update_region`,
/// `flush_range`, `touch_pages_range`, `prefetch_range`, advise, lock,
/// segment access). It is marked `#[inline]` so the compiler can fuse
/// the check into the calling stack frame and avoid a function-call
/// boundary on the hot path.
///
/// # Errors
///
/// Returns `MmapIoError::OutOfBounds` if the range exceeds bounds.
#[inline]
pub fn ensure_in_bounds(offset: u64, len: u64, total: u64) -> Result<()> {
    // Use a single saturating-add comparison rather than two branches.
    // `offset > total` is implied by `offset + len > total` when
    // `len == 0` is paired with `offset > total`; in the common case
    // (len > 0) the saturating_add catches both overflow and OOB.
    let end = offset.saturating_add(len);
    if end > total || offset > total {
        return Err(MmapIoError::OutOfBounds { offset, len, total });
    }
    Ok(())
}

/// Compute a safe byte slice range for a given total length, returning start..end as usize tuple.
///
/// `#[inline]` because this is on every read/write hot path; the
/// function body is small (one bounds check + two conversions) and
/// inlining removes the call/return overhead.
///
/// # Errors
///
/// Returns `MmapIoError::OutOfBounds` if the requested range exceeds
/// the total length, or does not fit in `usize` (only possible when a
/// caller passes a `total` larger than the address space).
#[inline]
pub fn slice_range(offset: u64, len: u64, total: u64) -> Result<(usize, usize)> {
    ensure_in_bounds(offset, len, total)?;
    // `offset + len <= total` was checked without overflow above.
    match (usize::try_from(offset), usize::try_from(offset + len)) {
        (Ok(start), Ok(end)) => Ok((start, end)),
        _ => Err(MmapIoError::OutOfBounds { offset, len, total }),
    }
}
