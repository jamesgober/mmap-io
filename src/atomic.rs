//! Atomic memory views for lock-free concurrent access to specific data types.
//!
//! # Lifetime safety (C3 fix)
//!
//! Atomic views returned by these methods are wrapper types that hold
//! the read guard on RW mappings for as long as the view is alive.
//! This prevents [`MemoryMappedFile::resize`] from running concurrently
//! and swapping out the underlying memory under a live view, which
//! would otherwise be use-after-free UB.
//!
//! Practical consequence: while any [`AtomicView`] or
//! [`AtomicSliceView`] is alive, calls to `resize()` on the same
//! mapping will block until every view has been dropped. For RO and
//! COW mappings the lock is not held (the underlying [`memmap2::Mmap`]
//! cannot be remapped), but the wrapper types are used uniformly so
//! callers don't need to know which mode they're in.

use crate::errors::{MmapIoError, Result};
use crate::mmap::{MapVariant, MemoryMappedFile};
use memmap2::MmapMut;
use parking_lot::RwLockReadGuard;
use std::marker::PhantomData;
use std::ops::Deref;
use std::sync::atomic::{AtomicU32, AtomicU64};

/// Internal guard variant: either we hold the RW read lock (so
/// `resize` blocks) or we don't need one (RO/COW mapping).
///
/// The `Locked` variant's field is never read directly; it exists
/// purely so the guard is dropped (releasing the read lock) when the
/// owning view drops. The `#[allow(dead_code)]` reflects that
/// "destructor-only" usage.
#[allow(dead_code)]
enum ViewGuard<'a> {
    /// RW mapping: the read guard keeps the lock held for the view's
    /// lifetime. Dropping it releases the lock, allowing `resize`.
    Locked(RwLockReadGuard<'a, MmapMut>),
    /// RO / COW mapping: no lock needed; the lifetime parameter
    /// alone is sufficient to keep the mapping alive.
    None,
}

/// A view into a single atomic value inside a memory-mapped file.
///
/// Implements [`Deref`] so callers can call atomic operations
/// (`load`, `store`, `fetch_add`, `compare_exchange`, etc.) directly
/// on the view as if it were a `&T`.
///
/// # Lifetime semantics
///
/// While this view is alive, the parent mapping cannot be resized.
/// For RW mappings, the view holds a read lock; `resize()` (which
/// needs the write lock) will block until the view is dropped.
pub struct AtomicView<'a, T> {
    _guard: ViewGuard<'a>,
    ptr: *const T,
    _marker: PhantomData<&'a T>,
}

// SAFETY: AtomicView is safe to send to another thread because:
// - For Locked, parking_lot::RwLockReadGuard is Send + Sync.
// - For None (RO/COW), the lifetime tie to &MemoryMappedFile means
//   the mapping outlives the view.
// - The pointer targets atomic memory (T is AtomicU32/AtomicU64),
//   which is Send + Sync by construction.
unsafe impl<T: Sync> Send for AtomicView<'_, T> {}
// SAFETY: Same justification as Send. Sharing &AtomicView across
// threads is no different from sharing &T where T is Sync.
unsafe impl<T: Sync> Sync for AtomicView<'_, T> {}

impl<T> Deref for AtomicView<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        // SAFETY: ptr was constructed from a valid in-bounds address
        // within the mapped region (verified in the constructors
        // below). The mapping is kept alive for the lifetime of this
        // view by either the read guard (RW) or the borrow of
        // &MemoryMappedFile (RO/COW). Memory at this address is
        // properly aligned for T (verified by the alignment check in
        // the constructors).
        unsafe { &*self.ptr }
    }
}

/// A view into a slice of atomic values inside a memory-mapped file.
///
/// Implements [`Deref`] so callers can use it as `&[T]` directly:
/// iteration, indexing, `.len()`, etc., all work.
///
/// See [`AtomicView`] for lifetime / resize semantics.
pub struct AtomicSliceView<'a, T> {
    _guard: ViewGuard<'a>,
    ptr: *const T,
    len: usize,
    _marker: PhantomData<&'a [T]>,
}

