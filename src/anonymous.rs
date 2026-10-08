//! Anonymous (file-less) memory mappings.
//!
//! [`AnonymousMmap`] is a process-local, RW memory region with no
//! backing file. Useful for scratch buffers shared between threads,
//! large temporary allocations that should bypass the heap, or as the
//! kernel-side substrate for fd-passing IPC patterns (where the
//! anonymous mapping's fd is shared with a child process; not exposed
//! here at 1.0 because the fd-passing surface is platform-specific).
//!
//! For shared memory between cooperating processes on the same host,
//! the cross-platform pattern is a file-backed mapping where both
//! processes map the same path. See `examples/10_ipc_shared_state.rs`
//! and the `T6` cross-process integration test.
//!
//! ## Differences from [`MemoryMappedFile`]
//!
//! - No file descriptor / handle; no `AsFd`/`AsRawFd`/`AsHandle` impls.
//! - No `resize` (anonymous mappings have no backing file to grow).
//! - No `flush` (volatile memory; nothing to persist).
//! - No `path` (there is no path).
//!
//! Reads, writes, slice access, the non-blocking `try_` methods, and
//! (feature `atomic`, since 1.1.0) atomic views work the same way.
//!
//! [`MemoryMappedFile`]: crate::mmap::MemoryMappedFile

use crate::raw::RawMmapMut;
use parking_lot::RwLock;

use crate::errors::{MmapIoError, Result};
use crate::mmap::{MappedSlice, MappedSliceMut};
use crate::utils::slice_range;
use crate::views::ViewRegistry;

// Mirrors the same constants used in `mmap.rs`. Kept module-local so a
// future refactor of one does not silently drift the other.
const ERR_ZERO_SIZE: &str = "Size must be greater than zero";

#[cfg(target_pointer_width = "64")]
const MAX_MMAP_SIZE: u64 = 128 * (1 << 40); // 128 TB

#[cfg(target_pointer_width = "32")]
const MAX_MMAP_SIZE: u64 = 2 * (1 << 30); // 2 GB

/// Process-local anonymous memory mapping (no backing file).
///
/// Created via [`AnonymousMmap::new`]. The mapping is RW; pages are
/// zero-initialized by the kernel on first touch. Memory is released
/// when the value is dropped.
///
/// # Examples
///
/// ```
/// use mmap_io::AnonymousMmap;
///
/// let mmap = AnonymousMmap::new(4096)?;
/// mmap.update_region(0, b"hello")?;
/// let mut buf = [0u8; 5];
/// mmap.read_into(0, &mut buf)?;
/// assert_eq!(&buf, b"hello");
/// # Ok::<(), mmap_io::MmapIoError>(())
/// ```
pub struct AnonymousMmap {
    pub(crate) map: RwLock<RawMmapMut>,
    len: u64,
    /// Live plain and atomic views; see `crate::views`.
    pub(crate) views: ViewRegistry,
}

impl AnonymousMmap {
    /// Allocate an anonymous RW mapping of `size` bytes.
    ///
    /// Pages are zero-initialized on first touch (kernel guarantee on
    /// every supported platform: Linux, macOS, Windows).
    ///
    /// # Errors
    ///
    /// - [`MmapIoError::ResizeFailed`] if `size` is zero or exceeds the
    ///   platform maximum (128 TB on 64-bit, 2 GB on 32-bit).
    /// - [`MmapIoError::Io`] if the kernel rejects the allocation
    ///   (out of address space, out of memory, etc.).
    pub fn new(size: u64) -> Result<Self> {
        if size == 0 {
            return Err(MmapIoError::ResizeFailed(ERR_ZERO_SIZE.into()));
        }
        if size > MAX_MMAP_SIZE {
            return Err(MmapIoError::ResizeFailed(format!(
                "Size {size} exceeds maximum safe limit of {MAX_MMAP_SIZE} bytes"
            )));
        }
        let len_usize = usize::try_from(size)
            .map_err(|_| MmapIoError::ResizeFailed(format!("Size {size} does not fit in usize")))?;
        let mmap = RawMmapMut::map_anon(len_usize)?;
        Ok(Self {
            map: RwLock::new(mmap),
            len: size,
            views: ViewRegistry::new(),
        })
    }

    /// Length of the mapping in bytes.
    #[must_use]
    pub fn len(&self) -> u64 {
        self.len
    }

