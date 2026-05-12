//! Memory locking operations to prevent pages from being swapped out.

use crate::errors::{MmapIoError, Result};
use crate::mmap::MemoryMappedFile;
use crate::utils::slice_range;

impl MemoryMappedFile {
    /// Lock memory pages to prevent them from being swapped to disk.
    ///
    /// This operation requires appropriate permissions (typically root/admin).
    /// Locked pages count against system limits.
    ///
    /// # Platform-specific behavior
    ///
    /// - **Unix**: Uses `mlock` system call
    /// - **Windows**: Uses `VirtualLock`
    ///
    /// # Errors
    ///
    /// Returns `MmapIoError::OutOfBounds` if the range exceeds file bounds.
    /// Returns `MmapIoError::LockFailed` if the lock operation fails (often due to permissions).
    #[cfg(feature = "locking")]
    pub fn lock(&self, offset: u64, len: u64) -> Result<()> {
        if len == 0 {
            return Ok(());
        }

        let total = self.current_len()?;
        let (start, end) = slice_range(offset, len, total)?;
        let length = end - start;

        // Get the base pointer for the mapping
        let ptr = match &self.inner.map {
            crate::mmap::MapVariant::Ro(m) => m.as_ptr(),
            crate::mmap::MapVariant::Rw(lock) => {
                let guard = lock.read();
                guard.as_ptr()
            }
            crate::mmap::MapVariant::Cow(m) => m.as_ptr(),
        };

        // SAFETY: `start` satisfies `start + length <= total` per
        // `slice_range` above, where `total` is the current mapped
        // length owned by `self.inner.map`. `ptr.add(start)` therefore
        // remains within the same allocated object (the OS mapping).
        // We never form a Rust reference to the memory at `addr`; only
        // the kernel reads it (via `mlock`/`VirtualLock`), which
        // operates on the address range itself.
        let addr = unsafe { ptr.add(start) };

        #[cfg(unix)]
        {
            // SAFETY: POSIX `mlock` requires:
            //   1. `[addr, addr + length)` lies within a mapped region
            //      of the process. Established by the
            //      `slice_range`/`ensure_in_bounds` check above.
            //   2. `length > 0` (we early-return on `len == 0` at the
            //      top of this method, and the bounds check guarantees
            //      `length == end - start > 0` reaches here).
            // The call locks the resident pages into RAM, preventing
            // them from being paged out. It does not access the memory
            // contents and does not retain `addr` after the call. On
            // failure (typically EPERM without CAP_IPC_LOCK, or ENOMEM
            // exceeding RLIMIT_MEMLOCK) it returns -1 and we surface
            // that as `LockFailed`. No UB is reachable from any failure
            // mode.
            // Reference: https://man7.org/linux/man-pages/man2/mlock.2.html
            let result = unsafe { libc::mlock(addr as *const libc::c_void, length) };

            if result != 0 {
                let err = std::io::Error::last_os_error();
                return Err(MmapIoError::LockFailed(format!(
                    "mlock failed: {err}. This operation typically requires elevated privileges."
                )));
            }
        }

        #[cfg(windows)]
        {
            extern "system" {
                fn VirtualLock(lpAddress: *const core::ffi::c_void, dwSize: usize) -> i32;
            }

            // SAFETY: `VirtualLock` (kernel32.dll) requires:
            //   1. `lpAddress` points within a committed region of the
            //      caller's address space, and
            //      `[lpAddress, lpAddress + dwSize)` does not cross a
            //      region boundary. The mmap-io mapping is a single
            //      committed region of length `total`, and our bounds
            //      check guarantees the range is inside it.
            //   2. `dwSize > 0` (guaranteed by the early-return on
            //      `len == 0` and the bounds-check arithmetic).
            // Like `mlock`, the function operates on the address range
            // without accessing the memory contents or retaining the
            // pointer. A return of 0 indicates failure; we read the
            // last OS error and return `LockFailed`.
            // Reference: https://learn.microsoft.com/en-us/windows/win32/api/memoryapi/nf-memoryapi-virtuallock
            let result = unsafe { VirtualLock(addr as *const core::ffi::c_void, length) };

            if result == 0 {
                let err = std::io::Error::last_os_error();
                return Err(MmapIoError::LockFailed(format!(
                    "VirtualLock failed: {err}. This operation may require elevated privileges."
                )));
            }
        }

        Ok(())
    }

