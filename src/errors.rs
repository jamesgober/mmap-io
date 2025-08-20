//! Crate-specific error types for mmap-io.

use std::io;
use thiserror::Error;

/// Result alias for mmap-io operations.
pub type Result<T> = std::result::Result<T, MmapIoError>;

/// Error type covering filesystem, mapping, bounds, and concurrency issues.
#[derive(Debug, Error)]
pub enum MmapIoError {
    /// Wrapper for `std::io::Error`.
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),

    /// Error returned when attempting an operation in an incompatible mode.
    #[error("invalid access mode: {0}")]
    InvalidMode(&'static str),

    /// Error when a requested offset/length pair is out of bounds.
    #[error("range out of bounds: offset={offset}, len={len}, total={total}")]
    OutOfBounds {
        /// Requested offset.
        offset: u64,
        /// Requested length.
        len: u64,
        /// Total size of the mapped file.
        total: u64,
    },

    /// Error when a flush operation fails.
    #[error("flush failed: {0}")]
    FlushFailed(String),

    /// Error when resizing is not allowed or fails.
    #[error("resize failed: {0}")]
    ResizeFailed(String),

    /// Error when memory advise fails.
    #[error("advice failed: {0}")]
    AdviceFailed(String),

    /// Error when lock operation fails.
    #[error("lock failed: {0}")]
    LockFailed(String),

    /// Error when unlock operation fails.
    #[error("unlock failed: {0}")]
    UnlockFailed(String),

    /// Error when alignment is required for atomic memory views.
    #[error("atomic alignment error: required={required}, offset={offset}")]
    Misaligned {
        /// Required alignment in bytes.
        required: u64,
        /// Provided offset in bytes.
        offset: u64,
    },

    /// Error when starting or running a watcher fails.
    #[error("watch failed: {0}")]
    WatchFailed(String),
}
