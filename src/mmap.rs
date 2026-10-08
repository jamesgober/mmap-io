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
    /// Touch pages lazily on first access (same as Never for now).
    Lazy,
}

/// Low-level memory-mapped file abstraction with safe, concurrent access.
use std::{
    fs::{File, OpenOptions},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    sync::Arc,
};

use memmap2::{Mmap, MmapMut, MmapOptions};

use crate::flush::FlushPolicy;

use parking_lot::RwLock;

use crate::errors::{MmapIoError, Result};
use crate::utils::{ensure_in_bounds, slice_range};

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
    /// Copy-on-Write mapping (private). Writes affect this mapping only; the underlying file remains unchanged.
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
    // Flush policy and accounting (RW only)
    pub(crate) flush_policy: FlushPolicy,
    pub(crate) written_since_last_flush: RwLock<u64>,
    // Time-based flusher background thread (used only when
    // FlushPolicy::EveryMillis is selected on the builder path). Held
    // here so the worker thread's lifetime is bound to the mapping;
    // Drop signals shutdown. See C2 fix in .dev/AUDIT.md.
    pub(crate) flusher: RwLock<Option<crate::flush::TimeBasedFlusher>>,
    // Huge pages preference (builder-set), effective on supported platforms
    #[cfg(feature = "hugepages")]
    pub(crate) huge_pages: bool,
}

