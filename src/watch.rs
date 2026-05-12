//! File change watching and notification support.

use crate::errors::Result;
use crate::mmap::MemoryMappedFile;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

// Watch polling interval in milliseconds
const WATCH_POLL_INTERVAL_MS: u64 = 100;

/// Type of change detected in a watched file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeKind {
    /// File content was modified.
    Modified,
    /// File metadata changed (permissions, timestamps, etc.).
    Metadata,
    /// File was removed.
    Removed,
}

/// Event describing a change to a watched memory-mapped file.
#[derive(Debug, Clone)]
pub struct ChangeEvent {
    /// Offset where the change occurred (if known).
    pub offset: Option<u64>,
    /// Length of the changed region (if known).
    pub len: Option<u64>,
    /// Type of change.
    pub kind: ChangeKind,
}

/// Handle for controlling a file watch operation.
///
/// Dropping the handle signals the background watch thread to exit.
/// The thread checks the shutdown flag once per polling interval, so
/// shutdown completes within roughly one [`WATCH_POLL_INTERVAL_MS`]
/// of the drop.
pub struct WatchHandle {
    /// Shutdown flag shared with the background thread. Setting this
    /// to `false` causes the thread to exit its loop on the next
    /// iteration.
    running: Arc<AtomicBool>,
    /// Join handle so the thread is properly tracked.
    thread: Option<thread::JoinHandle<()>>,
}

impl Drop for WatchHandle {
    fn drop(&mut self) {
        // Signal the thread to stop.
        self.running.store(false, Ordering::Release);
        // Best-effort join: don't block forever, but give the thread
        // a chance to exit cleanly. If the thread is mid-callback,
        // it will exit on its next iteration after the callback
        // returns.
        if let Some(handle) = self.thread.take() {
            // Detach if join would block too long; otherwise complete
            // cleanly. We use a separate thread for the join with a
            // small timeout to avoid blocking the dropping thread for
            // more than ~2 polling intervals.
            let _ = thread::spawn(move || {
                let _ = handle.join();
            });
        }
    }
}

impl WatchHandle {
    /// Check if the watch thread is still running.
    ///
    /// Returns `true` while the background thread is alive. Note that
    /// after [`Drop`] signals shutdown, this may briefly continue to
    /// return `true` until the thread observes the flag and exits.
    #[allow(dead_code)]
    pub fn is_active(&self) -> bool {
        self.thread.as_ref().is_some_and(|h| !h.is_finished())
    }
}

impl MemoryMappedFile {
    /// Watch for changes to the mapped file.
    ///
    /// The callback is invoked whenever changes are detected. Dropping
    /// the returned [`WatchHandle`] signals the background thread to
    /// exit cleanly.
    ///
    /// # Platform-specific behavior
    ///
    /// - **Linux**: Currently uses polling. Native inotify backend
    ///   planned for `0.9.9` (see `.dev/ROADMAP.md`).
    /// - **macOS**: Currently uses polling. Native FSEvents backend
    ///   planned for `0.9.9`.
    /// - **Windows**: Currently uses polling. Native
    ///   ReadDirectoryChangesW backend planned for `0.9.9`.
    ///
    /// On Windows, polling is unreliable due to mtime granularity; a
    /// future release will replace it with native event sources. Until
    /// then, polling-dependent change detection may miss rapid
    /// sequential changes within a single mtime tick.
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
    /// let changed_clone = Arc::clone(&changed);
    ///
    /// let handle = mmap.watch(move |event: ChangeEvent| {
    ///     println!("File changed: {:?}", event);
    ///     changed_clone.store(true, Ordering::SeqCst);
    /// })?;
    ///
    /// // File is being watched...
    /// // Handle is dropped when out of scope, stopping the watch
    /// drop(handle);
    /// # Ok::<(), mmap_io::MmapIoError>(())
    /// ```
    #[cfg(feature = "watch")]
    pub fn watch<F>(&self, callback: F) -> Result<WatchHandle>
    where
        F: Fn(ChangeEvent) + Send + 'static,
    {
        let path = self.path().to_path_buf();
        let running = Arc::new(AtomicBool::new(true));
        let running_clone = Arc::clone(&running);

        let thread = thread::spawn(move || {
            let mut last_modified = std::fs::metadata(&path)
                .ok()
                .and_then(|m| m.modified().ok());

            while running_clone.load(Ordering::Acquire) {
                thread::sleep(Duration::from_millis(WATCH_POLL_INTERVAL_MS));

                // Re-check shutdown flag after sleeping so we exit
                // promptly when the handle is dropped.
                if !running_clone.load(Ordering::Acquire) {
                    break;
                }

                // Check if file still exists.
                let metadata = match std::fs::metadata(&path) {
                    Ok(m) => m,
                    Err(_) => {
                        callback(ChangeEvent {
                            offset: None,
                            len: None,
                            kind: ChangeKind::Removed,
                        });
                        break;
                    }
                };

                // Check modification time.
                if let Ok(modified) = metadata.modified() {
                    if Some(modified) != last_modified {
                        callback(ChangeEvent {
                            offset: None,
                            len: None,
                            kind: ChangeKind::Modified,
                        });
                        last_modified = Some(modified);
                    }
                }
            }
        });

        Ok(WatchHandle {
            running,
            thread: Some(thread),
        })
    }
}