// SAFETY: see AtomicView's Send/Sync justification; identical here
// for slice-of-atomics.
unsafe impl<T: Sync> Send for AtomicSliceView<'_, T> {}
unsafe impl<T: Sync> Sync for AtomicSliceView<'_, T> {}

impl<T> Deref for AtomicSliceView<'_, T> {
    type Target = [T];
    fn deref(&self) -> &[T] {
        // SAFETY: ptr was constructed from a valid in-bounds address
        // within the mapped region with at least `len` properly
        // aligned T-elements (verified in the constructors). The
        // mapping is kept alive for the lifetime of this view as in
        // AtomicView::deref.
        unsafe { std::slice::from_raw_parts(self.ptr, self.len) }
    }
}

/// Internal helper: get the base pointer of the mapping and, when in
/// RW mode, the read guard that keeps the mapping alive and the
/// mapped memory stable for the duration of the returned view.
fn base_ptr_and_guard(mapping: &MemoryMappedFile) -> (*const u8, ViewGuard<'_>) {
    match &mapping.inner.map {
        MapVariant::Ro(m) => (m.as_ptr(), ViewGuard::None),
        MapVariant::Rw(lock) => {
            let guard = lock.read();
            let ptr = guard.as_ptr();
            (ptr, ViewGuard::Locked(guard))
        }
        MapVariant::Cow(m) => (m.as_ptr(), ViewGuard::None),
    }
}

impl MemoryMappedFile {
    /// Get an atomic view of a `u64` value at the specified offset.
    ///
    /// The offset must be 8-byte aligned (the alignment of
    /// [`AtomicU64`]). The returned view implements [`Deref<Target =
    /// AtomicU64>`], so atomic operations (`load`, `store`,
    /// `fetch_add`, `compare_exchange`, etc.) can be called directly:
    ///
    /// ```no_run
    /// use mmap_io::MemoryMappedFile;
    /// use std::sync::atomic::Ordering;
    ///
    /// let mmap = MemoryMappedFile::create_rw("counter.bin", 64)?;
    /// let counter = mmap.atomic_u64(0)?;
    /// counter.fetch_add(1, Ordering::SeqCst);
    /// # Ok::<(), mmap_io::MmapIoError>(())
    /// ```
    ///
    /// # Resize interaction
    ///
    /// On RW mappings, the returned view holds a read lock on the
    /// mapping. Calls to [`MemoryMappedFile::resize`] from any thread
    /// will block until every live `AtomicView` (and
    /// [`AtomicSliceView`]) on this mapping has been dropped. This is
    /// what makes the C3 fix sound: `resize()` cannot pull the rug
    /// out from under a live atomic reference.
    ///
    /// # Errors
    ///
    /// Returns [`MmapIoError::Misaligned`] if the offset is not
    /// 8-byte aligned.
    /// Returns [`MmapIoError::OutOfBounds`] if `offset + 8` exceeds
    /// the file's current length.
    #[cfg(feature = "atomic")]
    pub fn atomic_u64(&self, offset: u64) -> Result<AtomicView<'_, AtomicU64>> {
        const ALIGN: u64 = std::mem::align_of::<AtomicU64>() as u64;
        const SIZE: u64 = std::mem::size_of::<AtomicU64>() as u64;

        if offset % ALIGN != 0 {
            return Err(MmapIoError::Misaligned {
                required: ALIGN,
                offset,
            });
        }
        let total = self.current_len()?;
        if offset.saturating_add(SIZE) > total {
            return Err(MmapIoError::OutOfBounds {
                offset,
                len: SIZE,
                total,
            });
        }
        let offset_usize: usize = offset.try_into().map_err(|_| MmapIoError::OutOfBounds {
            offset,
            len: SIZE,
            total,
        })?;

