/// Hint for when to touch (prewarm) memory pages during mapping creation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TouchHint {
    /// Don't touch pages during creation (default).
    #[default]
    Never,
    /// Eagerly touch all pages during creation to prewarm page tables
    /// and improve first-access latency. Useful for benchmarking scenarios
    /// where you want consistent timing without page fault overhead.
    Eager,
    /// Same as `Never`: pages are faulted in by the OS on first
    /// access. No separate lazy prefetch is performed; the variant is
    /// kept for API compatibility.
    Lazy,
}

use std::{
    fs::{File, OpenOptions},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    sync::Arc,
};

use crate::raw::{RawMmap, RawMmapMut, RawMmapOptions};

use crate::flush::FlushPolicy;

use parking_lot::{RwLock, RwLockReadGuard, RwLockWriteGuard};

use crate::errors::{MmapIoError, Result};
use crate::utils::slice_range;
use crate::views::{PlainReg, ViewRegistry};

// Error message constants
const ERR_ZERO_SIZE: &str = "Size must be greater than zero";
const ERR_ZERO_LENGTH_FILE: &str = "Cannot map zero-length file";

// Maximum safe mmap size: 128TB (reasonable limit for most systems)
// This prevents accidental exhaustion of address space or disk
// Note: This is intentionally very large to support legitimate use cases
// while still preventing obvious errors like u64::MAX
#[cfg(target_pointer_width = "64")]
const MAX_MMAP_SIZE: u64 = 128 * (1 << 40); // 128 TB on 64-bit systems

#[cfg(target_pointer_width = "32")]
const MAX_MMAP_SIZE: u64 = 2 * (1 << 30); // 2 GB on 32-bit systems (practical limit)

/// Access mode for a memory-mapped file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MmapMode {
    /// Read-only mapping.
    ReadOnly,
    /// Read-write mapping.
    ReadWrite,
    /// Copy-on-write mode (`open_cow`, feature `cow`). The file is
    /// mapped privately (`MAP_PRIVATE` / `PAGE_WRITECOPY`): the write
    /// methods work (`update_region`, `as_slice_mut`, `chunks_mut`,
    /// atomic views), changes are visible through this mapping only,
    /// and they never reach the file. `flush` and `flush_range` are
    /// `Ok` no-ops, `pending_bytes` stays 0, and `resize` returns
    /// [`MmapIoError::InvalidMode`]. Writable since 1.1.0; earlier
    /// releases refused every write on this mode.
    CopyOnWrite,
}

#[doc(hidden)]
pub struct Inner {
    pub(crate) path: PathBuf,
    pub(crate) file: File,
    pub(crate) mode: MmapMode,
    // Cached mapping length for `len()`. Only written by `resize()`
    // while it holds the RW write lock; accessors that touch mapped
    // memory validate against the length of the mapping they hold a
    // guard on, never against this value.
    pub(crate) cached_len: AtomicU64,
    // The mapping itself. We use an enum to hold either RO or RW mapping.
    pub(crate) map: MapVariant,
    // Flush policy and accounting (RW only). `written_since_last_flush`
    // counts bytes written through every write path since the last
    // successful full flush; `writes_since_last_flush` counts
    // `update_region` calls for `FlushPolicy::EveryWrites`. Both only
    // drive the policy's automatic flushes; an explicit `flush()`
    // always flushes.
    pub(crate) flush_policy: FlushPolicy,
    pub(crate) written_since_last_flush: AtomicU64,
    pub(crate) writes_since_last_flush: AtomicU64,
    // Time-based flusher background thread (used only when
    // FlushPolicy::EveryMillis is selected on the builder path). Held
    // here so the worker thread's lifetime is bound to the mapping;
    // Drop signals shutdown. See C2 fix in .dev/AUDIT.md.
    pub(crate) flusher: RwLock<Option<crate::flush::TimeBasedFlusher>>,
    // Live views of a writable (RW / COW) mapping, so an atomic view and
    // a plain byte view of the same bytes can never coexist. See
    // `crate::views`. Unused for RO mappings, which have no atomics.
    pub(crate) views: ViewRegistry,
    // Huge pages preference (builder-set), effective on supported platforms
    #[cfg(feature = "hugepages")]
    pub(crate) huge_pages: bool,
}

#[doc(hidden)]
pub enum MapVariant {
    Ro(RawMmap),
    Rw(RwLock<RawMmapMut>),
    /// Private, per-process copy-on-write mapping (`map_copy`). Writes
    /// go to private pages and never reach the file. Locked exactly
    /// like `Rw`: writers take the write lock, readers a read guard.
    Cow(RwLock<RawMmapMut>),
}

impl MapVariant {
    /// The lock of a writable mapping (`Rw` or `Cow`); `None` for `Ro`.
    #[inline]
    pub(crate) fn locked(&self) -> Option<&RwLock<RawMmapMut>> {
        match self {
            MapVariant::Ro(_) => None,
            MapVariant::Rw(lock) | MapVariant::Cow(lock) => Some(lock),
        }
    }
}

/// Memory-mapped file with safe, zero-copy region access.
///
/// This is the core type for memory-mapped file operations. It provides:
/// - Safe concurrent access through interior mutability
/// - Zero-copy reads and writes
/// - Automatic bounds checking
/// - Cross-platform compatibility
///
/// # Examples
///
/// ```no_run
/// use mmap_io::{MemoryMappedFile, MmapMode};
///
/// // Create a new 1KB file
/// let mmap = MemoryMappedFile::create_rw("data.bin", 1024)?;
///
/// // Write some data
/// mmap.update_region(0, b"Hello, world!")?;
/// mmap.flush()?;
///
/// // Open existing file read-only
/// let ro_mmap = MemoryMappedFile::open_ro("data.bin")?;
/// let data = ro_mmap.as_slice(0, 13)?;
/// assert_eq!(data, b"Hello, world!");
/// # Ok::<(), mmap_io::MmapIoError>(())
/// ```
///
/// Cloning this struct is cheap; it clones an Arc to the inner state.
/// For read-write mappings, interior mutability is protected with an `RwLock`.
#[derive(Clone)]
pub struct MemoryMappedFile {
    pub(crate) inner: Arc<Inner>,
}

impl MemoryMappedFile {
    /// Wrap a freshly built `Inner`.
    fn from_inner(inner: Inner) -> Self {
        Self {
            inner: Arc::new(inner),
        }
    }
}

impl std::fmt::Debug for MemoryMappedFile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut ds = f.debug_struct("MemoryMappedFile");
        ds.field("path", &self.inner.path)
            .field("mode", &self.inner.mode)
            .field("len", &self.len());
        #[cfg(feature = "hugepages")]
        {
            ds.field("huge_pages", &self.inner.huge_pages);
        }
        ds.finish()
    }
}

impl MemoryMappedFile {
    /// Builder for constructing a MemoryMappedFile with custom options.
    ///
    /// Example:
    /// ```
    /// # use mmap_io::{MemoryMappedFile, MmapMode};
    /// # use mmap_io::flush::FlushPolicy;
    /// // let mmap = MemoryMappedFile::builder("file.bin")
    /// //     .mode(MmapMode::ReadWrite)
    /// //     .size(1_000_000)
    /// //     .flush_policy(FlushPolicy::EveryBytes(1_000_000))
    /// //     .create().unwrap();
    /// ```
    pub fn builder<P: AsRef<Path>>(path: P) -> MemoryMappedFileBuilder {
        MemoryMappedFileBuilder {
            path: path.as_ref().to_path_buf(),
            size: None,
            mode: None,
            flush_policy: FlushPolicy::default(),
            touch_hint: TouchHint::default(),
            #[cfg(feature = "hugepages")]
            huge_pages: false,
        }
    }

    /// Create a new file (truncating if exists) and memory-map it in read-write mode with the given size.
    ///
    /// # Performance
    ///
    /// - **Time Complexity**: O(1) for mapping creation
    /// - **Memory Usage**: Virtual address space of `size` bytes (physical memory allocated on demand)
    /// - **I/O Operations**: One file creation, one truncate, one mmap syscall
    ///
    /// # Sparse file behavior
    ///
    /// The underlying `set_len(size)` call produces a sparse file on
    /// every supported platform (Linux ext4/xfs/btrfs, macOS APFS,
    /// Windows NTFS). Pages do not consume disk blocks until first
    /// write, so allocating a 1 TB region for an mmap-backed data
    /// structure does not require 1 TB of free disk; only the bytes
    /// you touch do. The filesystem's `du`/`stat` "allocated blocks"
    /// reflects actual usage; the apparent size matches `size`.
    ///
    /// # Errors
    ///
    /// Returns `MmapIoError::ResizeFailed` if size is zero or exceeds the maximum safe limit.
    /// Returns `MmapIoError::Io` if file creation or mapping fails.
    pub fn create_rw<P: AsRef<Path>>(path: P, size: u64) -> Result<Self> {
        if size == 0 {
            return Err(MmapIoError::ResizeFailed(ERR_ZERO_SIZE.into()));
        }
        if size > MAX_MMAP_SIZE {
            return Err(MmapIoError::ResizeFailed(format!(
                "Size {size} exceeds maximum safe limit of {MAX_MMAP_SIZE} bytes"
            )));
        }
        let path_ref = path.as_ref();
        let file = OpenOptions::new()
            .create(true)
            .write(true)
            .read(true)
            .truncate(true)
            .open(path_ref)?;
        file.set_len(size)?;
        // SAFETY: `RawMmapMut::map_mut` is `unsafe` because the OS does
        // not prevent another process from concurrently modifying the
        // backing file under the mapping, which would violate Rust's
        // aliasing model if anyone holds a `&mut [u8]` into the
        // mapping. We do not enforce single-writer at the OS level;
        // callers who share the file across processes are responsible
        // for synchronization (the crate documents this in REPS.md
        // section 5.1). Within this process, all mutable access to
        // the `RawMmapMut` is mediated by `parking_lot::RwLock`, so the
        // standard aliasing rules hold for intra-process access.
        // The file has just been created and `set_len(size)` succeeded,
        // so the kernel will produce a mapping of exactly `size` bytes.
        // Note: `create_rw` convenience ignores huge pages; use builder
        // for that.
        // Contract: `crate::raw::RawMmapMut::map_mut` (see `docs/SAFETY.md`, raw mapping layer).
        let mmap = unsafe { RawMmapMut::map_mut(&file)? };
        let inner = Inner {
            path: path_ref.to_path_buf(),
            file,
            mode: MmapMode::ReadWrite,
            cached_len: AtomicU64::new(size),
            map: MapVariant::Rw(RwLock::new(mmap)),
            flush_policy: FlushPolicy::default(),
            written_since_last_flush: AtomicU64::new(0),
            writes_since_last_flush: AtomicU64::new(0),
            flusher: RwLock::new(None),
            views: ViewRegistry::new(),
            #[cfg(feature = "hugepages")]
            huge_pages: false,
        };
        Ok(Self {
            inner: Arc::new(inner),
        })
    }

    /// Open an existing file and memory-map it read-only.
    ///
    /// # Errors
    ///
    /// Returns `MmapIoError::Io` if file opening or mapping fails.
    pub fn open_ro<P: AsRef<Path>>(path: P) -> Result<Self> {
        let path_ref = path.as_ref();
        let file = OpenOptions::new().read(true).open(path_ref)?;
        let len = file.metadata()?.len();
        // SAFETY: `RawMmap::map` is `unsafe` for the same cross-process
        // reason as `RawMmapMut::map_mut` (see `create_rw` above): the OS
        // does not prevent another process from writing to the backing
        // file. For a read-only mapping the in-process aliasing
        // hazard is reduced because we never hand out `&mut [u8]` into
        // the mapping, but cross-process modification can still cause
        // a race that surfaces as a torn read. This is documented as
        // out-of-scope; intra-process access is sound.
        // Contract: `crate::raw::RawMmap::map` (see `docs/SAFETY.md`, raw mapping layer).
        let mmap = unsafe { RawMmap::map(&file)? };
        let inner = Inner {
            path: path_ref.to_path_buf(),
            file,
            mode: MmapMode::ReadOnly,
            cached_len: AtomicU64::new(len),
            map: MapVariant::Ro(mmap),
            flush_policy: FlushPolicy::Never,
            written_since_last_flush: AtomicU64::new(0),
            writes_since_last_flush: AtomicU64::new(0),
            flusher: RwLock::new(None),
            views: ViewRegistry::new(),
            #[cfg(feature = "hugepages")]
            huge_pages: false,
        };
        Ok(Self {
            inner: Arc::new(inner),
        })
    }

    /// Open an existing file and memory-map it read-write.
    ///
    /// # Errors
    ///
    /// Returns `MmapIoError::ResizeFailed` if file is zero-length.
    /// Returns `MmapIoError::Io` if file opening or mapping fails.
    pub fn open_rw<P: AsRef<Path>>(path: P) -> Result<Self> {
        let path_ref = path.as_ref();
        let file = OpenOptions::new().read(true).write(true).open(path_ref)?;
        let len = file.metadata()?.len();
        if len == 0 {
            return Err(MmapIoError::ResizeFailed(ERR_ZERO_LENGTH_FILE.into()));
        }
        // SAFETY: see `create_rw` above for the full justification of
        // calling `RawMmapMut::map_mut`. Additionally, we have verified
        // here that the file is not zero-length (`len != 0`), which
        // avoids `EINVAL` from `mmap(2)` on Linux for zero-length
        // mappings. Note: `open_rw` convenience ignores huge pages;
        // use the builder for that.
        // Contract: `crate::raw::RawMmapMut::map_mut` (see `docs/SAFETY.md`, raw mapping layer).
        let mmap = unsafe { RawMmapMut::map_mut(&file)? };
        let inner = Inner {
            path: path_ref.to_path_buf(),
            file,
            mode: MmapMode::ReadWrite,
            cached_len: AtomicU64::new(len),
            map: MapVariant::Rw(RwLock::new(mmap)),
            flush_policy: FlushPolicy::default(),
            written_since_last_flush: AtomicU64::new(0),
            writes_since_last_flush: AtomicU64::new(0),
            flusher: RwLock::new(None),
            views: ViewRegistry::new(),
            #[cfg(feature = "hugepages")]
            huge_pages: false,
        };
        Ok(Self {
            inner: Arc::new(inner),
        })
    }

    /// Return current mapping mode.
    #[inline]
    #[must_use]
    pub fn mode(&self) -> MmapMode {
        self.inner.mode
    }

    /// Total length of the mapped file in bytes (cached).
    #[inline]
    #[must_use]
    pub fn len(&self) -> u64 {
        self.inner.cached_len.load(Ordering::Acquire)
    }

