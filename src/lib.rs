//! # mmap-io: High-performance memory-mapped file I/O for Rust
//!
//! This crate provides a safe, efficient interface for memory-mapped file operations
//! with support for concurrent access, segmented views, and optional async operations.
//!
//! ## Features
//!
//! - **Zero-copy I/O**: Direct memory access without buffer copying
//! - **Thread-safe**: Concurrent read/write access with proper synchronization
//! - **Segmented access**: Work with file regions without loading entire files
//! - **Cross-platform**: Works on Windows, Linux, macOS via memmap2
//! - **Async support**: Optional Tokio integration for async file operations
//!
//! ## Quick Start
//!
//! ```no_run
//! use mmap_io::{create_mmap, update_region, flush};
//!
//! // Create a 1MB memory-mapped file
//! let mmap = create_mmap("data.bin", 1024 * 1024)?;
//!
//! // Write data at offset 100
//! update_region(&mmap, 100, b"Hello, mmap!")?;
//!
//! // Ensure data is persisted
//! flush(&mmap)?;
//! # Ok::<(), mmap_io::MmapIoError>(())
//! ```
//!
//! ## Modules
//!
//! - [`errors`]: Error types for all mmap operations
//! - [`utils`]: Utility functions for alignment and bounds checking
//! - [`mmap`]: Core `MemoryMappedFile` implementation
//! - [`segment`]: Segmented views for working with file regions
//! - [`manager`]: High-level convenience functions
//!
//! ## Feature Flags
//!
//! - `async`: Enables Tokio-based async file operations

#![cfg_attr(not(test), deny(clippy::unwrap_used))]
#![deny(missing_docs)]
#![doc(html_root_url = "https://docs.rs/mmap-io")]

pub mod errors;
pub mod manager;
/// Memory-mapped file support.
pub mod mmap;
pub mod segment;
pub mod utils;

/// Provides functions for flushing memory-mapped file changes to disk.
pub mod flush;

#[cfg(feature = "advise")]
pub mod advise;

#[cfg(feature = "iterator")]
pub mod iterator;

#[cfg(feature = "locking")]
pub mod lock;

#[cfg(feature = "atomic")]
pub mod atomic;

#[cfg(feature = "watch")]
pub mod watch;

pub use errors::MmapIoError;
pub use manager::{
    copy_mmap, create_mmap, delete_mmap, flush, load_mmap, update_region, write_mmap,
};
pub use mmap::{MemoryMappedFile, MmapMode, TouchHint};

#[cfg(feature = "advise")]
pub use advise::MmapAdvice;

#[cfg(feature = "iterator")]
pub use iterator::{ChunkIterator, PageIterator};

#[cfg(feature = "watch")]
pub use watch::{ChangeEvent, ChangeKind, WatchHandle};
