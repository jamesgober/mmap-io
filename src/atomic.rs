//! Atomic memory views for lock-free concurrent access to specific data types.

use crate::errors::{MmapIoError, Result};
use crate::mmap::MemoryMappedFile;
use std::sync::atomic::{AtomicU32, AtomicU64};

impl MemoryMappedFile {
    /// Get an atomic view of a u64 value at the specified offset.
    ///
    /// The offset must be properly aligned for atomic operations (8-byte alignment for u64).
    /// This allows lock-free concurrent access to the value.
    ///
    /// # Safety
    ///
    /// The returned reference is valid for the lifetime of the memory mapping.
    /// The caller must ensure that the memory at this offset is not concurrently
    /// modified through non-atomic operations.
    ///
    /// # Errors
    ///
    /// Returns `MmapIoError::Misaligned` if the offset is not 8-byte aligned.
    /// Returns `MmapIoError::OutOfBounds` if the offset + 8 exceeds file bounds.
    #[cfg(feature = "atomic")]
    pub fn atomic_u64(&self, offset: u64) -> Result<&AtomicU64> {
        const ALIGN: u64 = std::mem::align_of::<AtomicU64>() as u64;
        const SIZE: u64 = std::mem::size_of::<AtomicU64>() as u64;

        // Check alignment
        if offset % ALIGN != 0 {
            return Err(MmapIoError::Misaligned {
                required: ALIGN,
                offset,
            });
        }

        // Check bounds
        let total = self.current_len()?;
        if offset + SIZE > total {
            return Err(MmapIoError::OutOfBounds {
                offset,
                len: SIZE,
                total,
            });
        }

        // Get the base pointer for the mapping
        let ptr = match &self.inner.map {
            crate::mmap::MapVariant::Ro(m) => m.as_ptr(),
            crate::mmap::MapVariant::Rw(lock) => {
                let guard = lock.read();
                guard.as_ptr()
            }
            crate::mmap::MapVariant::Cow(m) => m.as_ptr(),
        };

        // SAFETY: Multiple invariants are guaranteed:
        // 1. Alignment: We've verified offset % 8 == 0 (required for AtomicU64)
        // 2. Bounds: We've verified offset + 8 <= total file size
        // 3. Overflow: try_into() ensures offset fits in usize, preventing ptr arithmetic overflow
        // 4. Lifetime: The returned reference is bound to 'self', ensuring the mapping outlives it
        // 5. Validity: The memory is mapped and valid for the entire file size
        // 6. Atomicity: The hardware guarantees atomic operations on aligned 8-byte values
        let offset_usize = offset.try_into().map_err(|_| MmapIoError::OutOfBounds {
            offset,
            len: SIZE,
            total,
        })?;
        unsafe {
            // ptr.add() is safe because:
            // - offset_usize is guaranteed to be within bounds (checked above)
            // - The resulting pointer is within the mapped region
            let addr = ptr.add(offset_usize);
            let atomic_ptr = addr as *const AtomicU64;
            Ok(&*atomic_ptr)
        }
    }

    /// Get an atomic view of a u32 value at the specified offset.
    ///
    /// The offset must be properly aligned for atomic operations (4-byte alignment for u32).
    /// This allows lock-free concurrent access to the value.
    ///
    /// # Safety
    ///
    /// The returned reference is valid for the lifetime of the memory mapping.
    /// The caller must ensure that the memory at this offset is not concurrently
    /// modified through non-atomic operations.
    ///
    /// # Errors
    ///
    /// Returns `MmapIoError::Misaligned` if the offset is not 4-byte aligned.
    /// Returns `MmapIoError::OutOfBounds` if the offset + 4 exceeds file bounds.
    #[cfg(feature = "atomic")]
    pub fn atomic_u32(&self, offset: u64) -> Result<&AtomicU32> {
        const ALIGN: u64 = std::mem::align_of::<AtomicU32>() as u64;
        const SIZE: u64 = std::mem::size_of::<AtomicU32>() as u64;

        // Check alignment
        if offset % ALIGN != 0 {
            return Err(MmapIoError::Misaligned {
                required: ALIGN,
                offset,
            });
        }

        // Check bounds
        let total = self.current_len()?;
        if offset + SIZE > total {
            return Err(MmapIoError::OutOfBounds {
                offset,
                len: SIZE,
                total,
            });
        }

        // Get the base pointer for the mapping
        let ptr = match &self.inner.map {
            crate::mmap::MapVariant::Ro(m) => m.as_ptr(),
            crate::mmap::MapVariant::Rw(lock) => {
                let guard = lock.read();
                guard.as_ptr()
            }
            crate::mmap::MapVariant::Cow(m) => m.as_ptr(),
        };

        // SAFETY: Multiple invariants are guaranteed:
        // 1. Alignment: We've verified offset % 4 == 0 (required for AtomicU32)
        // 2. Bounds: We've verified offset + 4 <= total file size
        // 3. Overflow: try_into() ensures offset fits in usize, preventing ptr arithmetic overflow
        // 4. Lifetime: The returned reference is bound to 'self', ensuring the mapping outlives it
        // 5. Validity: The memory is mapped and valid for the entire file size
        // 6. Atomicity: The hardware guarantees atomic operations on aligned 4-byte values
        let offset_usize = offset.try_into().map_err(|_| MmapIoError::OutOfBounds {
            offset,
            len: SIZE,
            total,
        })?;
        unsafe {
            // ptr.add() is safe because:
            // - offset_usize is guaranteed to be within bounds (checked above)
            // - The resulting pointer is within the mapped region
            let addr = ptr.add(offset_usize);
            let atomic_ptr = addr as *const AtomicU32;
            Ok(&*atomic_ptr)
        }
    }