    /// Unlock previously locked memory pages.
    ///
    /// This allows the pages to be swapped out again if needed.
    ///
    /// # Platform-specific behavior
    ///
    /// - **Unix**: Uses `munlock` system call
    /// - **Windows**: Uses `VirtualUnlock`
    ///
    /// # Errors
    ///
    /// Returns `MmapIoError::OutOfBounds` if the range exceeds file bounds.
    /// Returns `MmapIoError::UnlockFailed` if the unlock operation fails.
    #[cfg(feature = "locking")]
    pub fn unlock(&self, offset: u64, len: u64) -> Result<()> {
        if len == 0 {
            return Ok(());
        }

        let total = self.current_len()?;
        let (start, end) = slice_range(offset, len, total)?;
        let length = end - start;

        // Get the base pointer for the mapping
        let ptr = match &self.inner.map {
            crate::mmap::MapVariant::Ro(m) => m.as_ptr(),
            crate::mmap::MapVariant::Rw(lock) => {
                let guard = lock.read();
                guard.as_ptr()
            }
            crate::mmap::MapVariant::Cow(m) => m.as_ptr(),
        };

        // SAFETY: same justification as in `lock`: `start + length`
        // is within the mapping per the prior `slice_range` check, so
        // `ptr.add(start)` is in-bounds of the underlying allocated
        // object. The resulting pointer is only handed to a kernel
        // syscall below; no Rust reference is formed.
        let addr = unsafe { ptr.add(start) };

        #[cfg(unix)]
        {
            // SAFETY: POSIX `munlock` requires the same range
            // preconditions as `mlock` (range lies within a mapped
            // region, length > 0). Both are established above.
            // `munlock` does not access the memory contents; it removes
            // the lock that prevented paging. If the range was not
            // previously locked, the syscall is still well-defined and
            // simply succeeds (or returns ENOMEM on Linux, which we
            // surface as `UnlockFailed` rather than treating as UB).
            // Reference: https://man7.org/linux/man-pages/man2/mlock.2.html
            let result = unsafe { libc::munlock(addr as *const libc::c_void, length) };

            if result != 0 {
                let err = std::io::Error::last_os_error();
                return Err(MmapIoError::UnlockFailed(format!("munlock failed: {err}")));
            }
        }

        #[cfg(windows)]
        {
            extern "system" {
                fn VirtualUnlock(lpAddress: *const core::ffi::c_void, dwSize: usize) -> i32;
            }

            // SAFETY: `VirtualUnlock` (kernel32.dll) requires the same
            // range preconditions as `VirtualLock`. The function does
            // not read or write the memory; it operates on the locked-
            // pages bookkeeping for the address range. If the range
            // was not previously locked, the function returns 0 with
            // `GetLastError() == ERROR_NOT_LOCKED` (158), which we
            // detect below and treat as a soft success.
            // Reference: https://learn.microsoft.com/en-us/windows/win32/api/memoryapi/nf-memoryapi-virtualunlock
            let result = unsafe { VirtualUnlock(addr as *const core::ffi::c_void, length) };

            if result == 0 {
                let err = std::io::Error::last_os_error();
                // VirtualUnlock can fail if pages weren't locked, which is often not an error
                let err_code = err.raw_os_error().unwrap_or(0);
                if err_code != 158 {
                    // ERROR_NOT_LOCKED
                    return Err(MmapIoError::UnlockFailed(format!(
                        "VirtualUnlock failed: {err}"
                    )));
                }
            }
        }

        Ok(())
    }

