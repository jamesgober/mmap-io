//! Zero-copy iterator-based access to a memory-mapped file.
//!
//! [`ChunkIterator`] and [`PageIterator`] yield [`MappedSlice<'a>`]
//! items that borrow directly from the underlying mapping (no
//! allocation, no copy). On RW mappings the iterator holds a read
//! guard for its entire lifetime, which blocks any concurrent
//! `resize()` until iteration completes.
//!
//! The owned variants ([`ChunkIteratorOwned`], [`PageIteratorOwned`])
//! yield `Result<Vec<u8>>` for callers that genuinely need owned
//! buffers (e.g. when handing data to a thread that outlives the
//! mapping borrow). They allocate one `Vec<u8>` per item.

use crate::errors::{MmapIoError, Result};
use crate::mmap::{MapVariant, MappedSlice, MemoryMappedFile};
use crate::utils::page_size;
use memmap2::MmapMut;
use parking_lot::RwLockReadGuard;
use std::marker::PhantomData;

/// Internal guard variant: holds the RW read lock alive (so the
/// underlying mapping cannot be remapped via `resize()`), or is
/// `None` for RO / COW mappings whose mappings are inherently
/// immutable.
///
/// The `Held` variant's field is never read; it exists for its
/// destructor only (drops the read lock when the iterator is
/// dropped).
#[allow(dead_code)]
enum IterGuard<'a> {
    /// RW mapping: read guard kept alive for the iterator's life.
    Held(RwLockReadGuard<'a, MmapMut>),
    /// RO / COW mapping: no lock to hold.
    None,
}

/// Iterator over fixed-size chunks of a memory-mapped file.
///
/// Yields [`MappedSlice<'a>`] items that borrow directly from the
/// mapped region. The iterator holds the underlying read lock (on RW
/// mappings) for its lifetime, so calls to `resize()` from another
/// thread will block until the iterator is dropped.
///
/// # Examples
///
/// ```no_run
/// use mmap_io::MemoryMappedFile;
///
/// let mmap = MemoryMappedFile::open_ro("data.bin")?;
///
/// // Iterate over 4 KiB chunks. Each `chunk` is `MappedSlice<'_>`,
/// // which derefs to `&[u8]`.
/// for (i, chunk) in mmap.chunks(4096).enumerate() {
///     let _ = chunk.len();
///     let _first_byte = chunk[0];
///     println!("Chunk {} has {} bytes", i, chunk.len());
/// }
/// # Ok::<(), mmap_io::MmapIoError>(())
/// ```
pub struct ChunkIterator<'a> {
    /// Base pointer to the mapped region. Valid for `'a` because the
    /// guard (or the immutable underlying mapping for RO/COW) keeps
    /// the address space stable.
    base: *const u8,
    /// Total bytes in the mapping. Captured at iterator construction
    /// and not re-checked; the guard (RW) or immutable mapping
    /// (RO/COW) ensures the length cannot change during iteration.
    total_len: usize,
    /// Bytes per yielded chunk. The final chunk may be shorter.
    chunk_size: usize,
    /// Offset into the mapped region of the next chunk to yield.
    current_offset: usize,
    /// Lifetime / unmap guard.
    _guard: IterGuard<'a>,
    /// Notional borrow tying the raw pointer to `'a`.
    _marker: PhantomData<&'a [u8]>,
}

// SAFETY: ChunkIterator is `Send` because:
// - For `Held`, parking_lot's `RwLockReadGuard` is `Send` and `Sync`.
// - For `None`, no lock is held; the pointer targets an immutable
//   mapping that lives at least as long as `'a`.
// - The pointer itself targets `u8`, which is `Send + Sync`.
// The iterator yields immutable slices, so multiple yielded items
// can coexist as standard shared borrows.
unsafe impl<'a> Send for ChunkIterator<'a> {}
// SAFETY: same justification as `Send`; sharing `&ChunkIterator`
// across threads only allows calling `next()` from one thread at a
// time (Iterator's contract requires `&mut self`), and the read-only
// access pattern matches.
unsafe impl<'a> Sync for ChunkIterator<'a> {}

