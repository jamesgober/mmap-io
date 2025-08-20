#![allow(dead_code)]
//! Flush policy configuration for MemoryMappedFile.
//!
//! Controls when writes to a RW mapping should be flushed to disk.

use parking_lot::RwLock;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

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
    /// Flush automatically every N milliseconds when there are pending writes.
    EveryMillis(u64),
}

/// Time-based flush manager that handles automatic flushing at regular intervals.
/// This is used internally when FlushPolicy::EveryMillis is configured.
pub struct TimeBasedFlusher {
    interval: Duration,
    last_flush: Arc<RwLock<Option<Instant>>>,
    _handle: Option<thread::JoinHandle<()>>,
}

impl TimeBasedFlusher {
    /// Create a new time-based flusher with the given interval.
    /// Returns None if interval_ms is 0.
    pub fn new<F>(interval_ms: u64, flush_callback: F) -> Option<Self>
    where
        F: Fn() -> bool + Send + 'static,
    {
        if interval_ms == 0 {
            return None;
        }

        let interval = Duration::from_millis(interval_ms);
        let last_flush = Arc::new(RwLock::new(Some(Instant::now())));
        let last_flush_clone = Arc::clone(&last_flush);

        let handle = thread::spawn(move || {
            loop {
                thread::sleep(interval);

                // Check if we should flush
                let should_flush = {
                    let last = last_flush_clone.read();
                    if let Some(last_time) = *last {
                        last_time.elapsed() >= interval
                    } else {
                        false
                    }
                };

                if should_flush {
                    // Attempt flush via callback
                    let flushed = flush_callback();
                    if flushed {
                        // Update last flush time
                        *last_flush_clone.write() = Some(Instant::now());
                    }
                }
            }
        });

        Some(Self {
            interval,
            last_flush,
            _handle: Some(handle),
        })
    }

    /// Manually trigger an immediate flush and update the last flush time.
    pub fn manual_flush(&self) {
        *self.last_flush.write() = Some(Instant::now());
    }

    /// Check if it's time for a flush based on the interval.
    pub fn should_flush(&self) -> bool {
        let last = self.last_flush.read();
        if let Some(last_time) = *last {
            last_time.elapsed() >= self.interval
        } else {
            true // First flush
        }
    }
}

impl Drop for TimeBasedFlusher {
    fn drop(&mut self) {
        // The background thread will naturally exit when the handle is dropped
        // We don't join here to avoid blocking the drop
    }
}
