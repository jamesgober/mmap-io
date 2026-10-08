//! Memory locking operations to prevent pages from being swapped out.

use crate::errors::{MmapIoError, Result};
use crate::mmap::MemoryMappedFile;
use crate::utils::{page_size, slice_range};

impl MemoryMappedFile {
    /// Lock memory pages to prevent them from being swapped to disk.
    ///
    /// This operation requires appropriate permissions (typically root/admin).
    /// Locked pages count against system limits. The range is widened
    /// down to a page boundary (the kernel works in whole pages).
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
        self.lock_range(offset, len, true).map_err(|e| match e {
            LockError::Range(e) => e,
            LockError::Os(err) => MmapIoError::LockFailed(if cfg!(windows) {
                format!("VirtualLock failed: {err}. This operation may require elevated privileges.")
            } else {
                format!("mlock failed: {err}. This operation typically requires elevated privileges.")
            }),
        })
    }

    /// Unlock previously locked memory pages.
    ///
    /// This allows the pages to be swapped out again if needed.
    /// Unlocking pages that were not locked succeeds on every platform.
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
        self.lock_range(offset, len, false).map_err(|e| match e {
            LockError::Range(e) => e,
            LockError::Os(err) => MmapIoError::UnlockFailed(if cfg!(windows) {
                format!("VirtualUnlock failed: {err}")
            } else {
                format!("munlock failed: {err}")
            }),
        })
    }

    /// Validate the range under read access and lock or unlock it.
    fn lock_range(&self, offset: u64, len: u64, lock: bool) -> std::result::Result<(), LockError> {
        if len == 0 {
            return Ok(());
        }
        // Hold read access (a read guard for RW/COW mappings) until the
        // syscall returns, so `resize()` cannot unmap the range while
        // the kernel is working on it.
        let map = self.map_read();
        let (start, end) = slice_range(offset, len, map.len() as u64).map_err(LockError::Range)?;
        let aligned_start = start - start % page_size();
        // In bounds; `wrapping_add` avoids `unsafe` and forms no
        // reference to the bytes.
        let addr = map.base_ptr().wrapping_add(aligned_start).cast_mut();
        // SAFETY: `lock_span` requires a page-aligned, non-empty range
        // inside a live raw-layer mapping: the managed mapping base is
        // page aligned and `aligned_start` is a page multiple,
        // `end - aligned_start >= len > 0`, `end <= map.len()`, and `map`
        // keeps the mapping alive until this returns. `mlock` /
        // `VirtualLock` do not read or write the bytes.
        unsafe { crate::raw::lock_span(addr, end - aligned_start, lock) }.map_err(LockError::Os)
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
        // A concurrent shrink between reading the length and locking
        // shows up as `OutOfBounds` from `lock`, never as an
        // out-of-range syscall.
        self.lock(0, self.len())
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
        self.unlock(0, self.len())
    }
}

/// Why `lock_range` failed: a range error to pass through unchanged,
/// or an OS error to wrap in `LockFailed` / `UnlockFailed`.
enum LockError {
    Range(MmapIoError),
    Os(std::io::Error),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::create_mmap;
    use std::fs;
    use std::path::PathBuf;

    /// A path in a fresh private temp dir; the dir is removed when
    /// the returned `TempDir` drops.
    fn tmp_path(name: &str) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join(name);
        (dir, path)
    }

    #[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
    #[test]
    #[cfg(feature = "locking")]
    fn test_lock_unlock_operations() {
        let (_dir, path) = tmp_path("lock_ops");
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

    #[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
    #[test]
    #[cfg(feature = "locking")]
    fn test_lock_with_different_modes() {
        let (_dir, path) = tmp_path("lock_modes");
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

    #[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
    #[test]
    #[cfg(all(feature = "locking", unix))]
    fn test_multiple_lock_regions() {
        let (_dir, path) = tmp_path("multi_lock");
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