impl<'a> ChunkIterator<'a> {
    pub(crate) fn new(mmap: &'a MemoryMappedFile, chunk_size: usize) -> Result<Self> {
        let total_len = usize::try_from(mmap.current_len()?)
            .map_err(|_| MmapIoError::ResizeFailed("mapping length exceeds usize::MAX".into()))?;

        let (base, guard) = match &mmap.inner.map {
            MapVariant::Ro(m) => (m.as_ptr(), IterGuard::None),
            MapVariant::Rw(lock) => {
                let g = lock.read();
                let ptr = g.as_ptr();
                (ptr, IterGuard::Held(g))
            }
            MapVariant::Cow(m) => (m.as_ptr(), IterGuard::None),
        };

        Ok(Self {
            base,
            total_len,
            chunk_size,
            current_offset: 0,
            _guard: guard,
            _marker: PhantomData,
        })
    }
}

impl<'a> Iterator for ChunkIterator<'a> {
    type Item = MappedSlice<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.chunk_size == 0 || self.current_offset >= self.total_len {
            return None;
        }
        let remaining = self.total_len - self.current_offset;
        let chunk_len = remaining.min(self.chunk_size);

        // SAFETY: `base.add(current_offset)` produces a pointer
        // inside the mapped region because `current_offset <
        // total_len <= mapping length` (the mapping is stable for
        // `'a` per the guard / immutable variant in construction).
        // `slice::from_raw_parts` with `chunk_len <= remaining`
        // produces a slice that does not escape the mapped region.
        // The resulting `&'a [u8]` shares the lifetime of the guard
        // (`'a`), so multiple yielded chunks can coexist as
        // immutable borrows. No mutation of the mapping is possible
        // while the guard is alive (write lock would be required and
        // is blocked by the held read guard).
        let slice: &'a [u8] =
            unsafe { std::slice::from_raw_parts(self.base.add(self.current_offset), chunk_len) };
        self.current_offset += chunk_len;
        Some(MappedSlice::owned(slice))
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        if self.chunk_size == 0 {
            return (0, Some(0));
        }
        let remaining = self.total_len.saturating_sub(self.current_offset);
        let chunks = remaining.div_ceil(self.chunk_size);
        (chunks, Some(chunks))
    }
}

impl<'a> ExactSizeIterator for ChunkIterator<'a> {}

/// Iterator over page-aligned chunks of a memory-mapped file.
///
/// Equivalent to `mmap.chunks(page_size())` with the same zero-copy
/// guarantees.
///
/// # Examples
///
/// ```no_run
/// use mmap_io::MemoryMappedFile;
///
/// let mmap = MemoryMappedFile::open_ro("data.bin")?;
/// for page in mmap.pages() {
///     let _ = page.len();
/// }
/// # Ok::<(), mmap_io::MmapIoError>(())
/// ```
pub struct PageIterator<'a> {
    inner: ChunkIterator<'a>,
}

impl<'a> PageIterator<'a> {
    pub(crate) fn new(mmap: &'a MemoryMappedFile) -> Result<Self> {
        let ps = page_size();
        Ok(Self {
            inner: ChunkIterator::new(mmap, ps)?,
        })
    }
}

impl<'a> Iterator for PageIterator<'a> {
    type Item = MappedSlice<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        self.inner.next()
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.inner.size_hint()
    }
}

impl<'a> ExactSizeIterator for PageIterator<'a> {}

/// Migration-aid iterator that yields owned `Vec<u8>` chunks. Each
/// chunk is allocated and copied from the mapping. Prefer
/// [`ChunkIterator`] (via `chunks()`) for zero-copy access.
///
/// # Examples
///
/// ```no_run
/// use mmap_io::MemoryMappedFile;
///
/// let mmap = MemoryMappedFile::open_ro("data.bin")?;
/// for chunk in mmap.chunks_owned(4096) {
///     let bytes: Vec<u8> = chunk?;
///     let _ = bytes;
/// }
/// # Ok::<(), mmap_io::MmapIoError>(())
/// ```
pub struct ChunkIteratorOwned<'a> {
    inner: ChunkIterator<'a>,
}

impl<'a> ChunkIteratorOwned<'a> {
    pub(crate) fn new(mmap: &'a MemoryMappedFile, chunk_size: usize) -> Result<Self> {
        Ok(Self {
            inner: ChunkIterator::new(mmap, chunk_size)?,
        })
    }
}

impl<'a> Iterator for ChunkIteratorOwned<'a> {
    type Item = Result<Vec<u8>>;

    fn next(&mut self) -> Option<Self::Item> {
        self.inner.next().map(|slice| Ok(slice.as_slice().to_vec()))
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.inner.size_hint()
    }
}