    /// Whether the mapped file is empty.
    #[inline]
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Get a zero-copy read-only slice for the given `[offset, offset + len)`.
    ///
    /// Works on all three mapping modes. The returned [`MappedSlice`]
    /// implements `Deref<Target = [u8]>`, so callers use it directly
    /// (indexing, iteration, passing as `&[u8]` via `&*slice` or
    /// `slice.as_ref()`).
    ///
    /// For RW and COW mappings the slice holds an internal read guard
    /// for its lifetime. Other readers are not blocked, but every
    /// operation that needs the write lock is: `resize()`,
    /// `update_region()`, `as_slice_mut()`, and `chunks_mut()` wait
    /// until the slice is dropped, whatever region they touch. Calling
    /// one of those on the thread that holds the slice deadlocks; drop
    /// the slice first, or use the non-blocking
    /// [`try_update_region`](Self::try_update_region) /
    /// [`try_as_slice_mut`](Self::try_as_slice_mut), which return
    /// "would block" instead of waiting. Taking more read views on the
    /// same thread is fine.
    ///
    /// # Performance
    ///
    /// - **Time Complexity**: O(1) - direct pointer access
    /// - **Memory Usage**: No additional allocation (zero-copy)
    /// - **Cache Behavior**: May trigger page faults on first access
    ///
    /// A zero-length request returns an empty slice at any offset
    /// without taking the lock.
    ///
    /// # Atomic views
    ///
    /// On `ReadWrite` and `CopyOnWrite` mappings a `MappedSlice` and an
    /// atomic view of the same bytes cannot be alive at the same time:
    /// an atomic store would race with the slice's plain reads. A
    /// request that overlaps a live atomic view returns
    /// [`MmapIoError::InvalidMode`]; read those bytes through the
    /// atomic view, or copy them with [`read_into`](Self::read_into),
    /// which reads them atomically. Ranges next to an atomic view are
    /// unaffected.
    ///
    /// # Errors
    ///
    /// Returns [`MmapIoError::OutOfBounds`] if `offset + len` exceeds
    /// the file's current length.
    /// Returns [`MmapIoError::InvalidMode`] if the range overlaps a
    /// live atomic view (since 1.1.0).
    pub fn as_slice(&self, offset: u64, len: u64) -> Result<MappedSlice<'_>> {
        if len == 0 {
            return Ok(MappedSlice::owned(&[]));
        }
        let map = self.map_read();
        let (start, end) = slice_range(offset, len, map.len() as u64)?;
        map.into_view(&self.inner.views, start..end)
    }

    /// Migration shim that mirrors the 0.9.6 `as_slice` signature:
    /// returns `Result<&[u8]>` directly for `ReadOnly` mappings, and
    /// `MmapIoError::InvalidMode` for `ReadWrite` and (since 1.1.0,
    /// when copy-on-write mappings became writable) `CopyOnWrite`
    /// mappings. A plain `&[u8]` tied to `&self` cannot keep writers
    /// out, so it is only handed out for memory nothing can write.
    ///
    /// **Prefer [`as_slice`](Self::as_slice)** for new code; that
    /// method returns a [`MappedSlice<'_>`] which works uniformly
    /// across all three mapping modes (RO, COW, AND RW).
    ///
    /// This shim exists because 0.9.7 changed `as_slice`'s return
    /// type from `Result<&[u8]>` to `Result<MappedSlice<'_>>`. That
    /// was a breaking change Cargo's resolver did not flag (the
    /// 0.9.6 → 0.9.7 bump is treated as a minor version per
    /// pre-1.0 semver rules), so downstream code that bound the
    /// return type as `let s: &[u8] = ...` failed to compile.
    /// Callers in that position can switch one method name:
    /// `as_slice(off, len)` → `as_slice_bytes(off, len)` and their
    /// 0.9.6-shaped code compiles unchanged.
    ///
    /// # Errors
    ///
    /// Returns `MmapIoError::OutOfBounds` if `offset + len` exceeds
    /// the file's current length.
    /// Returns `MmapIoError::InvalidMode` on `ReadWrite` and
    /// `CopyOnWrite` mappings (use `as_slice` or `read_into` there).
    pub fn as_slice_bytes(&self, offset: u64, len: u64) -> Result<&[u8]> {
        match &self.inner.map {
            MapVariant::Ro(_) if len == 0 => Ok(&[]),
            MapVariant::Ro(m) => {
                let (start, end) = slice_range(offset, len, m.len() as u64)?;
                Ok(&m[start..end])
            }
            MapVariant::Rw(_) | MapVariant::Cow(_) => Err(MmapIoError::InvalidMode(
                "use as_slice() for writable mappings; as_slice_bytes is the 0.9.6 compat shim and supports read-only mappings only",
            )),
        }
    }

    /// Construct a `bytes::Bytes` containing the requested
    /// `[offset, offset + len)` slice of the mapping. Works on every
    /// mapping mode. The returned `Bytes` is independent of the
    /// mapping lifetime: one heap allocation + memcpy at the
    /// boundary, then the buffer can travel freely through the
    /// hyper / tower / tonic / axum / reqwest ecosystem.
    ///
    /// Available with `feature = "bytes"`.
    ///
    /// # Errors
    ///
    /// Returns `MmapIoError::OutOfBounds` if the range exceeds the
    /// file's current length.
    #[cfg(feature = "bytes")]
    pub fn read_bytes(&self, offset: u64, len: u64) -> Result<bytes::Bytes> {
        if len == 0 {
            return Ok(bytes::Bytes::new());
        }
        // Validate before allocating, then copy under the same guard
        // (bytes under live atomic views are read atomically).
        let map = self.map_read();
        let (start, end) = slice_range(offset, len, map.len() as u64)?;
        let mut buf = vec![0u8; end - start];
        map.copy_to(&self.inner.views, start, &mut buf);
        Ok(bytes::Bytes::from(buf))
    }

    /// Construct an `io::Read` + `io::Seek` cursor over the
    /// mapping. Useful for plugging the mapping into any parser /
    /// decoder that takes a generic `R: Read`: `serde_json::from_reader`,
    /// `flate2::read::GzDecoder`, `tar::Archive::new`,
    /// `image::ImageReader::new`, etc. The cursor borrows the
    /// mapping; multiple cursors can coexist and read concurrently.
    ///
    /// The cursor delegates each `read` call to `read_into`, which
    /// is bounds-checked. EOF is signalled the standard way (a
    /// zero-length `read` return). Since 1.1.0 it also implements
    /// `std::io::BufRead` (zero-copy on read-only mappings); see
    /// [`MmapReader`].
    #[must_use]
    pub fn reader(&self) -> MmapReader<'_> {
        MmapReader {
            mmap: self,
            pos: 0,
            buf: [0; READER_BUF_LEN],
            buf_start: 0,
            buf_len: 0,
        }
    }

    /// Get a zero-copy mutable slice for the given [offset, offset+len).
    /// Available on `ReadWrite` and (private writes, since 1.1.0)
    /// `CopyOnWrite` mappings.
    ///
    /// The returned guard holds the mapping's write lock for its
    /// lifetime: every other reader and writer (on any region) waits
    /// until it is dropped. Calling this while the same thread holds a
    /// [`MappedSlice`], an iterator item, or an atomic view of this
    /// mapping deadlocks; [`try_as_slice_mut`](Self::try_as_slice_mut)
    /// returns `Ok(None)` instead.
    ///
    /// A zero-length request returns an empty slice at any offset (it
    /// still takes the write lock).
    ///
    /// # Errors
    ///
    /// Returns `MmapIoError::InvalidMode` on a `ReadOnly` mapping.
    /// Returns `MmapIoError::OutOfBounds` if range exceeds file bounds.
    pub fn as_slice_mut(&self, offset: u64, len: u64) -> Result<MappedSliceMut<'_>> {
        let lock = self.write_lock("mutable access on read-only mapping")?;
        let guard = lock.write();
        let (start, end) = if len == 0 {
            (0, 0)
        } else {
            slice_range(offset, len, guard.len() as u64)?
        };
        Ok(MappedSliceMut {
            guard,
            range: start..end,
            pending: self.pending_counter(),
        })
    }

    /// Copy the provided bytes into the mapped file at the given offset.
    /// Bounds-checked, zero-copy write. Empty `data` is accepted at any
    /// offset (and in any mode) and does nothing.
    ///
    /// Takes the mapping's write lock for the duration of the copy, so
    /// it waits for every live [`MappedSlice`], iterator item, and
    /// atomic view to be dropped, whatever region they cover. Calling
    /// it on a thread that holds one of those deadlocks;
    /// [`try_update_region`](Self::try_update_region) returns
    /// `Ok(false)` instead.
    ///
    /// # Performance
    ///
    /// - **Time Complexity**: O(n) where n is data.len()
    /// - **Memory Usage**: No additional allocation
    /// - **I/O Operations**: May trigger flush based on flush policy
    ///
    /// On a `CopyOnWrite` mapping (since 1.1.0) the bytes go to private
    /// pages: visible through this mapping, never written to the file,
    /// and not counted in [`pending_bytes`](Self::pending_bytes).
    ///
    /// # Errors
    ///
    /// Returns `MmapIoError::InvalidMode` on a `ReadOnly` mapping.
    /// Returns `MmapIoError::OutOfBounds` if range exceeds file bounds.
    pub fn update_region(&self, offset: u64, data: &[u8]) -> Result<()> {
        if data.is_empty() {
            return Ok(());
        }
        let lock = self.write_lock("Update region requires ReadWrite or CopyOnWrite mode.")?;
        let len = data.len() as u64;
        {
            let mut guard = lock.write();
            let (start, end) = slice_range(offset, len, guard.len() as u64)?;
            guard[start..end].copy_from_slice(data);
        }
        // Apply flush policy after releasing the write lock; flushing
        // only needs a read guard.
        self.apply_flush_policy(len)
    }

    /// Non-blocking [`as_slice`](Self::as_slice): returns `Ok(None)`
    /// instead of waiting when the mapping's lock is held for writing
    /// (a live [`MappedSliceMut`], a running `update_region`,
    /// `chunks_mut`, or `resize`). Since 1.1.0.
    ///
    /// Readers never block each other, so this only reports "would
    /// block" while a writer holds the lock; a thread that holds read
    /// views can always take another. `ReadOnly` mappings have no lock
    /// and always return `Some`. Otherwise it behaves exactly like
    /// `as_slice`: the range is validated against the mapping under the
    /// lock (it is not checked when `None` is returned), a zero-length
    /// request returns an empty slice at any offset, and a range that
    /// overlaps a live atomic view is refused.
    ///
    /// The call never waits for a lock held by user code. It may wait
    /// briefly for the crate's internal view-tracking locks, which are
    /// only held for a few instructions.
    ///
    /// # Errors
    ///
    /// Returns [`MmapIoError::OutOfBounds`] if `offset + len` exceeds
    /// the mapping length.
    /// Returns [`MmapIoError::InvalidMode`] if the range overlaps a
    /// live atomic view.
    ///
    /// # Example
    ///
    /// ```
    /// use mmap_io::MemoryMappedFile;
    ///
    /// let dir = tempfile::tempdir()?;
    /// let mmap = MemoryMappedFile::create_rw(dir.path().join("t.bin"), 64)?;
    /// mmap.update_region(0, b"hello")?;
    ///
    /// let writer = mmap.as_slice_mut(32, 8)?; // holds the write lock
    /// assert!(mmap.try_as_slice(0, 5)?.is_none()); // would block
    /// drop(writer);
    /// let view = mmap.try_as_slice(0, 5)?.expect("lock is free");
    /// assert_eq!(&*view, b"hello");
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn try_as_slice(&self, offset: u64, len: u64) -> Result<Option<MappedSlice<'_>>> {
        if len == 0 {
            return Ok(Some(MappedSlice::owned(&[])));
        }
        let map = match &self.inner.map {
            MapVariant::Ro(m) => MapRead::Shared(m),
            MapVariant::Rw(lock) | MapVariant::Cow(lock) => match lock.try_read_recursive() {
                Some(guard) => MapRead::Guarded(guard),
                None => return Ok(None),
            },
        };
        let (start, end) = slice_range(offset, len, map.len() as u64)?;
        map.into_view(&self.inner.views, start..end).map(Some)
    }

    /// Non-blocking [`as_slice_mut`](Self::as_slice_mut): returns
    /// `Ok(None)` instead of waiting when any view or writer holds the
    /// mapping's lock. Since 1.1.0.
    ///
    /// This is the way to write from a thread that may itself hold a
    /// [`MappedSlice`], an iterator item, or an atomic view of the
    /// mapping: `as_slice_mut` would deadlock there, this returns
    /// `Ok(None)`. Otherwise it behaves like `as_slice_mut`: the range
    /// is validated under the lock (not checked when `None` is
    /// returned), and a zero-length request returns an empty slice.
    ///
    /// # Errors
    ///
    /// Returns [`MmapIoError::InvalidMode`] on a `ReadOnly` mapping
    /// (checked before the lock, so also when it would block).
    /// Returns [`MmapIoError::OutOfBounds`] if the range exceeds the
    /// mapping length.
    ///
    /// # Example
    ///
    /// ```
    /// use mmap_io::MemoryMappedFile;
    ///
    /// let dir = tempfile::tempdir()?;
    /// let mmap = MemoryMappedFile::create_rw(dir.path().join("t.bin"), 64)?;
    /// let reader = mmap.as_slice(0, 8)?;
    /// // `as_slice_mut` would deadlock here: this thread holds a view.
    /// assert!(mmap.try_as_slice_mut(16, 8)?.is_none());
    /// drop(reader);
    /// let mut w = mmap.try_as_slice_mut(16, 8)?.expect("lock is free");
    /// w.copy_from_slice(b"12345678");
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn try_as_slice_mut(&self, offset: u64, len: u64) -> Result<Option<MappedSliceMut<'_>>> {
        let lock = self.write_lock("mutable access on read-only mapping")?;
        let Some(guard) = lock.try_write() else {
            return Ok(None);
        };
        let (start, end) = if len == 0 {
            (0, 0)
        } else {
            slice_range(offset, len, guard.len() as u64)?
        };
        Ok(Some(MappedSliceMut {
            guard,
            range: start..end,
            pending: self.pending_counter(),
        }))
    }

    /// Non-blocking [`update_region`](Self::update_region): returns
    /// `Ok(false)` instead of waiting when any view or writer holds the
    /// mapping's lock, and `Ok(true)` once the bytes are written. Since
    /// 1.1.0.
    ///
    /// The return value is a `bool` rather than `Option<()>` because
    /// the only information is "written or not". `Ok(false)` means
    /// nothing was written and nothing was validated; retry later or
    /// drop the views this thread holds. Use it where `update_region`
    /// could deadlock: on a thread that may hold a [`MappedSlice`], an
    /// iterator item, or an atomic view of the mapping.
    ///
    /// Otherwise it behaves like `update_region`: empty `data` returns
    /// `Ok(true)` at any offset, the range is validated under the lock,
    /// the write is counted in [`pending_bytes`](Self::pending_bytes),
    /// and the [`FlushPolicy`] runs. A policy flush happens under the
    /// same lock, downgraded to a read guard, so the call never waits
    /// for another lock holder; it does wait for the disk when the
    /// policy flushes.
    ///
    /// # Errors
    ///
    /// Returns [`MmapIoError::InvalidMode`] on a `ReadOnly` mapping
    /// (checked before the lock).
    /// Returns [`MmapIoError::OutOfBounds`] if the range exceeds the
    /// mapping length.
    /// Returns [`MmapIoError::FlushFailed`] if a policy flush fails
    /// (the bytes are written in that case).
    ///
    /// # Example
    ///
    /// ```
    /// use mmap_io::MemoryMappedFile;
    ///
    /// let dir = tempfile::tempdir()?;
    /// let mmap = MemoryMappedFile::create_rw(dir.path().join("t.bin"), 64)?;
    /// let view = mmap.as_slice(0, 4)?;
    /// // `update_region` would deadlock on this thread.
    /// assert!(!mmap.try_update_region(8, b"data")?);
    /// drop(view);
    /// assert!(mmap.try_update_region(8, b"data")?);
    /// assert_eq!(&*mmap.as_slice(8, 4)?, b"data");
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn try_update_region(&self, offset: u64, data: &[u8]) -> Result<bool> {
        if data.is_empty() {
            return Ok(true);
        }
        let lock = self.write_lock("Update region requires ReadWrite or CopyOnWrite mode.")?;
        let Some(mut guard) = lock.try_write() else {
            return Ok(false);
        };
        let len = data.len() as u64;
        let (start, end) = slice_range(offset, len, guard.len() as u64)?;
        guard[start..end].copy_from_slice(data);
        if self.count_update(len) {
            // Flush under the lock we already hold, downgraded so
            // readers can proceed; never wait for another holder.
            let guard = RwLockWriteGuard::downgrade(guard);
            self.flush_mapped(&guard)?;
        }
        Ok(true)
    }

    /// Async write that enforces Async-Only Flushing semantics: always flush after write.
    ///
    /// Runs the underlying sync write + flush on the `blocking`
    /// crate's thread pool so the async scheduler is not stuck on
    /// disk I/O. `blocking` works on every async executor (tokio,
    /// smol, async-std), so callers are no longer locked into
    /// tokio (since 0.9.11).
    ///
    /// # Allocation
    ///
    /// `data` is copied into a heap-allocated `Vec<u8>` (one
    /// allocation of `data.len()` bytes) because the blocking task
    /// must own its input; the caller's slice cannot be borrowed
    /// across the thread hand-off. For large or frequent writes from
    /// async code, prefer calling [`update_region`](Self::update_region)
    /// plus [`flush_async`](Self::flush_async).
    ///
    /// # Errors
    ///
    /// Propagates any error from the synchronous [`update_region`](Self::update_region)
    /// call (out-of-bounds, mode mismatch, I/O failure).
    #[cfg(feature = "async")]
    pub async fn update_region_async(&self, offset: u64, data: &[u8]) -> Result<()> {
        let this = self.clone();
        let data_vec = data.to_vec();
        blocking::unblock(move || {
            this.update_region(offset, &data_vec)?;
            // Async-only flushing: unconditionally flush after write
            // when using the async path so post-await visibility is
            // consistent across platforms.
            this.flush()
        })
        .await
    }

    /// Flush changes to disk and wait for the OS to report them
    /// written. For read-only and copy-on-write mappings this is a
    /// no-op that returns `Ok`: copy-on-write changes live in private
    /// pages and are never written to the file.
    ///
    /// On a `ReadWrite` mapping every call flushes, whatever
    /// [`pending_bytes`](Self::pending_bytes) reports; the counter only
    /// drives the [`FlushPolicy`] automatic flushes. The flush is
    /// synchronous:
    ///
    /// - **Linux / Unix**: `msync(MS_SYNC)` over the whole mapping.
    ///   On Linux this writes the dirty pages and the metadata needed
    ///   to read them back (`fdatasync` semantics). On macOS `msync`
    ///   does not issue `F_FULLFSYNC`, so the drive's own write cache
    ///   may still hold the data.
    /// - **Windows**: `FlushViewOfFile` followed by `FlushFileBuffers`
    ///   on the file handle, which waits for the data to reach the
    ///   device.
    ///
    /// The mapping's read lock is held for the duration, so writers
    /// wait for the flush to finish.
    ///
    /// # Performance
    ///
    /// - **Time Complexity**: O(n) where n is the size of dirty pages
    /// - **I/O Operations**: Synchronous write-back of modified pages;
    ///   expect milliseconds, not nanoseconds (see `docs/PERFORMANCE.md`)
    ///
    /// # Errors
    ///
    /// Returns `MmapIoError::FlushFailed` if flush operation fails.
    pub fn flush(&self) -> Result<()> {
        match &self.inner.map {
            // Copy-on-write pages are private: there is nothing to
            // write back, by design.
            MapVariant::Ro(_) | MapVariant::Cow(_) => Ok(()),
            MapVariant::Rw(lock) => self.flush_mapped(&lock.read_recursive()),
        }
    }

    /// Durably flush `map`, the RW mapping the caller holds a guard
    /// on, and reset the pending counters.
    fn flush_mapped(&self, map: &RawMmapMut) -> Result<()> {
        // Take the counters before flushing: anything recorded after
        // this point (e.g. an atomic view dropped during the flush)
        // stays pending for the next flush.
        let bytes = self
            .inner
            .written_since_last_flush
            .swap(0, Ordering::AcqRel);
        let writes = self.inner.writes_since_last_flush.swap(0, Ordering::AcqRel);
        if let Err(e) = map.flush() {
            self.inner
                .written_since_last_flush
                .fetch_add(bytes, Ordering::AcqRel);
            self.inner
                .writes_since_last_flush
                .fetch_add(writes, Ordering::AcqRel);
            return Err(MmapIoError::FlushFailed(e.to_string()));
        }
        Ok(())
    }

    /// Async flush changes to disk. For read-only or COW mappings, this is a no-op.
    /// This method enforces "async-only flushing" semantics for async paths.
    /// Runtime-agnostic since 0.9.11 (uses `blocking::unblock`).
    ///
    /// # Errors
    ///
    /// Propagates any error from the synchronous [`flush`](Self::flush) call.
    #[cfg(feature = "async")]
    pub async fn flush_async(&self) -> Result<()> {
        let this = self.clone();
        blocking::unblock(move || this.flush()).await
    }

    /// Async flush a specific byte range to disk. Runtime-agnostic
    /// since 0.9.11.
    ///
    /// # Errors
    ///
    /// Propagates any error from the synchronous [`flush_range`](Self::flush_range)
    /// call (out-of-bounds or I/O failure).
    #[cfg(feature = "async")]
    pub async fn flush_range_async(&self, offset: u64, len: u64) -> Result<()> {
        let this = self.clone();
        blocking::unblock(move || this.flush_range(offset, len)).await
    }

    /// Flush a specific byte range to disk and wait for the OS to
    /// report it written. Same durability and platform behavior as
    /// [`flush`](Self::flush); the kernel works in whole pages, so the
    /// range is widened to page boundaries internally.
    ///
    /// A range that covers the whole mapping resets
    /// [`pending_bytes`](Self::pending_bytes) like `flush()`. A partial
    /// range leaves the counter unchanged: the crate does not track
    /// which bytes are dirty, so it cannot know how many pending bytes
    /// the range covered. On Windows, `FlushFileBuffers` flushes the
    /// whole file's buffers regardless of the range.
    ///
    /// A zero-length range is accepted at any offset and does nothing.
    /// On read-only and copy-on-write mappings the range is validated
    /// and nothing else happens.
    ///
    /// # Errors
    ///
    /// Returns `MmapIoError::OutOfBounds` if range exceeds file bounds.
    /// Returns `MmapIoError::FlushFailed` if flush operation fails.
    pub fn flush_range(&self, offset: u64, len: u64) -> Result<()> {
        if len == 0 {
            return Ok(());
        }
        match &self.inner.map {
            MapVariant::Ro(_) | MapVariant::Cow(_) => {
                // Nothing to write back (read-only, or private
                // copy-on-write pages), but the range is still
                // validated so callers get the same contract everywhere.
                let map = self.map_read();
                slice_range(offset, len, map.len() as u64)?;
                Ok(())
            }
            MapVariant::Rw(lock) => {
                let guard = lock.read_recursive();
                let (start, end) = slice_range(offset, len, guard.len() as u64)?;
                if start == 0 && end == guard.len() {
                    drop(guard);
                    return self.flush();
                }
                guard
                    .flush_range(start, end - start)
                    .map_err(|e| MmapIoError::FlushFailed(e.to_string()))
            }
        }
    }

    /// Start writing the whole mapping back to the file **without
    /// waiting** for the write to finish. Not durable: when this
    /// returns, the data may still be only in memory, and a crash or
    /// power loss can lose it. Call [`flush`](Self::flush) when the data
    /// must survive a crash. Since 1.1.0.
    ///
    /// Use it to get write-back started early (for example after a
    /// batch of writes, before a later `flush()`), so the durable flush
    /// has less left to do, or to keep dirty memory from piling up
    /// without paying for a synchronous flush. Equivalent to
    /// [`schedule_flush_range`](Self::schedule_flush_range) over the
    /// whole mapping; see there for the platform behavior.
    ///
    /// [`pending_bytes`](Self::pending_bytes) is not reset: nothing is
    /// known to be durable afterwards. `ReadOnly` and `CopyOnWrite`
    /// mappings have nothing to write back; the call is an `Ok` no-op.
    ///
    /// # Errors
    ///
    /// Returns [`MmapIoError::FlushFailed`] if the OS rejects the
    /// request.
    ///
    /// # Example
    ///
    /// ```
    /// use mmap_io::MemoryMappedFile;
    ///
    /// let dir = tempfile::tempdir()?;
    /// let mmap = MemoryMappedFile::create_rw(dir.path().join("log.bin"), 1 << 20)?;
    /// mmap.update_region(0, b"batch of records")?;
    /// mmap.schedule_flush()?; // write-back started, not durable yet
    /// // ... more work ...
    /// mmap.flush()?; // durable
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn schedule_flush(&self) -> Result<()> {
        match &self.inner.map {
            MapVariant::Ro(_) | MapVariant::Cow(_) => Ok(()),
            MapVariant::Rw(lock) => {
                let guard = lock.read_recursive();
                let len = guard.len();
                self.schedule_mapped(&guard, 0, len)
            }
        }
    }

    /// Start writing `[offset, offset + len)` back to the file
    /// **without waiting** for the write to finish. Not durable; see
    /// [`schedule_flush`](Self::schedule_flush). Since 1.1.0.
    ///
    /// The range is validated like [`flush_range`](Self::flush_range):
    /// against the mapping under its read guard, a zero-length range is
    /// accepted at any offset and does nothing, and on `ReadOnly` /
    /// `CopyOnWrite` mappings the range is validated and nothing else
    /// happens. [`pending_bytes`](Self::pending_bytes) is not changed.
    ///
    /// # Platform behavior
    ///
    /// - **Linux**: `sync_file_range(SYNC_FILE_RANGE_WRITE)` on the
    ///   backing file. This queues the dirty pages for write-out right
    ///   away; it does not wait, does not write metadata, and does not
    ///   flush the device cache. (Linux treats `msync(MS_ASYNC)` as a
    ///   no-op, which is why it is not used.)
    /// - **macOS and other Unix**: `msync(MS_ASYNC)` over the page
    ///   range, which schedules write-back and returns.
    /// - **Windows**: `FlushViewOfFile` without `FlushFileBuffers`: the
    ///   pages are handed to the file system cache, and the call does
    ///   not wait for the disk.
    ///
    /// The kernel works in whole pages, so neighbouring bytes on the
    /// same pages may be written as well.
    ///
    /// # Errors
    ///
    /// Returns [`MmapIoError::OutOfBounds`] if the range exceeds the
    /// mapping length.
    /// Returns [`MmapIoError::FlushFailed`] if the OS rejects the
    /// request.
    ///
    /// # Example
    ///
    /// ```
    /// use mmap_io::MemoryMappedFile;
    ///
    /// let dir = tempfile::tempdir()?;
    /// let mmap = MemoryMappedFile::create_rw(dir.path().join("a.bin"), 64 * 1024)?;
    /// mmap.update_region(4096, b"segment")?;
    /// mmap.schedule_flush_range(4096, 7)?;
    /// assert!(mmap.schedule_flush_range(64 * 1024, 1).is_err());
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn schedule_flush_range(&self, offset: u64, len: u64) -> Result<()> {
        if len == 0 {
            return Ok(());
        }
        match &self.inner.map {
            MapVariant::Ro(_) | MapVariant::Cow(_) => {
                let map = self.map_read();
                slice_range(offset, len, map.len() as u64)?;
                Ok(())
            }
            MapVariant::Rw(lock) => {
                let guard = lock.read_recursive();
                let (start, end) = slice_range(offset, len, guard.len() as u64)?;
                self.schedule_mapped(&guard, start, end - start)
            }
        }
    }

    /// Start non-durable write-back of `[start, start + count)` of the
    /// RW mapping `map`, which the caller holds a guard on and has
    /// validated the range against.
    fn schedule_mapped(&self, map: &RawMmapMut, start: usize, count: usize) -> Result<()> {
        if count == 0 {
            return Ok(());
        }
        #[cfg(target_os = "linux")]
        let result = {
            // Managed mappings start at file offset 0, so mapping
            // offsets are file offsets. `map` is unused: the request
            // goes to the file, whose page cache the mapping shares.
            let _ = map;
            crate::raw::start_writeback(&self.inner.file, start as u64, count as u64)
        };
        #[cfg(not(target_os = "linux"))]
        let result = map.flush_async_range(start, count);
        result.map_err(|e| MmapIoError::FlushFailed(format!("schedule_flush: {e}")))
    }

    /// Resize (grow or shrink) the mapped file (RW only). This remaps the file internally.
    ///
    /// The whole operation runs under the mapping's write lock, so it
    /// waits for every live [`MappedSlice`], iterator item, atomic
    /// view, and [`MappedSliceMut`] to be dropped before it touches the
    /// file. Calling `resize` on a thread that holds one of those
    /// deadlocks.
    ///
    /// Shrinking truncates the backing file on every platform; bytes
    /// past `new_size` are discarded, and growing again later exposes
    /// zeros there.
    ///
    /// # Platform notes
    ///
    /// - **Windows** refuses to truncate a file while any view of it is
    ///   mapped. `resize` unmaps its own view first, but a second,
    ///   independent mapping of the same file (another
    ///   `MemoryMappedFile` opened on the same path, in this or another
    ///   process) makes a shrink fail with `MmapIoError::Io`. The
    ///   mapping is restored at its old size in that case.
    /// - If remapping fails after the file was already truncated
    ///   (Windows only, e.g. out of address space), the mapping is left
    ///   empty: `len()` reports 0 and every access returns
    ///   `OutOfBounds` until a later `resize` succeeds.
    ///
    /// # Performance
    ///
    /// - **Time Complexity**: O(1) for the remap operation
    /// - **Memory Usage**: Allocates new virtual address space of `new_size`
    /// - **I/O Operations**: File truncate/extend + new mmap syscall
    /// - **Note**: Raw pointers from `as_ptr`/`as_mut_ptr` become invalid after resize
    ///
    /// # Errors
    ///
    /// Returns `MmapIoError::InvalidMode` if not in `ReadWrite` mode.
    /// Returns `MmapIoError::ResizeFailed` if new size is zero or exceeds the maximum safe limit.
    /// Returns `MmapIoError::Io` if resize operation fails.
    pub fn resize(&self, new_size: u64) -> Result<()> {
        if self.inner.mode != MmapMode::ReadWrite {
            return Err(MmapIoError::InvalidMode("Resize requires ReadWrite mode"));
        }
        if new_size == 0 {
            return Err(MmapIoError::ResizeFailed(
                "New size must be greater than zero".into(),
            ));
        }
        if new_size > MAX_MMAP_SIZE {
            return Err(MmapIoError::ResizeFailed(format!(
                "New size {new_size} exceeds maximum safe limit of {MAX_MMAP_SIZE} bytes"
            )));
        }
        let new_len = usize::try_from(new_size).map_err(|_| {
            MmapIoError::ResizeFailed(format!("New size {new_size} does not fit in usize"))
        })?;
        let MapVariant::Rw(lock) = &self.inner.map else {
            return Err(MmapIoError::InvalidMode("Resize requires ReadWrite mode"));
        };

        // Take the write lock BEFORE touching the file: a live view of
        // the tail must never observe a truncated file (SIGBUS on
        // Unix), and the cached length must change atomically with
        // the mapping.
        let mut guard = lock.write();
        let current_len = guard.len();
        let result = match new_len.cmp(&current_len) {
            std::cmp::Ordering::Equal => Ok(()),
            std::cmp::Ordering::Greater => {
                self.grow_locked(&mut guard, current_len as u64, new_size, new_len)
            }
            std::cmp::Ordering::Less => {
                self.shrink_locked(&mut guard, current_len, new_size, new_len)
            }
        };
        self.inner
            .cached_len
            .store(guard.len() as u64, Ordering::Release);
        result
    }

    /// Whether the builder asked for huge pages on this mapping.
    fn huge_pages(&self) -> bool {
        #[cfg(feature = "hugepages")]
        {
            self.inner.huge_pages
        }
        #[cfg(not(feature = "hugepages"))]
        {
            false
        }
    }

    /// Grow the file and remap. Caller holds the write lock.
    fn grow_locked(
        &self,
        map: &mut RawMmapMut,
        current_size: u64,
        new_size: u64,
        new_len: usize,
    ) -> Result<()> {
        // Extend first: mapping past end-of-file is invalid. The old
        // mapping stays valid because it only covers the old length.
        self.inner.file.set_len(new_size)?;
        match map_file_rw(&self.inner.file, new_len, self.huge_pages()) {
            Ok(new_map) => {
                // Old mapping is dropped (unmapped) here, after the new
                // one exists, while no other guard can be alive.
                *map = new_map;
                Ok(())
            }
            Err(e) => {
                // Best effort: put the file back to the size the live
                // mapping covers.
                let _ = self.inner.file.set_len(current_size);
                Err(e)
            }
        }
    }

    /// Truncate the file and remap. Caller holds the write lock.
    #[cfg(not(windows))]
    fn shrink_locked(
        &self,
        map: &mut RawMmapMut,
        _current_len: usize,
        new_size: u64,
        new_len: usize,
    ) -> Result<()> {
        // Map the surviving prefix first so a mapping failure leaves
        // the file and the old mapping untouched. Then truncate; the
        // old mapping's tail now lies past end-of-file, but nothing
        // can touch it: we hold the write lock and replace it next.
        let new_map = map_file_rw(&self.inner.file, new_len, self.huge_pages())?;
        self.inner.file.set_len(new_size)?;
        *map = new_map;
        Ok(())
    }

    /// Truncate the file and remap. Caller holds the write lock.
    #[cfg(windows)]
    fn shrink_locked(
        &self,
        map: &mut RawMmapMut,
        current_len: usize,
        new_size: u64,
        new_len: usize,
    ) -> Result<()> {
        // Windows rejects SetEndOfFile on a file with a mapped view
        // (ERROR_USER_MAPPED_FILE, os error 1224), so unmap our view
        // first by swapping in an empty anonymous placeholder.
        drop(std::mem::replace(map, RawMmapMut::map_anon(0)?));
        if let Err(e) = self.inner.file.set_len(new_size) {
            // File unchanged: restore the mapping at its old length.
            *map = map_file_rw(&self.inner.file, current_len, self.huge_pages())?;
            return Err(e.into());
        }
        *map = map_file_rw(&self.inner.file, new_len, self.huge_pages())?;
        Ok(())
    }

    /// Path to the underlying file.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.inner.path
    }

    /// Touch (prewarm) pages by reading the first byte of each page.
    /// This forces the OS to load all pages into physical memory, eliminating
    /// page faults during subsequent access. Useful for benchmarking and
    /// performance-critical sections.
    ///
    /// # Performance
    ///
    /// - **Time Complexity**: O(n) where n is the number of pages
    /// - **Memory Usage**: Forces all pages into physical memory
    /// - **I/O Operations**: May trigger disk reads for unmapped pages
    /// - **Cache Behavior**: Optimizes subsequent access patterns
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use mmap_io::MemoryMappedFile;
    ///
    /// let mmap = MemoryMappedFile::open_ro("data.bin")?;
    ///
    /// // Prewarm all pages before performance-critical section
    /// mmap.touch_pages()?;
    ///
    /// // Now all subsequent accesses will be fast (no page faults)
    /// let data = mmap.as_slice(0, 1024)?;
    /// # Ok::<(), mmap_io::MmapIoError>(())
    /// ```
    ///
    /// # Errors
    ///
    /// Never returns an error today; the `Result` is kept so a future
    /// platform-specific prefault call can report failure without an
    /// API change.
    pub fn touch_pages(&self) -> Result<()> {
        // Hold the read guard (RW) for the whole walk so the mapping
        // cannot be remapped underneath it. We never form a Rust
        // reference to the mapped memory, only one volatile byte read
        // per page.
        let map = self.map_read();
        self.touch_locked(&map, 0, map.len(), crate::utils::page_size());
        Ok(())
    }

    /// Touch (prewarm) a specific range of pages.
    /// Similar to `touch_pages()` but only affects the specified range.
    ///
    /// # Arguments
    ///
    /// * `offset` - Starting offset in bytes
    /// * `len` - Length of range to touch in bytes
    ///
    /// # Errors
    ///
    /// Returns `MmapIoError::OutOfBounds` if range exceeds file bounds.
    pub fn touch_pages_range(&self, offset: u64, len: u64) -> Result<()> {
        if len == 0 {
            return Ok(());
        }
        let map = self.map_read();
        let (start, end) = slice_range(offset, len, map.len() as u64)?;
        // Walk every page that intersects [start, end). `end` is within
        // the mapping, so the page holding byte `end - 1` is mapped.
        let page_sz = crate::utils::page_size();
        let first_page = start - start % page_sz;
        self.touch_locked(&map, first_page, end - first_page, page_sz);
        Ok(())
    }

    /// Touch `[start, start + len)` of the mapping `map` gives access
    /// to; the range must be inside it.
    fn touch_locked(&self, map: &MapRead<'_>, start: usize, len: usize, page_sz: usize) {
        match map {
            MapRead::Shared(_) => touch_range_with_ptr(map.base_ptr(), start, len, page_sz),
            // SAFETY: the range is inside the mapping (caller contract)
            // and `map` holds its read guard for the call, which is
            // `ViewRegistry::touch`'s contract. Bytes under live atomic
            // views are touched with atomic loads instead of plain
            // volatile reads.
            MapRead::Guarded(_) => unsafe {
                self.inner.views.touch(map.base_ptr(), start, len, page_sz)
            },
        }
    }
}

