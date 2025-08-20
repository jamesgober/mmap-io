//! Utility helpers for alignment, page size, and safe range calculations.

use crate::errors::{MmapIoError, Result};

/// Get the system page size in bytes.
#[must_use]
pub fn page_size() -> usize {
    // Use Rust standard library when available; otherwise fallback to libc via page_size crate pattern.
    // On modern Rust and platforms, std::io::Write doesn't expose page size; use `page_size` crate approach inline:
    // However, to keep pure dependencies, use cfg to call platform-specific APIs.
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
    unsafe {
        GetSystemInfo(sysinfo.as_mut_ptr());
        let s = sysinfo.assume_init();
        s.dwPageSize as usize
    }
}

#[cfg(not(target_os = "windows"))]
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn unix_page_size() -> usize {
    // SAFETY: sysconf with _SC_PAGESIZE is safe to call.
    unsafe {
        let page_size = libc::sysconf(libc::_SC_PAGESIZE);
        // Page size should always be positive and fit in usize
        // Cast is safe because page sizes are always reasonable values
        page_size.max(0) as usize
    }
}

/// Align a value up to the nearest multiple of `alignment`.
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