#[doc(hidden)]
pub enum MapVariant {
    Ro(Mmap),
    Rw(RwLock<MmapMut>),
    /// Private, per-process copy-on-write mapping. Underlying file is not modified by writes.
    Cow(Mmap),
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
        // SAFETY: `MmapMut::map_mut` is `unsafe` because the OS does
        // not prevent another process from concurrently modifying the
        // backing file under the mapping, which would violate Rust's
        // aliasing model if anyone holds a `&mut [u8]` into the
        // mapping. We do not enforce single-writer at the OS level;
        // callers who share the file across processes are responsible
        // for synchronization (the crate documents this in REPS.md
        // section 5.1). Within this process, all mutable access to
        // the `MmapMut` is mediated by `parking_lot::RwLock`, so the
        // standard aliasing rules hold for intra-process access.
        // The file has just been created and `set_len(size)` succeeded,
        // so the kernel will produce a mapping of exactly `size` bytes.
        // Note: `create_rw` convenience ignores huge pages; use builder
        // for that.
        // Reference: https://docs.rs/memmap2/latest/memmap2/struct.MmapMut.html#method.map_mut
        let mmap = unsafe { MmapMut::map_mut(&file)? };
        let inner = Inner {
            path: path_ref.to_path_buf(),
            file,
            mode: MmapMode::ReadWrite,
            cached_len: AtomicU64::new(size),
            map: MapVariant::Rw(RwLock::new(mmap)),
            flush_policy: FlushPolicy::default(),
            written_since_last_flush: RwLock::new(0),
            flusher: RwLock::new(None),
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
        // SAFETY: `Mmap::map` is `unsafe` for the same cross-process
        // reason as `MmapMut::map_mut` (see `create_rw` above): the OS
        // does not prevent another process from writing to the backing
        // file. For a read-only mapping the in-process aliasing
        // hazard is reduced because we never hand out `&mut [u8]` into
        // the mapping, but cross-process modification can still cause
        // a race that surfaces as a torn read. This is documented as
        // out-of-scope; intra-process access is sound.
        // Reference: https://docs.rs/memmap2/latest/memmap2/struct.Mmap.html#method.map
        let mmap = unsafe { Mmap::map(&file)? };
        let inner = Inner {
            path: path_ref.to_path_buf(),
            file,
            mode: MmapMode::ReadOnly,
            cached_len: AtomicU64::new(len),
            map: MapVariant::Ro(mmap),
            flush_policy: FlushPolicy::Never,
            written_since_last_flush: RwLock::new(0),
            flusher: RwLock::new(None),
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
        // calling `MmapMut::map_mut`. Additionally, we have verified
        // here that the file is not zero-length (`len != 0`), which
        // avoids `EINVAL` from `mmap(2)` on Linux for zero-length
        // mappings. Note: `open_rw` convenience ignores huge pages;
        // use the builder for that.
        // Reference: https://docs.rs/memmap2/latest/memmap2/struct.MmapMut.html#method.map_mut
        let mmap = unsafe { MmapMut::map_mut(&file)? };
        let inner = Inner {
            path: path_ref.to_path_buf(),
            file,
            mode: MmapMode::ReadWrite,
            cached_len: AtomicU64::new(len),
            map: MapVariant::Rw(RwLock::new(mmap)),
            flush_policy: FlushPolicy::default(),
            written_since_last_flush: RwLock::new(0),
            flusher: RwLock::new(None),
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
    /// For RW mappings the slice holds an internal read guard for its
    /// lifetime. Other readers are not blocked, but every operation
    /// that needs the write lock is: `resize()`, `update_region()`,
    /// `as_slice_mut()`, and `chunks_mut()` wait until the slice is
    /// dropped, whatever region they touch. Calling one of those on
    /// the thread that holds the slice deadlocks; drop the slice
    /// first. Taking more read views on the same thread is fine.
    ///
    /// # Performance
    ///
    /// - **Time Complexity**: O(1) - direct pointer access
    /// - **Memory Usage**: No additional allocation (zero-copy)
    /// - **Cache Behavior**: May trigger page faults on first access
    ///
    /// # Errors
    ///
    /// Returns [`MmapIoError::OutOfBounds`] if `offset + len` exceeds
    /// the file's current length.
    pub fn as_slice(&self, offset: u64, len: u64) -> Result<MappedSlice<'_>> {
        let map = self.map_read();
        let (start, end) = slice_range(offset, len, map.len() as u64)?;
        Ok(map.into_slice(start..end))
    }

    /// Migration shim that mirrors the 0.9.6 `as_slice` signature:
    /// returns `Result<&[u8]>` directly for `ReadOnly` and
    /// `CopyOnWrite` mappings, and `MmapIoError::InvalidMode` for
    /// `ReadWrite` mappings.
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
    /// Returns `MmapIoError::InvalidMode` on `ReadWrite` mappings
    /// (matching the 0.9.6 behavior; use `as_slice` or `read_into`
    /// for RW).
    pub fn as_slice_bytes(&self, offset: u64, len: u64) -> Result<&[u8]> {
        match &self.inner.map {
            MapVariant::Ro(m) | MapVariant::Cow(m) => {
                let (start, end) = slice_range(offset, len, m.len() as u64)?;
                Ok(&m[start..end])
            }
            MapVariant::Rw(_) => Err(MmapIoError::InvalidMode(
                "use as_slice() for RW mappings; as_slice_bytes is the 0.9.6 compat shim and supports RO/COW only",
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
        let slice = self.as_slice(offset, len)?;
        Ok(bytes::Bytes::copy_from_slice(slice.as_slice()))
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
    /// zero-length `read` return).
    #[must_use]
    pub fn reader(&self) -> MmapReader<'_> {
        MmapReader { mmap: self, pos: 0 }
    }

    /// Get a zero-copy mutable slice for the given [offset, offset+len).
    /// Only available in `ReadWrite` mode.
    ///
    /// The returned guard holds the mapping's write lock for its
    /// lifetime: every other reader and writer (on any region) waits
    /// until it is dropped. Calling this while the same thread holds a
    /// [`MappedSlice`], an iterator item, or an atomic view of this
    /// mapping deadlocks.
    ///
    /// # Errors
    ///
    /// Returns `MmapIoError::InvalidMode` if not in `ReadWrite` mode.
    /// Returns `MmapIoError::OutOfBounds` if range exceeds file bounds.
    pub fn as_slice_mut(&self, offset: u64, len: u64) -> Result<MappedSliceMut<'_>> {
        match &self.inner.map {
            MapVariant::Ro(_) => Err(MmapIoError::InvalidMode(
                "mutable access on read-only mapping",
            )),
            MapVariant::Rw(lock) => {
                let guard = lock.write();
                let (start, end) = slice_range(offset, len, guard.len() as u64)?;
                Ok(MappedSliceMut {
                    guard,
                    range: start..end,
                })
            }
            // COW mappings are exposed read-only; see `open_cow`.
            MapVariant::Cow(_) => Err(MmapIoError::InvalidMode(
                "mutable access on copy-on-write mapping (read-only)",
            )),
        }
    }

    /// Copy the provided bytes into the mapped file at the given offset.
    /// Bounds-checked, zero-copy write.
    ///
    /// Takes the mapping's write lock for the duration of the copy, so
    /// it waits for every live [`MappedSlice`], iterator item, and
    /// atomic view to be dropped, whatever region they cover. Calling
    /// it on a thread that holds one of those deadlocks.
    ///
    /// # Performance
    ///
    /// - **Time Complexity**: O(n) where n is data.len()
    /// - **Memory Usage**: No additional allocation
    /// - **I/O Operations**: May trigger flush based on flush policy
    ///
    /// # Errors
    ///
    /// Returns `MmapIoError::InvalidMode` if not in `ReadWrite` mode.
    /// Returns `MmapIoError::OutOfBounds` if range exceeds file bounds.
    pub fn update_region(&self, offset: u64, data: &[u8]) -> Result<()> {
        if data.is_empty() {
            return Ok(());
        }
        if self.inner.mode != MmapMode::ReadWrite {
            return Err(MmapIoError::InvalidMode(
                "Update region requires ReadWrite mode.",
            ));
        }
        let len = data.len() as u64;
        match &self.inner.map {
            MapVariant::Ro(_) => Err(MmapIoError::InvalidMode(
                "Cannot write to read-only mapping",
            )),
            MapVariant::Rw(lock) => {
                {
                    let mut guard = lock.write();
                    let (start, end) = slice_range(offset, len, guard.len() as u64)?;
                    guard[start..end].copy_from_slice(data);
                }
                // Apply flush policy after releasing the write lock;
                // flushing only needs a read guard.
                self.apply_flush_policy(len)?;
                Ok(())
            }
            MapVariant::Cow(_) => Err(MmapIoError::InvalidMode(
                "Cannot write to copy-on-write mapping (read-only)",
            )),
        }
    }

    /// Async write that enforces Async-Only Flushing semantics: always flush after write.
    ///
    /// Runs the underlying sync write + flush on the `blocking`
    /// crate's thread pool so the async scheduler is not stuck on
    /// disk I/O. `blocking` works on every async executor (tokio,
    /// smol, async-std), so callers are no longer locked into
    /// tokio (since 0.9.11).
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

    /// Flush changes to disk. For read-only mappings, this is a no-op.
    ///
    /// Smart internal guards:
    /// - Skip I/O when there are no pending writes (accumulator is zero)
    /// - On Linux, use msync(MS_ASYNC) as a cheaper hint; fall back to full flush on error
    ///
    /// # Performance
    ///
    /// - **Time Complexity**: O(n) where n is the size of dirty pages
    /// - **I/O Operations**: Triggers disk write of modified pages
    /// - **Optimization**: Skips flush if no writes since last flush
    /// - **Platform**: Linux uses async msync for better performance
    ///
    /// # Errors
    ///
    /// Returns `MmapIoError::FlushFailed` if flush operation fails.
    pub fn flush(&self) -> Result<()> {
        match &self.inner.map {
            MapVariant::Ro(_) => Ok(()),
            MapVariant::Cow(_) => Ok(()), // no-op for COW
            MapVariant::Rw(lock) => {
                // Fast path: no pending writes => skip flushing I/O
                if *self.inner.written_since_last_flush.read() == 0 {
                    return Ok(());
                }

                // Platform-optimized path: Linux MS_ASYNC best-effort
                #[cfg(all(unix, target_os = "linux"))]
                {
                    if let Ok(len) = self.current_len() {
                        if len > 0 && self.try_linux_async_flush(len as usize)? {
                            return Ok(());
                        }
                    }
                }

                // Fallback/full flush using memmap2 API
                let guard = lock.read();
                guard
                    .flush()
                    .map_err(|e| MmapIoError::FlushFailed(e.to_string()))?;
                // Reset accumulator after a successful flush
                *self.inner.written_since_last_flush.write() = 0;
                Ok(())
            }
        }
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

    /// Flush a specific byte range to disk.
    ///
    /// Smart internal guards:
    /// - Skip I/O when there are no pending writes in accumulator
    /// - Optimize microflushes (< page size) with page-aligned batching
    /// - On Linux, prefer msync(MS_ASYNC) for the range; fall back to full range flush on error
    ///
    /// # Performance Optimizations
    ///
    /// - **Microflush Detection**: Ranges smaller than page size are batched
    /// - **Page Alignment**: Small ranges are expanded to page boundaries
    /// - **Async Hints**: Linux uses MS_ASYNC for better performance
    /// - **Zero-Copy**: No data copying during flush operations
    ///
    /// # Errors
    ///
    /// Returns `MmapIoError::OutOfBounds` if range exceeds file bounds.
    /// Returns `MmapIoError::FlushFailed` if flush operation fails.
    pub fn flush_range(&self, offset: u64, len: u64) -> Result<()> {
        if len == 0 {
            return Ok(());
        }
        ensure_in_bounds(offset, len, self.current_len()?)?;
        match &self.inner.map {
            MapVariant::Ro(_) => Ok(()),
            MapVariant::Cow(_) => Ok(()), // no-op for COW
            MapVariant::Rw(lock) => {
                // If we have no accumulated writes, skip I/O
                if *self.inner.written_since_last_flush.read() == 0 {
                    return Ok(());
                }

                let (start, end) = slice_range(offset, len, self.current_len()?)?;
                let range_len = end - start;

                // Microflush optimization: For small ranges, align to page boundaries
                // to reduce syscall overhead and improve cache locality
                let (optimized_start, optimized_len) = if range_len < crate::utils::page_size() {
                    use crate::utils::{align_up, page_size};
                    let page_sz = page_size();
                    let aligned_start = (start / page_sz) * page_sz;
                    let aligned_end = align_up(end as u64, page_sz as u64) as usize;
                    let file_len = self.current_len()? as usize;
                    let bounded_end = std::cmp::min(aligned_end, file_len);
                    let bounded_len = bounded_end.saturating_sub(aligned_start);
                    (aligned_start, bounded_len)
                } else {
                    (start, range_len)
                };

                // Linux MS_ASYNC optimization
                #[cfg(all(unix, target_os = "linux"))]
                {
                    // SAFETY: `optimized_start` is within the mapped region (bounded above
                    // by `file_len` via the microflush calculation, or by the validated
                    // `start` from `slice_range` on the non-micro path), so `base.add(...)`
                    // produces a pointer inside the mapping. `msync` (POSIX) on a valid
                    // pointer + length within a mapped region with MS_ASYNC schedules an
                    // asynchronous writeback and does not access the memory after the call
                    // returns. Reference: https://man7.org/linux/man-pages/man2/msync.2.html
                    let msync_res: i32 = {
                        let guard = lock.read();
                        let base = guard.as_ptr();
                        let ptr = unsafe { base.add(optimized_start) } as *mut libc::c_void;
                        unsafe { libc::msync(ptr, optimized_len, libc::MS_ASYNC) }
                    };
                    if msync_res == 0 {
                        // C1 fix: a range flush must NOT zero the global accumulator.
                        // Debit by the bytes actually flushed (clamped at zero) so the
                        // FlushPolicy threshold tracking remains accurate for the
                        // unflushed pages. See .dev/AUDIT.md C1.
                        let mut acc = self.inner.written_since_last_flush.write();
                        *acc = acc.saturating_sub(optimized_len as u64);
                        return Ok(());
                    }
                    // else fall through to full flush_range
                }

                let guard = lock.read();
                guard
                    .flush_range(optimized_start, optimized_len)
                    .map_err(|e| MmapIoError::FlushFailed(e.to_string()))?;
                // C1 fix: same debit logic as the MS_ASYNC path above.
                let mut acc = self.inner.written_since_last_flush.write();
                *acc = acc.saturating_sub(optimized_len as u64);
                Ok(())
            }
        }
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

    /// Grow the file and remap. Caller holds the write lock.
    fn grow_locked(
        &self,
        map: &mut MmapMut,
        current_size: u64,
        new_size: u64,
        new_len: usize,
    ) -> Result<()> {
        // Extend first: mapping past end-of-file is invalid. The old
        // mapping stays valid because it only covers the old length.
        self.inner.file.set_len(new_size)?;
        match map_file_rw(&self.inner.file, new_len) {
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
        map: &mut MmapMut,
        _current_len: usize,
        new_size: u64,
        new_len: usize,
    ) -> Result<()> {
        // Map the surviving prefix first so a mapping failure leaves
        // the file and the old mapping untouched. Then truncate; the
        // old mapping's tail now lies past end-of-file, but nothing
        // can touch it: we hold the write lock and replace it next.
        let new_map = map_file_rw(&self.inner.file, new_len)?;
        self.inner.file.set_len(new_size)?;
        *map = new_map;
        Ok(())
    }

    /// Truncate the file and remap. Caller holds the write lock.
    #[cfg(windows)]
    fn shrink_locked(
        &self,
        map: &mut MmapMut,
        current_len: usize,
        new_size: u64,
        new_len: usize,
    ) -> Result<()> {
        // Windows rejects SetEndOfFile on a file with a mapped view
        // (ERROR_USER_MAPPED_FILE, os error 1224), so unmap our view
        // first by swapping in an empty anonymous placeholder.
        drop(std::mem::replace(map, MmapMut::map_anon(0)?));
        if let Err(e) = self.inner.file.set_len(new_size) {
            // File unchanged: restore the mapping at its old length.
            *map = map_file_rw(&self.inner.file, current_len)?;
            return Err(e.into());
        }
        *map = map_file_rw(&self.inner.file, new_len)?;
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
        touch_range_with_ptr(map.as_ptr(), 0, map.len(), crate::utils::page_size());
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
        touch_range_with_ptr(map.as_ptr(), first_page, end - first_page, page_sz);
        Ok(())
    }
}

// 0.9.8 ergonomic and introspection surface.
//
// These methods are additive (no breaking changes) and were tracked
// under audit IDs E1, E2, E6, E7, F2, F5, F9.
impl MemoryMappedFile {
    /// Open `path` for read-write, creating it at `default_size` if
    /// it does not exist. Convenience for the common pattern of
    /// `if path.exists() { open_rw } else { create_rw }`.
    ///
    /// If the file already exists, `default_size` is **ignored** and
    /// the file is opened at its current length. Use [`resize`](Self::resize)
    /// afterward if you need to change the size.
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
    /// (only checked on the create path; existing files of any size
    /// are accepted).
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
        let p = path.as_ref();
        if p.exists() {
            Self::open_rw(p)
        } else {
            Self::create_rw(p, default_size)
        }
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
                let mmap = unsafe { Mmap::map(&file)? };
                let inner = Inner {
                    path: path_ref,
                    file,
                    mode,
                    cached_len: AtomicU64::new(len),
                    map: MapVariant::Ro(mmap),
                    flush_policy: FlushPolicy::Never,
                    written_since_last_flush: RwLock::new(0),
                    flusher: RwLock::new(None),
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
                let mmap = unsafe { MmapMut::map_mut(&file)? };
                let inner = Inner {
                    path: path_ref,
                    file,
                    mode,
                    cached_len: AtomicU64::new(len),
                    map: MapVariant::Rw(RwLock::new(mmap)),
                    flush_policy: FlushPolicy::default(),
                    written_since_last_flush: RwLock::new(0),
                    flusher: RwLock::new(None),
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
                // SAFETY: see `open_cow`.
                let mmap = unsafe {
                    let mut opts = MmapOptions::new();
                    opts.len(len as usize);
                    opts.map(&file)?
                };
                let inner = Inner {
                    path: path_ref,
                    file,
                    mode,
                    cached_len: AtomicU64::new(len),
                    map: MapVariant::Cow(mmap),
                    flush_policy: FlushPolicy::Never,
                    written_since_last_flush: RwLock::new(0),
                    flusher: RwLock::new(None),
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

    /// Bytes written since the last successful flush. Mainly useful
    /// for diagnostics / observability under
    /// [`FlushPolicy::EveryBytes`] and
    /// [`FlushPolicy::EveryWrites`]: callers can poll this to see
    /// how close they are to the next auto-flush.
    ///
    /// Reads only the accumulator (one atomic read of a `u64` under
    /// the parking_lot read lock); no I/O is performed.
    #[inline]
    #[must_use]
    pub fn pending_bytes(&self) -> u64 {
        *self.inner.written_since_last_flush.read()
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
            MapVariant::Rw(lock) => lock.read_recursive().as_ptr(),
            MapVariant::Cow(m) => m.as_ptr(),
        }
    }

    /// Raw mutable pointer to the start of the mapped region.
    /// Available only on `ReadWrite` mappings.
    ///
    /// See [`as_ptr`](Self::as_ptr) for the safety contract; the
    /// same rules apply, plus the caller MUST NOT alias this
    /// pointer with any live Rust `&` reference to the same bytes
    /// (a [`MappedSlice`] would alias).
    ///
    /// # Errors
    ///
    /// Returns [`MmapIoError::InvalidMode`] if the mapping is not
    /// `ReadWrite`.
    ///
    /// # Safety
    ///
    /// Same as [`as_ptr`](Self::as_ptr), plus the no-aliasing rule
    /// above.
    pub unsafe fn as_mut_ptr(&self) -> Result<*mut u8> {
        match &self.inner.map {
            // A read guard is enough to read the base address and does
            // not deadlock when this thread already holds a view. The
            // pointer comes from memmap2's raw mapping pointer (not
            // from a `&[u8]`), so writing through it is permitted.
            MapVariant::Rw(lock) => Ok(lock.read_recursive().as_ptr().cast_mut()),
            MapVariant::Ro(_) | MapVariant::Cow(_) => Err(MmapIoError::InvalidMode(
                "as_mut_ptr requires ReadWrite mode",
            )),
        }
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
/// For RW mappings this holds a recursive read guard, so the mapping
/// cannot be remapped (by `resize`) or written while it lives, and a
/// thread that already holds a view does not deadlock behind a queued
/// writer. RO and COW mappings are never remapped, so a plain borrow
/// is enough. Range checks against `len()` of this value are checks
/// against the mapping actually being accessed.
pub(crate) enum MapRead<'a> {
    /// RO / COW mapping.
    Shared(&'a [u8]),
    /// RW mapping, read-locked.
    Guarded(RwLockReadGuard<'a, MmapMut>),
}

impl<'a> MapRead<'a> {
    /// Turn this access into a `MappedSlice` over `range`. The range
    /// must have been validated against `self.len()`.
    pub(crate) fn into_slice(self, range: std::ops::Range<usize>) -> MappedSlice<'a> {
        match self {
            MapRead::Shared(s) => MappedSlice::owned(&s[range]),
            MapRead::Guarded(g) => MappedSlice::guarded(g, range),
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
            MapVariant::Ro(m) | MapVariant::Cow(m) => MapRead::Shared(m),
            MapVariant::Rw(lock) => MapRead::Guarded(lock.read_recursive()),
        }
    }
}

/// Map `len` bytes of `file` read-write from offset 0.
fn map_file_rw(file: &File, len: usize) -> Result<MmapMut> {
    // SAFETY: `MmapOptions::map_mut` is `unsafe` because the OS does
    // not stop another process from modifying the file under the
    // mapping (see `create_rw`). Callers pass a `len` no larger than
    // the file's current length, so every mapped page is backed by
    // the file. Within this process all access to the returned
    // mapping goes through the `RwLock` in `MapVariant::Rw`.
    // Reference: https://docs.rs/memmap2/latest/memmap2/struct.MmapOptions.html#method.map_mut
    let map = unsafe { MmapOptions::new().len(len).map_mut(file)? };
    Ok(map)
}

impl MemoryMappedFile {
    // Helper method to attempt Linux-specific async flush
    #[cfg(all(unix, target_os = "linux"))]
    fn try_linux_async_flush(&self, len: usize) -> Result<bool> {
        use std::os::fd::AsRawFd;

        // Get the file descriptor (unused but kept for potential future use)
        let _fd = self.inner.file.as_raw_fd();

        // Try to get the mapping pointer for msync
        match &self.inner.map {
            MapVariant::Rw(lock) => {
                let guard = lock.read();
                let ptr = guard.as_ptr() as *mut libc::c_void;

                // SAFETY: POSIX `msync` requires:
                //   1. `addr` is page-aligned. `guard.as_ptr()` returns
                //      the base of the mapping, which the kernel
                //      page-aligned at `mmap(2)` time.
                //   2. `[addr, addr + len)` lies within a mapped region.
                //      `len` is `self.current_len()` which equals the
                //      mapping length at the time the read guard was
                //      acquired (the guard prevents `resize` from
                //      shrinking the mapping under us).
                //   3. `flags` is a valid combination. `MS_ASYNC` is a
                //      defined Linux/POSIX flag that schedules
                //      asynchronous writeback and returns immediately.
                // `msync` does not access the memory at `ptr` from
                // Rust's perspective; it queues a kernel writeback. The
                // pointer is not retained past the call.
                // Reference: https://man7.org/linux/man-pages/man2/msync.2.html
                let ret = unsafe { libc::msync(ptr, len, libc::MS_ASYNC) };

                if ret == 0 {
                    // MS_ASYNC succeeded, reset accumulator
                    *self.inner.written_since_last_flush.write() = 0;
                    Ok(true)
                } else {
                    // Fall back to full flush
                    Ok(false)
                }
            }
            _ => Ok(false),
        }
    }
}

// (Removed duplicate import of RwLockWriteGuard)

/// Create a memory mapping with optional huge pages support.
///
/// When `huge` is true on Linux, this function attempts to use actual huge pages
/// via MAP_HUGETLB first, falling back to Transparent Huge Pages (THP) if that fails.
///
/// **Fallback Behavior**: This is a best-effort optimization:
/// 1. First attempts MAP_HUGETLB for guaranteed huge pages
/// 2. Falls back to regular mapping with MADV_HUGEPAGE hint
/// 3. Finally falls back to regular pages if THP is unavailable
///
/// The function will silently fall back through these options and never fail
/// due to huge page unavailability alone.
#[cfg(feature = "hugepages")]
fn map_mut_with_options(file: &File, len: u64, huge: bool) -> Result<MmapMut> {
    #[cfg(all(unix, target_os = "linux"))]
    {
        if huge {
            // First, try to create a mapping that can accommodate huge pages
            // by aligning to huge page boundaries
            if let Ok(mmap) = try_create_optimized_mapping(file, len) {
                log::debug!("Successfully created optimized mapping for huge pages");
                return Ok(mmap);
            }
        }

        // SAFETY: see `create_rw` for the general justification of
        // calling `MmapMut::map_mut`. This call path is the
        // hugepages-feature fallback after the optimized hugepage map
        // attempt failed; it always maps a regular-page mapping of the
        // file as-is.
        let mmap = unsafe { MmapMut::map_mut(file) }.map_err(MmapIoError::Io)?;

        if huge {
            // Request Transparent Huge Pages (THP) for existing mapping
            // This is a hint to the kernel - not a guarantee
            // SAFETY: `madvise(addr, length, MADV_HUGEPAGE)` requires
            // the same range preconditions as the generic `madvise`
            // path in `advise.rs`: `addr` page-aligned and the range
            // within a mapped region. Both are satisfied here:
            // `mmap.as_ptr()` is the kernel-aligned base of the
            // freshly created mapping, and `len` is exactly the
            // mapping length. `MADV_HUGEPAGE` is a Linux extension
            // that hints the kernel to back the region with huge
            // pages; it does not access the memory contents.
            // Reference: https://man7.org/linux/man-pages/man2/madvise.2.html
            unsafe {
                let mmap_ptr = mmap.as_ptr() as *mut libc::c_void;

                // MADV_HUGEPAGE: Enable THP for this memory region
                let ret = libc::madvise(mmap_ptr, len as usize, libc::MADV_HUGEPAGE);

                if ret == 0 {
                    log::debug!("Successfully requested THP for {} bytes", len);
                } else {
                    log::debug!("madvise(MADV_HUGEPAGE) failed, using regular pages");
                }
            }
        }

        Ok(mmap)
    }
    #[cfg(not(all(unix, target_os = "linux")))]
    {
        // Huge pages are Linux-specific, ignore the flag on other platforms
        let _ = (len, huge);
        // SAFETY: see `create_rw`. This is the non-Linux fallback;
        // huge pages have no effect outside Linux so we just produce
        // a normal mapping.
        unsafe { MmapMut::map_mut(file) }.map_err(MmapIoError::Io)
    }
}

/// Create an optimized mapping that's more likely to use huge pages.
/// This function tries to create mappings that are aligned and sized
/// appropriately for huge page usage.
#[cfg(all(unix, target_os = "linux", feature = "hugepages"))]
fn try_create_optimized_mapping(file: &File, len: u64) -> Result<MmapMut> {
    // For files larger than 2MB (typical huge page size), we can try to optimize
    const HUGE_PAGE_SIZE: u64 = 2 * 1024 * 1024; // 2MB

    if len >= HUGE_PAGE_SIZE {
        // Create the mapping and immediately advise huge pages
        // SAFETY: see `create_rw` for the general `MmapMut::map_mut`
        // contract. This is the first-tier hugepages attempt: we map
        // the file normally then immediately request THP backing
        // before any access touches the pages.
        let mmap = unsafe { MmapMut::map_mut(file) }.map_err(MmapIoError::Io)?;

        // SAFETY: both `madvise` calls below operate on the freshly
        // created mapping's full extent (`mmap_ptr`, `len`). The base
        // is page-aligned by `mmap(2)` construction, and `len` is the
        // mapping length, so `[mmap_ptr, mmap_ptr + len)` is exactly
        // the mapped region. `MADV_HUGEPAGE` and `MADV_POPULATE_WRITE`
        // (a Linux 5.14+ flag, defined locally because libc's constant
        // is gated behind a more recent libc version than our MSRV
        // permits) both operate on the address range without forming
        // a Rust reference; failures are non-fatal hints.
        // References:
        //   https://man7.org/linux/man-pages/man2/madvise.2.html
        //   https://man7.org/linux/man-pages/man2/madvise.2.html (MADV_POPULATE_WRITE)
        unsafe {
            let mmap_ptr = mmap.as_ptr() as *mut libc::c_void;

            // First try MADV_HUGEPAGE
            let ret = libc::madvise(mmap_ptr, len as usize, libc::MADV_HUGEPAGE);

            if ret == 0 {
                // Then try to populate the mapping to encourage huge page allocation
                // MADV_POPULATE_WRITE is relatively new, so we'll use a fallback
                #[cfg(target_os = "linux")]
                {
                    const MADV_POPULATE_WRITE: i32 = 23; // Define the constant manually
                    let populate_ret = libc::madvise(mmap_ptr, len as usize, MADV_POPULATE_WRITE);

                    if populate_ret == 0 {
                        log::debug!("Successfully created and populated optimized mapping");
                    } else {
                        log::debug!(
                            "Optimization successful, populate failed (expected on older kernels)"
                        );
                    }
                }
                #[cfg(not(target_os = "linux"))]
                {
                    log::debug!("Successfully created optimized mapping (populate not available)");
                }
            }
        }

        Ok(mmap)
    } else {
        Err(MmapIoError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "File too small for huge page optimization",
        )))
    }
}

#[cfg(not(all(unix, target_os = "linux", feature = "hugepages")))]
#[allow(dead_code)]
fn try_create_optimized_mapping(_file: &File, _len: u64) -> Result<MmapMut> {
    Err(MmapIoError::Io(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "Huge pages not supported on this platform",
    )))
}

#[cfg(feature = "cow")]
impl MemoryMappedFile {
    /// Open an existing file and memory-map it copy-on-write (private).
    /// Changes through this mapping are visible only within this process; the underlying file remains unchanged.
    ///
    /// # Errors
    ///
    /// Returns [`MmapIoError::Io`] if the file cannot be opened or mapped.
    /// Returns [`MmapIoError::ResizeFailed`] if the file is zero-length.
    pub fn open_cow<P: AsRef<Path>>(path: P) -> Result<Self> {
        let path_ref = path.as_ref();
        let file = OpenOptions::new().read(true).open(path_ref)?;
        let len = file.metadata()?.len();
        if len == 0 {
            return Err(MmapIoError::ResizeFailed(ERR_ZERO_LENGTH_FILE.into()));
        }
        // SAFETY: `MmapOptions::map` carries the same cross-process
        // aliasing hazard as `Mmap::map`: another process modifying
        // the backing file can produce torn reads. The crate marks
        // this as out-of-scope per REPS.md section 5.1. Within this
        // process, no `&mut [u8]` ever points into a COW mapping
        // (phase-1 COW is read-only at the Rust API level), so the
        // aliasing rules are trivially satisfied.
        // `opts.len(len as usize)` constrains the mapping to the file
        // size we just queried; `len > 0` is verified above.
        // Reference: https://docs.rs/memmap2/latest/memmap2/struct.MmapOptions.html#method.map
        let mmap = unsafe {
            let mut opts = MmapOptions::new();
            opts.len(len as usize);
            #[cfg(unix)]
            {
                // memmap2 currently does not expose a stable .private() on all Rust/MSRV combos.
                // On Unix, map() of a read-only file yields an immutable mapping; for COW semantics
                // we rely on platform-specific behavior when writing is disallowed here in phase-1.
                // When writable COW is introduced, we will use platform flags via memmap2 internals.
                opts.map(&file)?
            }
            #[cfg(not(unix))]
            {
                // On Windows, memmap2 maps with appropriate WRITECOPY semantics internally for private mappings.
                opts.map(&file)?
            }
        };
        let inner = Inner {
            path: path_ref.to_path_buf(),
            file,
            mode: MmapMode::CopyOnWrite,
            cached_len: AtomicU64::new(len),
            map: MapVariant::Cow(mmap),
            // COW never flushes underlying file in phase-1
            flush_policy: FlushPolicy::Never,
            written_since_last_flush: RwLock::new(0),
            flusher: RwLock::new(None),
            #[cfg(feature = "hugepages")]
            huge_pages: false,
        };
        Ok(Self {
            inner: Arc::new(inner),
        })
    }
}

impl MemoryMappedFile {
    fn apply_flush_policy(&self, written: u64) -> Result<()> {
        match self.inner.flush_policy {
            FlushPolicy::Never | FlushPolicy::Manual => Ok(()),
            FlushPolicy::Always => {
                // Record then flush immediately
                *self.inner.written_since_last_flush.write() += written;
                self.flush()
            }
            FlushPolicy::EveryBytes(n) => {
                let n = n as u64;
                if n == 0 {
                    return Ok(());
                }
                let mut acc = self.inner.written_since_last_flush.write();
                *acc += written;
                if *acc >= n {
                    // Do not reset prematurely; let flush() clear on success
                    drop(acc);
                    self.flush()
                } else {
                    Ok(())
                }
            }
            FlushPolicy::EveryWrites(w) => {
                if w == 0 {
                    return Ok(());
                }
                let mut acc = self.inner.written_since_last_flush.write();
                *acc += 1;
                if *acc >= w as u64 {
                    drop(acc);
                    self.flush()
                } else {
                    Ok(())
                }
            }
            FlushPolicy::EveryMillis(ms) => {
                if ms == 0 {
                    return Ok(());
                }

                // Record the write
                *self.inner.written_since_last_flush.write() += written;

                // For EveryMillis, time-based flushing is handled by the background thread
                // The policy just ensures writes are tracked
                Ok(())
            }
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
    /// Length is `buf.len()`; performs bounds checks.
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
        let map = self.map_read();
        let (start, end) = slice_range(offset, buf.len() as u64, map.len() as u64)?;
        buf.copy_from_slice(&map[start..end]);
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
            MapVariant::Cow(m) => Some(m.as_ptr() as usize),
            MapVariant::Rw(lock) => Some(lock.read_recursive().as_ptr() as usize),
        }
    }
}

#[cfg(target_os = "linux")]
fn smaps_hugepage_lookup(base: usize) -> Option<bool> {
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

    /// Request Huge Pages (Linux MAP_HUGETLB). No-op on non-Linux platforms.
    #[cfg(feature = "hugepages")]
    pub fn huge_pages(mut self, enable: bool) -> Self {
        self.huge_pages = enable;
        self
    }

    /// Create a new mapping; for ReadWrite requires size for creation.
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
                let size = self.size.ok_or_else(|| {
                    MmapIoError::ResizeFailed(
                        "Size must be set for create() in ReadWrite mode".into(),
                    )
                })?;
                if size == 0 {
                    return Err(MmapIoError::ResizeFailed(ERR_ZERO_SIZE.into()));
                }
                if size > MAX_MMAP_SIZE {
                    return Err(MmapIoError::ResizeFailed(format!(
                        "Size {size} exceeds maximum safe limit of {MAX_MMAP_SIZE} bytes"
                    )));
                }
                let path_ref = &self.path;
                let file = OpenOptions::new()
                    .create(true)
                    .write(true)
                    .read(true)
                    .truncate(true)
                    .open(path_ref)?;
                file.set_len(size)?;
                // Map with consideration for huge pages if requested
                #[cfg(feature = "hugepages")]
                let mmap = map_mut_with_options(&file, size, self.huge_pages)?;
                #[cfg(not(feature = "hugepages"))]
                // SAFETY: see `MemoryMappedFile::create_rw` for the
                // full justification. Identical preconditions: we just
                // created/truncated the file and set its length to
                // `size` via `set_len`; the kernel will produce a
                // mapping of exactly that length.
                let mmap = unsafe { MmapMut::map_mut(&file)? };

                // Build the Inner now (without a live flusher), wrap in Arc.
                // We attach the time-based flusher AFTER the Arc exists so that
                // Arc::downgrade produces a real Weak that can later upgrade,
                // unlike the previous Weak::new() (dangling). C2 fix.
                let inner = Inner {
                    path: path_ref.clone(),
                    file,
                    mode,
                    cached_len: AtomicU64::new(size),
                    map: MapVariant::Rw(RwLock::new(mmap)),
                    flush_policy: self.flush_policy,
                    written_since_last_flush: RwLock::new(0),
                    flusher: RwLock::new(None),
                    #[cfg(feature = "hugepages")]
                    huge_pages: self.huge_pages,
                };

                let mmap_file = MemoryMappedFile {
                    inner: Arc::new(inner),
                };

                // C2 fix: install the time-based flusher with a real weak ref.
                // The flusher is stored on Inner so its background thread's
                // lifetime is bound to the mapping. When the last Arc<Inner>
                // drops, the flusher's Drop runs and signals the thread to exit.
                if let FlushPolicy::EveryMillis(ms) = self.flush_policy {
                    if ms > 0 {
                        let inner_weak = Arc::downgrade(&mmap_file.inner);
                        let flusher = crate::flush::TimeBasedFlusher::new(ms, move || {
                            // Upgrade the weak ref. Returns None once the
                            // mapping has been dropped; thread will continue
                            // to wake but the callback short-circuits.
                            let Some(inner) = inner_weak.upgrade() else {
                                return false;
                            };
                            // Only flush if there are pending writes.
                            let pending = *inner.written_since_last_flush.read() > 0;
                            if !pending {
                                return false;
                            }
                            let temp = MemoryMappedFile { inner };
                            temp.flush().is_ok()
                        });
                        // Store the flusher on Inner so its background thread
                        // lives as long as the mapping. The Option allows for
                        // ms == 0 (which TimeBasedFlusher::new returns None for).
                        *mmap_file.inner.flusher.write() = flusher;
                    }
                }

                // Apply touch hint if specified
                if self.touch_hint == TouchHint::Eager {
                    log::debug!("Eagerly touching all pages for {size} bytes");
                    if let Err(e) = mmap_file.touch_pages() {
                        log::warn!("Failed to eagerly touch pages: {e}");
                        // Don't fail the creation, just log the warning
                    }
                }

                Ok(mmap_file)
            }
            MmapMode::ReadOnly => {
                let path_ref = &self.path;
                let file = OpenOptions::new().read(true).open(path_ref)?;
                let len = file.metadata()?.len();
                // SAFETY: see `MemoryMappedFile::open_ro` for the full
                // justification of calling `Mmap::map`. The file was
                // just opened read-only; cross-process modification is
                // the only residual hazard and is documented as
                // out-of-scope.
                let mmap = unsafe { Mmap::map(&file)? };
                let inner = Inner {
                    path: path_ref.clone(),
                    file,
                    mode,
                    cached_len: AtomicU64::new(len),
                    map: MapVariant::Ro(mmap),
                    flush_policy: FlushPolicy::Never,
                    written_since_last_flush: RwLock::new(0),
                    flusher: RwLock::new(None),
                    #[cfg(feature = "hugepages")]
                    huge_pages: false,
                };
                Ok(MemoryMappedFile {
                    inner: Arc::new(inner),
                })
            }
            #[cfg(feature = "cow")]
            MmapMode::CopyOnWrite => {
                let path_ref = &self.path;
                let file = OpenOptions::new().read(true).open(path_ref)?;
                let len = file.metadata()?.len();
                if len == 0 {
                    return Err(MmapIoError::ResizeFailed(ERR_ZERO_LENGTH_FILE.into()));
                }
                // SAFETY: see `open_cow` above for the full
                // justification. Identical preconditions: file just
                // opened read-only, `len > 0` verified, and the COW
                // mapping is exposed as read-only at the Rust API.
                let mmap = unsafe {
                    let mut opts = MmapOptions::new();
                    opts.len(len as usize);
                    opts.map(&file)?
                };
                let inner = Inner {
                    path: path_ref.clone(),
                    file,
                    mode,
                    cached_len: AtomicU64::new(len),
                    map: MapVariant::Cow(mmap),
                    flush_policy: FlushPolicy::Never,
                    written_since_last_flush: RwLock::new(0),
                    flusher: RwLock::new(None),
                    #[cfg(feature = "hugepages")]
                    huge_pages: false,
                };
                Ok(MemoryMappedFile {
                    inner: Arc::new(inner),
                })
            }
            #[cfg(not(feature = "cow"))]
            MmapMode::CopyOnWrite => Err(MmapIoError::InvalidMode(
                "CopyOnWrite mode requires 'cow' feature",
            )),
        }
    }

    /// Open an existing file with provided mode (size ignored).
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
        match mode {
            MmapMode::ReadOnly => {
                let path_ref = &self.path;
                let file = OpenOptions::new().read(true).open(path_ref)?;
                let len = file.metadata()?.len();
                // SAFETY: see `MemoryMappedFile::open_ro`.
                let mmap = unsafe { Mmap::map(&file)? };
                let inner = Inner {
                    path: path_ref.clone(),
                    file,
                    mode,
                    cached_len: AtomicU64::new(len),
                    map: MapVariant::Ro(mmap),
                    flush_policy: FlushPolicy::Never,
                    written_since_last_flush: RwLock::new(0),
                    flusher: RwLock::new(None),
                    #[cfg(feature = "hugepages")]
                    huge_pages: false,
                };
                Ok(MemoryMappedFile {
                    inner: Arc::new(inner),
                })
            }
            MmapMode::ReadWrite => {
                let path_ref = &self.path;
                let file = OpenOptions::new().read(true).write(true).open(path_ref)?;
                let len = file.metadata()?.len();
                if len == 0 {
                    return Err(MmapIoError::ResizeFailed(ERR_ZERO_LENGTH_FILE.into()));
                }
                #[cfg(feature = "hugepages")]
                let mmap = map_mut_with_options(&file, len, self.huge_pages)?;
                #[cfg(not(feature = "hugepages"))]
                // SAFETY: see `MemoryMappedFile::open_rw`. File opened
                // read+write, `len > 0` verified above.
                let mmap = unsafe { MmapMut::map_mut(&file)? };
                let inner = Inner {
                    path: path_ref.clone(),
                    file,
                    mode,
                    cached_len: AtomicU64::new(len),
                    map: MapVariant::Rw(RwLock::new(mmap)),
                    flush_policy: self.flush_policy,
                    written_since_last_flush: RwLock::new(0),
                    flusher: RwLock::new(None),
                    #[cfg(feature = "hugepages")]
                    huge_pages: self.huge_pages,
                };
                Ok(MemoryMappedFile {
                    inner: Arc::new(inner),
                })
            }
            #[cfg(feature = "cow")]
            MmapMode::CopyOnWrite => {
                let path_ref = &self.path;
                let file = OpenOptions::new().read(true).open(path_ref)?;
                let len = file.metadata()?.len();
                if len == 0 {
                    return Err(MmapIoError::ResizeFailed(ERR_ZERO_LENGTH_FILE.into()));
                }
                // SAFETY: see `open_cow`.
                let mmap = unsafe {
                    let mut opts = MmapOptions::new();
                    opts.len(len as usize);
                    opts.map(&file)?
                };
                let inner = Inner {
                    path: path_ref.clone(),
                    file,
                    mode,
                    cached_len: AtomicU64::new(len),
                    map: MapVariant::Cow(mmap),
                    flush_policy: FlushPolicy::Never,
                    written_since_last_flush: RwLock::new(0),
                    flusher: RwLock::new(None),
                    #[cfg(feature = "hugepages")]
                    huge_pages: false,
                };
                Ok(MemoryMappedFile {
                    inner: Arc::new(inner),
                })
            }
            #[cfg(not(feature = "cow"))]
            MmapMode::CopyOnWrite => Err(MmapIoError::InvalidMode(
                "CopyOnWrite mode requires 'cow' feature",
            )),
        }
    }

    /// Terminal builder method that opens the file if it exists, or
    /// creates it (using the builder's configured size) if it does
    /// not. Requires [`.size()`](Self::size) to be set for the
    /// create path; the existing-file path uses the file's current
    /// length. The configured `mode` (default `ReadWrite`),
    /// `flush_policy`, `touch_hint`, and `huge_pages` apply to both
    /// paths.
    ///
    /// # Errors
    ///
    /// Returns [`MmapIoError::ResizeFailed`] if creating a new file
    /// and `.size()` was not set or was set to zero.
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
        if self.path.exists() {
            self.open()
        } else {
            self.create()
        }
    }
}

// Move this to the top-level with other use statements:
use parking_lot::{RwLockReadGuard, RwLockWriteGuard};

/// Wrapper for a mutable slice that holds a write lock guard,
/// ensuring exclusive access for the lifetime of the slice.
pub struct MappedSliceMut<'a> {
    guard: RwLockWriteGuard<'a, MmapMut>,
    range: std::ops::Range<usize>,
}

impl<'a> MappedSliceMut<'a> {
    /// Construct a `MappedSliceMut` that holds a write guard for its
    /// lifetime. Used by RW file-backed paths and by `AnonymousMmap`.
    pub(crate) fn guarded(
        guard: RwLockWriteGuard<'a, MmapMut>,
        range: std::ops::Range<usize>,
    ) -> Self {
        Self { guard, range }
    }

    /// Get the mutable slice.
    ///
    /// Note: This method is intentionally named `as_mut` for consistency,
    /// even though it conflicts with the standard trait naming.
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
/// For RO and COW mappings this is a thin wrapper around a `&[u8]`
/// borrowed directly from the underlying immutable mapping. For RW
/// mappings this also holds the `RwLock` read guard for its lifetime,
/// blocking any concurrent `resize()` (and every write, which also
/// needs the write lock) while the slice is alive.
///
/// Implements [`Deref<Target = [u8]>`] and [`AsRef<[u8]>`], so callers
/// can use it as a byte slice directly: indexing, iteration,
/// `slice.len()`, `&slice[..]`, etc. all work.
pub struct MappedSlice<'a> {
    inner: MappedSliceInner<'a>,
}

enum MappedSliceInner<'a> {
    /// RO / COW: the mapping is immutable; we lend a direct slice.
    Owned(&'a [u8]),
    /// RW: the read guard keeps the mapping alive (and prevents
    /// `resize()` and writes from running) for the slice's lifetime.
    /// `bytes` is computed once at construction so `Deref` does no
    /// range arithmetic or bounds checks.
    Guarded {
        _guard: RwLockReadGuard<'a, MmapMut>,
        bytes: *const [u8],
    },
}

// SAFETY: `MappedSlice` only ever hands out `&[u8]` to bytes that no
// one can mutate while it lives (RO/COW mappings are immutable; for RW
// the held read guard excludes every writer). `Owned` holds a `&[u8]`,
// which is `Send + Sync`. `Guarded` holds a parking_lot read guard,
// which is `Send` because this crate enables parking_lot's
// `send_guard` feature (checked at compile time by
// `_ASSERT_GUARDS_SEND_SYNC` below) and `Sync` because `MmapMut` is
// `Sync`; the raw `bytes` pointer is only a cached view of memory
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
    assert_send_sync::<RwLockReadGuard<'static, MmapMut>>();
    assert_send_sync::<RwLockWriteGuard<'static, MmapMut>>();
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

    /// Construct a `MappedSlice` that holds a read guard for its
    /// lifetime. Used for RW paths to keep the mapping stable.
    ///
    /// # Panics
    ///
    /// Panics if `range` is not within `guard`'s mapping. Callers
    /// validate the range against `guard.len()` first.
    pub(crate) fn guarded(
        guard: RwLockReadGuard<'a, MmapMut>,
        range: std::ops::Range<usize>,
    ) -> Self {
        let bytes: *const [u8] = &guard[range];
        Self {
            inner: MappedSliceInner::Guarded {
                _guard: guard,
                bytes,
            },
        }
    }

    /// Borrow the underlying byte slice.
    #[must_use]
    pub fn as_slice(&self) -> &[u8] {
        match &self.inner {
            MappedSliceInner::Owned(s) => s,
            // SAFETY: `bytes` was derived from `&_guard[range]` at
            // construction. The mapping it points into cannot be
            // unmapped, remapped, or written while `_guard` (a read
            // guard on the mapping's lock) is alive, and `_guard`
            // lives exactly as long as `self`. The returned borrow is
            // tied to `&self`, so it cannot outlive the guard.
            MappedSliceInner::Guarded { bytes, .. } => unsafe { &**bytes },
        }
    }

    /// Length of the slice in bytes.
    #[must_use]
    pub fn len(&self) -> usize {
        self.as_slice().len()
    }

    /// Whether the slice is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl std::ops::Deref for MappedSlice<'_> {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        self.as_slice()
    }
}