// 0.9.8 ergonomic and introspection surface.
//
// These methods are additive (no breaking changes) and were tracked
// under audit IDs E1, E2, E6, E7, F2, F5, F9.
impl MemoryMappedFile {
    /// Open `path` for read-write, creating it at `default_size` if
    /// it does not exist.
    ///
    /// The file is never truncated. If it already exists and is not
    /// empty, `default_size` is **ignored** and the file is opened at
    /// its current length; use [`resize`](Self::resize) afterward if
    /// you need to change the size. An existing zero-length file is
    /// extended to `default_size`. Creation is exclusive, so a file
    /// created concurrently by another process is opened rather than
    /// overwritten.
    ///
    /// # Sparse file behavior
    ///
    /// On the create path, the file is allocated via `set_len` which
    /// produces a sparse file on every supported platform. Disk blocks
    /// are consumed lazily as pages are written; allocating a large
    /// `default_size` for a structure that will fill incrementally is
    /// safe and cheap. See [`create_rw`](Self::create_rw) for details.
    ///
    /// # Errors
    ///
    /// Returns [`MmapIoError::Io`] if the filesystem rejects the
    /// create or open call.
    /// Returns [`MmapIoError::ResizeFailed`] if `default_size` is zero
    /// or exceeds the maximum safe size (only checked when the file is
    /// created or extended; non-empty existing files of any size are
    /// accepted).
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use mmap_io::MemoryMappedFile;
    ///
    /// // Either opens "data.bin" or creates it at 1 MiB if absent.
    /// let mmap = MemoryMappedFile::open_or_create("data.bin", 1024 * 1024)?;
    /// # Ok::<(), mmap_io::MmapIoError>(())
    /// ```
    pub fn open_or_create<P: AsRef<Path>>(path: P, default_size: u64) -> Result<Self> {
        Self::builder(path)
            .mode(MmapMode::ReadWrite)
            .size(default_size)
            .open_or_create()
    }

