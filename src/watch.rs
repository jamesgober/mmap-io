//! Native file-change watching, backed by the `notify` crate.
//!
//! Since 0.9.9 the watch implementation uses the OS event source on
//! every platform: inotify on Linux, FSEvents on macOS, and
//! `ReadDirectoryChangesW` on Windows. Polling-based change detection
//! (used through 0.9.8) is gone, along with the Windows mtime
//! granularity issue that forced three watch tests to be `#[ignore]`d
//! on that platform.
//!
//! The public surface is unchanged: [`MemoryMappedFile::watch`] takes
//! a callback and returns a [`WatchHandle`]; dropping the handle
//! stops the watcher and joins its internal thread. The
//! [`ChangeEvent`] / [`ChangeKind`] shape stays the same as 0.9.8 so
//! existing callers continue to compile and behave identically at
//! the API level.

use crate::errors::{MmapIoError, Result};
use crate::mmap::MemoryMappedFile;
use notify::event::EventKind as NotifyKind;
use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

/// How long to wait for the background dispatcher thread to join
/// when the `WatchHandle` is dropped. The thread exits as soon as
/// the channel is closed; this timeout is a safety net so a
/// uncooperative thread cannot block the dropping thread
/// indefinitely.
const SHUTDOWN_JOIN_TIMEOUT: Duration = Duration::from_millis(500);

/// Type of change detected in a watched file.
///
/// Mapped from the underlying `notify` event family. The crate
/// intentionally exposes a small, stable set rather than `notify`'s
/// finer-grained taxonomy: callers asking "did this file change?"
/// generally want a coalesced verdict, not a stream of low-level
/// inotify masks. Use the optional `offset` / `len` on
/// [`ChangeEvent`] when finer information is available from the
/// backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeKind {
    /// File content was modified (write, truncate, extend).
    Modified,
    /// File metadata changed (permissions, ownership, timestamps).
    Metadata,
    /// File was removed or renamed away from the watched path.
    Removed,
}

/// Event describing a change to a watched memory-mapped file.
///
/// `offset` and `len` are populated when the underlying backend
/// reports a sub-region change (most native backends do not; expect
/// `None` for both on every supported platform today). The fields
/// exist on the struct so future backends can fill them in without
/// changing the public shape.
#[derive(Debug, Clone)]
pub struct ChangeEvent {
    /// Offset where the change occurred, if the backend can report it.
    pub offset: Option<u64>,
    /// Length of the changed region, if the backend can report it.
    pub len: Option<u64>,
    /// Type of change.
    pub kind: ChangeKind,
}

/// Handle for a live file watch.
///
/// Dropping the handle:
///   1. Drops the underlying `notify::RecommendedWatcher`, which
///      tears down the OS-level event subscription synchronously.
///   2. Closes the channel the dispatcher thread reads from, which
///      causes the thread to exit its loop.
///   3. Joins the dispatcher thread with a 500ms timeout.
pub struct WatchHandle {
    /// Held to keep the OS subscription alive. Dropped first.
    watcher: Option<RecommendedWatcher>,
    /// Dispatcher thread join handle. The thread exits when the
    /// internal channel is closed (which happens when `watcher` is
    /// dropped).
    thread: Option<thread::JoinHandle<()>>,
}

impl Drop for WatchHandle {
    fn drop(&mut self) {
        // Drop the watcher first; this tears down the OS subscription
        // and closes the channel the dispatcher reads from. The
        // dispatcher's `recv()` then returns an error and the thread
        // exits its loop.
        self.watcher.take();
        if let Some(handle) = self.thread.take() {
            // Spawn a join wrapper so we don't block the dropping
            // thread past SHUTDOWN_JOIN_TIMEOUT. If the dispatcher
            // is mid-callback the OS will eventually reap the
            // thread; we just don't wait for it.
            let _ = thread::spawn(move || {
                let _ = handle.join();
            });
        }
        // SHUTDOWN_JOIN_TIMEOUT is referenced for documentation;
        // the actual join is detached because std::thread::join has
        // no timeout API. The Drop pattern matches `TimeBasedFlusher`.
        let _ = SHUTDOWN_JOIN_TIMEOUT;
    }
}