    /// Whether the mapping has zero length. Always `false` for a
    /// successfully-constructed `AnonymousMmap` (the constructor
    /// rejects zero size); provided for API symmetry with
    /// `MemoryMappedFile::is_empty`.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Copy `buf.len()` bytes from the mapping starting at `offset`
    /// into `buf`.
    ///
    /// Bytes under a live atomic view are read with atomic loads, so
    /// the copy never races with concurrent atomic stores.
    ///
    /// # Errors
    ///
    /// Returns [`MmapIoError::OutOfBounds`] if the range exceeds the
    /// mapping length.
    pub fn read_into(&self, offset: u64, buf: &mut [u8]) -> Result<()> {
        let (start, _end) = slice_range(offset, buf.len() as u64, self.len)?;
        // Recursive: a thread that already holds a view must not
        // deadlock behind a queued writer.
        let guard = self.map.read_recursive();
        // SAFETY: `[start, end)` lies within the mapping (checked
        // against `self.len`, which never changes), `guard` is a read
        // guard held for the call, and every atomic view of this
        // mapping registers in `self.views`: `copy_out`'s contract.
        unsafe { self.views.copy_out(RawMmapMut::as_ptr(&guard), start, buf) };
        Ok(())
    }

    /// Write `data.len()` bytes into the mapping starting at `offset`.
    ///
    /// Takes the write lock, so it waits for every live view; calling
    /// it on a thread that holds one deadlocks.
    ///
    /// # Errors
    ///
    /// Returns [`MmapIoError::OutOfBounds`] if the range exceeds the
    /// mapping length.
    pub fn update_region(&self, offset: u64, data: &[u8]) -> Result<()> {
        let (start, end) = slice_range(offset, data.len() as u64, self.len)?;
        let mut guard = self.map.write();
        guard[start..end].copy_from_slice(data);
        Ok(())
    }