impl<'a> ExactSizeIterator for ChunkIteratorOwned<'a> {}

/// Migration-aid iterator that yields owned page-sized `Vec<u8>`
/// buffers. Prefer [`PageIterator`] (via `pages()`) for zero-copy.
pub struct PageIteratorOwned<'a> {
    inner: PageIterator<'a>,
}

impl<'a> PageIteratorOwned<'a> {
    pub(crate) fn new(mmap: &'a MemoryMappedFile) -> Result<Self> {
        Ok(Self {
            inner: PageIterator::new(mmap)?,
        })
    }
}

impl<'a> Iterator for PageIteratorOwned<'a> {
    type Item = Result<Vec<u8>>;

    fn next(&mut self) -> Option<Self::Item> {
        self.inner.next().map(|slice| Ok(slice.as_slice().to_vec()))
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.inner.size_hint()
    }
}

impl<'a> ExactSizeIterator for PageIteratorOwned<'a> {}

/// Mutable chunk iterator: callback-based because Rust's borrow
/// checker does not allow yielding multiple mutable references from
/// one iterator. The iterator acquires the underlying RW write lock
/// once and holds it for the entire iteration, then drives the
/// caller's closure on each chunk in order.
pub struct ChunkIteratorMut<'a> {
    mmap: &'a MemoryMappedFile,
    chunk_size: usize,
    total_len: u64,
    _phantom: PhantomData<&'a mut [u8]>,
}

impl<'a> ChunkIteratorMut<'a> {
    pub(crate) fn new(mmap: &'a MemoryMappedFile, chunk_size: usize) -> Result<Self> {
        let total_len = mmap.current_len()?;
        Ok(Self {
            mmap,
            chunk_size,
            total_len,
            _phantom: PhantomData,
        })
    }

    /// Process each chunk under a single held write guard. The
    /// closure receives `(offset, &mut [u8])` for each chunk in
    /// order. Returning `Err` aborts iteration and surfaces the
    /// error to the caller.
    ///
    /// The closure's error type is the crate's [`MmapIoError`].
    /// Callers carrying a foreign error type should map into
    /// `MmapIoError` before returning (e.g. via `.map_err(|e|
    /// MmapIoError::Io(...))`).
    pub fn for_each_mut<F>(self, mut f: F) -> Result<()>
    where
        F: FnMut(u64, &mut [u8]) -> Result<()>,
    {
        if self.chunk_size == 0 || self.total_len == 0 {
            return Ok(());
        }
        match &self.mmap.inner.map {
            MapVariant::Ro(_) => Err(MmapIoError::InvalidMode(
                "chunks_mut requires ReadWrite mode",
            )),
            MapVariant::Cow(_) => Err(MmapIoError::InvalidMode(
                "chunks_mut on copy-on-write mapping is not supported (phase-1 read-only)",
            )),
            MapVariant::Rw(lock) => {
                let mut guard = lock.write();
                let total = self.total_len as usize;
                let chunk_size = self.chunk_size;
                let mut offset = 0usize;
                while offset < total {
                    let remaining = total - offset;
                    let chunk_len = remaining.min(chunk_size);
                    let end = offset + chunk_len;
                    f(offset as u64, &mut guard[offset..end])?;
                    offset = end;
                }
                Ok(())
            }
        }
    }