impl AsRef<[u8]> for MappedSlice<'_> {
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

/// `io::Read` + `io::Seek` cursor over a memory-mapped file.
///
/// Constructed via [`MemoryMappedFile::reader`]. Each `read` call
/// delegates to `read_into`, which is bounds-checked. EOF is
/// signalled by a zero-length read.
///
/// The cursor borrows the mapping; multiple cursors can coexist
/// and read concurrently on the same mapping.
pub struct MmapReader<'a> {
    mmap: &'a MemoryMappedFile,
    pos: u64,
}

impl<'a> std::io::Read for MmapReader<'a> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
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

impl<'a> std::io::Seek for MmapReader<'a> {
    fn seek(&mut self, pos: std::io::SeekFrom) -> std::io::Result<u64> {
        use std::io::SeekFrom;
        let total = self.mmap.len();
        let new_pos = match pos {
            SeekFrom::Start(n) => n,
            SeekFrom::End(delta) => {
                if delta >= 0 {
                    total.saturating_add(delta as u64)
                } else {
                    total.saturating_sub((-delta) as u64)
                }
            }
            SeekFrom::Current(delta) => {
                if delta >= 0 {
                    self.pos.saturating_add(delta as u64)
                } else {
                    self.pos.saturating_sub((-delta) as u64)
                }
            }
        };
        self.pos = new_pos;
        Ok(self.pos)
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
    /// EOF).
    pub fn set_position(&mut self, pos: u64) {
        self.pos = pos;
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