    /// Get multiple atomic u64 views starting at the specified offset.
    ///
    /// Returns a slice of atomic values. All values must be within bounds
    /// and the offset must be 8-byte aligned.
    ///
    /// # Errors
    ///
    /// Returns `MmapIoError::Misaligned` if the offset is not 8-byte aligned.
    /// Returns `MmapIoError::OutOfBounds` if the range exceeds file bounds.
    #[cfg(feature = "atomic")]
    pub fn atomic_u64_slice(&self, offset: u64, count: usize) -> Result<&[AtomicU64]> {
        const ALIGN: u64 = std::mem::align_of::<AtomicU64>() as u64;
        const SIZE: u64 = std::mem::size_of::<AtomicU64>() as u64;

        // Check alignment
        if offset % ALIGN != 0 {
            return Err(MmapIoError::Misaligned {
                required: ALIGN,
                offset,
            });
        }

        // Check bounds
        let total_size = SIZE * count as u64;
        let total = self.current_len()?;
        if offset + total_size > total {
            return Err(MmapIoError::OutOfBounds {
                offset,
                len: total_size,
                total,
            });
        }

        // Get the base pointer for the mapping
        let ptr = match &self.inner.map {
            crate::mmap::MapVariant::Ro(m) => m.as_ptr(),
            crate::mmap::MapVariant::Rw(lock) => {
                let guard = lock.read();
                guard.as_ptr()
            }
            crate::mmap::MapVariant::Cow(m) => m.as_ptr(),
        };

        // SAFETY: Multiple invariants are guaranteed:
        // 1. Alignment: We've verified offset % 8 == 0 (required for AtomicU64 array)
        // 2. Bounds: We've verified offset + (count * 8) <= total file size
        // 3. Overflow: try_into() ensures offset fits in usize, preventing ptr arithmetic overflow
        // 4. Lifetime: The returned slice is bound to 'self', ensuring the mapping outlives it
        // 5. Validity: The memory is mapped and valid for the entire requested range
        // 6. Atomicity: Each element in the slice maintains 8-byte alignment for atomic operations
        let offset_usize = offset.try_into().map_err(|_| MmapIoError::OutOfBounds {
            offset,
            len: total_size,
            total,
        })?;
        unsafe {
            // ptr.add() is safe because:
            // - offset_usize is guaranteed to be within bounds (checked above)
            // - The resulting pointer is within the mapped region
            let addr = ptr.add(offset_usize);
            let atomic_ptr = addr as *const AtomicU64;
            // from_raw_parts is safe because:
            // - atomic_ptr points to valid, aligned memory
            // - count elements fit within the mapped region (verified above)
            Ok(std::slice::from_raw_parts(atomic_ptr, count))
        }
    }