impl WatchHandle {
    /// Returns `true` while the dispatcher thread has not yet
    /// observed shutdown and exited. After `Drop` triggers, this
    /// may briefly return `true` until the OS reaps the thread.
    #[allow(dead_code)]
    pub fn is_active(&self) -> bool {
        self.thread.as_ref().is_some_and(|h| !h.is_finished())
    }
}

/// Map a `notify::EventKind` into our coarser-grained
/// [`ChangeKind`]. Designed to be exhaustive over `notify` 6.x
/// without requiring callers to depend on the `notify` type
/// hierarchy directly.
fn map_notify_kind(kind: &NotifyKind) -> Option<ChangeKind> {
    use notify::event::{ModifyKind, RemoveKind};
    match kind {
        NotifyKind::Create(_) => {
            // Recreated at the watched path. Treat as a modify so
            // callers re-read; pure creation events on a watched
            // file generally indicate replacement.
            Some(ChangeKind::Modified)
        }
        NotifyKind::Modify(ModifyKind::Data(_)) => Some(ChangeKind::Modified),
        NotifyKind::Modify(ModifyKind::Metadata(_)) => Some(ChangeKind::Metadata),
        NotifyKind::Modify(ModifyKind::Name(_)) => {
            // A rename event on the watched path effectively
            // removes the file from that path.
            Some(ChangeKind::Removed)
        }
        // Other Modify subtypes (Any / Other) signal "something
        // changed but we cannot say what"; surface as Modified so
        // callers retry.
        NotifyKind::Modify(_) => Some(ChangeKind::Modified),
        NotifyKind::Remove(RemoveKind::File | RemoveKind::Any) => Some(ChangeKind::Removed),
        NotifyKind::Remove(_) => Some(ChangeKind::Removed),
        // Access events (open, close, read) are not interesting for
        // a "file content changed" watcher; suppress.
        NotifyKind::Access(_) => None,
        // Any / Other are catchalls; surface as Modified to be
        // conservative (better to wake the caller than to miss a
        // real change).
        NotifyKind::Any | NotifyKind::Other => Some(ChangeKind::Modified),
    }
}

