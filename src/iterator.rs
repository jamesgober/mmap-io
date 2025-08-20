//! Iterator-based access for efficient sequential processing of memory-mapped files.

use crate::errors::Result;
use crate::mmap::MemoryMappedFile;
use crate::utils::page_size;
use std::marker::PhantomData;

/// Iterator over fixed-size chunks of a memory-mapped file.
///
/// # Examples
///
/// ```no_run
/// use mmap_io::MemoryMappedFile;
///
/// let mmap = MemoryMappedFile::open_ro("data.bin")?;
///
/// // Iterate over 4KB chunks
/// for (offset, chunk) in mmap.chunks(4096).enumerate() {
///     let chunk_data = chunk?;
///     println!("Chunk {} at offset {}: {} bytes",
///              offset, offset * 4096, chunk_data.len());
/// }
/// # Ok::<(), mmap_io::MmapIoError>(())
/// ```
pub struct ChunkIterator<'a> {
    mmap: &'a MemoryMappedFile,
    chunk_size: usize,
    current_offset: u64,
    total_len: u64,
    // Reusable buffer to avoid allocations on each iteration
    buffer: Vec<u8>,
}

impl<'a> ChunkIterator<'a> {
    /// Create a new chunk iterator.
    pub(crate) fn new(mmap: &'a MemoryMappedFile, chunk_size: usize) -> Result<Self> {
        let total_len = mmap.current_len()?;
        // Pre-allocate buffer with chunk_size capacity
        let buffer = Vec::with_capacity(chunk_size);
        Ok(Self {
            mmap,
            chunk_size,
            current_offset: 0,
            total_len,
            buffer,
        })
    }
}

impl<'a> Iterator for ChunkIterator<'a> {
    type Item = Result<Vec<u8>>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.current_offset >= self.total_len {
            return None;
        }

        let remaining = self.total_len - self.current_offset;
        let chunk_len = remaining.min(self.chunk_size as u64);

        // Resize the reusable buffer to the exact chunk size needed
        self.buffer.resize(chunk_len as usize, 0);

        // For RW mappings, we need to use read_into
        match self.mmap.read_into(self.current_offset, &mut self.buffer) {
            Ok(()) => {
                self.current_offset += chunk_len;
                // Clone the buffer data to return ownership
                // This is necessary because we reuse the buffer
                Some(Ok(self.buffer.clone()))
            }
            Err(e) => Some(Err(e)),
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = self.total_len.saturating_sub(self.current_offset);
        let chunks = (remaining as usize).div_ceil(self.chunk_size);
        (chunks, Some(chunks))
    }
}

impl<'a> ExactSizeIterator for ChunkIterator<'a> {}

/// Iterator over page-aligned chunks of a memory-mapped file.
///
/// Pages are aligned to the system's page size for optimal performance.
///
/// # Examples
///
/// ```no_run
/// use mmap_io::MemoryMappedFile;
///
/// let mmap = MemoryMappedFile::open_ro("data.bin")?;
///
/// // Iterate over system pages
/// for page in mmap.pages() {
///     let page_data = page?;
///     // Process page...
/// }
/// # Ok::<(), mmap_io::MmapIoError>(())
/// ```
pub struct PageIterator<'a> {
    inner: ChunkIterator<'a>,
}

impl<'a> PageIterator<'a> {
    /// Create a new page iterator.
    pub(crate) fn new(mmap: &'a MemoryMappedFile) -> Result<Self> {
        let ps = page_size();
        Ok(Self {
            inner: ChunkIterator::new(mmap, ps)?,
        })
    }
}

impl<'a> Iterator for PageIterator<'a> {
    type Item = Result<Vec<u8>>;

    fn next(&mut self) -> Option<Self::Item> {
        self.inner.next()
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.inner.size_hint()
    }
}

impl<'a> ExactSizeIterator for PageIterator<'a> {}

/// Mutable iterator over fixed-size chunks of a memory-mapped file.
///
/// This iterator provides mutable access to chunks, but due to Rust's borrowing
/// rules, it cannot yield multiple mutable references simultaneously. Instead,
/// it provides a callback-based interface.
pub struct ChunkIteratorMut<'a> {
    mmap: &'a MemoryMappedFile,
    chunk_size: usize,
    current_offset: u64,
    total_len: u64,
    _phantom: PhantomData<&'a mut [u8]>,
}