    /// Construct a `MemoryMappedFile` from a pre-opened `File`. The
    /// `File` must have permissions matching `mode` (read for any
    /// mode, write for `ReadWrite`).
    ///
    /// This is the escape hatch for callers that have already opened
    /// the file via their own `OpenOptions` (e.g. with `O_DIRECT`,
    /// `O_NOATIME`, or a custom security context) and want to mmap
    /// it without re-opening.
    ///
    /// # Errors
    ///
    /// Returns [`MmapIoError::ResizeFailed`] if the file is
    /// zero-length on the `ReadWrite` or `CopyOnWrite` paths
    /// (`mmap(2)` rejects zero-length mappings on Linux).
    /// Returns [`MmapIoError::Io`] if `metadata()` or the mapping
    /// call fails. The `path` argument is informational only; it
    /// is used by [`path`](Self::path) and error messages.
    pub fn from_file<P: AsRef<Path>>(file: File, mode: MmapMode, path: P) -> Result<Self> {
        let path_ref = path.as_ref().to_path_buf();
        let len = file.metadata()?.len();
        match mode {
            MmapMode::ReadOnly => {
                // SAFETY: see `open_ro` for the full justification.
                let mmap = unsafe { RawMmap::map(&file)? };
                let inner = Inner {
                    path: path_ref,
                    file,
                    mode,
                    cached_len: AtomicU64::new(len),
                    map: MapVariant::Ro(mmap),
                    flush_policy: FlushPolicy::Never,
                    written_since_last_flush: AtomicU64::new(0),
                    writes_since_last_flush: AtomicU64::new(0),
                    flusher: RwLock::new(None),
                    views: ViewRegistry::new(),
                    #[cfg(feature = "hugepages")]
                    huge_pages: false,
                };
                Ok(Self {
                    inner: Arc::new(inner),
                })
            }
            MmapMode::ReadWrite => {
                if len == 0 {
                    return Err(MmapIoError::ResizeFailed(ERR_ZERO_LENGTH_FILE.into()));
                }
                // SAFETY: see `open_rw`.
                let mmap = unsafe { RawMmapMut::map_mut(&file)? };
                let inner = Inner {
                    path: path_ref,
                    file,
                    mode,
                    cached_len: AtomicU64::new(len),
                    map: MapVariant::Rw(RwLock::new(mmap)),
                    flush_policy: FlushPolicy::default(),
                    written_since_last_flush: AtomicU64::new(0),
                    writes_since_last_flush: AtomicU64::new(0),
                    flusher: RwLock::new(None),
                    views: ViewRegistry::new(),
                    #[cfg(feature = "hugepages")]
                    huge_pages: false,
                };
                Ok(Self {
                    inner: Arc::new(inner),
                })
            }
            #[cfg(feature = "cow")]
            MmapMode::CopyOnWrite => {
                if len == 0 {
                    return Err(MmapIoError::ResizeFailed(ERR_ZERO_LENGTH_FILE.into()));
                }
                let mmap = map_file_cow(&file, len)?;
                let inner = Inner {
                    path: path_ref,
                    file,
                    mode,
                    cached_len: AtomicU64::new(len),
                    map: MapVariant::Cow(RwLock::new(mmap)),
                    flush_policy: FlushPolicy::Never,
                    written_since_last_flush: AtomicU64::new(0),
                    writes_since_last_flush: AtomicU64::new(0),
                    flusher: RwLock::new(None),
                    views: ViewRegistry::new(),
                    #[cfg(feature = "hugepages")]
                    huge_pages: false,
                };
                Ok(Self {
                    inner: Arc::new(inner),
                })
            }
            #[cfg(not(feature = "cow"))]
            MmapMode::CopyOnWrite => Err(MmapIoError::InvalidMode(
                "CopyOnWrite mode requires 'cow' feature",
            )),
        }
    }

    /// Consume this mapping and return the underlying [`File`]. The
    /// mapping is dropped (memory unmapped, background flusher
    /// stopped) before the file is returned, so the caller can
    /// safely perform file-level operations (truncate, sync_all,
    /// rename, etc.) on the returned handle.
    ///
    /// Returns the mapping unchanged (wrapped in `Err`) if other
    /// [`MemoryMappedFile`] clones of this mapping exist; the
    /// underlying [`File`] is shared and cannot be extracted while
    /// other handles hold references. Drop the other clones first.
    ///
    /// # Errors
    ///
    /// Returns `Err(self)` if other clones of this `MemoryMappedFile`
    /// are alive when this call runs.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use mmap_io::MemoryMappedFile;
    /// use std::io::Write;
    ///
    /// let mmap = MemoryMappedFile::create_rw("data.bin", 1024)?;
    /// mmap.update_region(0, b"hello")?;
    /// mmap.flush()?;
    ///
    /// // Drop the mapping and reclaim the File.
    /// let mut file = mmap.unmap().expect("no other clones alive");
    /// file.write_all(b"more bytes via plain File")?;
    /// # Ok::<(), mmap_io::MmapIoError>(())
    /// ```
    pub fn unmap(self) -> std::result::Result<File, Self> {
        match Arc::try_unwrap(self.inner) {
            Ok(inner) => {
                // Destructure so we can control drop order: stop the
                // background flusher first (it may hold a Weak ref
                // back into Inner via Arc::downgrade), then drop the
                // mapping (releases address space; on Windows this
                // must happen BEFORE the file handle is closed), then
                // hand back the file by value.
                let Inner {
                    file, map, flusher, ..
                } = inner;
                drop(flusher);
                drop(map);
                Ok(file)
            }
            Err(arc) => Err(Self { inner: arc }),
        }
    }

    /// Return the [`FlushPolicy`] this mapping was constructed with.
    ///
    /// `FlushPolicy::Manual` (or the alias `Never`) is the default
    /// when not set via the builder.
    #[inline]
    #[must_use]
    pub fn flush_policy(&self) -> FlushPolicy {
        self.inner.flush_policy
    }

    /// Bytes written since the last successful full flush, under
    /// every flush policy. Mainly useful for diagnostics and for
    /// seeing how close [`FlushPolicy::EveryBytes`] is to its next
    /// automatic flush.
    ///
    /// Counted write paths: [`update_region`](Self::update_region)
    /// (at the write), [`MappedSliceMut`] from
    /// [`as_slice_mut`](Self::as_slice_mut) or `SegmentMut` (its full
    /// length, when it is dropped), `chunks_mut` (bytes handed to the
    /// closure), atomic views (their byte size, when dropped; stores
    /// cannot be observed individually), and
    /// [`as_mut_ptr`](Self::as_mut_ptr) (the whole mapping length,
    /// since writes through the pointer are invisible to the crate).
    /// The count is reset by [`flush`](Self::flush) and by a
    /// [`flush_range`](Self::flush_range) that covers the whole
    /// mapping. It is a policy heuristic, not a dirty-page tracker.
    ///
    /// One atomic load; no I/O is performed.
    #[inline]
    #[must_use]
    pub fn pending_bytes(&self) -> u64 {
        self.inner.written_since_last_flush.load(Ordering::Acquire)
    }