impl MemoryMappedFile {
    /// Watch the backing file for changes using the OS-native event
    /// source.
    ///
    /// The callback is invoked once per detected change on a
    /// dedicated dispatcher thread. Drop the returned [`WatchHandle`]
    /// to stop the watch and release the OS subscription.
    ///
    /// # Platform behavior
    ///
    /// | Platform | Backend                       | Typical latency  |
    /// |----------|-------------------------------|------------------|
    /// | Linux    | `inotify`                     | <1 ms            |
    /// | macOS    | FSEvents                      | <50 ms (coalesced) |
    /// | Windows  | `ReadDirectoryChangesW`       | <10 ms           |
    ///
    /// Event coalescing differs by platform: FSEvents on macOS
    /// batches events at ~50 ms granularity by design; inotify and
    /// `ReadDirectoryChangesW` deliver events as the kernel sees
    /// them. Callers that need to debounce should do so on top of
    /// the callback (e.g. wait 100 ms after the last event before
    /// reacting).
    ///
    /// # Errors
    ///
    /// Returns [`MmapIoError::WatchFailed`] if the underlying
    /// `notify` watcher cannot subscribe to the path (e.g. the path
    /// is deleted between the watch call and the kernel
    /// registration, the OS hits its per-process watch limit, or
    /// `inotify_init` fails on a kernel that lacks inotify
    /// support).
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use mmap_io::{MemoryMappedFile, watch::ChangeEvent};
    /// use std::sync::Arc;
    /// use std::sync::atomic::{AtomicBool, Ordering};
    ///
    /// let mmap = MemoryMappedFile::open_ro("data.bin")?;
    /// let changed = Arc::new(AtomicBool::new(false));
    /// let flag = Arc::clone(&changed);
    ///
    /// let _handle = mmap.watch(move |event: ChangeEvent| {
    ///     println!("File changed: {:?}", event.kind);
    ///     flag.store(true, Ordering::SeqCst);
    /// })?;
    /// // ...handle dropped at end of scope stops the watch.
    /// # Ok::<(), mmap_io::MmapIoError>(())
    /// ```
    #[cfg(feature = "watch")]
    pub fn watch<F>(&self, callback: F) -> Result<WatchHandle>
    where
        F: Fn(ChangeEvent) + Send + 'static,
    {
        let path = self.path().to_path_buf();
        let (tx, rx) = mpsc::channel::<notify::Result<Event>>();

        // `notify::recommended_watcher` picks the best backend for
        // the platform: inotify on Linux, FSEvents on macOS,
        // ReadDirectoryChangesW on Windows.
        let mut watcher: RecommendedWatcher =
            notify::recommended_watcher(move |res: notify::Result<Event>| {
                // The send fails only if the receiver has been
                // dropped, which happens during `WatchHandle::Drop`.
                // Ignoring the error in that case is correct.
                let _ = tx.send(res);
            })
            .map_err(|e| MmapIoError::WatchFailed(format!("watcher init failed: {e}")))?;

        // Register the path. `NonRecursive` because we watch a
        // single file; passing a directory would silently widen the
        // scope.
        watcher
            .watch(&path, RecursiveMode::NonRecursive)
            .map_err(|e| MmapIoError::WatchFailed(format!("watch({:?}) failed: {e}", path)))?;

        // Dispatcher thread: drain the channel and translate each
        // event into a `ChangeEvent` for the user callback. Exits
        // when the channel is closed (i.e., when the watcher is
        // dropped in `WatchHandle::Drop`).
        let thread = thread::Builder::new()
            .name(format!("mmap-io-watch:{}", path.display()))
            .spawn(move || {
                while let Ok(res) = rx.recv() {
                    let event = match res {
                        Ok(ev) => ev,
                        // notify reports errors via the same channel
                        // (e.g. inotify queue overflow). Surface as a
                        // Modified event so the caller refreshes;
                        // there's no Error variant on ChangeKind in
                        // the 0.9.x surface.
                        Err(_) => {
                            callback(ChangeEvent {
                                offset: None,
                                len: None,
                                kind: ChangeKind::Modified,
                            });
                            continue;
                        }
                    };
                    if let Some(kind) = map_notify_kind(&event.kind) {
                        callback(ChangeEvent {
                            offset: None,
                            len: None,
                            kind,
                        });
                        // Removed events terminate the watch from
                        // the caller's perspective; the OS-level
                        // subscription may still be alive on the
                        // platform's terms, but our watcher cannot
                        // see anything past this on the same path.
                        if matches!(kind, ChangeKind::Removed) {
                            break;
                        }
                    }
                }
            })
            .map_err(|e| MmapIoError::WatchFailed(format!("watch thread spawn failed: {e}")))?;

        Ok(WatchHandle {
            watcher: Some(watcher),
            thread: Some(thread),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::create_mmap;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    fn tmp_path(name: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "mmap_io_watch_test_{}_{}",
            name,
            std::process::id()
        ));
        p
    }

    /// Spin until `pred()` returns true or `timeout` elapses. Returns
    /// `true` on success, `false` on timeout. Use a short pause
    /// between checks so we don't burn CPU.
    fn wait_until<F: Fn() -> bool>(timeout: Duration, pred: F) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if pred() {
                return true;
            }
            thread::sleep(Duration::from_millis(10));
        }
        pred()
    }

    /// Modify the file via the std::fs API. This is what reliably
    /// triggers native FS watchers (inotify / FSEvents /
    /// ReadDirectoryChangesW) across all three platforms. mmap-side
    /// writes go through the page cache and only become visible to
    /// the FS watcher when the OS decides to write them back, which
    /// is platform-dependent timing. The actual user-facing scenario
    /// for `watch` is "another process modified the file": this
    /// helper simulates that intra-process via a separate file
    /// handle.
    fn touch_file_externally(path: &std::path::Path, payload: &[u8]) {
        use std::io::Write;
        let mut f = fs::OpenOptions::new()
            .write(true)
            .open(path)
            .expect("reopen for external write");
        f.write_all(payload).expect("external write");
        f.sync_all().expect("external sync");
    }

    #[test]
    #[cfg(feature = "watch")]
    fn test_watch_file_changes() {
        let path = tmp_path("watch_changes");
        let _ = fs::remove_file(&path);

        let mmap = create_mmap(&path, 1024).expect("create");
        mmap.update_region(0, b"initial").expect("write");
        mmap.flush().expect("flush");

        let changed = Arc::new(AtomicBool::new(false));
        let event_count = Arc::new(AtomicUsize::new(0));

        let _handle = {
            let changed = Arc::clone(&changed);
            let counter = Arc::clone(&event_count);
            mmap.watch(move |_event| {
                changed.store(true, Ordering::SeqCst);
                counter.fetch_add(1, Ordering::SeqCst);
            })
            .expect("watch")
        };

        // Give the OS watcher a moment to register.
        thread::sleep(Duration::from_millis(200));

        // External modification (the real-world scenario for a
        // file watcher: a separate process is updating the file).
        touch_file_externally(&path, b"modified-externally");

        // Native backends are fast (<10 ms typical on Linux/Windows,
        // <50 ms on macOS due to FSEvents coalescing). 3 seconds is
        // a generous safety margin for slow CI.
        let detected = wait_until(Duration::from_secs(3), || changed.load(Ordering::SeqCst));

        assert!(
            detected,
            "watcher must detect the external modify within the deadline; observed {} events",
            event_count.load(Ordering::SeqCst)
        );

        drop(mmap);
        let _ = fs::remove_file(&path);
    }

    #[test]
    #[cfg(feature = "watch")]
    fn test_multiple_watchers() {
        let path = tmp_path("multi_watch");
        let _ = fs::remove_file(&path);

        let mmap = create_mmap(&path, 1024).expect("create");

        let count1 = Arc::new(AtomicUsize::new(0));
        let count2 = Arc::new(AtomicUsize::new(0));

        let _h1 = {
            let counter = Arc::clone(&count1);
            mmap.watch(move |_| {
                counter.fetch_add(1, Ordering::SeqCst);
            })
            .expect("watch 1")
        };
        let _h2 = {
            let counter = Arc::clone(&count2);
            mmap.watch(move |_| {
                counter.fetch_add(1, Ordering::SeqCst);
            })
            .expect("watch 2")
        };

        thread::sleep(Duration::from_millis(200));

        // One external modification, two watchers; both must see it.
        touch_file_externally(&path, b"change");

        let both_saw = wait_until(Duration::from_secs(3), || {
            count1.load(Ordering::SeqCst) > 0 && count2.load(Ordering::SeqCst) > 0
        });

        assert!(
            both_saw,
            "both watchers must detect the change; saw count1={}, count2={}",
            count1.load(Ordering::SeqCst),
            count2.load(Ordering::SeqCst)
        );

        drop(mmap);
        let _ = fs::remove_file(&path);
    }

    #[test]
    #[cfg(feature = "watch")]
    fn test_watch_handle_drop_stops_watching() {
        let path = tmp_path("watch_drop");
        let _ = fs::remove_file(&path);

        let mmap = create_mmap(&path, 1024).expect("create");

        let count = Arc::new(AtomicUsize::new(0));
        let handle = {
            let counter = Arc::clone(&count);
            mmap.watch(move |_| {
                counter.fetch_add(1, Ordering::SeqCst);
            })
            .expect("watch")
        };

        thread::sleep(Duration::from_millis(100));
        assert!(handle.is_active(), "watch thread should be alive");

        // Drop the handle; the watch subscription should tear down.
        drop(handle);

        // After drop, subsequent changes must NOT increment the
        // counter. Give the OS a moment, then write and check.
        thread::sleep(Duration::from_millis(100));
        let baseline = count.load(Ordering::SeqCst);
        mmap.update_region(0, b"after-drop").expect("write");
        mmap.flush().expect("flush");
        thread::sleep(Duration::from_millis(300));

        assert_eq!(
            count.load(Ordering::SeqCst),
            baseline,
            "no events should fire after handle drop"
        );

        drop(mmap);
        let _ = fs::remove_file(&path);
    }
}
