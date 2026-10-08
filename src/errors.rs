//! Crate-specific error types for mmap-io.

use std::fmt;
use std::io;

/// Result alias for mmap-io operations.
pub type Result<T> = std::result::Result<T, MmapIoError>;

/// Error type covering filesystem, mapping, bounds, and concurrency issues.
#[derive(Debug)]
pub enum MmapIoError {
    /// Wrapper for `std::io::Error`.
    Io(io::Error),

    /// Error returned when attempting an operation in an incompatible mode.
    InvalidMode(&'static str),

    /// Error when a requested offset/length pair is out of bounds.
    OutOfBounds {
        /// Requested offset.
        offset: u64,
        /// Requested length.
        len: u64,
        /// Total size of the mapped file.
        total: u64,
    },

    /// Error when a flush operation fails.
    FlushFailed(String),

    /// Error when resizing is not allowed or fails.
    ResizeFailed(String),

    /// Error when memory advise fails.
    AdviceFailed(String),

    /// Error when lock operation fails.
    LockFailed(String),

    /// Error when unlock operation fails.
    UnlockFailed(String),

    /// Error when alignment is required for atomic memory views.
    Misaligned {
        /// Required alignment in bytes.
        required: u64,
        /// Provided offset in bytes.
        offset: u64,
    },

    /// Error when starting or running a watcher fails.
    WatchFailed(String),
}

impl fmt::Display for MmapIoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "I/O error: {e}"),
            Self::InvalidMode(msg) => write!(f, "invalid access mode: {msg}"),
            Self::OutOfBounds { offset, len, total } => write!(
                f,
                "range out of bounds: offset={offset}, len={len}, total={total}"
            ),
            Self::FlushFailed(msg) => write!(f, "flush failed: {msg}"),
            Self::ResizeFailed(msg) => write!(f, "resize failed: {msg}"),
            Self::AdviceFailed(msg) => write!(f, "advice failed: {msg}"),
            Self::LockFailed(msg) => write!(f, "lock failed: {msg}"),
            Self::UnlockFailed(msg) => write!(f, "unlock failed: {msg}"),
            Self::Misaligned { required, offset } => write!(
                f,
                "atomic alignment error: required={required}, offset={offset}"
            ),
            Self::WatchFailed(msg) => write!(f, "watch failed: {msg}"),
        }
    }
}

impl std::error::Error for MmapIoError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<io::Error> for MmapIoError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}