    /// Raw read-only pointer to the start of the mapped region.
    ///
    /// Useful for handing the mapping to a C library expecting a
    /// `const void *` plus a length. Combine with
    /// [`len()`](Self::len) to express the full region. The caller
    /// is responsible for not dereferencing past `len()`, not
    /// retaining the pointer past a [`resize`](Self::resize) call,
    /// and not holding the pointer across an `unmap`. The pointer
    /// stays valid for as long as `&self` is alive and no
    /// `resize()` has been called.
    ///
    /// On RW mappings the pointer aliases with the same memory that
    /// [`as_slice`](Self::as_slice) and
    /// [`as_slice_mut`](Self::as_slice_mut) lend out; honour Rust
    /// aliasing rules at the FFI boundary.
    ///
    /// # Safety
    ///
    /// The caller MUST:
    /// - Not dereference past `self.len()` bytes from the returned
    ///   pointer.
    /// - Not hold the pointer across calls to
    ///   [`resize`](Self::resize), which can move the mapping to a
    ///   different virtual address.
    /// - Honour Rust aliasing rules: do not form a `&mut` reference
    ///   to the same bytes while a Rust `&` (e.g. an active
    ///   [`MappedSlice`]) exists.
    #[must_use]
    pub unsafe fn as_ptr(&self) -> *const u8 {
        match &self.inner.map {
            MapVariant::Ro(m) => m.as_ptr(),
            MapVariant::Rw(lock) | MapVariant::Cow(lock) => lock.read_recursive().as_ptr(),
        }
    }

    /// Raw mutable pointer to the start of the mapped region.
    /// Available on `ReadWrite` mappings and, since 1.1.0, on
    /// `CopyOnWrite` mappings (writes through it stay private).
    ///
    /// See [`as_ptr`](Self::as_ptr) for the safety contract; the
    /// same rules apply, plus the caller MUST NOT alias this
    /// pointer with any live Rust `&` reference to the same bytes
    /// (a [`MappedSlice`] would alias).
    ///
    /// # Errors
    ///
    /// Returns [`MmapIoError::InvalidMode`] on a `ReadOnly` mapping.
    ///
    /// # Safety
    ///
    /// Same as [`as_ptr`](Self::as_ptr), plus the no-aliasing rule
    /// above.
    pub unsafe fn as_mut_ptr(&self) -> Result<*mut u8> {
        // A read guard is enough to read the base address and does not
        // deadlock when this thread already holds a view. The pointer
        // comes from the raw mapping's base pointer (not from a
        // `&[u8]`), so writing through it is permitted.
        let lock = self.write_lock("as_mut_ptr requires a writable mapping")?;
        let guard = lock.read_recursive();
        // Writes through the pointer are invisible to us; count the
        // whole mapping as pending so EveryMillis/EveryBytes still see
        // a dirty mapping (no-op for copy-on-write).
        self.record_write(guard.len() as u64);
        Ok(guard.as_ptr().cast_mut())
    }

    /// Hint the kernel that the given `[offset, offset + len)` range
    /// of the **backing file** will be read soon. On Linux this
    /// issues `posix_fadvise(POSIX_FADV_WILLNEED)` against the file
    /// descriptor, which prompts the page cache to start
    /// readahead. On platforms that do not expose an equivalent
    /// syscall this is a no-op that returns `Ok(())`.
    ///
    /// This is distinct from [`advise`](Self::advise) (with
    /// `MmapAdvice::WillNeed`), which operates on the **mapped
    /// virtual memory range** via `madvise`. They are
    /// complementary: `prefetch_range` warms the page cache from
    /// the file side, `advise` from the VM side. Issuing both is
    /// occasionally useful for cold reads of huge files.
    ///
    /// # Errors
    ///
    /// Returns [`MmapIoError::OutOfBounds`] if the range exceeds the
    /// mapping's current length.
    /// Returns [`MmapIoError::AdviceFailed`] if the underlying
    /// syscall reports an error.
    #[cfg(any(target_os = "linux", target_os = "android"))]
    pub fn prefetch_range(&self, offset: u64, len: u64) -> Result<()> {
        use std::os::fd::AsRawFd;
        if len == 0 {
            return Ok(());
        }
        crate::utils::ensure_in_bounds(offset, len, self.current_len()?)?;
        let fd = self.inner.file.as_raw_fd();
        // SAFETY: `posix_fadvise64` is a documented syscall that
        // takes an fd, offset, length, and advice flag. The fd is
        // owned by `self.inner.file` and remains valid for the
        // duration of the call. The kernel reads the file backing
        // the fd; no Rust references are formed. Off-by-one on
        // `offset + len > file size` is harmless on Linux (the
        // kernel silently clamps), but we still bounds-check above
        // so the documented contract holds.
        // Reference: https://man7.org/linux/man-pages/man2/posix_fadvise.2.html
        let ret = unsafe {
            libc::posix_fadvise(
                fd,
                offset as libc::off_t,
                len as libc::off_t,
                libc::POSIX_FADV_WILLNEED,
            )
        };
        if ret == 0 {
            Ok(())
        } else {
            Err(MmapIoError::AdviceFailed(format!(
                "posix_fadvise(WILLNEED) failed with errno {ret}"
            )))
        }
    }

    /// No-op fallback on non-Linux platforms. See the Linux variant
    /// for the contract.
    ///
    /// # Errors
    ///
    /// Returns [`MmapIoError::OutOfBounds`] if `[offset, offset + len)`
    /// exceeds the current mapping length. Otherwise always `Ok(())`.
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    pub fn prefetch_range(&self, offset: u64, len: u64) -> Result<()> {
        if len == 0 {
            return Ok(());
        }
        crate::utils::ensure_in_bounds(offset, len, self.current_len()?)?;
        Ok(())
    }
}

/// Walk a mapped region with stride `page_sz`, performing one volatile
/// byte read per page to force the OS to fault each page into the
/// process's resident set. The caller has already established (a) the
/// pointer points to a valid mapping of at least `start + walk_len`
/// bytes and (b) holds the lifetime guard required for the underlying
/// mapping mode (read guard for RW; no guard needed for RO/COW which
/// are inherently immutable). `read_volatile` is wrapped in
/// `black_box` so the optimiser cannot eliminate the dead read.
#[inline]
fn touch_range_with_ptr(base: *const u8, start: usize, walk_len: usize, page_sz: usize) {
    if walk_len == 0 || page_sz == 0 {
        return;
    }
    let end = start + walk_len;
    let mut off = start;
    // SAFETY:
    //   1. `base.add(off)` produces a pointer inside the mapped region
    //      because `off < end <= mapping length` (the caller validates
    //      this before invoking).
    //   2. `read_volatile::<u8>` reads exactly one byte; the kernel
    //      page-faults that page in if it isn't already resident. A
    //      one-byte read is well-defined for any mapped page on every
    //      supported OS (POSIX `mmap` / Windows `MapViewOfFile`).
    //   3. The mapping cannot be remapped or shrunk while this loop
    //      runs: for RW the caller holds the read lock; for RO/COW the
    //      underlying mapping is immutable for `'self`.
    //   4. `black_box` defeats LLVM dead-store elimination so the read
    //      is observable and actually triggers the fault.
    // Reference: https://doc.rust-lang.org/std/ptr/fn.read_volatile.html
    while off < end {
        unsafe {
            let byte = std::ptr::read_volatile(base.add(off));
            std::hint::black_box(byte);
        }
        off += page_sz;
    }
}

/// Read access to the mapped bytes, used by every read-side accessor.
///
/// For RW and COW mappings this holds a recursive read guard, so the
/// mapping cannot be remapped (by `resize`) or written while it lives,
/// and a thread that already holds a view does not deadlock behind a
/// queued writer. RO mappings are never remapped or written, so a
/// plain borrow is enough. Range checks against `len()` of this value
/// are checks against the mapping actually being accessed.
pub(crate) enum MapRead<'a> {
    /// RO mapping.
    Shared(&'a [u8]),
    /// RW / COW mapping, read-locked.
    Guarded(RwLockReadGuard<'a, RawMmapMut>),
}

impl<'a> MapRead<'a> {
    /// Length of the mapping this access covers. Inherent, so it is
    /// used instead of `<[u8]>::len` through `Deref`: it does not form
    /// a reference to the whole mapping.
    #[inline]
    pub(crate) fn len(&self) -> usize {
        match self {
            MapRead::Shared(s) => s.len(),
            MapRead::Guarded(g) => g.len(),
        }
    }

    /// Base address of the mapping, without forming a slice reference.
    #[inline]
    pub(crate) fn base_ptr(&self) -> *const u8 {
        match self {
            MapRead::Shared(s) => s.as_ptr(),
            MapRead::Guarded(g) => RawMmapMut::as_ptr(g),
        }
    }

    /// Turn this access into a `MappedSlice` over `range`, registering
    /// it as a plain view in `views` for RW / COW mappings. The range
    /// must have been validated against `self.len()` and be non-empty.
    ///
    /// # Errors
    ///
    /// `InvalidMode` if the range overlaps a live atomic view.
    pub(crate) fn into_view(
        self,
        views: &'a ViewRegistry,
        range: std::ops::Range<usize>,
    ) -> Result<MappedSlice<'a>> {
        match self {
            MapRead::Shared(s) => Ok(MappedSlice::owned(&s[range])),
            MapRead::Guarded(g) => {
                let reg = views.register_plain(range.start, range.end)?;
                Ok(MappedSlice::guarded(g, reg, range))
            }
        }
    }

    /// Copy `dst.len()` bytes at mapping offset `start` into `dst`.
    /// The range must have been validated against `self.len()`. On RW /
    /// COW mappings bytes under a live atomic view are read with atomic
    /// loads.
    pub(crate) fn copy_to(&self, views: &ViewRegistry, start: usize, dst: &mut [u8]) {
        match self {
            MapRead::Shared(s) => dst.copy_from_slice(&s[start..start + dst.len()]),
            // SAFETY: `copy_out` needs the mapping valid for
            // `[start, start + dst.len())` and a read guard held for the
            // call: the caller validated the range against `self.len()`,
            // and `g` is that read guard. Atomic views of this mapping
            // all register in `views` (see `crate::atomic`).
            MapRead::Guarded(g) => unsafe { views.copy_out(RawMmapMut::as_ptr(g), start, dst) },
        }
    }
}

impl std::ops::Deref for MapRead<'_> {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        match self {
            MapRead::Shared(s) => s,
            MapRead::Guarded(g) => g,
        }
    }
}

impl MemoryMappedFile {
    /// Acquire read access to the current mapping. See [`MapRead`].
    pub(crate) fn map_read(&self) -> MapRead<'_> {
        match &self.inner.map {
            MapVariant::Ro(m) => MapRead::Shared(m),
            MapVariant::Rw(lock) | MapVariant::Cow(lock) => MapRead::Guarded(lock.read_recursive()),
        }
    }

    /// The lock of a writable (`ReadWrite` or `CopyOnWrite`) mapping,
    /// or `InvalidMode(msg)` for a read-only one.
    pub(crate) fn write_lock(&self, msg: &'static str) -> Result<&RwLock<RawMmapMut>> {
        self.inner.map.locked().ok_or(MmapIoError::InvalidMode(msg))
    }

    /// Whether writes count toward `pending_bytes` and the flush
    /// policy: only shared (`ReadWrite`) mappings have anything to
    /// flush.
    #[inline]
    pub(crate) fn tracks_writes(&self) -> bool {
        matches!(self.inner.map, MapVariant::Rw(_))
    }

    /// The pending-bytes counter for write guards, or `None` for
    /// copy-on-write mappings, whose writes are never flushed.
    #[inline]
    pub(crate) fn pending_counter(&self) -> Option<&AtomicU64> {
        if self.tracks_writes() {
            Some(&self.inner.written_since_last_flush)
        } else {
            None
        }
    }
}

/// Map `len` bytes of `file` read-write from offset 0. With `huge`
/// set (and the `hugepages` feature on), also issue the transparent
/// huge page hint; see [`advise_huge_pages`].
fn map_file_rw(file: &File, len: usize, huge: bool) -> Result<RawMmapMut> {
    // SAFETY: `RawMmapOptions::map_mut` is `unsafe` because the OS does
    // not stop another process from modifying the file under the
    // mapping (see `create_rw`). Callers pass a `len` no larger than
    // the file's current length, so every mapped page is backed by
    // the file. Within this process all access to the returned
    // mapping goes through the `RwLock` in `MapVariant::Rw`.
    // Contract: `crate::raw::RawMmapOptions::map_mut` (see `docs/SAFETY.md`, raw mapping layer).
    let map = unsafe { RawMmapOptions::new().len(len).map_mut(file)? };
    #[cfg(feature = "hugepages")]
    if huge {
        advise_huge_pages(&map);
    }
    #[cfg(not(feature = "hugepages"))]
    let _ = huge;
    Ok(map)
}

/// Map the first `len` bytes of `file` privately (copy-on-write).
#[cfg(feature = "cow")]
fn map_file_cow(file: &File, len: u64) -> Result<RawMmapMut> {
    let len = usize::try_from(len).map_err(|_| {
        MmapIoError::ResizeFailed(format!("File length {len} does not fit in usize"))
    })?;
    // SAFETY: `RawMmapOptions::map_copy` carries the same cross-process
    // hazard as `RawMmap::map`: another process modifying the file can
    // change pages this mapping has not written yet (torn reads). The
    // crate marks that as out of scope (REPS.md section 5.1). Writes
    // through the mapping go to private pages (MAP_PRIVATE /
    // PAGE_WRITECOPY) and never reach the file or any other mapping, and
    // within the process every access goes through the `RwLock` in
    // `MapVariant::Cow`, exactly as for `Rw`. `len` is the file size the
    // caller just queried, so the window lies inside the file.
    // Contract: `crate::raw::RawMmapOptions::map_copy` (see `docs/SAFETY.md`, raw mapping layer).
    Ok(unsafe { RawMmapOptions::new().len(len).map_copy(file)? })
}

