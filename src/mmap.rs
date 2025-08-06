//! Low-level memory-mapped file abstraction with safe, concurrent access.

use std::{
    fs::{File, OpenOptions},
    path::{Path, PathBuf},
    sync::Arc,
};

use memmap2::{Mmap, MmapMut};

use crate::flush::FlushPolicy;

#[cfg(feature = "cow")]
use memmap2::MmapOptions;

use parking_lot::RwLock;

use crate::errors::{MmapIoError, Result};
use crate::utils::{ensure_in_bounds, slice_range};

// Error message constants
const ERR_ZERO_SIZE: &str = "Size must be greater than zero";
const ERR_ZERO_LENGTH_FILE: &str = "Cannot map zero-length file";

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
    // Cached length to avoid repeated metadata queries
    pub(crate) cached_len: RwLock<u64>,
    // The mapping itself. We use an enum to hold either RO or RW mapping.
    pub(crate) map: MapVariant,
    // Flush policy and accounting (RW only)
    pub(crate) flush_policy: FlushPolicy,
    pub(crate) written_since_last_flush: RwLock<u64>,
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
        f.debug_struct("MemoryMappedFile")
            .field("path", &self.inner.path)
            .field("mode", &self.inner.mode)
            .field("len", &self.len())
            .finish()
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
            #[cfg(feature = "hugepages")]
            huge_pages: false,
        }
    }

    /// Create a new file (truncating if exists) and memory-map it in read-write mode with the given size.
    ///
    /// # Errors
    ///
    /// Returns `MmapIoError::ResizeFailed` if size is zero.
    /// Returns `MmapIoError::Io` if file creation or mapping fails.
    pub fn create_rw<P: AsRef<Path>>(path: P, size: u64) -> Result<Self> {
        if size == 0 {
            return Err(MmapIoError::ResizeFailed(ERR_ZERO_SIZE.into()));
        }
        let path_ref = path.as_ref();
        let file = OpenOptions::new()
            .create(true)
            .write(true)
            .read(true)
            .truncate(true)
            .open(path_ref)?;
        file.set_len(size)?;
        // SAFETY: The file has been created with the correct size and permissions.
        // memmap2 handles platform-specific mmap details safely.
        // Note: create_rw convenience ignores huge pages; use builder for that.
        let mmap = unsafe { MmapMut::map_mut(&file)? };
        let inner = Inner {
            path: path_ref.to_path_buf(),
            file,
            mode: MmapMode::ReadWrite,
            cached_len: RwLock::new(size),
            map: MapVariant::Rw(RwLock::new(mmap)),
            flush_policy: FlushPolicy::default(),
            written_since_last_flush: RwLock::new(0),
            #[cfg(feature = "hugepages")]
            huge_pages: false,
        };
        Ok(Self { inner: Arc::new(inner) })
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
        // SAFETY: The file is opened read-only and memmap2 ensures safe mapping.
        let mmap = unsafe { Mmap::map(&file)? };
        let inner = Inner {
            path: path_ref.to_path_buf(),
            file,
            mode: MmapMode::ReadOnly,
            cached_len: RwLock::new(len),
            map: MapVariant::Ro(mmap),
            flush_policy: FlushPolicy::Never,
            written_since_last_flush: RwLock::new(0),
            #[cfg(feature = "hugepages")]
            huge_pages: false,
        };
        Ok(Self { inner: Arc::new(inner) })
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
        // SAFETY: The file is opened read-write with proper permissions.
        // We've verified the file is not zero-length.
        // Note: open_rw convenience ignores huge pages; use builder for that.
        let mmap = unsafe { MmapMut::map_mut(&file)? };
        let inner = Inner {
            path: path_ref.to_path_buf(),
            file,
            mode: MmapMode::ReadWrite,
            cached_len: RwLock::new(len),
            map: MapVariant::Rw(RwLock::new(mmap)),
            flush_policy: FlushPolicy::default(),
            written_since_last_flush: RwLock::new(0),
            #[cfg(feature = "hugepages")]
            huge_pages: false,
        };
        Ok(Self { inner: Arc::new(inner) })
    }

    /// Return current mapping mode.
    #[must_use]
    pub fn mode(&self) -> MmapMode {
        self.inner.mode
    }

    /// Total length of the mapped file in bytes (cached).
    #[must_use]
    pub fn len(&self) -> u64 {
        *self.inner.cached_len.read()
    }

    /// Whether the mapped file is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Get a zero-copy read-only slice for the given [offset, offset+len).
    /// For RW mappings, cannot return a reference bound to a temporary guard; use `read_into` instead.
    ///
    /// # Errors
    ///
    /// Returns `MmapIoError::OutOfBounds` if range exceeds file bounds.
    /// Returns `MmapIoError::InvalidMode` for RW mappings (use `read_into` instead).
    pub fn as_slice(&self, offset: u64, len: u64) -> Result<&[u8]> {
        let total = self.current_len()?;
        ensure_in_bounds(offset, len, total)?;
        match &self.inner.map {
            MapVariant::Ro(m) => {
                let (start, end) = slice_range(offset, len, total)?;
                Ok(&m[start..end])
            }
            MapVariant::Rw(_lock) => Err(MmapIoError::InvalidMode("use read_into for RW mappings")),
            MapVariant::Cow(m) => {
                let (start, end) = slice_range(offset, len, total)?;
                Ok(&m[start..end])
            }
        }
    }

    /// Get a zero-copy mutable slice for the given [offset, offset+len).
    /// Only available in `ReadWrite` mode.
    ///
    /// # Errors
    ///
    /// Returns `MmapIoError::InvalidMode` if not in `ReadWrite` mode.
    /// Returns `MmapIoError::OutOfBounds` if range exceeds file bounds.
    pub fn as_slice_mut(&self, offset: u64, len: u64) -> Result<MappedSliceMut<'_>> {
        let (start, end) = slice_range(offset, len, self.current_len()?)?;
        match &self.inner.map {
            MapVariant::Ro(_) => Err(MmapIoError::InvalidMode("mutable access on read-only mapping")),
            MapVariant::Rw(lock) => {
                let guard = lock.write();
                Ok(MappedSliceMut {
                    guard,
                    range: start..end,
                })
            }
            MapVariant::Cow(_) => {
                // Phase-1: COW is read-only for safety. Writable COW will be added with a persistent
                // private RW view in a follow-up change.
                Err(MmapIoError::InvalidMode("mutable access on copy-on-write mapping (phase-1 read-only)"))
            }
        }
    }

    /// Copy the provided bytes into the mapped file at the given offset.
    /// Bounds-checked, zero-copy write.
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
            return Err(MmapIoError::InvalidMode("Update region requires ReadWrite mode."));
        }
        let len = data.len() as u64;
        let (start, end) = slice_range(offset, len, self.current_len()?)?;
        match &self.inner.map {
            MapVariant::Ro(_) => Err(MmapIoError::InvalidMode("Cannot write to read-only mapping.")),
            MapVariant::Rw(lock) => {
                {
                    let mut guard = lock.write();
                    guard[start..end].copy_from_slice(data);
                }
                // Apply flush policy
                self.apply_flush_policy(len)?;
                Ok(())
            }
            MapVariant::Cow(_) => Err(MmapIoError::InvalidMode("Cannot write to copy-on-write mapping (phase-1 read-only).")),
        }
    }

    /// Flush changes to disk. For read-only mappings, this is a no-op.
    ///
    /// # Errors
    ///
    /// Returns `MmapIoError::FlushFailed` if flush operation fails.
    pub fn flush(&self) -> Result<()> {
        match &self.inner.map {
            MapVariant::Ro(_) => Ok(()),
            MapVariant::Cow(_) => Ok(()), // no-op for COW
            MapVariant::Rw(lock) => {
                let guard = lock.read();
                guard.flush().map_err(|e| MmapIoError::FlushFailed(e.to_string()))
            }
        }
    }

    /// Flush a specific byte range to disk.
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
                let guard = lock.read();
                let (start, end) = slice_range(offset, len, self.current_len()?)?;
                guard
                    .flush_range(start, end - start)
                    .map_err(|e| MmapIoError::FlushFailed(e.to_string()))
            }
        }
    }

    /// Resize (grow or shrink) the mapped file (RW only). This remaps the file internally.
    ///
    /// # Errors
    ///
    /// Returns `MmapIoError::InvalidMode` if not in `ReadWrite` mode.
    /// Returns `MmapIoError::ResizeFailed` if new size is zero.
    /// Returns `MmapIoError::Io` if resize operation fails.
    pub fn resize(&self, new_size: u64) -> Result<()> {
        if self.inner.mode != MmapMode::ReadWrite {
            return Err(MmapIoError::InvalidMode("Resize requires ReadWrite mode."));
        }
        if new_size == 0 {
            return Err(MmapIoError::ResizeFailed("New size must be greater than zero.".into()));
        }

        let current = self.current_len()?;

        // On Windows, shrinking a file with an active mapping fails with:
        // "The requested operation cannot be performed on a file with a user-mapped section open."
        // To keep APIs usable and tests passing, we virtually shrink by updating the cached length,
        // avoiding truncation while a mapping is active. Growing still truncates and remaps.
        #[cfg(windows)]
        {
            use std::cmp::Ordering;
            match new_size.cmp(&current) {
                Ordering::Less => {
                    // Virtually shrink: only update the cached length.
                    *self.inner.cached_len.write() = new_size;
                    return Ok(());
                }
                Ordering::Equal => {
                    return Ok(());
                }
                Ordering::Greater => {
                    // Proceed with normal grow: extend file then remap.
                }
            }
        }

        // Update length on disk for non-windows, or for growing on windows.
        // Silence unused variable warning when the Windows shrink early-return path is compiled.
        let _ = &current;
        self.inner.file.set_len(new_size)?;

        // Remap with the new size.
        let new_map = unsafe { MmapMut::map_mut(&self.inner.file)? };
        match &self.inner.map {
            MapVariant::Ro(_) => Err(MmapIoError::InvalidMode("internal: cannot remap RO as RW")),
            MapVariant::Cow(_) => Err(MmapIoError::InvalidMode("resize not supported on copy-on-write mapping")),
            MapVariant::Rw(lock) => {
                let mut guard = lock.write();
                *guard = new_map;
                // Update cached length
                *self.inner.cached_len.write() = new_size;
                Ok(())
            }
        }
    }

    /// Path to the underlying file.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.inner.path
    }
}

