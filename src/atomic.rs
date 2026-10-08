//! Atomic memory views for lock-free concurrent access to specific data types.
//!
//! Atomic views are available on `ReadWrite` and (since 1.1.0)
//! `CopyOnWrite` mappings. On a copy-on-write mapping the stores land
//! in private pages, are shared by every thread using this mapping, and
//! never reach the file. Read-only mappings are backed by pages the
//! process may not write, and an atomic view hands out `&AtomicU64` /
//! `&AtomicU32`, whose safe `store` / `fetch_add` methods would fault
//! on those pages, so requesting a view on one returns
//! [`MmapIoError::InvalidMode`].
//!
//! # Lifetime safety
//!
//! Atomic views returned by these methods are wrapper types that hold
//! a read guard on the mapping for as long as the view is alive. This
//! prevents [`MemoryMappedFile::resize`] from running concurrently
//! and swapping out the underlying memory under a live view, which
//! would otherwise be use-after-free.
//!
//! Dropping a view adds its byte size to
//! [`MemoryMappedFile::pending_bytes`], because individual atomic
//! stores are invisible to the crate's flush accounting. Call
//! [`MemoryMappedFile::flush`] when stored values must be durable.
//!
//! Practical consequence: while any [`AtomicView`] or
//! [`AtomicSliceView`] is alive, calls to `resize()` and to every
//! write method (`update_region`, `as_slice_mut`, `chunks_mut`) on the
//! same mapping block until every view has been dropped. Calling one
//! of those methods on the thread that holds the view deadlocks.
//!
//! # Mixing atomic and plain access
//!
//! An atomic view and a [`MappedSlice`](crate::MappedSlice) both hold
//! read guards, so they can be alive at the same time. Reading bytes
//! through a `MappedSlice` that another thread is concurrently storing
//! to through an atomic view is a data race under the Rust memory
//! model; keep atomic regions and plain-byte regions disjoint, or use
//! the atomic view for every access to those bytes.

use crate::errors::{MmapIoError, Result};
use crate::mmap::MemoryMappedFile;
use crate::raw::RawMmapMut;
use parking_lot::RwLockReadGuard;
use std::marker::PhantomData;
use std::ops::Deref;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

/// The atomic integer types a view can expose. Sealed: every
/// implementor has the same size and alignment as its integer type
/// and accepts every bit pattern, which the pointer cast in
/// [`view_parts`] relies on.
trait AtomicCell: Sync + private::Sealed {}
impl AtomicCell for AtomicU32 {}
impl AtomicCell for AtomicU64 {}
mod private {
    pub trait Sealed {}
    impl Sealed for std::sync::atomic::AtomicU32 {}
    impl Sealed for std::sync::atomic::AtomicU64 {}
}

/// A view into a single atomic value inside a memory-mapped file.
///
/// Implements [`Deref`] so callers can call atomic operations
/// (`load`, `store`, `fetch_add`, `compare_exchange`, etc.) directly
/// on the view as if it were a `&T`.
///
/// # Lifetime semantics
///
/// While this view is alive, the parent mapping cannot be resized or
/// written through the locking write paths: the view holds a read
/// lock, and `resize()` / `update_region()` (which need the write
/// lock) block until the view is dropped.
pub struct AtomicView<'a, T> {
    _guard: RwLockReadGuard<'a, RawMmapMut>,
    ptr: *const T,
    /// The mapping's pending-bytes counter (`None` when writes are not
    /// tracked: copy-on-write); see `Drop`.
    pending: Option<&'a AtomicU64>,
    _marker: PhantomData<&'a T>,
}

impl<T> Drop for AtomicView<'_, T> {
    fn drop(&mut self) {
        // Stores through the view cannot be observed individually, so
        // the view's bytes count as written once it is released.
        if let Some(pending) = self.pending {
            pending.fetch_add(std::mem::size_of::<T>() as u64, Ordering::AcqRel);
        }
    }
}

// SAFETY: AtomicView is safe to send to another thread because:
// - The read guard is Send (this crate enables parking_lot's
//   `send_guard` feature; the compile-time assertion
//   `_ASSERT_GUARDS_SEND_SYNC` in mmap.rs fails the build otherwise)
//   and Sync (`RawMmapMut` is Sync).
// - The pointer targets memory owned by the guarded mapping, which
//   stays mapped while the guard is alive, wherever the guard lives.
// - `T: Sync` is required, so handing `&T` to another thread is sound.
unsafe impl<T: Sync> Send for AtomicView<'_, T> {}
// SAFETY: Same justification as Send. Sharing &AtomicView across
// threads only exposes `&T`, which is sound because `T: Sync`.
unsafe impl<T: Sync> Sync for AtomicView<'_, T> {}

