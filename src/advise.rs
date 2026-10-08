//! Memory advise operations for optimizing OS behavior.

use crate::errors::{MmapIoError, Result};
use crate::mmap::MemoryMappedFile;
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
    /// A zero-length range is accepted at any offset and does nothing.
    ///
    /// # Errors
    ///
    /// Returns `MmapIoError::OutOfBounds` if the range exceeds file bounds.
    /// Returns `MmapIoError::AdviceFailed` if the system call fails.
    #[cfg(feature = "advise")]
    pub fn advise(&self, offset: u64, len: u64, advice: MmapAdvice) -> Result<()> {
        if len == 0 {
            return Ok(());
        }

        // Hold read access (a read guard for RW mappings) until the
        // syscall below returns, so `resize()` cannot unmap the range
        // while the kernel is working on it.
        let map = self.map_read();
        let (start, end) = slice_range(offset, len, map.len() as u64)?;
        // Widen down to a page boundary: `madvise` rejects unaligned
        // addresses with EINVAL. The mapping base is page-aligned
        // (every mapping starts at file offset 0), so a page-aligned
        // offset gives a page-aligned address.
        let aligned_start = start - start % page_size();
        let region = &map[aligned_start..end];
        let addr = region.as_ptr();
        let length = region.len();

        #[cfg(unix)]
        {
            use libc::{
                madvise, MADV_DONTNEED, MADV_NORMAL, MADV_RANDOM, MADV_SEQUENTIAL, MADV_WILLNEED,
            };

            let advice_flag = match advice {
                MmapAdvice::Normal => MADV_NORMAL,
                MmapAdvice::Random => MADV_RANDOM,
                MmapAdvice::Sequential => MADV_SEQUENTIAL,
                MmapAdvice::WillNeed => MADV_WILLNEED,
                MmapAdvice::DontNeed => MADV_DONTNEED,
            };

            // SAFETY: POSIX `madvise` (and Linux's extension) requires:
            //   1. `addr` is page-aligned: `aligned_start` is a
            //      multiple of the page size and the mapping base is
            //      page-aligned by `mmap(2)`.
            //   2. The range `[addr, addr + length)` lies within a
            //      mapped region of the process: it is `region`, a
            //      subslice of the mapping that `map` keeps mapped
            //      until after this call returns.
            //   3. `advice_flag` is one of the documented constants.
            //      Each branch of the match above selects exactly one
            //      libc constant.
            // `madvise` does not access the memory at `addr` in the
            // sense of forming a reference to it; it advises the
            // kernel's VM subsystem about expected access patterns. For
            // MADV_DONTNEED specifically, the kernel may zero pages
            // backed by anonymous memory, but for our file-backed
            // mappings the next read will re-fault from the file, so
            // there is no soundness issue.
            // Reference: https://man7.org/linux/man-pages/man2/madvise.2.html
            let result = unsafe { madvise(addr as *mut libc::c_void, length, advice_flag) };

            if result != 0 {
                let err = std::io::Error::last_os_error();
                return Err(MmapIoError::AdviceFailed(format!("madvise failed: {err}")));
            }
        }

        #[cfg(windows)]
        {
            // Windows only supports prefetching (WillNeed equivalent)
            if matches!(advice, MmapAdvice::WillNeed) {
                // Field names mirror the Win32 definition.
                #[allow(non_snake_case)]
                #[repr(C)]
                struct WIN32_MEMORY_RANGE_ENTRY {
                    VirtualAddress: *mut core::ffi::c_void,
                    NumberOfBytes: usize,
                }

                extern "system" {
                    fn PrefetchVirtualMemory(
                        hProcess: *mut core::ffi::c_void,
                        NumberOfEntries: usize,
                        VirtualAddresses: *const WIN32_MEMORY_RANGE_ENTRY,
                        Flags: u32,
                    ) -> i32;

                    fn GetCurrentProcess() -> *mut core::ffi::c_void;
                }

                let entry = WIN32_MEMORY_RANGE_ENTRY {
                    VirtualAddress: addr as *mut core::ffi::c_void,
                    NumberOfBytes: length,
                };

                // SAFETY: `PrefetchVirtualMemory` (kernel32.dll,
                // documented on MSDN) requires:
                //   1. `hProcess` is a valid process handle with the
                //      PROCESS_QUERY_INFORMATION and PROCESS_VM_READ
                //      access rights. `GetCurrentProcess()` returns a
                //      pseudo-handle to the current process which
                //      always has full rights.
                //   2. `NumberOfEntries == 1` matches the size of the
                //      single-element `entry` array pointed to by
                //      `VirtualAddresses`.
                //   3. Each `WIN32_MEMORY_RANGE_ENTRY` describes a
                //      region within the caller's address space.
                //      `addr` and `length` describe `region`, a
                //      subslice of the mapping that `map` keeps mapped
                //      until after this call returns.
                //   4. `Flags` is reserved and must be 0.
                // The function does not retain pointers past the call
                // and does not mutate the described memory; it merely
                // hints the page cache to load the pages.
                // Reference: https://learn.microsoft.com/en-us/windows/win32/api/memoryapi/nf-memoryapi-prefetchvirtualmemory
                let result = unsafe {
                    PrefetchVirtualMemory(
                        GetCurrentProcess(),
                        1,
                        &entry,
                        0, // No special flags
                    )
                };

                if result == 0 {
                    let err = std::io::Error::last_os_error();
                    return Err(MmapIoError::AdviceFailed(format!(
                        "PrefetchVirtualMemory failed: {err}"
                    )));
                }
            }
            // Other advice types are no-ops on Windows
        }

        Ok(())
    }
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

    #[test]
    #[cfg(feature = "advise")]
    fn test_advise_operations() {
        let (_dir, path) = tmp_path("advise_ops");
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
        let (_dir, path) = tmp_path("advise_modes");
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