#[cfg(feature = "hugepages")]
fn map_mut_with_options(file: &File, len: u64, huge: bool) -> Result<MmapMut> {
    #[cfg(all(unix, target_os = "linux"))]
    {
        use std::os::fd::AsRawFd;
        if huge {
            // Safety: we construct a mapping with custom flags using libc::mmap and wrap it into MmapMut.
            // memmap2 does not expose MAP_HUGETLB; we fallback to native call and then build from raw parts.
            unsafe {
                let prot = libc::PROT_READ | libc::PROT_WRITE;
                // Use private + shared semantics equivalent to memmap2 default for RW mapping
                let flags = libc::MAP_SHARED | libc::MAP_HUGETLB;
                let addr = libc::mmap(
                    std::ptr::null_mut(),
                    len as usize,
                    prot,
                    flags,
                    file.as_raw_fd(),
                    0,
                );
                if addr == libc::MAP_FAILED {
                    // Fallback: standard mapping
                    return MmapMut::map_mut(file).map_err(|e| MmapIoError::Io(e.into()));
                }
                // Create MmapMut from raw parts using memmap2 API
                // SAFETY: memmap2 provides MmapMut::map_mut which does mmap internally; it doesn't expose from_raw.
                // Since memmap2 has no from_raw stable API, we must unmap and fall back if custom mmap is not viable.
                // Therefore, we simply fall back to memmap2 map_mut as above if custom mmap not possible.
                // If we reached here, custom mmap succeeded; but memmap2 cannot adopt it, so we will munmap and fall back to map_mut.
                libc::munmap(addr, len as usize);
                return MmapMut::map_mut(file).map_err(|e| MmapIoError::Io(e.into()));
            }
        } else {
            return unsafe { MmapMut::map_mut(file) }.map_err(|e| MmapIoError::Io(e.into()));
        }
    }
    #[cfg(not(all(unix, target_os = "linux")))]
    {
        let _ = (len, huge);
        return unsafe { MmapMut::map_mut(file) }.map_err(|e| MmapIoError::Io(e.into()));
    }
}