impl<T> Deref for AtomicView<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        // SAFETY: ptr was constructed in `view_parts` from an
        // in-bounds, correctly aligned address within the mapping
        // that `_guard` keeps mapped for the lifetime of this view.
        // The returned borrow is tied to `&self`.
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
    _guard: RwLockReadGuard<'a, RawMmapMut>,
    ptr: *const T,
    len: usize,
    /// The mapping's pending-bytes counter, if tracked; see `Drop`.
    pending: Option<&'a AtomicU64>,
    _marker: PhantomData<&'a [T]>,
}

impl<T> Drop for AtomicSliceView<'_, T> {
    fn drop(&mut self) {
        // See `AtomicView`'s `Drop`.
        if let Some(pending) = self.pending {
            let bytes = (std::mem::size_of::<T>() as u64).saturating_mul(self.len as u64);
            pending.fetch_add(bytes, Ordering::AcqRel);
        }
    }
}

// SAFETY: see AtomicView's Send justification; identical here for a
// slice of atomics (the guard is Send, `T: Sync`).
unsafe impl<T: Sync> Send for AtomicSliceView<'_, T> {}
// SAFETY: see AtomicView's Sync justification; shared access only
// exposes `&[T]` with `T: Sync`.
unsafe impl<T: Sync> Sync for AtomicSliceView<'_, T> {}

impl<T> Deref for AtomicSliceView<'_, T> {
    type Target = [T];
    fn deref(&self) -> &[T] {
        // SAFETY: ptr was constructed in `view_parts` from an
        // in-bounds, correctly aligned address with at least `len`
        // consecutive T-elements inside the mapping that `_guard`
        // keeps mapped for the lifetime of this view.
        unsafe { std::slice::from_raw_parts(self.ptr, self.len) }
    }
}

/// Validate a request for `count` consecutive `T` values at `offset`
/// and return the read guard plus a pointer to the first element.
///
/// Checks run in this order: mapping mode (`InvalidMode` for RO),
/// alignment (`Misaligned`), bounds (`OutOfBounds`). Bounds are
/// checked against the mapping length read under the returned guard,
/// so a concurrent `resize()` cannot invalidate the result.
fn view_parts<T: AtomicCell>(
    mapping: &MemoryMappedFile,
    offset: u64,
    count: usize,
) -> Result<(RwLockReadGuard<'_, RawMmapMut>, *const T)> {
    let align = std::mem::align_of::<T>() as u64;
    let size = std::mem::size_of::<T>() as u64;

    let lock = mapping.write_lock("atomic views require a ReadWrite or CopyOnWrite mapping")?;
    if offset % align != 0 {
        return Err(MmapIoError::Misaligned {
            required: align,
            offset,
        });
    }
    let len = size.saturating_mul(count as u64);
    // Recursive so a thread that already holds a view or slice cannot
    // deadlock behind a queued writer.
    let guard = lock.read_recursive();
    let total = guard.len() as u64;
    if offset.saturating_add(len) > total {
        return Err(MmapIoError::OutOfBounds { offset, len, total });
    }
    // `offset <= total`, and `total` came from a `usize`, so the cast
    // is lossless.
    let start = offset as usize;
    // SAFETY: `start + len <= guard.len()` was checked above against
    // the length of the mapping that `guard` keeps mapped, so
    // `as_ptr().add(start)` stays within (or one past the end of) the
    // same allocation. The cast to `*const T` is sound because:
    //   1. The mapping base is page-aligned and `offset` is a multiple
    //      of `align_of::<T>()`, so the address is aligned for `T`.
    //   2. `T` is `AtomicU32` or `AtomicU64` (the sealed `AtomicCell`
    //      trait), which have the same size and layout as `u32`/`u64`
    //      and accept every bit pattern; `size == align` for both, so
    //      every element of a `count`-long run is also aligned.
    //   3. The bytes are writable: only RW and COW mappings (both
    //      mapped PROT_READ|PROT_WRITE) reach this point.
    // Reference: https://doc.rust-lang.org/std/sync/atomic/struct.AtomicU64.html
    let ptr = unsafe { guard.as_ptr().add(start) }.cast::<T>();
    Ok((guard, ptr))
}

