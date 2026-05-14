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
    //! Runtime-agnostic async helpers for file lifecycle operations.
    //!
    //! Since 0.9.11 these wrap `std::fs` calls inside
    //! `blocking::unblock`, which dispatches to a dedicated thread
    //! pool managed by the `blocking` crate. The pool is shared
    //! with every other crate using `blocking` (smol, async-net,
    //! async-fs, etc.) and does not require a specific async
    //! runtime. Works on tokio, smol, async-std, or any future
    //! executor that drives `Future`s to completion.
    use std::fs;
    use std::path::{Path, PathBuf};

    use crate::errors::{MmapIoError, Result};
    use crate::mmap::MemoryMappedFile;

    /// Create a new file with the specified size asynchronously, then map it RW.
    ///
    /// # Errors
    ///
    /// Returns errors from the underlying filesystem call or mapping.
    pub async fn create_mmap_async<P: AsRef<Path>>(path: P, size: u64) -> Result<MemoryMappedFile> {
        let path: PathBuf = path.as_ref().to_path_buf();
        blocking::unblock(move || -> Result<MemoryMappedFile> {
            let file = fs::OpenOptions::new()
                .create(true)
                .write(true)
                .read(true)
                .truncate(true)
                .open(&path)
                .map_err(MmapIoError::Io)?;
            file.set_len(size).map_err(MmapIoError::Io)?;
            drop(file);
            MemoryMappedFile::open_rw(&path)
        })
        .await
    }

    /// Copy a file asynchronously.
    ///
    /// # Errors
    ///
    /// Returns `MmapIoError::Io` if the underlying copy operation fails.
    pub async fn copy_mmap_async<P: AsRef<Path>>(src: P, dst: P) -> Result<()> {
        let src: PathBuf = src.as_ref().to_path_buf();
        let dst: PathBuf = dst.as_ref().to_path_buf();
        blocking::unblock(move || fs::copy(&src, &dst).map(|_| ()).map_err(MmapIoError::Io)).await
    }

    /// Delete a file asynchronously.
    ///
    /// # Errors
    ///
    /// Returns `MmapIoError::Io` if the underlying delete operation fails.
    pub async fn delete_mmap_async<P: AsRef<Path>>(path: P) -> Result<()> {
        let path: PathBuf = path.as_ref().to_path_buf();
        blocking::unblock(move || fs::remove_file(&path).map_err(MmapIoError::Io)).await
    }
}