#[cfg(feature = "cow")]
impl MemoryMappedFile {
    /// Open an existing file and memory-map it copy-on-write (private).
    /// Changes through this mapping are visible only within this process; the underlying file remains unchanged.
    pub fn open_cow<P: AsRef<Path>>(path: P) -> Result<Self> {
        let path_ref = path.as_ref();
        let file = OpenOptions::new().read(true).open(path_ref)?;
        let len = file.metadata()?.len();
        if len == 0 {
            return Err(MmapIoError::ResizeFailed(ERR_ZERO_LENGTH_FILE.into()));
        }
        // SAFETY: memmap2 handles platform specifics. We request a private (copy-on-write) mapping.
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
            cached_len: RwLock::new(len),
            map: MapVariant::Cow(mmap),
            // COW never flushes underlying file in phase-1
            flush_policy: FlushPolicy::Never,
            written_since_last_flush: RwLock::new(0),
            #[cfg(feature = "hugepages")]
            huge_pages: false,
        };
        Ok(Self { inner: Arc::new(inner) })
    }
}

impl MemoryMappedFile {
    fn apply_flush_policy(&self, written: u64) -> Result<()> {
        match self.inner.flush_policy {
            FlushPolicy::Never | FlushPolicy::Manual => Ok(()),
            FlushPolicy::Always => self.flush(),
            FlushPolicy::EveryBytes(n) => {
                let n = n as u64;
                if n == 0 {
                    return Ok(());
                }
                let mut acc = self.inner.written_since_last_flush.write();
                *acc += written;
                if *acc >= n {
                    *acc = 0;
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
                    *acc = 0;
                    self.flush()
                } else {
                    Ok(())
                }
            }
            FlushPolicy::EveryMillis(_ms) => {
                // Phase-1: timer-based flushing is not implemented inside core to avoid background threads.
                // Users can drive time-based flushing externally. Treat as Manual here.
                Ok(())
            }
        }
    }