impl MemoryMappedFile {
    /// Get an atomic view of a `u64` value at the specified offset.
    ///
    /// The mapping must be `ReadWrite` or `CopyOnWrite` and the offset
    /// must be 8-byte aligned (the alignment of [`AtomicU64`]). The returned view
    /// implements [`Deref<Target = AtomicU64>`], so atomic operations
    /// (`load`, `store`, `fetch_add`, `compare_exchange`, etc.) can be
    /// called directly:
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
    /// The returned view holds a read lock on the mapping. Calls to
    /// [`MemoryMappedFile::resize`] and to the locking write methods
    /// from any thread block until every live `AtomicView` (and
    /// [`AtomicSliceView`]) on this mapping has been dropped, so
    /// `resize()` cannot unmap memory under a live atomic reference.
    ///
    /// # Errors
    ///
    /// Returns [`MmapIoError::InvalidMode`] if the mapping is
    /// `ReadOnly`.
    /// Returns [`MmapIoError::Misaligned`] if the offset is not
    /// 8-byte aligned.
    /// Returns [`MmapIoError::OutOfBounds`] if `offset + 8` exceeds
    /// the file's current length.
    #[cfg(feature = "atomic")]
    pub fn atomic_u64(&self, offset: u64) -> Result<AtomicView<'_, AtomicU64>> {
        let (guard, ptr) = view_parts::<AtomicU64>(self, offset, 1)?;
        Ok(AtomicView {
            _guard: guard,
            ptr,
            pending: self.pending_counter(),
            _marker: PhantomData,
        })
    }

    /// Get an atomic view of a `u32` value at the specified offset.
    ///
    /// 4-byte alignment is required. See [`atomic_u64`] for the
    /// full mode / lifetime / resize contract; the only difference
    /// is the element type.
    ///
    /// [`atomic_u64`]: Self::atomic_u64
    ///
    /// # Errors
    ///
    /// Returns [`MmapIoError::InvalidMode`] if the mapping is
    /// `ReadOnly`.
    /// Returns [`MmapIoError::Misaligned`] if the offset is not
    /// 4-byte aligned.
    /// Returns [`MmapIoError::OutOfBounds`] if `offset + 4` exceeds
    /// the file's current length.
    #[cfg(feature = "atomic")]
    pub fn atomic_u32(&self, offset: u64) -> Result<AtomicView<'_, AtomicU32>> {
        let (guard, ptr) = view_parts::<AtomicU32>(self, offset, 1)?;
        Ok(AtomicView {
            _guard: guard,
            ptr,
            pending: self.pending_counter(),
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
    /// See [`atomic_u64`](Self::atomic_u64) for the full mode /
    /// lifetime / resize contract.
    ///
    /// # Errors
    ///
    /// Returns [`MmapIoError::InvalidMode`] if the mapping is
    /// `ReadOnly`.
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
        let (guard, ptr) = view_parts::<AtomicU64>(self, offset, count)?;
        Ok(AtomicSliceView {
            _guard: guard,
            ptr,
            len: count,
            pending: self.pending_counter(),
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
    /// Returns [`MmapIoError::InvalidMode`] if the mapping is
    /// `ReadOnly`.
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
        let (guard, ptr) = view_parts::<AtomicU32>(self, offset, count)?;
        Ok(AtomicSliceView {
            _guard: guard,
            ptr,
            len: count,
            pending: self.pending_counter(),
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

        // RO mode: the pages are read-only, so a view (whose safe
        // `store` would fault) is refused.
        let mmap = MemoryMappedFile::open_ro(&path).expect("open ro");
        assert!(matches!(
            mmap.atomic_u64(0),
            Err(MmapIoError::InvalidMode(_))
        ));
        assert!(matches!(
            mmap.atomic_u32_slice(0, 2),
            Err(MmapIoError::InvalidMode(_))
        ));
        drop(mmap);

        #[cfg(feature = "cow")]
        {
            // COW mode: atomics work on the private pages and never
            // reach the file.
            let mmap = MemoryMappedFile::open_cow(&path).expect("open cow");
            {
                let atomic = mmap.atomic_u64(0).expect("atomic cow");
                assert_eq!(atomic.load(Ordering::SeqCst), 42);
                atomic.store(7, Ordering::SeqCst);
            }
            assert_eq!(mmap.atomic_u64(0).expect("again").load(Ordering::SeqCst), 7);
            assert_eq!(mmap.pending_bytes(), 0);
            drop(mmap);
        }

        // RW again: the stored value persisted.
        let mmap = MemoryMappedFile::open_rw(&path).expect("open rw");
        {
            let atomic = mmap.atomic_u64(0).expect("atomic rw");
            assert_eq!(atomic.load(Ordering::SeqCst), 42);
        }
        drop(mmap);

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
