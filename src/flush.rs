//! Flush policy configuration for MemoryMappedFile.
//!
//! Controls when writes to a RW mapping should be flushed to disk.

use parking_lot::{Condvar, Mutex, MutexGuard};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

/// Policy controlling when to flush dirty pages to disk.
///
/// The policy only controls *automatic* flushes. An explicit
/// [`MemoryMappedFile::flush`](crate::MemoryMappedFile::flush) always
/// flushes, whatever the policy. Byte thresholds count every write
/// path (see
/// [`MemoryMappedFile::pending_bytes`](crate::MemoryMappedFile::pending_bytes));
/// `Always`, `EveryBytes`, and `EveryWrites` are evaluated after each
/// `update_region` call.
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
/// once per interval. The thread sleeps on a condition variable, so it
/// wakes once per interval (not on a fixed polling tick) and is woken
/// immediately on shutdown.
///
/// # Shutdown
///
/// Dropping the `TimeBasedFlusher` signals the thread and joins it. If
/// the callback is running at that moment, `Drop` waits for it to
/// return. If `Drop` runs on the worker thread itself (the callback
/// can end up releasing the last reference to the mapping), the thread
/// is not joined, since a thread cannot join itself; it exits as soon
/// as the callback returns. `Drop` never panics.
pub(crate) struct TimeBasedFlusher {
    shared: Arc<Shared>,
    /// Worker thread handle. `Option` so Drop can take ownership.
    thread: Option<thread::JoinHandle<()>>,
}

/// State shared between the owner and the worker thread.
struct Shared {
    /// Set to `true` to ask the worker to exit.
    stop: Mutex<bool>,
    /// Signalled when `stop` is set.
    wake: Condvar,
}

impl TimeBasedFlusher {
    /// Create a new time-based flusher with the given interval and
    /// callback. Returns `None` if `interval_ms` is zero (flushing
    /// at every zero ms is meaningless; callers should pick a
    /// different policy instead) or if the OS refuses to start the
    /// worker thread (logged as a warning).
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
        let shared = Arc::new(Shared {
            stop: Mutex::new(false),
            wake: Condvar::new(),
        });
        let worker = Arc::clone(&shared);

        let spawned = thread::Builder::new()
            .name("mmap-io-flusher".into())
            .spawn(move || {
                let mut stop = worker.stop.lock();
                loop {
                    // Sleep one interval; wake early only for shutdown.
                    let deadline = Instant::now() + interval;
                    while !*stop && !worker.wake.wait_until(&mut stop, deadline).timed_out() {}
                    if *stop {
                        break;
                    }
                    // Run the callback without holding the lock, so a
                    // concurrent Drop can set `stop` without waiting.
                    MutexGuard::unlocked(&mut stop, || {
                        let _ = flush_callback();
                    });
                }
            });

        match spawned {
            Ok(handle) => Some(Self {
                shared,
                thread: Some(handle),
            }),
            Err(e) => {
                log::warn!("could not start the EveryMillis flusher thread: {e}");
                None
            }
        }
    }
}

impl Drop for TimeBasedFlusher {
    fn drop(&mut self) {
        *self.shared.stop.lock() = true;
        self.shared.wake.notify_all();
        if let Some(handle) = self.thread.take() {
            // Joining from the worker itself would deadlock (std panics
            // instead); in that case the worker exits on its own once
            // the callback returns.
            if handle.thread().id() != thread::current().id() {
                // `Err` only means the callback panicked; nothing to do.
                let _ = handle.join();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    #[cfg_attr(
        miri,
        ignore = "parking_lot_core futex call: Miri rejects &AtomicI32 for the *mut u32 syscall argument (dependency false positive)"
    )]
    #[test]
    fn drop_stops_and_joins_the_worker() {
        let calls = Arc::new(AtomicUsize::new(0));
        let c = Arc::clone(&calls);
        let flusher = TimeBasedFlusher::new(5, move || {
            c.fetch_add(1, Ordering::SeqCst);
            true
        })
        .expect("flusher");
        // Wait (bounded) for the callback to run at least once instead
        // of a fixed sleep, which is too short on a loaded machine.
        let deadline = Instant::now() + Duration::from_secs(10);
        while calls.load(Ordering::SeqCst) == 0 && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(1));
        }
        drop(flusher);
        let after_drop = calls.load(Ordering::SeqCst);
        assert!(after_drop > 0, "callback never ran");
        thread::sleep(Duration::from_millis(50));
        assert_eq!(
            calls.load(Ordering::SeqCst),
            after_drop,
            "callback ran after Drop returned"
        );
    }

    #[cfg_attr(
        miri,
        ignore = "parking_lot_core futex call: Miri rejects &AtomicI32 for the *mut u32 syscall argument (dependency false positive)"
    )]
    #[test]
    fn drop_with_long_interval_returns_promptly() {
        let flusher = TimeBasedFlusher::new(60_000, || false).expect("flusher");
        let start = Instant::now();
        drop(flusher);
        assert!(start.elapsed() < Duration::from_secs(5));
    }

    #[cfg_attr(
        miri,
        ignore = "parking_lot_core futex call: Miri rejects &AtomicI32 for the *mut u32 syscall argument (dependency false positive)"
    )]
    #[test]
    fn drop_on_the_worker_thread_does_not_panic() {
        // The callback drops the flusher itself, so Drop runs on the
        // worker thread and must not try to join it.
        let slot: Arc<Mutex<Option<TimeBasedFlusher>>> = Arc::new(Mutex::new(None));
        let done = Arc::new(AtomicBool::new(false));
        let (s, d) = (Arc::clone(&slot), Arc::clone(&done));
        let flusher = TimeBasedFlusher::new(5, move || {
            if let Some(f) = s.lock().take() {
                drop(f);
                d.store(true, Ordering::SeqCst);
            }
            false
        })
        .expect("flusher");
        *slot.lock() = Some(flusher);
        let deadline = Instant::now() + Duration::from_secs(5);
        while !done.load(Ordering::SeqCst) && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(5));
        }
        assert!(
            done.load(Ordering::SeqCst),
            "worker never dropped the flusher"
        );
    }

    #[test]
    fn zero_interval_is_rejected() {
        assert!(TimeBasedFlusher::new(0, || false).is_none());
    }
}