/// Hint the kernel to back `map` with transparent huge pages.
///
/// Linux only: issues `madvise(MADV_HUGEPAGE)` over the whole mapping.
/// This is a hint, not a request that can fail the mapping. For
/// file-backed mappings the kernel honors it only where the page cache
/// of that filesystem can use huge pages (for example tmpfs or shmem
/// mounted with `huge=`); on most disk filesystems the mapping stays
/// on base pages. `MAP_HUGETLB` is not used: it needs a hugetlbfs file.
/// Elsewhere this does nothing. Failures are logged at debug level.
#[cfg(feature = "hugepages")]
pub(crate) fn advise_huge_pages(map: &RawMmapMut) {
    #[cfg(target_os = "linux")]
    {
        if map.is_empty() {
            return;
        }
        // SAFETY: `madvise(addr, len, MADV_HUGEPAGE)` requires `addr`
        // page-aligned and `[addr, addr + len)` inside a mapping of
        // this process. `map.as_ptr()` is the base of a mapping that
        // starts at file offset 0, which `mmap(2)` page-aligns, and
        // `map.len()` is that mapping's length; `map` is borrowed for
        // the duration of the call, so the range stays mapped.
        // MADV_HUGEPAGE only changes the kernel's page-size policy for
        // the range; it does not read or write the contents.
        // Reference: https://man7.org/linux/man-pages/man2/madvise.2.html
        let ret = unsafe {
            libc::madvise(
                map.as_ptr() as *mut libc::c_void,
                map.len(),
                libc::MADV_HUGEPAGE,
            )
        };
        if ret == 0 {
            log::debug!("madvise(MADV_HUGEPAGE) accepted for {} bytes", map.len());
        } else {
            log::debug!(
                "madvise(MADV_HUGEPAGE) failed: {}; using base pages",
                std::io::Error::last_os_error()
            );
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = map;
    }
}

#[cfg(feature = "cow")]
impl MemoryMappedFile {
    /// Open an existing file and memory-map it in copy-on-write mode.
    ///
    /// The file only needs read permission. The mapping is private
    /// (`MAP_PRIVATE` / `PAGE_WRITECOPY`): since 1.1.0 every write
    /// method works on it ([`update_region`](Self::update_region),
    /// [`as_slice_mut`](Self::as_slice_mut), `chunks_mut`, atomic
    /// views), each written page is copied on first write, and the
    /// changes are visible through this mapping (and its clones) only.
    /// They never reach the file, and they are lost when the mapping is
    /// dropped. [`flush`](Self::flush) and
    /// [`flush_range`](Self::flush_range) are `Ok` no-ops,
    /// [`pending_bytes`](Self::pending_bytes) stays 0, and
    /// [`resize`](Self::resize) returns [`MmapIoError::InvalidMode`].
    ///
    /// Locking works as for `ReadWrite`: a live read view blocks the
    /// write methods, and a write on the thread that holds a view
    /// deadlocks (use the `try_` methods to avoid that).
    ///
    /// Pages not yet written may still show later changes made to the
    /// file by others (POSIX leaves this unspecified; Windows shows
    /// them), the same caveat as any mapping of a shared file.
    ///
    /// # Errors
    ///
    /// Returns [`MmapIoError::Io`] if the file cannot be opened or mapped.
    /// Returns [`MmapIoError::ResizeFailed`] if the file is zero-length.
    ///
    /// # Example
    ///
    /// ```
    /// use mmap_io::MemoryMappedFile;
    ///
    /// let dir = tempfile::tempdir()?;
    /// let path = dir.path().join("cow.bin");
    /// std::fs::write(&path, b"original")?;
    ///
    /// let cow = MemoryMappedFile::open_cow(&path)?;
    /// cow.update_region(0, b"EDIT")?;
    /// assert_eq!(&*cow.as_slice(0, 8)?, b"EDITinal");
    /// cow.flush()?; // no-op: private pages are never written back
    /// assert_eq!(std::fs::read(&path)?, b"original");
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn open_cow<P: AsRef<Path>>(path: P) -> Result<Self> {
        let path_ref = path.as_ref();
        let file = OpenOptions::new().read(true).open(path_ref)?;
        let len = file.metadata()?.len();
        if len == 0 {
            return Err(MmapIoError::ResizeFailed(ERR_ZERO_LENGTH_FILE.into()));
        }
        let mmap = map_file_cow(&file, len)?;
        let inner = Inner {
            path: path_ref.to_path_buf(),
            file,
            mode: MmapMode::CopyOnWrite,
            cached_len: AtomicU64::new(len),
            map: MapVariant::Cow(RwLock::new(mmap)),
            // Nothing to flush: writes stay in private pages.
            flush_policy: FlushPolicy::Never,
            written_since_last_flush: AtomicU64::new(0),
            writes_since_last_flush: AtomicU64::new(0),
            flusher: RwLock::new(None),
            views: ViewRegistry::new(),
            #[cfg(feature = "hugepages")]
            huge_pages: false,
        };
        Ok(Self {
            inner: Arc::new(inner),
        })
    }
}

impl MemoryMappedFile {
    /// Add `bytes` to the pending-bytes counter without evaluating the
    /// flush policy. Used by write paths that cannot flush at the
    /// point of the write (guards being dropped, closures returning).
    /// Copy-on-write writes are not counted: they are never flushed.
    pub(crate) fn record_write(&self, bytes: u64) {
        if let Some(pending) = self.pending_counter() {
            pending.fetch_add(bytes, Ordering::AcqRel);
        }
    }

    /// Count one `update_region` of `written` bytes and run the flush
    /// policy. Called after the write lock has been released. A no-op
    /// on copy-on-write mappings.
    fn apply_flush_policy(&self, written: u64) -> Result<()> {
        if self.count_update(written) {
            self.flush()
        } else {
            Ok(())
        }
    }

    /// Count one `update_region` of `written` bytes and report whether
    /// the flush policy asks for a flush now. Always `false` on
    /// copy-on-write mappings, which have nothing to flush.
    fn count_update(&self, written: u64) -> bool {
        if !self.tracks_writes() {
            return false;
        }
        let pending = self
            .inner
            .written_since_last_flush
            .fetch_add(written, Ordering::AcqRel)
            .saturating_add(written);
        let writes = self
            .inner
            .writes_since_last_flush
            .fetch_add(1, Ordering::AcqRel)
            .saturating_add(1);
        match self.inner.flush_policy {
            // EveryMillis flushes from its background thread, which
            // checks `pending_bytes()`.
            FlushPolicy::Never | FlushPolicy::Manual | FlushPolicy::EveryMillis(_) => false,
            FlushPolicy::Always => true,
            FlushPolicy::EveryBytes(n) => n > 0 && pending >= n as u64,
            FlushPolicy::EveryWrites(w) => w > 0 && writes >= w as u64,
        }
    }

    /// Return the mapping length (cached). Same value as
    /// [`len`](Self::len); kept for API compatibility.
    ///
    /// The value can be stale by the time the caller uses it if
    /// another thread calls [`resize`](Self::resize) concurrently.
    /// Every accessor in this crate re-validates its range under the
    /// mapping lock, so passing a stale length to them yields
    /// `OutOfBounds` rather than an out-of-range access.
    ///
    /// # Errors
    ///
    /// Never returns an error; the `Result` is kept for API
    /// compatibility.
    pub fn current_len(&self) -> Result<u64> {
        Ok(self.len())
    }

    /// Read bytes from the mapping into the provided buffer starting at `offset`.
    /// Length is `buf.len()`; performs bounds checks. An empty `buf`
    /// is accepted at any offset.
    ///
    /// The range may overlap live atomic views: those bytes are read
    /// with atomic loads of the view's element size, so the copy never
    /// races with concurrent atomic stores (each element is copied
    /// whole, but the buffer as a whole is not one atomic snapshot).
    ///
    /// # Performance
    ///
    /// - **Time Complexity**: O(n) where n is buf.len()
    /// - **Memory Usage**: Uses provided buffer, no additional allocation
    /// - **Cache Behavior**: Sequential access pattern is cache-friendly
    ///
    /// # Errors
    ///
    /// Returns `MmapIoError::OutOfBounds` if range exceeds file bounds.
    pub fn read_into(&self, offset: u64, buf: &mut [u8]) -> Result<()> {
        if buf.is_empty() {
            return Ok(());
        }
        let map = self.map_read();
        let (start, _end) = slice_range(offset, buf.len() as u64, map.len() as u64)?;
        map.copy_to(&self.inner.views, start, buf);
        Ok(())
    }
}

// Hugepage runtime introspection (1.0.0).
//
// The hugepages builder flag is a request; the kernel decides what to
// actually back the mapping with. `is_hugepage_backed` answers the
// question after the fact, by inspecting `/proc/self/smaps` on Linux.
impl MemoryMappedFile {
    /// Report whether the kernel currently backs this mapping with
    /// huge pages.
    ///
    /// Returns:
    /// - `Some(true)` if any portion of the mapping is backed by huge
    ///   pages (transparent or explicit HugeTLB).
    /// - `Some(false)` if the mapping is backed by regular pages only.
    /// - `None` on platforms without a queryable hugepage status
    ///   (everything except Linux at present), or if the status could
    ///   not be determined (e.g. `/proc/self/smaps` unreadable, no
    ///   matching entry found).
    ///
    /// On Linux, this parses `/proc/self/smaps`, locating the entry
    /// whose address range contains the mapping's base, and inspects
    /// `AnonHugePages`, `Private_Hugetlb`, and `Shared_Hugetlb`. Any
    /// non-zero value yields `Some(true)`.
    ///
    /// # Notes
    ///
    /// Treat `None` as "unknown", not as "definitely regular pages".
    /// The result reflects state at the moment of the call; the kernel
    /// may promote or demote pages over time (Transparent Huge Pages).
    #[must_use]
    pub fn is_hugepage_backed(&self) -> Option<bool> {
        #[cfg(target_os = "linux")]
        {
            let base = self.base_addr()?;
            smaps_hugepage_lookup(base)
        }
        #[cfg(not(target_os = "linux"))]
        {
            None
        }
    }

    /// Return the base address of the mapping as a `usize`. The lock
    /// (for RW) is released before the address is returned; the address
    /// remains the kernel-reported base for the mapping's current
    /// generation. If a concurrent `resize` runs between this call and
    /// any subsequent address-keyed lookup, the lookup may not find the
    /// mapping. Callers must treat that as "unknown".
    #[cfg(target_os = "linux")]
    fn base_addr(&self) -> Option<usize> {
        match &self.inner.map {
            MapVariant::Ro(m) => Some(m.as_ptr() as usize),
            MapVariant::Rw(lock) | MapVariant::Cow(lock) => {
                Some(lock.read_recursive().as_ptr() as usize)
            }
        }
    }
}

#[cfg(target_os = "linux")]
pub(crate) fn smaps_hugepage_lookup(base: usize) -> Option<bool> {
    use std::io::BufRead;
    let file = std::fs::File::open("/proc/self/smaps").ok()?;
    let reader = std::io::BufReader::new(file);

    let mut in_range = false;
    let mut found_any_hugepage = false;
    let mut matched_at_least_once = false;

    for line in reader.lines() {
        let line = line.ok()?;
        if let Some((lo, hi)) = parse_smaps_range(&line) {
            if in_range {
                // Just finished the matched entry. Decide.
                return Some(found_any_hugepage);
            }
            if base >= lo && base < hi {
                in_range = true;
                matched_at_least_once = true;
                found_any_hugepage = false;
            }
        } else if in_range {
            if let Some(kb) = parse_smaps_kb_field(
                &line,
                &["AnonHugePages:", "Private_Hugetlb:", "Shared_Hugetlb:"],
            ) {
                if kb > 0 {
                    found_any_hugepage = true;
                }
            }
        }
    }

    if matched_at_least_once {
        Some(found_any_hugepage)
    } else {
        None
    }
}

#[cfg(target_os = "linux")]
fn parse_smaps_range(line: &str) -> Option<(usize, usize)> {
    // Range header lines look like:
    //   7f1234567000-7f1234578000 rw-s 00000000 00:00 0
    // and are distinguished from stat lines by the leading hex range.
    let first = line.split_whitespace().next()?;
    let (lo_s, hi_s) = first.split_once('-')?;
    let lo = usize::from_str_radix(lo_s, 16).ok()?;
    let hi = usize::from_str_radix(hi_s, 16).ok()?;
    Some((lo, hi))
}

#[cfg(target_os = "linux")]
fn parse_smaps_kb_field(line: &str, prefixes: &[&str]) -> Option<u64> {
    for p in prefixes {
        if let Some(rest) = line.strip_prefix(p) {
            // rest looks like "       128 kB"
            let num = rest.split_whitespace().next()?;
            return num.parse::<u64>().ok();
        }
    }
    None
}

/// The builder's huge-page request (always `false` without the
/// `hugepages` feature).
fn builder_huge_pages(builder: &MemoryMappedFileBuilder) -> bool {
    #[cfg(feature = "hugepages")]
    {
        builder.huge_pages
    }
    #[cfg(not(feature = "hugepages"))]
    {
        let _ = builder;
        false
    }
}

/// Builder for MemoryMappedFile construction with options.
pub struct MemoryMappedFileBuilder {
    path: PathBuf,
    size: Option<u64>,
    mode: Option<MmapMode>,
    flush_policy: FlushPolicy,
    touch_hint: TouchHint,
    #[cfg(feature = "hugepages")]
    huge_pages: bool,
}

impl MemoryMappedFileBuilder {
    /// Specify the size (required for create/ReadWrite new files).
    pub fn size(mut self, size: u64) -> Self {
        self.size = Some(size);
        self
    }

    /// Specify the mode (ReadOnly, ReadWrite, CopyOnWrite).
    pub fn mode(mut self, mode: MmapMode) -> Self {
        self.mode = Some(mode);
        self
    }

    /// Specify the flush policy.
    pub fn flush_policy(mut self, policy: FlushPolicy) -> Self {
        self.flush_policy = policy;
        self
    }

    /// Specify when to touch (prewarm) memory pages.
    pub fn touch_hint(mut self, hint: TouchHint) -> Self {
        self.touch_hint = hint;
        self
    }

    /// Ask for transparent huge pages on `ReadWrite` mappings.
    ///
    /// On Linux the mapping gets a `madvise(MADV_HUGEPAGE)` hint after
    /// it is created (and again after every `resize`). The kernel
    /// decides whether to use huge pages; for files on most disk
    /// filesystems it will not, while tmpfs/shmem mounted with
    /// `huge=` can. `MAP_HUGETLB` is not attempted, and nothing is
    /// pre-faulted. On macOS and Windows the flag has no effect. Use
    /// [`MemoryMappedFile::is_hugepage_backed`] to see what the kernel
    /// actually did. Ignored for `ReadOnly` and `CopyOnWrite`.
    #[cfg(feature = "hugepages")]
    pub fn huge_pages(mut self, enable: bool) -> Self {
        self.huge_pages = enable;
        self
    }

    /// Create a new mapping; for ReadWrite requires size for creation.
    ///
    /// For `ReadWrite` (the default mode here) the file is created or
    /// truncated to `size`. For `ReadOnly` and `CopyOnWrite` the file
    /// must already exist and is opened as-is.
    ///
    /// # Errors
    ///
    /// Returns [`MmapIoError::ResizeFailed`] if size is missing or zero
    /// for a `ReadWrite` create.
    /// Returns [`MmapIoError::InvalidMode`] if the requested mode is
    /// `CopyOnWrite` without the `cow` feature enabled.
    /// Returns [`MmapIoError::Io`] if file creation or mapping fails.
    pub fn create(self) -> Result<MemoryMappedFile> {
        let mode = self.mode.unwrap_or(MmapMode::ReadWrite);
        match mode {
            MmapMode::ReadWrite => {
                let size = validated_create_size(self.size)?;
                let file = OpenOptions::new()
                    .create(true)
                    .write(true)
                    .read(true)
                    .truncate(true)
                    .open(&self.path)?;
                file.set_len(size)?;
                self.finish_rw(file, size)
            }
            MmapMode::ReadOnly | MmapMode::CopyOnWrite => self.open_existing(mode),
        }
    }

    /// Like [`create`](Self::create), but fail if the file already
    /// exists instead of truncating it. Since 1.1.0.
    ///
    /// The file is created with an exclusive create (`O_CREAT | O_EXCL`
    /// on Unix, `CREATE_NEW` on Windows), so of several threads or
    /// processes racing to create the same path exactly one succeeds
    /// and nobody's data is truncated. The new file is sized to
    /// `size` (sparse) and mapped `ReadWrite` with every builder option
    /// applied (flush policy including the `EveryMillis` flusher, touch
    /// hint, huge pages), exactly as `create` does.
    ///
    /// The mode must be `ReadWrite` (the default); a new, empty file
    /// cannot be opened read-only or copy-on-write. Size and mode are
    /// checked before anything touches the filesystem. If sizing or
    /// mapping the new file fails, the file is removed again (best
    /// effort) so no half-created file is left behind.
    ///
    /// # Errors
    ///
    /// Returns [`MmapIoError::Io`] with `ErrorKind::AlreadyExists` if
    /// the path exists (the existing file is left untouched).
    /// Returns [`MmapIoError::ResizeFailed`] if `size` was not set, is
    /// zero, or exceeds the maximum safe size.
    /// Returns [`MmapIoError::InvalidMode`] if the mode is not
    /// `ReadWrite`.
    /// Returns [`MmapIoError::Io`] if creating, sizing, or mapping the
    /// file fails for another reason.
    ///
    /// # Example
    ///
    /// ```
    /// use mmap_io::{MemoryMappedFile, MmapIoError};
    ///
    /// let dir = tempfile::tempdir()?;
    /// let path = dir.path().join("fresh.bin");
    /// let mmap = MemoryMappedFile::builder(&path).size(4096).create_new()?;
    /// assert_eq!(mmap.len(), 4096);
    ///
    /// // A second create_new on the same path refuses to truncate it.
    /// let err = MemoryMappedFile::builder(&path).size(4096).create_new().unwrap_err();
    /// assert!(matches!(err, MmapIoError::Io(ref e) if e.kind() == std::io::ErrorKind::AlreadyExists));
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn create_new(self) -> Result<MemoryMappedFile> {
        let mode = self.mode.unwrap_or(MmapMode::ReadWrite);
        if mode != MmapMode::ReadWrite {
            return Err(MmapIoError::InvalidMode(
                "create_new creates a new ReadWrite mapping; open existing files with open()",
            ));
        }
        let size = validated_create_size(self.size)?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&self.path)?;
        let path = self.path.clone();
        let result = match file.set_len(size) {
            Ok(()) => self.finish_rw(file, size),
            Err(e) => {
                drop(file);
                Err(e.into())
            }
        };
        if result.is_err() {
            // We created the file exclusively a moment ago and it holds
            // no data: remove it rather than leave a stray file. The
            // handle is closed by now (Windows cannot delete open files).
            let _ = std::fs::remove_file(&path);
        }
        result
    }

    /// Open an existing file with provided mode (size ignored).
    ///
    /// The mode defaults to `ReadOnly`. For `ReadWrite`, the
    /// configured `flush_policy` (including the `EveryMillis`
    /// background flusher), `touch_hint`, and `huge_pages` apply
    /// exactly as they do for [`create`](Self::create).
    ///
    /// # Errors
    ///
    /// Returns [`MmapIoError::InvalidMode`] if `CopyOnWrite` was
    /// requested without the `cow` feature.
    /// Returns [`MmapIoError::Io`] if the file cannot be opened or
    /// mapped, or [`MmapIoError::ResizeFailed`] if the file is
    /// zero-length.
    pub fn open(self) -> Result<MemoryMappedFile> {
        let mode = self.mode.unwrap_or(MmapMode::ReadOnly);
        self.open_existing(mode)
    }

    /// Terminal builder method that opens the file if it exists, or
    /// creates it (using the builder's configured size) if it does
    /// not. The mode defaults to `ReadWrite`.
    ///
    /// For `ReadWrite` the file is never truncated: a non-empty
    /// existing file is mapped at its current length (`size` is
    /// ignored), and a missing or zero-length file is created or
    /// extended to `size`. Creation uses an exclusive create, so two
    /// processes racing to create the same path cannot truncate each
    /// other's data. The configured `flush_policy`, `touch_hint`, and
    /// `huge_pages` apply on both paths. For `ReadOnly` and
    /// `CopyOnWrite` this is the same as [`open`](Self::open).
    ///
    /// # Errors
    ///
    /// Returns [`MmapIoError::ResizeFailed`] if the file has to be
    /// created or extended and `.size()` was not set or was set to
    /// zero.
    /// Returns [`MmapIoError::Io`] if the open or create call fails.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use mmap_io::{MemoryMappedFile, MmapMode};
    /// use mmap_io::flush::FlushPolicy;
    ///
    /// let mmap = MemoryMappedFile::builder("data.bin")
    ///     .mode(MmapMode::ReadWrite)
    ///     .size(1024 * 1024) // used only if creating
    ///     .flush_policy(FlushPolicy::EveryBytes(64 * 1024))
    ///     .open_or_create()?;
    /// # Ok::<(), mmap_io::MmapIoError>(())
    /// ```
    pub fn open_or_create(self) -> Result<MemoryMappedFile> {
        let mode = self.mode.unwrap_or(MmapMode::ReadWrite);
        if mode != MmapMode::ReadWrite {
            return self.open_existing(mode);
        }
        let (file, len) = open_or_create_rw_file(&self.path, self.size)?;
        self.finish_rw(file, len)
    }

    /// Open an existing file in `mode`. Shared by `open`, and by
    /// `create` / `open_or_create` for the non-RW modes.
    fn open_existing(self, mode: MmapMode) -> Result<MemoryMappedFile> {
        match mode {
            MmapMode::ReadOnly => {
                let file = OpenOptions::new().read(true).open(&self.path)?;
                let len = file.metadata()?.len();
                // SAFETY: see `MemoryMappedFile::open_ro` for the full
                // justification of calling `RawMmap::map`. The file was
                // just opened read-only; cross-process modification is
                // the only residual hazard and is documented as
                // out-of-scope.
                let mmap = unsafe { RawMmap::map(&file)? };
                Ok(MemoryMappedFile::from_inner(Inner {
                    path: self.path,
                    file,
                    mode,
                    cached_len: AtomicU64::new(len),
                    map: MapVariant::Ro(mmap),
                    flush_policy: FlushPolicy::Never,
                    written_since_last_flush: AtomicU64::new(0),
                    writes_since_last_flush: AtomicU64::new(0),
                    flusher: RwLock::new(None),
                    views: ViewRegistry::new(),
                    #[cfg(feature = "hugepages")]
                    huge_pages: false,
                }))
            }
            MmapMode::ReadWrite => {
                let file = OpenOptions::new().read(true).write(true).open(&self.path)?;
                let len = file.metadata()?.len();
                if len == 0 {
                    return Err(MmapIoError::ResizeFailed(ERR_ZERO_LENGTH_FILE.into()));
                }
                self.finish_rw(file, len)
            }
            #[cfg(feature = "cow")]
            MmapMode::CopyOnWrite => {
                let file = OpenOptions::new().read(true).open(&self.path)?;
                let len = file.metadata()?.len();
                if len == 0 {
                    return Err(MmapIoError::ResizeFailed(ERR_ZERO_LENGTH_FILE.into()));
                }
                let mmap = map_file_cow(&file, len)?;
                Ok(MemoryMappedFile::from_inner(Inner {
                    path: self.path,
                    file,
                    mode,
                    cached_len: AtomicU64::new(len),
                    map: MapVariant::Cow(RwLock::new(mmap)),
                    flush_policy: FlushPolicy::Never,
                    written_since_last_flush: AtomicU64::new(0),
                    writes_since_last_flush: AtomicU64::new(0),
                    flusher: RwLock::new(None),
                    views: ViewRegistry::new(),
                    #[cfg(feature = "hugepages")]
                    huge_pages: false,
                }))
            }
            #[cfg(not(feature = "cow"))]
            MmapMode::CopyOnWrite => Err(MmapIoError::InvalidMode(
                "CopyOnWrite mode requires 'cow' feature",
            )),
        }
    }

    /// Map an RW file of `len` bytes and apply every builder option:
    /// huge-page hint, flush policy (starting the `EveryMillis`
    /// background flusher), and touch hint. Every RW builder path ends
    /// here so `create`, `open`, and `open_or_create` behave alike.
    fn finish_rw(self, file: File, len: u64) -> Result<MemoryMappedFile> {
        let map_len = usize::try_from(len).map_err(|_| {
            MmapIoError::ResizeFailed(format!("File length {len} does not fit in usize"))
        })?;
        let huge = builder_huge_pages(&self);
        let mmap = map_file_rw(&file, map_len, huge)?;
        let mmap_file = MemoryMappedFile::from_inner(Inner {
            path: self.path,
            file,
            mode: MmapMode::ReadWrite,
            cached_len: AtomicU64::new(len),
            map: MapVariant::Rw(RwLock::new(mmap)),
            flush_policy: self.flush_policy,
            written_since_last_flush: AtomicU64::new(0),
            writes_since_last_flush: AtomicU64::new(0),
            flusher: RwLock::new(None),
            views: ViewRegistry::new(),
            #[cfg(feature = "hugepages")]
            huge_pages: huge,
        });

        // The flusher holds a Weak that must point at the live Arc, so
        // it can only be attached after the Arc exists.
        if let FlushPolicy::EveryMillis(ms) = self.flush_policy {
            start_time_based_flusher(&mmap_file, ms);
        }

        if self.touch_hint == TouchHint::Eager {
            log::debug!("Eagerly touching all pages for {len} bytes");
            if let Err(e) = mmap_file.touch_pages() {
                // Prewarming is an optimization; never fail the open.
                log::warn!("Failed to eagerly touch pages: {e}");
            }
        }
        Ok(mmap_file)
    }
}

