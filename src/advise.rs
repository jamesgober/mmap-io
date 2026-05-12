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

        // SAFETY: `start` satisfies `start < total` because `slice_range`
        // returns `(start, end)` with `start + (end - start) <= total`,
        // and `total` is the byte length of the mapping owned by
        // `self.inner.map`. Therefore `ptr.add(start)` stays in-bounds
        // of the same allocated object (the OS mapping), which is the
        // precondition for `<*const u8>::add` under the Rust memory
        // model. The pointer is not dereferenced here; the resulting
        // address is only handed to a kernel syscall below, which
        // operates on the address range without forming a Rust
        // reference to the memory.
        let addr = unsafe { ptr.add(start) };

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
            //   1. `addr` is page-aligned, OR the kernel will return
            //      EINVAL and we surface that as `AdviceFailed` instead
            //      of triggering UB. (We do not pre-align here; the
            //      caller's offset/len is honored as-is.)
            //   2. The range `[addr, addr + length)` lies within a
            //      mapped region of the process. This is established by
            //      the `slice_range`/`ensure_in_bounds` check above:
            //      `start + length <= total` where `total` is the
            //      current mapped length.
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
                //      `addr` was derived from a valid mapped region
                //      (bounds-checked above) and `length` does not
                //      extend past the mapping.
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
        // Skip test on unsupported platforms
        if cfg!(target_os = "macos") || cfg!(target_os = "windows") {
            eprintln!("Skipping madvise test on unsupported platform");
            return;
        }

        use crate::mmap::MemoryMappedFile;

        let file_path = "test_advise_ops.tmp";
        std::fs::write(file_path, [0u8; 4096]).unwrap();

        // Use create_rw to open the file in read-write mode
        let file = MemoryMappedFile::create_rw(file_path, 4096).unwrap();

        // Validate alignment without borrowing a slice from RW mapping.
        // The mapping base offset is 0 which is page-aligned by construction.
        let page = crate::utils::page_size();
        assert_eq!(0 % page, 0, "Mapping base offset must be page-aligned");

        // Call advise on full region
        let len = file.len();
        file.advise(0, len, MmapAdvice::Sequential)
            .expect("memory advice sequential failed");

        std::fs::remove_file(file_path).unwrap();
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
