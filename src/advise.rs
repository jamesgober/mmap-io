//! Memory advise operations for optimizing OS behavior.

use crate::errors::{MmapIoError, Result};
use crate::mmap::MemoryMappedFile;
use crate::utils::slice_range;

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
    /// - **Unix**: Uses `madvise` system call
    /// - **Windows**: Uses `PrefetchVirtualMemory` for `WillNeed`, no-op for others
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

        // SAFETY: We've validated the range is within bounds
        let addr = unsafe { ptr.add(start) };

        #[cfg(unix)]
        {
            use libc::{madvise, MADV_NORMAL, MADV_RANDOM, MADV_SEQUENTIAL, MADV_WILLNEED, MADV_DONTNEED};
            
            let advice_flag = match advice {
                MmapAdvice::Normal => MADV_NORMAL,
                MmapAdvice::Random => MADV_RANDOM,
                MmapAdvice::Sequential => MADV_SEQUENTIAL,
                MmapAdvice::WillNeed => MADV_WILLNEED,
                MmapAdvice::DontNeed => MADV_DONTNEED,
            };

            // SAFETY: madvise is safe to call with validated parameters
            let result = unsafe {
                madvise(
                    addr as *mut libc::c_void,
                    length,
                    advice_flag,
                )
            };

            if result != 0 {
                let err = std::io::Error::last_os_error();
                return Err(MmapIoError::AdviceFailed(format!(
                    "madvise failed: {err}"
                )));
            }
        }

        #[cfg(windows)]
        {
            // Windows only supports prefetching (WillNeed equivalent)
            if matches!(advice, MmapAdvice::WillNeed) {
                use std::mem;
                use std::ptr;

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

                // SAFETY: PrefetchVirtualMemory is safe with valid memory range
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

    fn tmp_path(name: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!("mmap_io_advise_test_{}_{}", name, std::process::id()));
        p
    }

    #[test]
    #[cfg(feature = "advise")]
    fn test_advise_operations() {
        let path = tmp_path("advise_ops");
        let _ = fs::remove_file(&path);

        let mmap = create_mmap(&path, 8192).expect("create");

        // Test various advice types
        mmap.advise(0, 4096, MmapAdvice::Sequential).expect("sequential advice");
        mmap.advise(4096, 4096, MmapAdvice::Random).expect("random advice");
        mmap.advise(0, 8192, MmapAdvice::Normal).expect("normal advice");
        mmap.advise(0, 1024, MmapAdvice::WillNeed).expect("will need advice");
        mmap.advise(7168, 1024, MmapAdvice::DontNeed).expect("dont need advice");

        // Test empty range (should be no-op)
        mmap.advise(0, 0, MmapAdvice::Normal).expect("empty range");

        // Test out of bounds
        assert!(mmap.advise(8192, 1, MmapAdvice::Normal).is_err());
        assert!(mmap.advise(0, 8193, MmapAdvice::Normal).is_err());

        fs::remove_file(&path).expect("cleanup");
    }

    #[test]
    #[cfg(feature = "advise")]
    fn test_advise_with_different_modes() {
        let path = tmp_path("advise_modes");
        let _ = fs::remove_file(&path);

        // Create and test with RW mode
        let mmap = create_mmap(&path, 4096).expect("create");
        mmap.advise(0, 4096, MmapAdvice::Sequential).expect("rw advise");
        drop(mmap);

        // Test with RO mode
        let mmap = MemoryMappedFile::open_ro(&path).expect("open ro");
        mmap.advise(0, 4096, MmapAdvice::Random).expect("ro advise");

        #[cfg(feature = "cow")]
        {
            // Test with COW mode
            let mmap = MemoryMappedFile::open_cow(&path).expect("open cow");
            mmap.advise(0, 4096, MmapAdvice::WillNeed).expect("cow advise");
        }

        fs::remove_file(&path).expect("cleanup");
    }
}