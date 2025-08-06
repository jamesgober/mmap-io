#![allow(dead_code)]
//! Flush policy configuration for MemoryMappedFile.
//!
//! Controls when writes to a RW mapping should be flushed to disk.

/// Policy controlling when to flush dirty pages to disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FlushPolicy {
    /// Never flush implicitly; flush() must be called by the user.
    #[default]
    Never,
    /// Alias of Never for semantic clarity when using the builder API.
    Manual,
    /// Flush after every write/update_region call.
    Always,
    /// Flush when at least N bytes have been written since the last flush.
    EveryBytes(usize),
    /// Flush after every W writes (calls to update_region).
    EveryWrites(usize),
    /// Reserved for future time-based flushing (no-op for now).
    EveryMillis(u64),
}