        let (base, guard) = base_ptr_and_guard(self);
        // SAFETY: alignment, bounds, and target validity are
        // established above. `base.add(offset_usize)` yields a
        // pointer inside the mapped region (offset_usize < total <=
        // mapping length). Casting `*const u8` -> `*const AtomicU64`
        // is sound because:
        //   1. The offset satisfies the AtomicU64 alignment requirement
        //      (verified by `offset % ALIGN == 0`).
        //   2. AtomicU64 has the same in-memory layout as u64 (8
        //      bytes, no padding), so any 8 bytes at the right
        //      alignment are a valid AtomicU64 bit pattern (all u64
        //      bit patterns are valid AtomicU64 values).
        //   3. The pointed-to memory is mapped and remains alive for
        //      the returned view's lifetime (via the guard for RW,
        //      via the borrow of `&self` for RO/COW).
        let ptr = unsafe { base.add(offset_usize) as *const AtomicU64 };
        Ok(AtomicView {
            _guard: guard,
            ptr,
            _marker: PhantomData,
        })
    }

    /// Get an atomic view of a `u32` value at the specified offset.
    ///
    /// 4-byte alignment is required. See [`atomic_u64`] for the
    /// full lifetime / resize / safety contract; the only difference
    /// is the element type.
    ///
    /// [`atomic_u64`]: Self::atomic_u64
    ///
    /// # Errors
    ///
    /// Returns [`MmapIoError::Misaligned`] if the offset is not
    /// 4-byte aligned.
    /// Returns [`MmapIoError::OutOfBounds`] if `offset + 4` exceeds
    /// the file's current length.
    #[cfg(feature = "atomic")]
    pub fn atomic_u32(&self, offset: u64) -> Result<AtomicView<'_, AtomicU32>> {
        const ALIGN: u64 = std::mem::align_of::<AtomicU32>() as u64;
        const SIZE: u64 = std::mem::size_of::<AtomicU32>() as u64;

        if offset % ALIGN != 0 {
            return Err(MmapIoError::Misaligned {
                required: ALIGN,
                offset,
            });
        }
        let total = self.current_len()?;
        if offset.saturating_add(SIZE) > total {
            return Err(MmapIoError::OutOfBounds {
                offset,
                len: SIZE,
                total,
            });
        }
        let offset_usize: usize = offset.try_into().map_err(|_| MmapIoError::OutOfBounds {
            offset,
            len: SIZE,
            total,
        })?;

        let (base, guard) = base_ptr_and_guard(self);
        // SAFETY: same justification as atomic_u64 but with AtomicU32
        // (4-byte alignment, 4-byte size, same layout-equivalence to
        // u32).
        let ptr = unsafe { base.add(offset_usize) as *const AtomicU32 };
        Ok(AtomicView {
            _guard: guard,
            ptr,
            _marker: PhantomData,
        })
    }

    /// Get a slice view of `count` `AtomicU64` values starting at
    /// the specified offset.
    ///
    /// All elements must lie within the file. The returned view
    /// implements [`Deref<Target = [AtomicU64]>`], so iteration,
    /// indexing, and `.len()` work directly.
    ///
    /// See [`atomic_u64`](Self::atomic_u64) for the full lifetime /
    /// resize / safety contract.
    ///
    /// # Errors
    ///
    /// Returns [`MmapIoError::Misaligned`] if the offset is not
    /// 8-byte aligned.
    /// Returns [`MmapIoError::OutOfBounds`] if the requested range
    /// exceeds the file's current length.
    #[cfg(feature = "atomic")]
    pub fn atomic_u64_slice(
        &self,
        offset: u64,
        count: usize,
    ) -> Result<AtomicSliceView<'_, AtomicU64>> {
        const ALIGN: u64 = std::mem::align_of::<AtomicU64>() as u64;
        const SIZE: u64 = std::mem::size_of::<AtomicU64>() as u64;

        if offset % ALIGN != 0 {
            return Err(MmapIoError::Misaligned {
                required: ALIGN,
                offset,
            });
        }
        let total_size = SIZE.saturating_mul(count as u64);
        let total = self.current_len()?;
        if offset.saturating_add(total_size) > total {
            return Err(MmapIoError::OutOfBounds {
                offset,
                len: total_size,
                total,
            });
        }
        let offset_usize: usize = offset.try_into().map_err(|_| MmapIoError::OutOfBounds {
            offset,
            len: total_size,
            total,
        })?;

        let (base, guard) = base_ptr_and_guard(self);
        // SAFETY: alignment of the first element implies alignment of
        // all subsequent elements (AtomicU64 size == 8 == alignment,
        // so `offset + i*8` is also aligned). `count` elements fit
        // within the mapping (verified above). slice::from_raw_parts
        // requires the pointer to point to `count` consecutive valid
        // T values, which holds for the same layout-equivalence
        // reasons as in atomic_u64.
        let ptr = unsafe { base.add(offset_usize) as *const AtomicU64 };
        Ok(AtomicSliceView {
            _guard: guard,
            ptr,
            len: count,
            _marker: PhantomData,
        })
    }

    /// Get a slice view of `count` `AtomicU32` values starting at
    /// the specified offset.
    ///
    /// 4-byte alignment is required. See
    /// [`atomic_u64_slice`](Self::atomic_u64_slice) for the full
    /// contract.
    ///
    /// # Errors
    ///
    /// Returns [`MmapIoError::Misaligned`] if the offset is not
    /// 4-byte aligned.
    /// Returns [`MmapIoError::OutOfBounds`] if the requested range
    /// exceeds the file's current length.
    #[cfg(feature = "atomic")]
    pub fn atomic_u32_slice(
        &self,
        offset: u64,
        count: usize,
    ) -> Result<AtomicSliceView<'_, AtomicU32>> {
        const ALIGN: u64 = std::mem::align_of::<AtomicU32>() as u64;
        const SIZE: u64 = std::mem::size_of::<AtomicU32>() as u64;

        if offset % ALIGN != 0 {
            return Err(MmapIoError::Misaligned {
                required: ALIGN,
                offset,
            });
        }
        let total_size = SIZE.saturating_mul(count as u64);
        let total = self.current_len()?;
        if offset.saturating_add(total_size) > total {
            return Err(MmapIoError::OutOfBounds {
                offset,
                len: total_size,
                total,
            });
        }
        let offset_usize: usize = offset.try_into().map_err(|_| MmapIoError::OutOfBounds {
            offset,
            len: total_size,
            total,
        })?;

        let (base, guard) = base_ptr_and_guard(self);
        // SAFETY: see atomic_u64_slice; identical reasoning for
        // AtomicU32 (4-byte alignment + size).
        let ptr = unsafe { base.add(offset_usize) as *const AtomicU32 };
        Ok(AtomicSliceView {
            _guard: guard,
            ptr,
            len: count,
            _marker: PhantomData,
        })
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

        // Aligned access; Deref<Target = AtomicU64> means the call
        // looks identical to the old &AtomicU64 API.
        let atomic = mmap.atomic_u64(0).expect("atomic at 0");
        atomic.store(0x1234567890ABCDEF, Ordering::SeqCst);
        assert_eq!(atomic.load(Ordering::SeqCst), 0x1234567890ABCDEF);

        let atomic2 = mmap.atomic_u64(8).expect("atomic at 8");
        atomic2.store(0xFEDCBA0987654321, Ordering::SeqCst);
        assert_eq!(atomic2.load(Ordering::SeqCst), 0xFEDCBA0987654321);

        // Misaligned offsets must error.
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

        // Out of bounds.
        assert!(mmap.atomic_u64(64).is_err());
        assert!(mmap.atomic_u64(57).is_err());

        // Drop views before removing the file (especially on
        // Windows where the read guard blocks file deletion via
        // the mapping).
        drop(atomic);
        drop(atomic2);
        fs::remove_file(&path).expect("cleanup");
    }

    #[test]
    #[cfg(feature = "atomic")]
    fn test_atomic_u32_operations() {
        let path = tmp_path("atomic_u32");
        let _ = fs::remove_file(&path);

        let mmap = create_mmap(&path, 32).expect("create");

        let atomic = mmap.atomic_u32(0).expect("atomic at 0");
        atomic.store(0x12345678, Ordering::SeqCst);
        assert_eq!(atomic.load(Ordering::SeqCst), 0x12345678);

        let atomic2 = mmap.atomic_u32(4).expect("atomic at 4");
        atomic2.store(0x87654321, Ordering::SeqCst);
        assert_eq!(atomic2.load(Ordering::SeqCst), 0x87654321);

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

        assert!(mmap.atomic_u32(32).is_err());
        assert!(mmap.atomic_u32(29).is_err());

        drop(atomic);
        drop(atomic2);
        fs::remove_file(&path).expect("cleanup");
    }

    #[test]
    #[cfg(feature = "atomic")]
    fn test_atomic_slices() {
        let path = tmp_path("atomic_slices");
        let _ = fs::remove_file(&path);

        let mmap = create_mmap(&path, 128).expect("create");

        // AtomicSliceView derefs to &[AtomicU64], so iter() works.
        let u64_slice = mmap.atomic_u64_slice(0, 4).expect("u64 slice");
        assert_eq!(u64_slice.len(), 4);
        for (i, atomic) in u64_slice.iter().enumerate() {
            atomic.store(i as u64 * 100, Ordering::SeqCst);
        }
        for (i, atomic) in u64_slice.iter().enumerate() {
            assert_eq!(atomic.load(Ordering::SeqCst), i as u64 * 100);
        }
        // Drop the u64 slice view BEFORE taking the u32 slice view
        // on the same RW mapping: views share the read lock, but
        // there's no reason to hold two simultaneously here.
        drop(u64_slice);

        let u32_slice = mmap.atomic_u32_slice(64, 8).expect("u32 slice");
        assert_eq!(u32_slice.len(), 8);
        for (i, atomic) in u32_slice.iter().enumerate() {
            atomic.store(i as u32 * 10, Ordering::SeqCst);
        }
        for (i, atomic) in u32_slice.iter().enumerate() {
            assert_eq!(atomic.load(Ordering::SeqCst), i as u32 * 10);
        }
        drop(u32_slice);

        // Misaligned slice.
        assert!(mmap.atomic_u64_slice(1, 2).is_err());
        assert!(mmap.atomic_u32_slice(2, 2).is_err());

        // Out of bounds.
        assert!(mmap.atomic_u64_slice(120, 2).is_err());
        assert!(mmap.atomic_u32_slice(124, 2).is_err());

        fs::remove_file(&path).expect("cleanup");
    }

    #[test]
    #[cfg(feature = "atomic")]
    fn test_atomic_with_different_modes() {
        let path = tmp_path("atomic_modes");
        let _ = fs::remove_file(&path);

        let mmap = create_mmap(&path, 16).expect("create");
        {
            let atomic = mmap.atomic_u64(0).expect("atomic");
            atomic.store(42, Ordering::SeqCst);
        }
        mmap.flush().expect("flush");
        drop(mmap);

        // RO mode.
        let mmap = MemoryMappedFile::open_ro(&path).expect("open ro");
        {
            let atomic = mmap.atomic_u64(0).expect("atomic ro");
            assert_eq!(atomic.load(Ordering::SeqCst), 42);
        }
        drop(mmap);

        #[cfg(feature = "cow")]
        {
            // COW mode.
            let mmap = MemoryMappedFile::open_cow(&path).expect("open cow");
            {
                let atomic = mmap.atomic_u64(0).expect("atomic cow");
                assert_eq!(atomic.load(Ordering::SeqCst), 42);
            }
            drop(mmap);
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
        {
            let atomic = mmap.atomic_u64(0).expect("atomic");
            atomic.store(0, Ordering::SeqCst);
        }

        // Each thread takes its own short-lived view. Read guards
        // stack (parking_lot's RwLock allows multiple readers), so
        // 4 concurrent fetch_add loops over the same atomic all
        // hold the lock at once without contention.
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

        let atomic = mmap.atomic_u64(0).expect("atomic final");
        assert_eq!(atomic.load(Ordering::SeqCst), 4000);
        drop(atomic);

        // Cannot remove file while mmap is alive on Windows. The
        // Arc keeps it alive past this scope; drop explicitly.
        drop(Arc::try_unwrap(mmap).ok());
        let _ = fs::remove_file(&path);
    }
}
