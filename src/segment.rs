//! Zero-copy segment views into a memory-mapped file.

use std::sync::Arc;

use crate::errors::Result;
use crate::mmap::MemoryMappedFile;
use crate::utils::slice_range;

/// Immutable view into a region of a memory-mapped file.
///
/// A `Segment` is a lightweight bookmark: it stores an offset and
/// length and holds an `Arc` to the parent mapping. Bounds are
/// validated at construction AND on every access, because the parent
/// can be resized between segment construction and use.
///
/// # Examples
///
/// ```no_run
/// use std::sync::Arc;
/// use mmap_io::{MemoryMappedFile, segment::Segment};
///
/// let mmap = Arc::new(MemoryMappedFile::open_ro("data.bin")?);
///
/// // Create a segment for bytes 100-200
/// let segment = Segment::new(mmap.clone(), 100, 100)?;
///
/// // Read the segment data
/// let data = segment.as_slice()?;
/// # Ok::<(), mmap_io::MmapIoError>(())
/// ```
///
/// # Behavior under resize
///
/// If the parent mapping is shrunk via [`MemoryMappedFile::resize`]
/// such that the segment's range no longer fits, subsequent calls to
/// [`as_slice`](Self::as_slice) return `MmapIoError::OutOfBounds`.
/// The segment is NOT invalidated as a type; it remains usable, but
/// the access will fail until the parent is grown again to cover the
/// range.
#[derive(Clone, Debug)]
pub struct Segment {
    parent: Arc<MemoryMappedFile>,
    offset: u64,
    len: u64,
}

impl Segment {
    /// Create a new immutable segment view. Performs initial bounds
    /// check against the parent's current length.
    ///
    /// The bounds check is repeated on every access via
    /// [`as_slice`](Self::as_slice) so that resize-after-construction
    /// is detected.
    ///
    /// # Errors
    ///
    /// Returns `MmapIoError::OutOfBounds` if the segment exceeds the
    /// parent's current length at construction time.
    pub fn new(parent: Arc<MemoryMappedFile>, offset: u64, len: u64) -> Result<Self> {
        let total = parent.current_len()?;
        let _ = slice_range(offset, len, total)?;
        Ok(Self {
            parent,
            offset,
            len,
        })
    }

    /// Return the segment as a read-only byte slice.
    ///
    /// Bounds are re-validated on every call. If the parent has been
    /// shrunk such that the segment's range no longer fits, returns
    /// `MmapIoError::OutOfBounds`.
    ///
    /// # Errors
    ///
    /// Returns `MmapIoError::OutOfBounds` if the segment's range is no
    /// longer within the parent's current bounds (e.g., after a
    /// shrinking resize).
    /// Returns `MmapIoError::InvalidMode` if the parent is a RW
    /// mapping (use [`MemoryMappedFile::read_slice`] when available,
    /// or [`MemoryMappedFile::read_into`] for a copy).
    pub fn as_slice(&self) -> Result<&[u8]> {
        // Re-validate on every access: parent could have been resized
        // since the segment was constructed.
        self.parent.as_slice(self.offset, self.len)
    }

    /// Length of the segment.
    #[must_use]
    pub fn len(&self) -> u64 {
        self.len
    }

    /// Check if the segment is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Offset of the segment in the file.
    #[must_use]
    pub fn offset(&self) -> u64 {
        self.offset
    }

    /// Parent mapping.
    #[must_use]
    pub fn parent(&self) -> &MemoryMappedFile {
        &self.parent
    }

    /// Check whether the segment's range is still within the parent's
    /// current bounds.
    ///
    /// Useful for callers that want to query without paying for an
    /// `as_slice` call (e.g., to decide whether to skip or re-create
    /// the segment after a known resize).
    #[must_use]
    pub fn is_valid(&self) -> bool {
        match self.parent.current_len() {
            Ok(total) => crate::utils::ensure_in_bounds(self.offset, self.len, total).is_ok(),
            Err(_) => false,
        }
    }
}

/// Mutable view into a region of a memory-mapped file.
///
/// Holds a reference to the parent mapping; mutable access is provided
/// on demand. Bounds are validated at construction AND on every
/// access, because the parent can be resized between segment
/// construction and use.
///
/// # Examples
///
/// ```no_run
/// use std::sync::Arc;
/// use mmap_io::{MemoryMappedFile, segment::SegmentMut};
///
/// let mmap = Arc::new(MemoryMappedFile::create_rw("data.bin", 1024)?);
///
/// // Create a mutable segment for bytes 0-100
/// let segment = SegmentMut::new(mmap.clone(), 0, 100)?;
///
/// // Write data to the segment
/// segment.write(b"Hello from segment!")?;
/// # Ok::<(), mmap_io::MmapIoError>(())
/// ```
///
/// # Behavior under resize
///
/// Same as [`Segment`]: access after a shrinking resize returns
/// `OutOfBounds`.
#[derive(Clone, Debug)]
pub struct SegmentMut {
    parent: Arc<MemoryMappedFile>,
    offset: u64,
    len: u64,
}

impl SegmentMut {
    /// Create a new mutable segment view. Performs initial bounds
    /// check against the parent's current length.
    ///
    /// The bounds check is repeated on every access so that
    /// resize-after-construction is detected.
    ///
    /// # Errors
    ///
    /// Returns `MmapIoError::OutOfBounds` if the segment exceeds the
    /// parent's current length at construction time.
    pub fn new(parent: Arc<MemoryMappedFile>, offset: u64, len: u64) -> Result<Self> {
        let total = parent.current_len()?;
        let _ = slice_range(offset, len, total)?;
        Ok(Self {
            parent,
            offset,
            len,
        })
    }

    /// Return a write-capable guard to the underlying bytes for this
    /// segment. The guard holds the write lock for the duration of
    /// the mutable borrow.
    ///
    /// Bounds are re-validated on every call.
    ///
    /// # Errors
    ///
    /// Returns `MmapIoError::OutOfBounds` if the segment's range is no
    /// longer within the parent's current bounds.
    /// Returns `MmapIoError::InvalidMode` if the parent is not in
    /// `ReadWrite` mode.
    pub fn as_slice_mut(&self) -> Result<crate::mmap::MappedSliceMut<'_>> {
        // Re-validate on every access (resize-safety).
        self.parent.as_slice_mut(self.offset, self.len)
    }

    /// Write bytes into this segment from the provided slice.
    ///
    /// Bounds are re-validated by the underlying `update_region` call.
    ///
    /// # Errors
    ///
    /// Returns `MmapIoError::OutOfBounds` if the write range exceeds
    /// the parent's current bounds.
    /// Returns `MmapIoError::InvalidMode` if the parent is not in
    /// `ReadWrite` mode.
    pub fn write(&self, data: &[u8]) -> Result<()> {
        // Allow partial writes by delegating to update_region; the
        // underlying call re-validates bounds.
        self.parent.update_region(self.offset, data)
    }

    /// Length of the segment.
    #[must_use]
    pub fn len(&self) -> u64 {
        self.len
    }

    /// Check if the segment is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Offset of the segment in the file.
    #[must_use]
    pub fn offset(&self) -> u64 {
        self.offset
    }

    /// Parent mapping.
    #[must_use]
    pub fn parent(&self) -> &MemoryMappedFile {
        &self.parent
    }

    /// Check whether the segment's range is still within the parent's
    /// current bounds. See [`Segment::is_valid`].
    #[must_use]
    pub fn is_valid(&self) -> bool {
        match self.parent.current_len() {
            Ok(total) => crate::utils::ensure_in_bounds(self.offset, self.len, total).is_ok(),
            Err(_) => false,
        }
    }
}
