//! Zero-copy segment views into a memory-mapped file.

use std::sync::Arc;

use crate::errors::Result;
use crate::mmap::MemoryMappedFile;
use crate::utils::slice_range;

/// Immutable view into a region of a memory-mapped file.
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
#[derive(Clone, Debug)]
pub struct Segment {
    parent: Arc<MemoryMappedFile>,
    offset: u64,
    len: u64,
}

impl Segment {
    /// Create a new immutable segment view. Performs bounds checks.
    ///
    /// # Errors
    ///
    /// Returns `MmapIoError::OutOfBounds` if the segment exceeds file bounds.
    pub fn new(parent: Arc<MemoryMappedFile>, offset: u64, len: u64) -> Result<Self> {
        // Validate bounds once at construction
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
    /// # Errors
    ///
    /// Returns errors from the underlying `MemoryMappedFile::as_slice` call.
    ///
    /// Note: Bounds are already validated at construction, so as_slice
    /// will not perform redundant validation.
    pub fn as_slice(&self) -> Result<&[u8]> {
        // Bounds already validated in constructor
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
}

/// Mutable view into a region of a memory-mapped file.
/// Holds a reference to the parent map; mutable access is provided on demand.
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
#[derive(Clone, Debug)]
pub struct SegmentMut {
    parent: Arc<MemoryMappedFile>,
    offset: u64,
    len: u64,
}

impl SegmentMut {
    /// Create a new mutable segment view. Performs bounds checks.
    ///
    /// # Errors
    ///
    /// Returns `MmapIoError::OutOfBounds` if the segment exceeds file bounds.
    pub fn new(parent: Arc<MemoryMappedFile>, offset: u64, len: u64) -> Result<Self> {
        // Validate bounds once at construction
        let total = parent.current_len()?;
        let _ = slice_range(offset, len, total)?;
        Ok(Self {
            parent,
            offset,
            len,
        })
    }

    /// Return a write-capable guard to the underlying bytes for this segment.
    /// The guard holds the write lock for the duration of the mutable borrow.
    ///
    /// # Errors
    ///
    /// Returns errors from the underlying `MemoryMappedFile::as_slice_mut` call.
    ///
    /// Note: Bounds are already validated at construction, so as_slice_mut
    /// will not perform redundant validation.
    pub fn as_slice_mut(&self) -> Result<crate::mmap::MappedSliceMut<'_>> {
        // Bounds already validated in constructor
        self.parent.as_slice_mut(self.offset, self.len)
    }

    /// Write bytes into this segment from the provided slice.
    ///
    /// # Errors
    ///
    /// Returns errors from the underlying `MemoryMappedFile::update_region` call.
    pub fn write(&self, data: &[u8]) -> Result<()> {
        if data.len() as u64 != self.len {
            // Allow partial writes by delegating to update_region only over provided length.
            return self.parent.update_region(self.offset, data);
        }
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
}