    /// Lock all pages of the memory-mapped file.
    ///
    /// Convenience method that locks the entire file.
    ///
    /// # Errors
    ///
    /// Returns `MmapIoError::LockFailed` if the lock operation fails.
    #[cfg(feature = "locking")]
    pub fn lock_all(&self) -> Result<()> {
        let len = self.current_len()?;
        self.lock(0, len)
    }

    /// Unlock all pages of the memory-mapped file.
    ///
    /// Convenience method that unlocks the entire file.
    ///
    /// # Errors
    ///
    /// Returns `MmapIoError::UnlockFailed` if the unlock operation fails.
    #[cfg(feature = "locking")]
    pub fn unlock_all(&self) -> Result<()> {
        let len = self.current_len()?;
        self.unlock(0, len)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::create_mmap;
    use std::fs;
    use std::path::PathBuf;

    fn tmp_path(name: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!("mmap_io_lock_test_{}_{}", name, std::process::id()));
        p
    }

    #[test]
    #[cfg(feature = "locking")]
    fn test_lock_unlock_operations() {
        let path = tmp_path("lock_ops");
        let _ = fs::remove_file(&path);

        let mmap = create_mmap(&path, 8192).expect("create");

        // Note: These operations may fail without appropriate privileges
        // We test that they at least don't panic

        // Test locking a range
        let lock_result = mmap.lock(0, 4096);
        if lock_result.is_ok() {
            // If we successfully locked, we should be able to unlock
            mmap.unlock(0, 4096)
                .expect("unlock should succeed after lock");
        } else {
            // Expected on systems without privileges
            println!("Lock failed (expected without privileges): {lock_result:?}");
        }

        // Test empty range (should be no-op)
        mmap.lock(0, 0).expect("empty lock");
        mmap.unlock(0, 0).expect("empty unlock");

        // Test out of bounds
        assert!(mmap.lock(8192, 1).is_err());
        assert!(mmap.unlock(8192, 1).is_err());

        // Test lock_all/unlock_all
        let lock_all_result = mmap.lock_all();
        if lock_all_result.is_ok() {
            mmap.unlock_all()
                .expect("unlock_all should succeed after lock_all");
        }

        fs::remove_file(&path).expect("cleanup");
    }

    #[test]
    #[cfg(feature = "locking")]
    fn test_lock_with_different_modes() {
        let path = tmp_path("lock_modes");
        let _ = fs::remove_file(&path);

        // Create and test with RW mode
        let mmap = create_mmap(&path, 4096).expect("create");
        let _ = mmap.lock(0, 1024); // May fail without privileges
        drop(mmap);

        // Test with RO mode
        let mmap = MemoryMappedFile::open_ro(&path).expect("open ro");
        let _ = mmap.lock(0, 1024); // May fail without privileges

        #[cfg(feature = "cow")]
        {
            // Test with COW mode
            let mmap = MemoryMappedFile::open_cow(&path).expect("open cow");
            let _ = mmap.lock(0, 1024); // May fail without privileges
        }

        fs::remove_file(&path).expect("cleanup");
    }

    #[test]
    #[cfg(all(feature = "locking", unix))]
    fn test_multiple_lock_regions() {
        let path = tmp_path("multi_lock");
        let _ = fs::remove_file(&path);

        let mmap = create_mmap(&path, 16384).expect("create");

        // Try to lock multiple non-overlapping regions
        // These may fail without privileges, but shouldn't panic
        let _ = mmap.lock(0, 4096);
        let _ = mmap.lock(4096, 4096);
        let _ = mmap.lock(8192, 4096);

        // Unlock in different order
        let _ = mmap.unlock(4096, 4096);
        let _ = mmap.unlock(0, 4096);
        let _ = mmap.unlock(8192, 4096);

        fs::remove_file(&path).expect("cleanup");
    }
}