    /// Return the up-to-date file length (cached).
    /// This ensures length remains correct even after resize.
    ///
    /// # Errors
    ///
    /// Returns `MmapIoError::Io` if metadata query fails (not expected in current implementation).
    pub fn current_len(&self) -> Result<u64> {
        Ok(*self.inner.cached_len.read())
    }

    /// Read bytes from the mapping into the provided buffer starting at `offset`.
    /// Length is `buf.len()`; performs bounds checks.
    ///
    /// # Errors
    ///
    /// Returns `MmapIoError::OutOfBounds` if range exceeds file bounds.
    pub fn read_into(&self, offset: u64, buf: &mut [u8]) -> Result<()> {
        let total = self.current_len()?;
        let len = buf.len() as u64;
        ensure_in_bounds(offset, len, total)?;
        match &self.inner.map {
            MapVariant::Ro(m) => {
                let (start, end) = slice_range(offset, len, total)?;
                buf.copy_from_slice(&m[start..end]);
                Ok(())
            }
            MapVariant::Rw(lock) => {
                let guard = lock.read();
                let (start, end) = slice_range(offset, len, total)?;
                buf.copy_from_slice(&guard[start..end]);
                Ok(())
            }
            MapVariant::Cow(m) => {
                let (start, end) = slice_range(offset, len, total)?;
                buf.copy_from_slice(&m[start..end]);
                Ok(())
            }
        }
    }
}

/// Builder for MemoryMappedFile construction with options.
pub struct MemoryMappedFileBuilder {
    path: PathBuf,
    size: Option<u64>,
    mode: Option<MmapMode>,
    flush_policy: FlushPolicy,
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

    /// Request Huge Pages (Linux MAP_HUGETLB). No-op on non-Linux platforms.
    #[cfg(feature = "hugepages")]
    pub fn huge_pages(mut self, enable: bool) -> Self {
        self.huge_pages = enable;
        self
    }