    /// Migration shim that mirrors the 0.9.6 `for_each_mut`
    /// signature: returns `Result<std::result::Result<(), E>>`
    /// where `E` is the closure's error type.
    ///
    /// **Prefer [`for_each_mut`](Self::for_each_mut)** for new
    /// code; that method was flattened to `Result<()>` (using
    /// the crate's `MmapIoError`) in 0.9.7. Foreign error types
    /// should be mapped via `.map_err(|e| MmapIoError::Io(...))`
    /// before returning.
    ///
    /// This shim exists for callers migrating off the 0.9.6
    /// signature. Internally it still uses the new single-held-
    /// guard implementation (the H2 perf win is preserved); only
    /// the return shape is back-compat.
    ///
    /// # Errors
    ///
    /// Returns the outer `Err(MmapIoError)` for any mmap-side
    /// failure (e.g. RW lock unavailable, OOB chunk during a
    /// concurrent resize). Returns `Ok(Err(E))` for closure
    /// errors. Returns `Ok(Ok(()))` when iteration completes
    /// cleanly.
    pub fn for_each_mut_legacy<F, E>(self, mut f: F) -> Result<std::result::Result<(), E>>
    where
        F: FnMut(u64, &mut [u8]) -> std::result::Result<(), E>,
    {
        if self.chunk_size == 0 || self.total_len == 0 {
            return Ok(Ok(()));
        }
        match &self.mmap.inner.map {
            MapVariant::Ro(_) => Err(MmapIoError::InvalidMode(
                "chunks_mut requires ReadWrite mode",
            )),
            MapVariant::Cow(_) => Err(MmapIoError::InvalidMode(
                "chunks_mut on copy-on-write mapping is not supported (phase-1 read-only)",
            )),
            MapVariant::Rw(lock) => {
                let mut guard = lock.write();
                let total = self.total_len as usize;
                let chunk_size = self.chunk_size;
                let mut offset = 0usize;
                while offset < total {
                    let remaining = total - offset;
                    let chunk_len = remaining.min(chunk_size);
                    let end = offset + chunk_len;
                    match f(offset as u64, &mut guard[offset..end]) {
                        Ok(()) => offset = end,
                        Err(e) => return Ok(Err(e)),
                    }
                }
                Ok(Ok(()))
            }
        }
    }
}