    /// Borrow a read-only slice of the mapping.
    ///
    /// The returned [`MappedSlice`] holds a read lock for its lifetime;
    /// concurrent reads are fine, but any concurrent `as_mut_slice` or
    /// `update_region` blocks until the slice is dropped.
    ///
    /// # Errors
    ///
    /// Returns [`MmapIoError::OutOfBounds`] if the range exceeds the
    /// mapping length.
    /// Returns [`MmapIoError::InvalidMode`] if the range overlaps a live
    /// atomic view (see `MemoryMappedFile::as_slice`).
    pub fn as_slice(&self, offset: u64, len: u64) -> Result<MappedSlice<'_>> {
        let (start, end) = slice_range(offset, len, self.len)?;
        let guard = self.map.read_recursive();
        let reg = self.views.register_plain(start, end)?;
        Ok(MappedSlice::guarded(guard, reg, start..end))
    }

    /// Borrow a mutable slice of the mapping.
    ///
    /// The returned [`MappedSliceMut`] holds an exclusive write lock for
    /// its lifetime; any concurrent reader or writer blocks until the
    /// slice is dropped.
    ///
    /// # Errors
    ///
    /// Returns [`MmapIoError::OutOfBounds`] if the range exceeds the
    /// mapping length.
    pub fn as_mut_slice(&self, offset: u64, len: u64) -> Result<MappedSliceMut<'_>> {
        let (start, end) = slice_range(offset, len, self.len)?;
        let guard = self.map.write();
        Ok(MappedSliceMut::guarded(guard, start..end))
    }

    /// Non-blocking [`as_slice`](Self::as_slice): returns `Ok(None)`
    /// instead of waiting while a writer ([`as_mut_slice`](Self::as_mut_slice)
    /// guard or a running `update_region`) holds the lock. Same
    /// validation as `as_slice` otherwise. Since 1.1.0.
    ///
    /// # Errors
    ///
    /// Returns [`MmapIoError::OutOfBounds`] if the range exceeds the
    /// mapping length.
    /// Returns [`MmapIoError::InvalidMode`] if the range overlaps a live
    /// atomic view.
    ///
    /// # Example
    ///
    /// ```
    /// use mmap_io::AnonymousMmap;
    ///
    /// let mmap = AnonymousMmap::new(4096)?;
    /// let w = mmap.as_mut_slice(0, 16)?;
    /// assert!(mmap.try_as_slice(100, 4)?.is_none());
    /// drop(w);
    /// assert!(mmap.try_as_slice(100, 4)?.is_some());
    /// # Ok::<(), mmap_io::MmapIoError>(())
    /// ```
    pub fn try_as_slice(&self, offset: u64, len: u64) -> Result<Option<MappedSlice<'_>>> {
        let (start, end) = slice_range(offset, len, self.len)?;
        let Some(guard) = self.map.try_read_recursive() else {
            return Ok(None);
        };
        let reg = self.views.register_plain(start, end)?;
        Ok(Some(MappedSlice::guarded(guard, reg, start..end)))
    }

    /// Non-blocking [`as_mut_slice`](Self::as_mut_slice): returns
    /// `Ok(None)` instead of waiting while any view or writer holds the
    /// lock, which is also what it returns on a thread that holds a
    /// view of this mapping (where `as_mut_slice` deadlocks). Since
    /// 1.1.0.
    ///
    /// # Errors
    ///
    /// Returns [`MmapIoError::OutOfBounds`] if the range exceeds the
    /// mapping length (checked before the lock: the length of an
    /// anonymous mapping never changes).
    ///
    /// # Example
    ///
    /// ```
    /// use mmap_io::AnonymousMmap;
    ///
    /// let mmap = AnonymousMmap::new(4096)?;
    /// let view = mmap.as_slice(0, 8)?;
    /// assert!(mmap.try_as_mut_slice(8, 8)?.is_none());
    /// drop(view);
    /// mmap.try_as_mut_slice(8, 8)?.expect("free").fill(1);
    /// # Ok::<(), mmap_io::MmapIoError>(())
    /// ```
    pub fn try_as_mut_slice(&self, offset: u64, len: u64) -> Result<Option<MappedSliceMut<'_>>> {
        let (start, end) = slice_range(offset, len, self.len)?;
        Ok(self
            .map
            .try_write()
            .map(|guard| MappedSliceMut::guarded(guard, start..end)))
    }

    /// Non-blocking [`update_region`](Self::update_region): returns
    /// `Ok(false)` instead of waiting while any view or writer holds
    /// the lock, `Ok(true)` once the bytes are written. Since 1.1.0.
    ///
    /// # Errors
    ///
    /// Returns [`MmapIoError::OutOfBounds`] if the range exceeds the
    /// mapping length (checked before the lock).
    ///
    /// # Example
    ///
    /// ```
    /// use mmap_io::AnonymousMmap;
    ///
    /// let mmap = AnonymousMmap::new(64)?;
    /// let view = mmap.as_slice(0, 4)?;
    /// assert!(!mmap.try_update_region(8, b"x")?);
    /// drop(view);
    /// assert!(mmap.try_update_region(8, b"x")?);
    /// # Ok::<(), mmap_io::MmapIoError>(())
    /// ```
    pub fn try_update_region(&self, offset: u64, data: &[u8]) -> Result<bool> {
        let (start, end) = slice_range(offset, data.len() as u64, self.len)?;
        let Some(mut guard) = self.map.try_write() else {
            return Ok(false);
        };
        guard[start..end].copy_from_slice(data);
        Ok(true)
    }

    /// Raw pointer to the start of the mapping.
    ///
    /// # Safety
    ///
    /// The caller must not retain the pointer beyond the lifetime of
    /// this `AnonymousMmap`. Reads through the pointer require no
    /// active mutable borrow elsewhere; writes through the pointer
    /// require no other active borrow at all. Use this only when
    /// bridging to FFI or unsafe code that needs a raw byte pointer.
    #[must_use]
    pub unsafe fn as_ptr(&self) -> *const u8 {
        // SAFETY of the function (not of this line): documented above.
        // This expression itself only takes a read lock and reads the
        // mapping's base address; the read guard is dropped on return,
        // which is the entire point of marking the function unsafe.
        let guard = self.map.read_recursive();
        RawMmapMut::as_ptr(&guard)
    }

    /// Raw mutable pointer to the start of the mapping.
    ///
    /// # Safety
    ///
    /// Same constraints as [`as_ptr`](Self::as_ptr), plus: the caller
    /// is responsible for ensuring no aliasing mutable references
    /// exist for any byte they write to via this pointer.
    #[must_use]
    pub unsafe fn as_mut_ptr(&self) -> *mut u8 {
        // SAFETY: see function docs. Lock guard is released on return.
        let mut guard = self.map.write();
        guard.as_mut_ptr()
    }
}

impl std::fmt::Debug for AnonymousMmap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AnonymousMmap")
            .field("len", &self.len)
            .finish()
    }
}