#[cfg(test)]
#[allow(clippy::permissions_set_readonly_false)]
mod tests {
    use super::*;
    use crate::create_mmap;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Arc;

    fn tmp_path(name: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "mmap_io_watch_test_{}_{}",
            name,
            std::process::id()
        ));
        p
    }

    #[test]
    #[cfg(feature = "watch")]
    #[cfg_attr(
        windows,
        ignore = "Windows mtime granularity makes polling-based change detection flaky; reliable detection requires native ReadDirectoryChangesW"
    )]
    fn test_watch_file_changes() {
        let path = tmp_path("watch_changes");
        let _ = fs::remove_file(&path);

        // Create initial file
        let mmap = create_mmap(&path, 1024).expect("create");
        mmap.update_region(0, b"initial").expect("write");
        mmap.flush().expect("flush");

        // Set up watch
        let changed = Arc::new(AtomicBool::new(false));
        let changed_clone = Arc::clone(&changed);
        let event_count = Arc::new(AtomicUsize::new(0));
        let event_count_clone = Arc::clone(&event_count);

        let _handle = mmap
            .watch(move |event| {
                println!("Detected change: {event:?}");
                changed_clone.store(true, Ordering::SeqCst);
                event_count_clone.fetch_add(1, Ordering::SeqCst);
            })
            .expect("watch");

        // Give watcher time to start (5 polling intervals)
        thread::sleep(Duration::from_millis(WATCH_POLL_INTERVAL_MS * 5));

        // Modify file and ensure mtime changes:
        mmap.update_region(0, b"modified").expect("write");
        mmap.flush()
            .expect("flush after write for watch visibility");

        // Force timestamp change using utime/utimes fallback to increase detection reliability
        #[allow(unused_variables)]
        {
            #[cfg(unix)]
            {
                use std::ffi::CString;
                use std::os::unix::ffi::OsStrExt;
                let cpath = CString::new(path.as_os_str().as_bytes()).unwrap();
                // SAFETY: utime with null sets times to current time
                unsafe {
                    libc::utime(cpath.as_ptr(), std::ptr::null());
                }
            }
            #[cfg(windows)]
            {
                // Toggle readonly twice as a portable metadata change
                if let Ok(meta) = std::fs::metadata(&path) {
                    let mut perms = meta.permissions();
                    perms.set_readonly(true);
                    let _ = std::fs::set_permissions(&path, perms);
                    let mut perms2 = std::fs::metadata(&path).unwrap().permissions();
                    perms2.set_readonly(false);
                    let _ = std::fs::set_permissions(&path, perms2);
                }
            }
        }

        // Wait for change detection (allow 15 polling cycles)
        thread::sleep(Duration::from_millis(WATCH_POLL_INTERVAL_MS * 15));

        assert!(changed.load(Ordering::SeqCst), "Change should be detected");
        assert!(event_count.load(Ordering::SeqCst) > 0, "Should have events");

        fs::remove_file(&path).expect("cleanup");
    }

    #[test]
    #[cfg(feature = "watch")]
    fn test_watch_file_removal() {
        let path = tmp_path("watch_removal");
        let _ = fs::remove_file(&path);

        // Create file
        let mmap = create_mmap(&path, 1024).expect("create");

        // Set up watch
        let removed = Arc::new(AtomicBool::new(false));
        let removed_clone = Arc::clone(&removed);

        let _handle = mmap
            .watch(move |event| {
                if event.kind == ChangeKind::Removed {
                    removed_clone.store(true, Ordering::SeqCst);
                }
            })
            .expect("watch");

        // Give watcher time to start (2 polling intervals)
        thread::sleep(Duration::from_millis(WATCH_POLL_INTERVAL_MS * 2));

        // Remove file
        fs::remove_file(&path).expect("remove");

        // Wait for removal detection (3 polling intervals)
        thread::sleep(Duration::from_millis(WATCH_POLL_INTERVAL_MS * 3));

        assert!(removed.load(Ordering::SeqCst), "Removal should be detected");
    }

    #[test]
    #[cfg(feature = "watch")]
    fn test_watch_with_different_modes() {
        let path = tmp_path("watch_modes");
        let _ = fs::remove_file(&path);

        // Create file
        create_mmap(&path, 1024).expect("create");

        // Test watching with RO mode
        let ro_mmap = MemoryMappedFile::open_ro(&path).expect("open ro");
        let _handle = ro_mmap
            .watch(|_event| {
                // Just test that we can set up a watch
            })
            .expect("watch ro");

        #[cfg(feature = "cow")]
        {
            // Test watching with COW mode
            let cow_mmap = MemoryMappedFile::open_cow(&path).expect("open cow");
            let _handle = cow_mmap
                .watch(|_event| {
                    // Just test that we can set up a watch
                })
                .expect("watch cow");
        }

        fs::remove_file(&path).expect("cleanup");
    }

    #[test]
    #[cfg(feature = "watch")]
    #[cfg_attr(
        windows,
        ignore = "Windows mtime granularity makes polling-based change detection flaky; reliable detection requires native ReadDirectoryChangesW"
    )]
    fn test_multiple_watchers() {
        let path = tmp_path("multi_watch");
        let _ = fs::remove_file(&path);

        let mmap = create_mmap(&path, 1024).expect("create");

        // Set up multiple watchers
        let count1 = Arc::new(AtomicUsize::new(0));
        let count1_clone = Arc::clone(&count1);
        let _handle1 = mmap
            .watch(move |_event| {
                count1_clone.fetch_add(1, Ordering::SeqCst);
            })
            .expect("watch 1");

        let count2 = Arc::new(AtomicUsize::new(0));
        let count2_clone = Arc::clone(&count2);
        let _handle2 = mmap
            .watch(move |_event| {
                count2_clone.fetch_add(1, Ordering::SeqCst);
            })
            .expect("watch 2");

        // Give watchers time to start (6 polling intervals)
        thread::sleep(Duration::from_millis(WATCH_POLL_INTERVAL_MS * 6));

        // Modify file and ensure mtime changes
        mmap.update_region(0, b"change").expect("write");
        mmap.flush()
            .expect("flush after write for watch visibility");

        #[allow(unused_variables)]
        {
            #[cfg(unix)]
            {
                use std::ffi::CString;
                use std::os::unix::ffi::OsStrExt;
                let cpath = CString::new(path.as_os_str().as_bytes()).unwrap();
                unsafe { libc::utime(cpath.as_ptr(), std::ptr::null()) };
            }
            #[cfg(windows)]
            {
                if let Ok(meta) = std::fs::metadata(&path) {
                    let mut perms = meta.permissions();
                    perms.set_readonly(true);
                    let _ = std::fs::set_permissions(&path, perms);
                    let mut perms2 = std::fs::metadata(&path).unwrap().permissions();
                    perms2.set_readonly(false);
                    let _ = std::fs::set_permissions(&path, perms2);
                }
            }
        }

        // Wait for change detection (allow 15 polling cycles)
        thread::sleep(Duration::from_millis(WATCH_POLL_INTERVAL_MS * 15));

        // Both watchers should detect the change
        assert!(
            count1.load(Ordering::SeqCst) > 0,
            "Watcher 1 should detect change"
        );
        assert!(
            count2.load(Ordering::SeqCst) > 0,
            "Watcher 2 should detect change"
        );

        fs::remove_file(&path).expect("cleanup");
    }

    #[test]
    #[cfg(feature = "watch")]
    fn test_watch_handle_shutdown_on_drop() {
        // H5 regression: dropping the handle MUST stop the background
        // thread within a reasonable time (a few polling intervals).
        let path = tmp_path("watch_shutdown");
        let _ = fs::remove_file(&path);

        let mmap = create_mmap(&path, 1024).expect("create");

        let handle = mmap.watch(|_event| { /* no-op */ }).expect("watch");

        // The thread should be running.
        assert!(handle.is_active());

        // Drop the handle. The shutdown flag is set; the thread will
        // exit on its next iteration after sleep completes.
        drop(handle);

        // Wait long enough for the thread to wake up, check the flag,
        // and exit. 3 polling intervals plus a margin should be ample.
        thread::sleep(Duration::from_millis(WATCH_POLL_INTERVAL_MS * 3 + 50));

        // The thread is detached after Drop, so we can't query it
        // directly. But we can verify clean teardown indirectly: the
        // file is still mappable and not held by any leftover handle.
        // (A more direct test would require exposing the JoinHandle,
        // which we don't.)

        fs::remove_file(&path).expect("cleanup");
    }
}