impl MemoryMappedFile {
    /// Zero-copy chunk iterator. Yields [`MappedSlice<'_>`] of size
    /// `chunk_size` (final chunk may be shorter).
    ///
    /// For RW mappings, the iterator holds a read guard for its
    /// lifetime; concurrent `resize()` blocks until the iterator is
    /// dropped.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use mmap_io::MemoryMappedFile;
    /// let mmap = MemoryMappedFile::open_ro("data.bin")?;
    /// for chunk in mmap.chunks(64 * 1024) {
    ///     let _ = chunk.len();
    /// }
    /// # Ok::<(), mmap_io::MmapIoError>(())
    /// ```
    #[cfg(feature = "iterator")]
    #[must_use]
    pub fn chunks(&self, chunk_size: usize) -> ChunkIterator<'_> {
        ChunkIterator::new(self, chunk_size).expect("chunk iterator creation should not fail")
    }

    /// Zero-copy page-aligned iterator.
    #[cfg(feature = "iterator")]
    #[must_use]
    pub fn pages(&self) -> PageIterator<'_> {
        PageIterator::new(self).expect("page iterator creation should not fail")
    }

    /// Migration-aid: chunk iterator yielding owned `Vec<u8>` items.
    /// Allocates one `Vec<u8>` per chunk and copies the data into it.
    /// Prefer `chunks()` for zero-copy.
    #[cfg(feature = "iterator")]
    #[must_use]
    pub fn chunks_owned(&self, chunk_size: usize) -> ChunkIteratorOwned<'_> {
        ChunkIteratorOwned::new(self, chunk_size)
            .expect("owned chunk iterator creation should not fail")
    }

    /// Migration-aid: page iterator yielding owned `Vec<u8>` items.
    /// Prefer `pages()` for zero-copy.
    #[cfg(feature = "iterator")]
    #[must_use]
    pub fn pages_owned(&self) -> PageIteratorOwned<'_> {
        PageIteratorOwned::new(self).expect("owned page iterator creation should not fail")
    }

    /// Callback-driven mutable iterator. Acquires a single write
    /// guard for the entire iteration. Available only on
    /// `ReadWrite` mappings.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use mmap_io::MemoryMappedFile;
    ///
    /// let mmap = MemoryMappedFile::open_rw("data.bin")?;
    /// mmap.chunks_mut(4096).for_each_mut(|_offset, chunk| {
    ///     chunk.fill(0);
    ///     Ok(())
    /// })?;
    /// # Ok::<(), mmap_io::MmapIoError>(())
    /// ```
    #[cfg(feature = "iterator")]
    #[must_use]
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
    fn test_chunk_iterator_zero_copy() {
        let path = tmp_path("chunk_iter");
        let _ = fs::remove_file(&path);

        let mmap = create_mmap(&path, 10240).expect("create");
        for i in 0..10 {
            let data = vec![i as u8; 1024];
            mmap.update_region(i * 1024, &data).expect("write");
        }
        mmap.flush().expect("flush");

        // Aligned chunks: 10 x 1024.
        let chunks: Vec<Vec<u8>> = mmap.chunks(1024).map(|s| s.as_slice().to_vec()).collect();
        assert_eq!(chunks.len(), 10);
        for (i, chunk) in chunks.iter().enumerate() {
            assert_eq!(chunk.len(), 1024);
            assert!(chunk.iter().all(|&b| b == i as u8));
        }

        // Unaligned chunks: 3000 / 3000 / 3000 / 1240.
        let chunks: Vec<Vec<u8>> = mmap.chunks(3000).map(|s| s.as_slice().to_vec()).collect();
        assert_eq!(chunks.len(), 4);
        assert_eq!(chunks[3].len(), 1240);

        drop(mmap);
        fs::remove_file(&path).expect("cleanup");
    }

    #[test]
    #[cfg(feature = "iterator")]
    fn test_page_iterator_zero_copy() {
        let path = tmp_path("page_iter");
        let _ = fs::remove_file(&path);

        let ps = page_size();
        let file_size = ps * 3 + 100;

        let mmap = create_mmap(&path, file_size as u64).expect("create");

        let pages: Vec<usize> = mmap.pages().map(|p| p.len()).collect();
        assert_eq!(pages.len(), 4);
        assert_eq!(pages[0], ps);
        assert_eq!(pages[1], ps);
        assert_eq!(pages[2], ps);
        assert_eq!(pages[3], 100);

        drop(mmap);
        fs::remove_file(&path).expect("cleanup");
    }

    #[test]
    #[cfg(feature = "iterator")]
    fn test_chunks_owned_compat() {
        let path = tmp_path("chunks_owned");
        let _ = fs::remove_file(&path);

        let mmap = create_mmap(&path, 4096).expect("create");
        mmap.update_region(0, &vec![0x11u8; 4096]).expect("write");

        let owned: Vec<Vec<u8>> = mmap
            .chunks_owned(1024)
            .collect::<Result<Vec<_>>>()
            .expect("collect");
        assert_eq!(owned.len(), 4);
        for v in &owned {
            assert_eq!(v.len(), 1024);
            assert!(v.iter().all(|&b| b == 0x11));
        }

        drop(mmap);
        fs::remove_file(&path).expect("cleanup");
    }

    #[test]
    #[cfg(feature = "iterator")]
    fn test_mutable_chunk_iterator_single_guard() {
        let path = tmp_path("mut_chunk_iter");
        let _ = fs::remove_file(&path);

        let mmap = create_mmap(&path, 4096).expect("create");

        mmap.chunks_mut(1024)
            .for_each_mut(|offset, chunk| {
                let value = (offset / 1024) as u8;
                chunk.fill(value);
                Ok(())
            })
            .expect("for_each_mut");

        mmap.flush().expect("flush");

        let mut buf = [0u8; 1024];
        for i in 0..4 {
            mmap.read_into(i * 1024, &mut buf).expect("read");
            assert!(buf.iter().all(|&b| b == i as u8));
        }

        drop(mmap);
        fs::remove_file(&path).expect("cleanup");
    }

    #[test]
    #[cfg(feature = "iterator")]
    fn test_iterator_size_hint() {
        let path = tmp_path("size_hint");
        let _ = fs::remove_file(&path);

        let mmap = create_mmap(&path, 10000).expect("create");

        {
            let iter = mmap.chunks(1000);
            assert_eq!(iter.size_hint(), (10, Some(10)));
        }
        {
            let iter = mmap.chunks(3000);
            assert_eq!(iter.size_hint(), (4, Some(4)));
        }

        drop(mmap);
        fs::remove_file(&path).expect("cleanup");
    }

    #[test]
    #[cfg(feature = "iterator")]
    fn test_iterator_zero_chunk_size_yields_nothing() {
        let path = tmp_path("zero_chunk");
        let _ = fs::remove_file(&path);

        let mmap = create_mmap(&path, 4096).expect("create");
        assert_eq!(mmap.chunks(0).count(), 0);

        drop(mmap);
        fs::remove_file(&path).expect("cleanup");
    }

    #[test]
    #[cfg(feature = "iterator")]
    fn test_one_byte_file_iteration() {
        let path = tmp_path("one_byte_iter");
        let _ = fs::remove_file(&path);

        let mmap = create_mmap(&path, 1).expect("create");
        let chunks: Vec<usize> = mmap.chunks(1024).map(|s| s.len()).collect();
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0], 1);

        drop(mmap);
        fs::remove_file(&path).expect("cleanup");
    }
}
