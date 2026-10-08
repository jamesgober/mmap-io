//! # mmap-io: memory-mapped file I/O for Rust
//!
//! Safe, zero-copy memory-mapped file operations with concurrent access,
//! segmented views, runtime-agnostic async, and atomic memory views.
//!
//! ## What you get
//!
//! - **Zero-copy reads** on read-only, read-write, and copy-on-write mappings.
//! - **Thread-safe** interior mutability via `parking_lot::RwLock`.
//! - **Segmented and chunked access** without loading whole files.
//! - **Cross-platform**: Linux, macOS, Windows, with per-platform fast paths.
//! - **Anonymous mappings** via [`AnonymousMmap`] for shared scratch memory.
//! - **Atomic views** (`feature = "atomic"`) for lock-free counters in mapped memory.
//! - **Runtime-agnostic async** (`feature = "async"`) on tokio, smol, async-std, or any executor.
//! - **`bytes::Bytes` integration** (`feature = "bytes"`) for the hyper/tower/tonic/axum/reqwest ecosystem.
//! - **Native file watching** (`feature = "watch"`) via inotify / FSEvents / `ReadDirectoryChangesW`.
//!
//! ## Quick start
//!
//! ```no_run
//! use mmap_io::MemoryMappedFile;
//!
//! // Opens "data.bin" if it exists; creates it at 1 MiB otherwise.
//! let mmap = MemoryMappedFile::open_or_create("data.bin", 1024 * 1024)?;
//! mmap.update_region(0, b"Hello, mmap!")?;
//! mmap.flush()?;
//! # Ok::<(), mmap_io::MmapIoError>(())
//! ```
//!
//! ## Modules
//!
//! - [`mmap`]: Core [`MemoryMappedFile`] implementation.
//! - [`anonymous`]: Process-local file-less [`AnonymousMmap`].
//! - [`segment`]: Segmented views for working with file regions.
//! - [`manager`]: High-level convenience functions ([`create_mmap`], [`load_mmap`], etc.).
//! - [`errors`]: Error types ([`MmapIoError`]).
//! - [`utils`]: Alignment and bounds-checking helpers.
//! - [`mod@flush`]: [`flush::FlushPolicy`] and time-based flushing.
//!
//! ## Feature flags
//!
//! All optional features are off by default except `advise` and `iterator`.
//! See the [README](https://github.com/jamesgober/mmap-io) for the full feature table.

#![cfg_attr(not(test), deny(clippy::unwrap_used))]
#![deny(missing_docs)]
#![doc(html_root_url = "https://docs.rs/mmap-io")]

/// Anonymous (file-less) memory mappings.
pub mod anonymous;
pub mod errors;
pub mod manager;
/// Memory-mapped file support.
pub mod mmap;
/// Raw platform mappings ([`raw::RawMmap`], [`raw::RawMmapMut`],
/// [`raw::RawMmapOptions`]): the `mmap` / `MapViewOfFile` layer with
/// checked offset and length handling, returning `std::io::Result`.
/// Use [`MemoryMappedFile`] unless you need a bare mapping without
/// locks, flush policy or path tracking.
pub mod raw;
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

pub use anonymous::AnonymousMmap;
pub use errors::MmapIoError;
pub use manager::{
    copy_mmap, create_mmap, delete_mmap, flush, load_mmap, update_region, write_mmap,
};
pub use mmap::{MappedSlice, MappedSliceMut, MemoryMappedFile, MmapMode, TouchHint};

#[cfg(feature = "advise")]
pub use advise::MmapAdvice;

#[cfg(feature = "iterator")]
pub use iterator::{ChunkIterator, PageIterator};

#[cfg(feature = "watch")]
pub use watch::{ChangeEvent, ChangeKind, WatchHandle};