    /// Get multiple atomic u32 views starting at the specified offset.
    ///
    /// Returns a slice of atomic values. All values must be within bounds
    /// and the offset must be 4-byte aligned.
    ///
    /// # Errors
    ///
    /// Returns `MmapIoError::Misaligned` if the offset is not 4-byte aligned.
    /// Returns `MmapIoError::OutOfBounds` if the range exceeds file bounds.
    #[cfg(feature = "atomic")]
    pub fn atomic_u32_slice(&self, offset: u64, count: usize) -> Result<&[AtomicU32]> {
        const ALIGN: u64 = std::mem::align_of::<AtomicU32>() as u64;
        const SIZE: u64 = std::mem::size_of::<AtomicU32>() as u64;

        // Check alignment
        if offset % ALIGN != 0 {
            return Err(MmapIoError::Misaligned {
                required: ALIGN,
                offset,
            });
        }

        // Check bounds
        let total_size = SIZE * count as u64;
        let total = self.current_len()?;
        if offset + total_size > total {
            return Err(MmapIoError::OutOfBounds {
                offset,
                len: total_size,
                total,
            });
        }

        // Get the base pointer for the mapping
        let ptr = match &self.inner.map {
            crate::mmap::MapVariant::Ro(m) => m.as_ptr(),
            crate::mmap::MapVariant::Rw(lock) => {
                let guard = lock.read();
                guard.as_ptr()
            }
            crate::mmap::MapVariant::Cow(m) => m.as_ptr(),
        };

        // SAFETY: Multiple invariants are guaranteed:
        // 1. Alignment: We've verified offset % 4 == 0 (required for AtomicU32 array)
        // 2. Bounds: We've verified offset + (count * 4) <= total file size
        // 3. Overflow: try_into() ensures offset fits in usize, preventing ptr arithmetic overflow
        // 4. Lifetime: The returned slice is bound to 'self', ensuring the mapping outlives it
        // 5. Validity: The memory is mapped and valid for the entire requested range
        // 6. Atomicity: Each element in the slice maintains 4-byte alignment for atomic operations
        let offset_usize = offset.try_into().map_err(|_| MmapIoError::OutOfBounds {
            offset,
            len: total_size,
            total,
        })?;
        unsafe {
            // ptr.add() is safe because:
            // - offset_usize is guaranteed to be within bounds (checked above)
            // - The resulting pointer is within the mapped region
            let addr = ptr.add(offset_usize);
            let atomic_ptr = addr as *const AtomicU32;
            // from_raw_parts is safe because:
            // - atomic_ptr points to valid, aligned memory
            // - count elements fit within the mapped region (verified above)
            Ok(std::slice::from_raw_parts(atomic_ptr, count))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::create_mmap;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::Ordering;

    fn tmp_path(name: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "mmap_io_atomic_test_{}_{}",
            name,
            std::process::id()
        ));
        p
    }

    #[test]
    #[cfg(feature = "atomic")]
    fn test_atomic_u64_operations() {
        let path = tmp_path("atomic_u64");
        let _ = fs::remove_file(&path);

        let mmap = create_mmap(&path, 64).expect("create");

        // Test aligned access
        let atomic = mmap.atomic_u64(0).expect("atomic at 0");
        atomic.store(0x1234567890ABCDEF, Ordering::SeqCst);
        assert_eq!(atomic.load(Ordering::SeqCst), 0x1234567890ABCDEF);

        // Test another aligned offset
        let atomic2 = mmap.atomic_u64(8).expect("atomic at 8");
        atomic2.store(0xFEDCBA0987654321, Ordering::SeqCst);
        assert_eq!(atomic2.load(Ordering::SeqCst), 0xFEDCBA0987654321);

        // Test misaligned access
        assert!(matches!(
            mmap.atomic_u64(1),
            Err(MmapIoError::Misaligned {
                required: 8,
                offset: 1
            })
        ));
        assert!(matches!(
            mmap.atomic_u64(7),
            Err(MmapIoError::Misaligned {
                required: 8,
                offset: 7
            })
        ));

        // Test out of bounds
        assert!(mmap.atomic_u64(64).is_err());
        assert!(mmap.atomic_u64(57).is_err()); // Would need 8 bytes

        fs::remove_file(&path).expect("cleanup");
    }

    #[test]
    #[cfg(feature = "atomic")]
    fn test_atomic_u32_operations() {
        let path = tmp_path("atomic_u32");
        let _ = fs::remove_file(&path);

        let mmap = create_mmap(&path, 32).expect("create");

        // Test aligned access
        let atomic = mmap.atomic_u32(0).expect("atomic at 0");
        atomic.store(0x12345678, Ordering::SeqCst);
        assert_eq!(atomic.load(Ordering::SeqCst), 0x12345678);

        // Test another aligned offset
        let atomic2 = mmap.atomic_u32(4).expect("atomic at 4");
        atomic2.store(0x87654321, Ordering::SeqCst);
        assert_eq!(atomic2.load(Ordering::SeqCst), 0x87654321);

        // Test misaligned access
        assert!(matches!(
            mmap.atomic_u32(1),
            Err(MmapIoError::Misaligned {
                required: 4,
                offset: 1
            })
        ));
        assert!(matches!(
            mmap.atomic_u32(3),
            Err(MmapIoError::Misaligned {
                required: 4,
                offset: 3
            })
        ));

        // Test out of bounds
        assert!(mmap.atomic_u32(32).is_err());
        assert!(mmap.atomic_u32(29).is_err()); // Would need 4 bytes

        fs::remove_file(&path).expect("cleanup");
    }