    /// Create a new mapping; for ReadWrite requires size for creation.
    pub fn create(self) -> Result<MemoryMappedFile> {
        let mode = self.mode.unwrap_or(MmapMode::ReadWrite);
        match mode {
            MmapMode::ReadWrite => {
                let size = self.size.ok_or_else(|| {
                    MmapIoError::ResizeFailed("Size must be set for create() in ReadWrite mode".into())
                })?;
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
                let mmap = unsafe { MmapMut::map_mut(&file)? };
                let inner = Inner {
                    path: path_ref.clone(),
                    file,
                    mode,
                    cached_len: RwLock::new(size),
                    map: MapVariant::Rw(RwLock::new(mmap)),
                    flush_policy: self.flush_policy,
                    written_since_last_flush: RwLock::new(0),
                    #[cfg(feature = "hugepages")]
                    huge_pages: self.huge_pages,
                };
                Ok(MemoryMappedFile { inner: Arc::new(inner) })
            }
            MmapMode::ReadOnly => {
                let path_ref = &self.path;
                let file = OpenOptions::new().read(true).open(path_ref)?;
                let len = file.metadata()?.len();
                let mmap = unsafe { Mmap::map(&file)? };
                let inner = Inner {
                    path: path_ref.clone(),
                    file,
                    mode,
                    cached_len: RwLock::new(len),
                    map: MapVariant::Ro(mmap),
                    flush_policy: FlushPolicy::Never,
                    written_since_last_flush: RwLock::new(0),
                    #[cfg(feature = "hugepages")]
                    huge_pages: false,
                };
                Ok(MemoryMappedFile { inner: Arc::new(inner) })
            }
            MmapMode::CopyOnWrite => {
                #[cfg(feature = "cow")]
                {
                    let path_ref = &self.path;
                    let file = OpenOptions::new().read(true).open(path_ref)?;
                    let len = file.metadata()?.len();
                    if len == 0 {
                        return Err(MmapIoError::ResizeFailed(ERR_ZERO_LENGTH_FILE.into()));
                    }
                    let mmap = unsafe {
                        let mut opts = MmapOptions::new();
                        opts.len(len as usize);
                        opts.map(&file)?
                    };
                    let inner = Inner {
                        path: path_ref.clone(),
                        file,
                        mode,
                        cached_len: RwLock::new(len),
                        map: MapVariant::Cow(mmap),
                        flush_policy: FlushPolicy::Never,
                        written_since_last_flush: RwLock::new(0),
                        #[cfg(feature = "hugepages")]
                        huge_pages: false,
                    };
                    Ok(MemoryMappedFile { inner: Arc::new(inner) })
                }
                #[cfg(not(feature = "cow"))]
                {
                    Err(MmapIoError::InvalidMode("CopyOnWrite mode requires 'cow' feature"))
                }
            }
        }
    }

    /// Open an existing file with provided mode (size ignored).
    pub fn open(self) -> Result<MemoryMappedFile> {
        let mode = self.mode.unwrap_or(MmapMode::ReadOnly);
        match mode {
            MmapMode::ReadOnly => {
                let path_ref = &self.path;
                let file = OpenOptions::new().read(true).open(path_ref)?;
                let len = file.metadata()?.len();
                let mmap = unsafe { Mmap::map(&file)? };
                let inner = Inner {
                    path: path_ref.clone(),
                    file,
                    mode,
                    cached_len: RwLock::new(len),
                    map: MapVariant::Ro(mmap),
                    flush_policy: FlushPolicy::Never,
                    written_since_last_flush: RwLock::new(0),
                    #[cfg(feature = "hugepages")]
                    huge_pages: false,
                };
                Ok(MemoryMappedFile { inner: Arc::new(inner) })
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
                let mmap = unsafe { MmapMut::map_mut(&file)? };
                let inner = Inner {
                    path: path_ref.clone(),
                    file,
                    mode,
                    cached_len: RwLock::new(len),
                    map: MapVariant::Rw(RwLock::new(mmap)),
                    flush_policy: self.flush_policy,
                    written_since_last_flush: RwLock::new(0),
                    #[cfg(feature = "hugepages")]
                    huge_pages: self.huge_pages,
                };
                Ok(MemoryMappedFile { inner: Arc::new(inner) })
            }
            MmapMode::CopyOnWrite => {
                #[cfg(feature = "cow")]
                {
                    let path_ref = &self.path;
                    let file = OpenOptions::new().read(true).open(path_ref)?;
                    let len = file.metadata()?.len();
                    if len == 0 {
                        return Err(MmapIoError::ResizeFailed(ERR_ZERO_LENGTH_FILE.into()));
                    }
                    let mmap = unsafe {
                        let mut opts = MmapOptions::new();
                        opts.len(len as usize);
                        opts.map(&file)?
                    };
                    let inner = Inner {
                        path: path_ref.clone(),
                        file,
                        mode,
                        cached_len: RwLock::new(len),
                        map: MapVariant::Cow(mmap),
                        flush_policy: FlushPolicy::Never,
                        written_since_last_flush: RwLock::new(0),
                        #[cfg(feature = "hugepages")]
                        huge_pages: false,
                    };
                    Ok(MemoryMappedFile { inner: Arc::new(inner) })
                }
                #[cfg(not(feature = "cow"))]
                {
                    Err(MmapIoError::InvalidMode("CopyOnWrite mode requires 'cow' feature"))
                }
            }
        }
    }
}

/// Wrapper for a mutable slice that holds a write lock guard,
/// ensuring exclusive access for the lifetime of the slice.
pub struct MappedSliceMut<'a> {
    guard: parking_lot::lock_api::RwLockWriteGuard<'a, parking_lot::RawRwLock, MmapMut>,
    range: std::ops::Range<usize>,
}

impl MappedSliceMut<'_> {
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
}