impl<'a> ChunkIteratorMut<'a> {
    /// Create a new mutable chunk iterator.
    pub(crate) fn new(mmap: &'a MemoryMappedFile, chunk_size: usize) -> Result<Self> {
        let total_len = mmap.current_len()?;
        Ok(Self {
            mmap,
            chunk_size,
            current_offset: 0,
            total_len,
            _phantom: PhantomData,
        })
    }

    /// Process each chunk with a callback function.
    ///
    /// The callback receives the offset and a mutable slice for each chunk.
    pub fn for_each_mut<F, E>(mut self, mut f: F) -> Result<std::result::Result<(), E>>
    where
        F: FnMut(u64, &mut [u8]) -> std::result::Result<(), E>,
    {
        while self.current_offset < self.total_len {
            let remaining = self.total_len - self.current_offset;
            let chunk_len = remaining.min(self.chunk_size as u64);

            let mut guard = self.mmap.as_slice_mut(self.current_offset, chunk_len)?;
            let slice = guard.as_mut();

            match f(self.current_offset, slice) {
                Ok(()) => {}
                Err(e) => return Ok(Err(e)),
            }

            self.current_offset += chunk_len;
        }
        Ok(Ok(()))
    }
}

impl MemoryMappedFile {
    /// Create an iterator over fixed-size chunks of the file.
    ///
    /// For read-only and copy-on-write mappings, this returns immutable slices.
    /// For read-write mappings, use `chunks_mut()` for mutable access.
    ///
    /// # Arguments
    ///
    /// * `chunk_size` - Size of each chunk in bytes
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use mmap_io::MemoryMappedFile;
    ///
    /// let mmap = MemoryMappedFile::open_ro("data.bin")?;
    ///
    /// // Process file in 1MB chunks
    /// for chunk in mmap.chunks(1024 * 1024) {
    ///     let data = chunk?;
    ///     // Process chunk...
    /// }
    /// # Ok::<(), mmap_io::MmapIoError>(())
    /// ```
    #[cfg(feature = "iterator")]
    pub fn chunks(&self, chunk_size: usize) -> ChunkIterator<'_> {
        ChunkIterator::new(self, chunk_size).expect("chunk iterator creation should not fail")
    }

    /// Create an iterator over page-aligned chunks of the file.
    ///
    /// Pages are aligned to the system's page size, which is typically 4KB on most systems.
    /// This can provide better performance for certain access patterns.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use mmap_io::MemoryMappedFile;
    ///
    /// let mmap = MemoryMappedFile::open_ro("data.bin")?;
    ///
    /// // Process file page by page
    /// for page in mmap.pages() {
    ///     let data = page?;
    ///     // Process page...
    /// }
    /// # Ok::<(), mmap_io::MmapIoError>(())
    /// ```
    #[cfg(feature = "iterator")]
    pub fn pages(&self) -> PageIterator<'_> {
        PageIterator::new(self).expect("page iterator creation should not fail")
    }

    /// Create a mutable iterator over fixed-size chunks of the file.
    ///
    /// This is only available for read-write mappings. Due to Rust's borrowing rules,
    /// this returns an iterator that processes chunks through a callback.
    ///
    /// # Arguments
    ///
    /// * `chunk_size` - Size of each chunk in bytes
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use mmap_io::{MemoryMappedFile, MmapMode};
    ///
    /// let mmap = MemoryMappedFile::open_rw("data.bin")?;
    ///
    /// // Zero out file in 4KB chunks
    /// mmap.chunks_mut(4096).for_each_mut(|offset, chunk| {
    ///     chunk.fill(0);
    ///     Ok::<(), std::io::Error>(())
    /// })??;
    /// # Ok::<(), mmap_io::MmapIoError>(())
    /// ```
    #[cfg(feature = "iterator")]
    pub fn chunks_mut(&self, chunk_size: usize) -> ChunkIteratorMut<'_> {
        ChunkIteratorMut::new(self, chunk_size)
            .expect("mutable chunk iterator creation should not fail")
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
            "mmap_io_iterator_test_{}_{}",
            name,
            std::process::id()
        ));
        p
    }

    #[test]
    #[cfg(feature = "iterator")]
    fn test_chunk_iterator() {
        let path = tmp_path("chunk_iter");
        let _ = fs::remove_file(&path);

        // Create file with pattern
        let mmap = create_mmap(&path, 10240).expect("create");
        for i in 0..10 {
            let data = vec![i as u8; 1024];
            mmap.update_region(i * 1024, &data).expect("write");
        }
        mmap.flush().expect("flush");

        // Test chunk iteration
        let chunks: Vec<_> = mmap
            .chunks(1024)
            .collect::<Result<Vec<_>>>()
            .expect("collect chunks");

        assert_eq!(chunks.len(), 10);
        for (i, chunk) in chunks.iter().enumerate() {
            assert_eq!(chunk.len(), 1024);
            assert!(chunk.iter().all(|&b| b == i as u8));
        }

        // Test with non-aligned chunk size
        let chunks: Vec<_> = mmap
            .chunks(3000)
            .collect::<Result<Vec<_>>>()
            .expect("collect chunks");

        assert_eq!(chunks.len(), 4); // 3000, 3000, 3000, 1240
        assert_eq!(chunks[3].len(), 1240);

        fs::remove_file(&path).expect("cleanup");
    }

    #[test]
    #[cfg(feature = "iterator")]
    fn test_page_iterator() {
        let path = tmp_path("page_iter");
        let _ = fs::remove_file(&path);

        let ps = page_size();
        let file_size = ps * 3 + 100; // 3 full pages + partial

        let mmap = create_mmap(&path, file_size as u64).expect("create");

        let pages: Vec<_> = mmap
            .pages()
            .collect::<Result<Vec<_>>>()
            .expect("collect pages");

        assert_eq!(pages.len(), 4); // 3 full + 1 partial
        assert_eq!(pages[0].len(), ps);
        assert_eq!(pages[1].len(), ps);
        assert_eq!(pages[2].len(), ps);
        assert_eq!(pages[3].len(), 100);

        fs::remove_file(&path).expect("cleanup");
    }

    #[test]
    #[cfg(feature = "iterator")]
    fn test_mutable_chunk_iterator() {
        let path = tmp_path("mut_chunk_iter");
        let _ = fs::remove_file(&path);

        let mmap = create_mmap(&path, 4096).expect("create");

        // Fill chunks with different values
        let result = mmap.chunks_mut(1024).for_each_mut(|offset, chunk| {
            let value = (offset / 1024) as u8;
            chunk.fill(value);
            Ok::<(), std::io::Error>(())
        });

        assert!(result.is_ok());
        assert!(result.unwrap().is_ok());

        mmap.flush().expect("flush");

        // Verify
        let mut buf = [0u8; 1024];
        for i in 0..4 {
            mmap.read_into(i * 1024, &mut buf).expect("read");
            assert!(buf.iter().all(|&b| b == i as u8));
        }

        fs::remove_file(&path).expect("cleanup");
    }

    #[test]
    #[cfg(feature = "iterator")]
    fn test_iterator_size_hint() {
        let path = tmp_path("size_hint");
        let _ = fs::remove_file(&path);

        let mmap = create_mmap(&path, 10000).expect("create");

        let iter = mmap.chunks(1000);
        assert_eq!(iter.size_hint(), (10, Some(10)));

        let iter = mmap.chunks(3000);
        assert_eq!(iter.size_hint(), (4, Some(4)));

        fs::remove_file(&path).expect("cleanup");
    }

    #[test]
    #[cfg(feature = "iterator")]
    fn test_empty_file_iteration() {
        let path = tmp_path("empty_iter");
        let _ = fs::remove_file(&path);

        let mmap = create_mmap(&path, 1).expect("create"); // Can't create 0-size
        mmap.resize(1).expect("resize"); // Keep it minimal

        let chunks: Vec<_> = mmap
            .chunks(1024)
            .collect::<Result<Vec<_>>>()
            .expect("collect");

        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].len(), 1);

        fs::remove_file(&path).expect("cleanup");
    }
}