    #[test]
    #[cfg(feature = "atomic")]
    fn test_atomic_slices() {
        let path = tmp_path("atomic_slices");
        let _ = fs::remove_file(&path);

        let mmap = create_mmap(&path, 128).expect("create");

        // Test u64 slice
        let slice = mmap.atomic_u64_slice(0, 4).expect("u64 slice");
        assert_eq!(slice.len(), 4);
        for (i, atomic) in slice.iter().enumerate() {
            atomic.store(i as u64 * 100, Ordering::SeqCst);
        }
        for (i, atomic) in slice.iter().enumerate() {
            assert_eq!(atomic.load(Ordering::SeqCst), i as u64 * 100);
        }

        // Test u32 slice
        let slice = mmap.atomic_u32_slice(64, 8).expect("u32 slice");
        assert_eq!(slice.len(), 8);
        for (i, atomic) in slice.iter().enumerate() {
            atomic.store(i as u32 * 10, Ordering::SeqCst);
        }
        for (i, atomic) in slice.iter().enumerate() {
            assert_eq!(atomic.load(Ordering::SeqCst), i as u32 * 10);
        }

        // Test misaligned slice
        assert!(mmap.atomic_u64_slice(1, 2).is_err());
        assert!(mmap.atomic_u32_slice(2, 2).is_err());

        // Test out of bounds slice
        assert!(mmap.atomic_u64_slice(120, 2).is_err()); // Would need 16 bytes
        assert!(mmap.atomic_u32_slice(124, 2).is_err()); // Would need 8 bytes

        fs::remove_file(&path).expect("cleanup");
    }

    #[test]
    #[cfg(feature = "atomic")]
    fn test_atomic_with_different_modes() {
        let path = tmp_path("atomic_modes");
        let _ = fs::remove_file(&path);

        // Create and write initial values
        let mmap = create_mmap(&path, 16).expect("create");
        let atomic = mmap.atomic_u64(0).expect("atomic");
        atomic.store(42, Ordering::SeqCst);
        mmap.flush().expect("flush");
        drop(mmap);

        // Test with RO mode
        let mmap = MemoryMappedFile::open_ro(&path).expect("open ro");
        let atomic = mmap.atomic_u64(0).expect("atomic ro");
        assert_eq!(atomic.load(Ordering::SeqCst), 42);
        // Note: Writing to RO atomic would be UB, so we don't test it

        #[cfg(feature = "cow")]
        {
            // Test with COW mode
            let mmap = MemoryMappedFile::open_cow(&path).expect("open cow");
            let atomic = mmap.atomic_u64(0).expect("atomic cow");
            assert_eq!(atomic.load(Ordering::SeqCst), 42);
            // COW writes would only affect this process
        }

        fs::remove_file(&path).expect("cleanup");
    }

    #[test]
    #[cfg(feature = "atomic")]
    fn test_concurrent_atomic_access() {
        use std::sync::Arc;
        use std::thread;

        let path = tmp_path("concurrent_atomic");
        let _ = fs::remove_file(&path);

        let mmap = Arc::new(create_mmap(&path, 8).expect("create"));
        let atomic = mmap.atomic_u64(0).expect("atomic");
        atomic.store(0, Ordering::SeqCst);

        // Spawn multiple threads incrementing the same atomic
        let handles: Vec<_> = (0..4)
            .map(|_| {
                let mmap = Arc::clone(&mmap);
                thread::spawn(move || {
                    let atomic = mmap.atomic_u64(0).expect("atomic in thread");
                    for _ in 0..1000 {
                        atomic.fetch_add(1, Ordering::SeqCst);
                    }
                })
            })
            .collect();

        for handle in handles {
            handle.join().expect("thread join");
        }

        // Verify all increments were recorded
        assert_eq!(atomic.load(Ordering::SeqCst), 4000);

        fs::remove_file(&path).expect("cleanup");
    }
}
