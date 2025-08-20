//! High-level API for managing memory-mapped files.
//!
//! Provides convenience functions that wrap low-level mmap operations.

use std::fs;
use std::path::Path;

use crate::errors::Result;
use crate::mmap::{MemoryMappedFile, MmapMode};

/// Create a new read-write memory-mapped file of the given size.
/// Truncates if the file already exists.
///
/// # Errors
///
/// Returns errors from `MemoryMappedFile::create_rw`.
pub fn create_mmap<P: AsRef<Path>>(path: P, size: u64) -> Result<MemoryMappedFile> {
    MemoryMappedFile::create_rw(path, size)
}

/// Load an existing memory-mapped file in the requested mode.
///
/// # Errors
///
/// Returns errors from `MemoryMappedFile::open_ro` or `open_rw`.
pub fn load_mmap<P: AsRef<Path>>(path: P, mode: MmapMode) -> Result<MemoryMappedFile> {
    match mode {
        MmapMode::ReadOnly => MemoryMappedFile::open_ro(path),
        MmapMode::ReadWrite => MemoryMappedFile::open_rw(path),
        #[cfg(feature = "cow")]
        MmapMode::CopyOnWrite => MemoryMappedFile::open_cow(path),
        #[cfg(not(feature = "cow"))]
        MmapMode::CopyOnWrite => Err(crate::errors::MmapIoError::InvalidMode(
            "copy-on-write mode not enabled (feature `cow`)",
        )),
    }
}

/// Write bytes at an offset into the specified file path (RW).
/// Convenience wrapper around creating/loading and `update_region`.
///
/// # Errors
///
/// Returns errors from file opening or update operations.
pub fn write_mmap<P: AsRef<Path>>(path: P, offset: u64, data: &[u8]) -> Result<()> {
    let mmap = MemoryMappedFile::open_rw(path)?;
    mmap.update_region(offset, data)
}

/// Update a region in an existing mapping (RW).
///
/// # Errors
///
/// Returns errors from `MemoryMappedFile::update_region`.
pub fn update_region(mmap: &MemoryMappedFile, offset: u64, data: &[u8]) -> Result<()> {
    mmap.update_region(offset, data)
}

/// Flush changes for an existing mapping.
///
/// # Errors
///
/// Returns errors from `MemoryMappedFile::flush`.
pub fn flush(mmap: &MemoryMappedFile) -> Result<()> {
    mmap.flush()
}

/// Copy a mapped file to a new destination using the filesystem.
/// This does not copy the mapping identity, only the underlying file contents.
///
/// # Errors
///
/// Returns `MmapIoError::Io` if the copy operation fails.
pub fn copy_mmap<P: AsRef<Path>>(src: P, dst: P) -> Result<()> {
    fs::copy(src, dst)?;
    Ok(())
}

/// Delete the file backing a mapping path. The mapping itself should be dropped by users before invoking this.
/// On Unix, deleting an open file keeps the data until last handle drops; prefer dropping mappings before deleting.
///
/// # Errors
///
/// Returns `MmapIoError::Io` if the delete operation fails.
pub fn delete_mmap<P: AsRef<Path>>(path: P) -> Result<()> {
    fs::remove_file(path)?;
    Ok(())
}

#[cfg(feature = "async")]
pub mod r#async {
    //! Async helpers (Tokio) for creating and copying files without blocking the current thread.
    use std::path::Path;

    use tokio::fs as tfs;

    use crate::errors::Result;
    use crate::mmap::MemoryMappedFile;

    /// Create a new file with the specified size asynchronously, then map it RW.
    ///
    /// # Errors
    ///
    /// Returns errors from async file operations or mapping.
    pub async fn create_mmap_async<P: AsRef<Path>>(path: P, size: u64) -> Result<MemoryMappedFile> {
        let path_ref = path.as_ref();
        // Create and set size via tokio
        let file = tfs::OpenOptions::new()
            .create(true)
            .write(true)
            .read(true)
            .truncate(true)
            .open(path_ref)
            .await?;
        file.set_len(size).await?;
        drop(file);
        MemoryMappedFile::open_rw(path_ref)
    }

    /// Copy a file asynchronously.
    ///
    /// # Errors
    ///
    /// Returns `MmapIoError::Io` if the async copy operation fails.
    pub async fn copy_mmap_async<P: AsRef<Path>>(src: P, dst: P) -> Result<()> {
        tfs::copy(src, dst).await?;
        Ok(())
    }

    /// Delete a file asynchronously.
    ///
    /// # Errors
    ///
    /// Returns `MmapIoError::Io` if the async delete operation fails.
    pub async fn delete_mmap_async<P: AsRef<Path>>(path: P) -> Result<()> {
        tfs::remove_file(path).await?;
        Ok(())
    }
}