/// Validate the builder size for creating a new RW file.
fn validated_create_size(size: Option<u64>) -> Result<u64> {
    let size = size.ok_or_else(|| {
        MmapIoError::ResizeFailed("Size must be set for create() in ReadWrite mode".into())
    })?;
    if size == 0 {
        return Err(MmapIoError::ResizeFailed(ERR_ZERO_SIZE.into()));
    }
    if size > MAX_MMAP_SIZE {
        return Err(MmapIoError::ResizeFailed(format!(
            "Size {size} exceeds maximum safe limit of {MAX_MMAP_SIZE} bytes"
        )));
    }
    Ok(size)
}

/// Open `path` read-write without ever truncating it, creating it at
/// `size` if it does not exist and extending it to `size` if it is
/// empty. Returns the file and its length.
///
/// The create step uses `create_new`, so a file created by someone else
/// between our open attempt and our create is opened, not truncated.
fn open_or_create_rw_file(path: &Path, size: Option<u64>) -> Result<(File, u64)> {
    let mut opts = OpenOptions::new();
    opts.read(true).write(true);
    // Two passes cover the race where another process creates the
    // file between our failed open and our exclusive create.
    for _ in 0..2 {
        match opts.open(path) {
            Ok(file) => {
                let len = file.metadata()?.len();
                if len > 0 {
                    return Ok((file, len));
                }
                let size = validated_create_size(size)?;
                file.set_len(size)?;
                return Ok((file, size));
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let size = validated_create_size(size)?;
                match OpenOptions::new()
                    .read(true)
                    .write(true)
                    .create_new(true)
                    .open(path)
                {
                    Ok(file) => {
                        file.set_len(size)?;
                        return Ok((file, size));
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                    Err(e) => return Err(e.into()),
                }
            }
            Err(e) => return Err(e.into()),
        }
    }
    // Created and deleted again by someone else twice in a row.
    Err(MmapIoError::Io(std::io::Error::other(
        "file was concurrently created and removed",
    )))
}

/// Attach an `EveryMillis(ms)` background flusher to `mmap_file`.
/// `ms == 0` disables time-based flushing.
fn start_time_based_flusher(mmap_file: &MemoryMappedFile, ms: u64) {
    let inner_weak = Arc::downgrade(&mmap_file.inner);
    let flusher = crate::flush::TimeBasedFlusher::new(ms, move || {
        // Upgrade the weak ref. Returns None once the mapping has
        // been dropped; the callback then does nothing.
        let Some(inner) = inner_weak.upgrade() else {
            return false;
        };
        if inner.written_since_last_flush.load(Ordering::Acquire) == 0 {
            return false;
        }
        MemoryMappedFile { inner }.flush().is_ok()
    });
    // Stored on Inner so the worker lives exactly as long as the
    // mapping; dropping Inner stops it.
    *mmap_file.inner.flusher.write() = flusher;
}

/// Raw pointer to `guard[range]` without forming a reference to the
/// whole mapping (another part of it may be under a live atomic view).
///
/// # Panics
///
/// Panics if `range` is not within the mapping.
fn sub_slice_ptr(guard: &RawMmapMut, range: std::ops::Range<usize>) -> *const [u8] {
    assert!(
        range.start <= range.end && range.end <= guard.len(),
        "range checked by the caller"
    );
    // `wrapping_add` stays in bounds (asserted above) and avoids
    // `unsafe`; the pointer is only dereferenced by `MappedSlice`.
    std::ptr::slice_from_raw_parts(
        guard.as_ptr().wrapping_add(range.start),
        range.end - range.start,
    )
}

/// Wrapper for a mutable slice that holds a write lock guard,
/// ensuring exclusive access for the lifetime of the slice.
///
/// For file-backed mappings, dropping it adds its length to
/// [`MemoryMappedFile::pending_bytes`]; the bytes are assumed written.
pub struct MappedSliceMut<'a> {
    guard: RwLockWriteGuard<'a, RawMmapMut>,
    range: std::ops::Range<usize>,
    /// Pending-bytes counter of the owning `MemoryMappedFile`; `None`
    /// for `AnonymousMmap`, which has nothing to flush.
    pending: Option<&'a AtomicU64>,
}

impl<'a> MappedSliceMut<'a> {
    /// Construct a `MappedSliceMut` that holds a write guard for its
    /// lifetime. Used by `AnonymousMmap`, which has no flush
    /// accounting.
    pub(crate) fn guarded(
        guard: RwLockWriteGuard<'a, RawMmapMut>,
        range: std::ops::Range<usize>,
    ) -> Self {
        Self {
            guard,
            range,
            pending: None,
        }
    }

    /// Get the mutable slice.
    ///
    /// Note: This method is intentionally named `as_mut` for consistency,
    /// even though it conflicts with the standard trait naming.
    // Public since 0.9.x; renaming it or replacing it with an `AsMut`
    // impl would break callers, so the lint is silenced here.
    #[allow(clippy::should_implement_trait)]
    pub fn as_mut(&mut self) -> &mut [u8] {
        // Avoid clone by using the range directly
        let start = self.range.start;
        let end = self.range.end;
        &mut self.guard[start..end]
    }

    /// Length of the mutable slice in bytes.
    #[must_use]
    pub fn len(&self) -> usize {
        self.range.end - self.range.start
    }

    /// Whether the slice is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.range.start == self.range.end
    }
}

impl Drop for MappedSliceMut<'_> {
    fn drop(&mut self) {
        // Runs before the write guard is released, so a flush cannot
        // observe the lock free while this write is still uncounted.
        if let Some(pending) = self.pending {
            pending.fetch_add(self.len() as u64, Ordering::AcqRel);
        }
    }
}

impl std::ops::Deref for MappedSliceMut<'_> {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        &self.guard[self.range.clone()]
    }
}

impl std::ops::DerefMut for MappedSliceMut<'_> {
    fn deref_mut(&mut self) -> &mut [u8] {
        let start = self.range.start;
        let end = self.range.end;
        &mut self.guard[start..end]
    }
}

/// Wrapper for an immutable slice into a memory-mapped file.
///
/// For RO mappings this is a thin wrapper around a `&[u8]` borrowed
/// directly from the underlying immutable mapping. For RW and COW
/// mappings this also holds the `RwLock` read guard for its lifetime,
/// blocking any concurrent `resize()` (and every write, which also
/// needs the write lock) while the slice is alive, and it keeps atomic
/// views off its bytes (creating an overlapping atomic view returns
/// `InvalidMode`). Iterator items that overlap a live atomic view are
/// owned copies instead (see `MemoryMappedFile::chunks`).
///
/// Implements [`Deref<Target = [u8]>`] and [`AsRef<[u8]>`], so callers
/// can use it as a byte slice directly: indexing, iteration,
/// `slice.len()`, `&slice[..]`, etc. all work.
pub struct MappedSlice<'a> {
    inner: MappedSliceInner<'a>,
}

