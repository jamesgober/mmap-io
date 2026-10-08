//! Memory advise operations for optimizing OS behavior.

use crate::errors::{MmapIoError, Result};
use crate::mmap::{MapVariant, MemoryMappedFile};
use crate::utils::{page_size, slice_range};

/// Memory access pattern advice for the OS.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MmapAdvice {
    /// Normal access pattern (default).
    Normal,
    /// Random access pattern.
    Random,
    /// Sequential access pattern.
    Sequential,
    /// Will need this range soon.
    WillNeed,
    /// Won't need this range soon.
    DontNeed,
}

impl MemoryMappedFile {
    /// Advise the OS about expected access patterns for a memory range.
    ///
    /// This can help the OS optimize memory management, prefetching, and caching.
    /// The advice is a hint and may be ignored by the OS.
    ///
    /// # Platform-specific behavior
    ///
    /// - **Unix**: Uses `madvise` system call. `madvise` requires a
    ///   page-aligned start address, so the range is widened down to
    ///   the start of the page containing `offset`; the hint can
    ///   therefore cover up to one page minus one byte before
    ///   `offset`. The end is not widened past the mapping.
    /// - **Windows**: Uses `PrefetchVirtualMemory` for `WillNeed`, no-op for others
    ///
    /// # `DontNeed` on copy-on-write mappings
    ///
    /// On a `CopyOnWrite` mapping, `MADV_DONTNEED` throws the private
    /// copies of the pages away: on Linux the next access reads the
    /// file again, so private changes in the (page-widened) range are
    /// lost. Because that changes the mapped bytes, `DontNeed` on a
    /// copy-on-write mapping takes the write lock like a write method:
    /// it waits for every live view of the mapping, and calling it on a
    /// thread that holds one deadlocks. Every other hint, and
    /// `DontNeed` on read-only and read-write mappings (where dirty data
    /// stays in the page cache and the bytes do not change), only takes
    /// a read guard.
    ///
    /// A zero-length range is accepted at any offset and does nothing.
    ///
    /// # Errors
    ///
    /// Returns `MmapIoError::OutOfBounds` if the range exceeds file bounds.
    /// Returns `MmapIoError::AdviceFailed` if the system call fails.
    ///
    /// # Example
    ///
    /// ```
    /// use mmap_io::{MemoryMappedFile, MmapAdvice};
    ///
    /// let dir = tempfile::tempdir()?;
    /// let mmap = MemoryMappedFile::create_rw(dir.path().join("a.bin"), 3 * 4096)?;
    /// mmap.advise(0, mmap.len(), MmapAdvice::Sequential)?;
    /// mmap.advise(5000, 100, MmapAdvice::WillNeed)?;
    /// assert!(mmap.advise(mmap.len(), 1, MmapAdvice::Normal).is_err());
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    #[cfg(feature = "advise")]
    pub fn advise(&self, offset: u64, len: u64, advice: MmapAdvice) -> Result<()> {
        if len == 0 {
            return Ok(());
        }
        if advice == MmapAdvice::DontNeed {
            if let MapVariant::Cow(lock) = &self.inner.map {
                // Discards private pages: exclude every view first.
                let guard = lock.write();
                return advise_mapped(guard.as_ptr(), guard.len(), offset, len, advice);
            }
        }
        // Hold read access (a read guard for RW/COW mappings) until the
        // syscall returns, so `resize()` cannot unmap the range while
        // the kernel is working on it.
        let map = self.map_read();
        advise_mapped(map.base_ptr(), map.len(), offset, len, advice)
    }
}

/// Validate `[offset, offset + len)` against a mapping of `total` bytes
/// at `base` and apply `advice`. The caller holds the guard that keeps
/// the mapping alive (and, for `DontNeed` on private memory, the write
/// guard) for the duration of the call.
fn advise_mapped(
    base: *const u8,
    total: usize,
    offset: u64,
    len: u64,
    advice: MmapAdvice,
) -> Result<()> {
    let (start, end) = slice_range(offset, len, total as u64)?;
    // Widen down to a page boundary: `madvise` rejects unaligned
    // addresses with EINVAL. The mapping base is page-aligned (every
    // managed mapping starts at file offset 0), so a page-aligned
    // offset gives a page-aligned address.
    let aligned_start = start - start % page_size();
    // In bounds (`aligned_start <= start < total`); `wrapping_add` keeps
    // this free of `unsafe`, and no reference to the bytes is formed.
    let addr = base.wrapping_add(aligned_start).cast_mut();
    // SAFETY: `advise_span` requires a page-aligned, non-empty range
    // inside a live mapping created by the raw layer: `aligned_start`
    // is a page multiple of a page-aligned base, `end - aligned_start
    // >= end - start = len > 0`, and `end <= total`. The caller's guard
    // keeps the mapping alive until this returns. `DontNeed` on private
    // memory only reaches here under the write guard (see `advise`),
    // so no reference into the range is alive.
    unsafe { crate::raw::advise_span(addr, end - aligned_start, advice) }.map_err(|e| {
        let call = if cfg!(windows) {
            "PrefetchVirtualMemory"
        } else {
            "madvise"
        };
        MmapIoError::AdviceFailed(format!("{call} failed: {e}"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::create_mmap;
    use std::fs;
    use std::path::PathBuf;

    fn tmp_path(name: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "mmap_io_advise_test_{}_{}",
            name,
            std::process::id()
        ));
        p
    }

    #[test]
    #[cfg(feature = "advise")]
    fn test_advise_operations() {
        let path = tmp_path("advise_ops");
        let _ = fs::remove_file(&path);
        let file = create_mmap(&path, 3 * 4096).expect("create");

        // Full region, then offsets that are not page-aligned (EINVAL
        // from madvise before the range was widened).
        let len = file.len();
        file.advise(0, len, MmapAdvice::Sequential)
            .expect("advise full range");
        file.advise(1, 10, MmapAdvice::WillNeed)
            .expect("advise unaligned offset");
        file.advise(4097, 4096, MmapAdvice::Random)
            .expect("advise range spanning a page boundary");
        file.advise(len - 1, 1, MmapAdvice::Normal)
            .expect("advise last byte");
        assert!(file.advise(len, 1, MmapAdvice::Normal).is_err());

        drop(file);
        fs::remove_file(&path).expect("cleanup");
    }

    #[test]
    #[cfg(feature = "advise")]
    fn test_advise_with_different_modes() {
        let path = tmp_path("advise_modes");
        let _ = fs::remove_file(&path);

        // Create and test with RW mode
        let mmap = create_mmap(&path, 4096).expect("create");
        mmap.advise(0, 4096, MmapAdvice::Sequential)
            .expect("rw advise");
        drop(mmap);

        // Test with RO mode
        let mmap = MemoryMappedFile::open_ro(&path).expect("open ro");
        mmap.advise(0, 4096, MmapAdvice::Random).expect("ro advise");

        #[cfg(feature = "cow")]
        {
            // Test with COW mode
            let mmap = MemoryMappedFile::open_cow(&path).expect("open cow");
            mmap.advise(0, 4096, MmapAdvice::WillNeed)
                .expect("cow advise");
        }

        fs::remove_file(&path).expect("cleanup");
    }
}
