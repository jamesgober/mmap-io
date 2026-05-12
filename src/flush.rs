//! Flush policy configuration for MemoryMappedFile.
//!
//! Controls when writes to a RW mapping should be flushed to disk.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

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

/// Time-based flush manager that flushes pending writes at a regular
/// interval.
///
/// Used internally when `FlushPolicy::EveryMillis` is configured. The
/// flusher owns a background thread that calls a user-provided callback
/// every `interval_ms` milliseconds (or shorter, when shutdown is
/// requested).
///
/// # Shutdown
///
/// Dropping the `TimeBasedFlusher` signals the background thread to
/// exit via an `AtomicBool` flag. The thread checks the flag both
/// before sleeping and after waking, so the longest it can outlive
/// the `Drop` call is roughly `min(interval_ms, SHUTDOWN_POLL_MS)`.
pub(crate) struct TimeBasedFlusher {
    /// Shutdown signal shared with the background thread.
    running: Arc<AtomicBool>,
    /// Worker thread handle. `Option` so Drop can take ownership.
    thread: Option<thread::JoinHandle<()>>,
}

/// Maximum delay between shutdown-flag checks. Lets the thread exit
/// promptly even if `interval_ms` is much larger than this. Tuned for
/// "fast enough to not block process teardown" without being so small
/// that it wastes wakeups during normal operation.
const SHUTDOWN_POLL_MS: u64 = 50;

impl TimeBasedFlusher {
    /// Create a new time-based flusher with the given interval and
    /// callback. Returns `None` if `interval_ms` is zero (flushing
    /// at every zero ms is meaningless; callers should pick a
    /// different policy instead).
    ///
    /// The callback is invoked from the background thread once per
    /// interval. It returns `true` if a flush was performed (used by
    /// the flusher only for internal accounting; semantically a
    /// best-effort signal).
    pub(crate) fn new<F>(interval_ms: u64, flush_callback: F) -> Option<Self>
    where
        F: Fn() -> bool + Send + 'static,
    {
        if interval_ms == 0 {
            return None;
        }

        let interval = Duration::from_millis(interval_ms);
        let running = Arc::new(AtomicBool::new(true));
        let running_clone = Arc::clone(&running);
        let shutdown_poll = Duration::from_millis(SHUTDOWN_POLL_MS.min(interval_ms));

        let handle = thread::spawn(move || {
            // Sleep in small slices so we observe shutdown promptly
            // even when the configured interval is large.
            let mut elapsed = Duration::ZERO;
            while running_clone.load(Ordering::Acquire) {
                // Sleep one slice, then check the shutdown flag.
                // `saturating_sub` guards against the case where a
                // previous `thread::sleep` overshot and pushed
                // `elapsed` past `interval`: the next slice clamps to
                // zero (yielding immediately) rather than panicking
                // on a Duration underflow.
                let remaining = interval.saturating_sub(elapsed);
                let slice = shutdown_poll.min(remaining);
                thread::sleep(slice);
                elapsed += slice;

                if !running_clone.load(Ordering::Acquire) {
                    break;
                }

                if elapsed >= interval {
                    let _ = flush_callback();
                    elapsed = Duration::ZERO;
                }
            }
        });

        Some(Self {
            running,
            thread: Some(handle),
        })
    }
}

impl Drop for TimeBasedFlusher {
    fn drop(&mut self) {
        // Signal shutdown.
        self.running.store(false, Ordering::Release);
        // Detach the thread without blocking; it will observe the
        // shutdown flag on its next slice boundary. We do not join
        // synchronously to keep Drop predictable in latency-
        // sensitive contexts. If the caller needs to guarantee the
        // worker has exited (e.g., for test isolation), they should
        // sleep for ~2 * SHUTDOWN_POLL_MS after dropping.
        if let Some(handle) = self.thread.take() {
            // Move the join into a detached helper thread so Drop
            // returns immediately. The OS reaps the thread either
            // way; this just keeps the JoinHandle from leaking its
            // OS-level state.
            let _ = thread::spawn(move || {
                let _ = handle.join();
            });
        }
    }
}