enum MappedSliceInner<'a> {
    /// RO: the mapping is immutable; we lend a direct slice.
    Owned(&'a [u8]),
    /// RW / COW: the read guard keeps the mapping alive (and prevents
    /// `resize()` and writes from running) for the slice's lifetime,
    /// and the registration keeps atomic views off these bytes.
    /// `bytes` is computed once at construction so `Deref` does no
    /// range arithmetic or bounds checks.
    Guarded {
        _reg: PlainReg<'a>,
        _guard: RwLockReadGuard<'a, RawMmapMut>,
        bytes: *const [u8],
    },
    /// Owned copy of the bytes, used for iterator items (and reader
    /// buffers) that overlap a live atomic view: those bytes cannot be
    /// lent as `&[u8]`, so they are copied with atomic loads instead.
    // Only built by the iterators, when atomic views can exist.
    #[cfg_attr(not(all(feature = "atomic", feature = "iterator")), allow(dead_code))]
    Snapshot(Box<[u8]>),
}

// SAFETY: `MappedSlice` only ever hands out `&[u8]` to bytes that no
// one can mutate while it lives: RO mappings are immutable; for RW and
// COW the held read guard excludes every writer and the plain-view
// registration excludes atomic views of the same bytes. `Owned` holds
// a `&[u8]` and `Snapshot` a `Box<[u8]>`, both `Send + Sync`.
// `Guarded` holds a parking_lot read guard, which is `Send` because
// this crate enables parking_lot's `send_guard` feature (checked at
// compile time by `_ASSERT_GUARDS_SEND_SYNC` below) and `Sync` because
// `RawMmapMut` is `Sync`, plus a `PlainReg` (a shared reference to the
// `Sync` registry and two indices, or nothing without the `atomic`
// feature). The raw `bytes` pointer is only a cached view of memory
// owned by that guarded mapping, so moving or sharing it across
// threads is no different from moving or sharing the guard itself.
unsafe impl Send for MappedSlice<'_> {}
// SAFETY: see the `Send` impl above; shared access only reads.
unsafe impl Sync for MappedSlice<'_> {}

/// Compile-time proof that the guards stored in `MappedSlice`,
/// `MappedSliceMut`, the iterators, and the atomic views are
/// `Send + Sync`. This fails to build if parking_lot's `send_guard`
/// feature is ever dropped, which would make the `unsafe impl Send`
/// blocks in this crate unsound.
const _ASSERT_GUARDS_SEND_SYNC: fn() = || {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<RwLockReadGuard<'static, RawMmapMut>>();
    assert_send_sync::<RwLockWriteGuard<'static, RawMmapMut>>();
};

impl<'a> MappedSlice<'a> {
    /// Construct a `MappedSlice` from a direct `&[u8]`. Used for RO
    /// and COW paths where the underlying mapping is already
    /// immutable.
    pub(crate) fn owned(slice: &'a [u8]) -> Self {
        Self {
            inner: MappedSliceInner::Owned(slice),
        }
    }

    /// Construct a `MappedSlice` that holds a read guard and a plain
    /// view registration for its lifetime. Used for RW / COW paths.
    ///
    /// # Panics
    ///
    /// Panics if `range` is not within `guard`'s mapping. Callers
    /// validate the range against `guard.len()` first.
    pub(crate) fn guarded(
        guard: RwLockReadGuard<'a, RawMmapMut>,
        reg: PlainReg<'a>,
        range: std::ops::Range<usize>,
    ) -> Self {
        let bytes = sub_slice_ptr(&guard, range);
        Self {
            inner: MappedSliceInner::Guarded {
                _reg: reg,
                _guard: guard,
                bytes,
            },
        }
    }

    /// Construct an owned copy (see `MappedSliceInner::Snapshot`).
    // Only called by the iterators, when atomic views can exist.
    #[cfg_attr(not(all(feature = "atomic", feature = "iterator")), allow(dead_code))]
    pub(crate) fn snapshot(bytes: Box<[u8]>) -> Self {
        Self {
            inner: MappedSliceInner::Snapshot(bytes),
        }
    }

    /// Borrow the underlying byte slice.
    #[inline]
    #[must_use]
    pub fn as_slice(&self) -> &[u8] {
        match &self.inner {
            MappedSliceInner::Owned(s) => s,
            // SAFETY: `bytes` was derived from `&_guard[range]` at
            // construction. While `_guard` (a read guard on the
            // mapping's lock) is alive the mapping cannot be unmapped
            // or remapped, and no `&mut` into it can exist, because
            // `resize` and every `&mut`-producing path need the write
            // lock. `_guard` lives exactly as long as `self`, and the
            // returned borrow is tied to `&self`, so it cannot outlive
            // the guard. Atomic views also hold read guards, but the
            // `PlainReg` stored next to the guard makes the registry
            // refuse any atomic view overlapping these bytes for as long
            // as the slice lives (and the slice was only created because
            // no such view existed), so nothing stores to them.
            MappedSliceInner::Guarded { bytes, .. } => unsafe { &**bytes },
            MappedSliceInner::Snapshot(b) => b,
        }
    }

    /// Length of the slice in bytes.
    #[inline]
    #[must_use]
    pub fn len(&self) -> usize {
        self.as_slice().len()
    }

    /// Whether the slice is empty.
    #[inline]
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl std::ops::Deref for MappedSlice<'_> {
    type Target = [u8];

    #[inline]
    fn deref(&self) -> &[u8] {
        self.as_slice()
    }
}

impl AsRef<[u8]> for MappedSlice<'_> {
    #[inline]
    fn as_ref(&self) -> &[u8] {
        self.as_slice()
    }
}

/// Conversion into `bytes::Bytes`. Copies the slice into a fresh
/// `Bytes` (one allocation + memcpy). The resulting `Bytes` outlives
/// the mapping borrow because it owns its data; this is the point
/// of the conversion for handing data into async networking code.
///
/// Available with `feature = "bytes"`.
#[cfg(feature = "bytes")]
impl From<MappedSlice<'_>> for bytes::Bytes {
    fn from(slice: MappedSlice<'_>) -> Self {
        Self::copy_from_slice(slice.as_slice())
    }
}

/// Borrowing version of the `Bytes` conversion. Identical cost
/// (one allocation + memcpy) but takes a reference so the caller
/// can keep using the `MappedSlice` after the conversion.
#[cfg(feature = "bytes")]
impl From<&MappedSlice<'_>> for bytes::Bytes {
    fn from(slice: &MappedSlice<'_>) -> Self {
        Self::copy_from_slice(slice.as_slice())
    }
}

/// `io::Read` + `io::Seek` + `io::BufRead` cursor over a memory-mapped
/// file.
///
/// Constructed via [`MemoryMappedFile::reader`]. Each `read` call
/// delegates to `read_into`, which is bounds-checked. EOF is
/// signalled by a zero-length read.
///
/// The cursor borrows the mapping; multiple cursors can coexist
/// and read concurrently on the same mapping.
///
/// # `BufRead` (since 1.1.0)
///
/// - **`ReadOnly` mappings**: [`fill_buf`](std::io::BufRead::fill_buf)
///   returns the rest of the mapping from the current position,
///   zero-copy (the buffer is the mapped memory, which nothing can
///   change), so `lines()`, `read_until` and `split` never copy into an
///   intermediate buffer.
/// - **`ReadWrite` and `CopyOnWrite` mappings**: the mapped bytes can
///   be written, and lending them as `&[u8]` would require the reader
///   to hold a read guard between calls (blocking every writer, and
///   deadlocking a write on the reader's thread). Instead `fill_buf`
///   copies up to 4 KiB into a buffer inside the reader, through
///   `read_into` (so bytes under a live atomic view are read with
///   atomic loads), and holds no lock between calls. The buffered bytes
///   are a snapshot taken when the buffer was filled, as with
///   `std::io::BufReader` over a file.
///
/// The reader holds no lock and owns no heap memory, so it can be
/// dropped or forgotten at any point. `read`, `seek` and
/// `set_position` discard buffered bytes.
///
/// # Example
///
/// ```
/// use std::io::BufRead;
/// use mmap_io::MemoryMappedFile;
///
/// let dir = tempfile::tempdir()?;
/// let path = dir.path().join("log.txt");
/// std::fs::write(&path, "ok\nERROR disk\nok\n")?;
/// let log = MemoryMappedFile::open_ro(&path)?;
/// let errors: Vec<String> = log
///     .reader()
///     .lines()
///     .filter_map(Result::ok)
///     .filter(|l| l.starts_with("ERROR"))
///     .collect();
/// assert_eq!(errors, ["ERROR disk"]);
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub struct MmapReader<'a> {
    mmap: &'a MemoryMappedFile,
    pos: u64,
    /// `BufRead` buffer for writable mappings: `buf[..buf_len]` holds
    /// the file bytes starting at `buf_start`. Inline (no heap) so the
    /// reader keeps no drop glue: dropping it is a no-op, exactly as in
    /// 1.0, and borrows of the mapping end at its last use.
    buf: [u8; READER_BUF_LEN],
    buf_start: u64,
    buf_len: usize,
}

/// Size of the inline `BufRead` buffer for writable mappings.
const READER_BUF_LEN: usize = 4096;

impl<'a> std::io::Read for MmapReader<'a> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        // `read` copies fresh bytes; drop any `fill_buf` snapshot.
        self.buf_len = 0;
        let total = self.mmap.len();
        if self.pos >= total {
            return Ok(0); // EOF
        }
        let remaining = total - self.pos;
        let want = u64::min(buf.len() as u64, remaining) as usize;
        if want == 0 {
            return Ok(0);
        }
        self.mmap
            .read_into(self.pos, &mut buf[..want])
            .map_err(std::io::Error::other)?;
        self.pos += want as u64;
        Ok(want)
    }
}

/// Same contract as `std::io::Cursor`: seeking past the end is allowed
/// (the next `read` returns 0), seeking to a negative position or past
/// `u64::MAX` returns `ErrorKind::InvalidInput` and leaves the position
/// unchanged.
impl<'a> std::io::Seek for MmapReader<'a> {
    fn seek(&mut self, pos: std::io::SeekFrom) -> std::io::Result<u64> {
        use std::io::SeekFrom;
        self.buf_len = 0;
        let (base, delta) = match pos {
            SeekFrom::Start(n) => {
                self.pos = n;
                return Ok(n);
            }
            SeekFrom::End(delta) => (self.mmap.len(), delta),
            SeekFrom::Current(delta) => (self.pos, delta),
        };
        let new_pos = if delta >= 0 {
            base.checked_add(delta.unsigned_abs())
        } else {
            base.checked_sub(delta.unsigned_abs())
        };
        match new_pos {
            Some(p) => {
                self.pos = p;
                Ok(p)
            }
            None => Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "invalid seek to a negative or overflowing position",
            )),
        }
    }
}

impl<'a> MmapReader<'a> {
    /// Current cursor position in bytes from the start of the file.
    #[must_use]
    pub fn position(&self) -> u64 {
        self.pos
    }

    /// Set the cursor position directly (no validation; out-of-range
    /// positions are clamped at the next `read` call which returns
    /// EOF). Discards any buffered `BufRead` bytes.
    pub fn set_position(&mut self, pos: u64) {
        self.buf_len = 0;
        self.pos = pos;
    }
}

/// `BufRead` (since 1.1.0): zero-copy on `ReadOnly` mappings, a 4 KiB
/// inline copy on writable ones. See [`MmapReader`].
impl<'a> std::io::BufRead for MmapReader<'a> {
    fn fill_buf(&mut self) -> std::io::Result<&[u8]> {
        if let MapVariant::Ro(m) = &self.mmap.inner.map {
            // Immutable mapping: lend the remaining bytes directly.
            let start = usize::try_from(self.pos).unwrap_or(usize::MAX).min(m.len());
            return Ok(&m[start..]);
        }
        let buffered_end = self.buf_start + self.buf_len as u64;
        if self.buf_len == 0 || self.pos < self.buf_start || self.pos >= buffered_end {
            self.buf_len = 0;
            let total = self.mmap.len();
            if self.pos >= total {
                return Ok(&[]);
            }
            let n = (total - self.pos).min(READER_BUF_LEN as u64) as usize;
            self.mmap
                .read_into(self.pos, &mut self.buf[..n])
                .map_err(std::io::Error::other)?;
            self.buf_start = self.pos;
            self.buf_len = n;
        }
        // `buf_start <= pos < buf_start + buf_len`, so the offset is in
        // the buffer.
        let from = (self.pos - self.buf_start) as usize;
        Ok(&self.buf[from..self.buf_len])
    }

    fn consume(&mut self, amt: usize) {
        // `amt` must not exceed what `fill_buf` returned; a larger value
        // only moves the cursor further (like `Cursor`), never past
        // `u64::MAX`.
        self.pos = self.pos.saturating_add(amt as u64);
    }
}

// `AsFd` / `AsRawFd` (Unix) and `AsHandle` / `AsRawHandle` (Windows)
// expose the underlying OS handle to callers that need to hand it
// to a C library, the `nix` crate, `rustix`, `polling`, etc., without
// going through `unmap`.

#[cfg(unix)]
impl std::os::fd::AsFd for MemoryMappedFile {
    fn as_fd(&self) -> std::os::fd::BorrowedFd<'_> {
        self.inner.file.as_fd()
    }
}

#[cfg(unix)]
impl std::os::fd::AsRawFd for MemoryMappedFile {
    fn as_raw_fd(&self) -> std::os::fd::RawFd {
        std::os::fd::AsRawFd::as_raw_fd(&self.inner.file)
    }
}

#[cfg(windows)]
impl std::os::windows::io::AsHandle for MemoryMappedFile {
    fn as_handle(&self) -> std::os::windows::io::BorrowedHandle<'_> {
        self.inner.file.as_handle()
    }
}

#[cfg(windows)]
impl std::os::windows::io::AsRawHandle for MemoryMappedFile {
    fn as_raw_handle(&self) -> std::os::windows::io::RawHandle {
        std::os::windows::io::AsRawHandle::as_raw_handle(&self.inner.file)
    }
}

impl std::fmt::Debug for MappedSlice<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Forward to byte-slice Debug so callers can use the wrapper
        // with `assert_eq!` and `dbg!` without losing readability.
        std::fmt::Debug::fmt(self.as_slice(), f)
    }
}

impl PartialEq for MappedSlice<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.as_slice() == other.as_slice()
    }
}

impl Eq for MappedSlice<'_> {}

impl PartialEq<[u8]> for MappedSlice<'_> {
    fn eq(&self, other: &[u8]) -> bool {
        self.as_slice() == other
    }
}

impl PartialEq<&[u8]> for MappedSlice<'_> {
    fn eq(&self, other: &&[u8]) -> bool {
        self.as_slice() == *other
    }
}

impl<const N: usize> PartialEq<[u8; N]> for MappedSlice<'_> {
    fn eq(&self, other: &[u8; N]) -> bool {
        self.as_slice() == other.as_slice()
    }
}

impl<const N: usize> PartialEq<&[u8; N]> for MappedSlice<'_> {
    fn eq(&self, other: &&[u8; N]) -> bool {
        self.as_slice() == other.as_slice()
    }
}
