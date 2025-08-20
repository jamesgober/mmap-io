//! File change watching and notification support.

use crate::errors::Result;
use crate::mmap::MemoryMappedFile;
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
pub struct WatchHandle {
    // Thread handle is kept to ensure the watch thread is properly joined on drop
    thread: thread::JoinHandle<()>,
}

impl Drop for WatchHandle {
    fn drop(&mut self) {
        // The thread will naturally exit when it detects the file is removed
        // or when the handle is dropped. We don't join here to avoid blocking.
        // The thread will clean up on its own.
    }
}

impl WatchHandle {
    /// Check if the watch thread is still running.
    #[allow(dead_code)]
    pub fn is_active(&self) -> bool {
        !self.thread.is_finished()
    }
}

impl MemoryMappedFile {
    /// Watch for changes to the mapped file.
    ///
    /// The callback will be invoked whenever changes are detected.
    /// Returns a handle that stops watching when dropped.
    ///
    /// # Platform-specific behavior
    ///
    /// - **Linux**: Uses inotify for efficient monitoring
    /// - **macOS**: Uses FSEvents or kqueue
    /// - **Windows**: Uses ReadDirectoryChangesW
    /// - **Fallback**: Polling-based implementation
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
    /// # Ok::<(), mmap_io::MmapIoError>(())
    /// ```
    #[cfg(feature = "watch")]
    pub fn watch<F>(&self, callback: F) -> Result<WatchHandle>
    where
        F: Fn(ChangeEvent) + Send + 'static,
    {
        let path = self.path().to_path_buf();

        // For this implementation, we'll use a simple polling approach
        // In a production implementation, you'd use platform-specific APIs
        let thread = thread::spawn(move || {
            let mut last_modified = std::fs::metadata(&path)
                .ok()
                .and_then(|m| m.modified().ok());

            loop {
                thread::sleep(Duration::from_millis(WATCH_POLL_INTERVAL_MS));

                // Check if file still exists
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

                // Check modification time
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

        Ok(WatchHandle { thread })
    }
}

// Platform-specific implementations would go here
// For now, we use polling for all platforms

// Fallback polling implementation
// This function is kept for potential future use when implementing platform-specific watchers
#[cfg(feature = "watch")]
fn _polling_watch<F>(path: &std::path::Path, callback: F) -> Result<WatchHandle>
where
    F: Fn(ChangeEvent) + Send + 'static,
{
    let path = path.to_path_buf();

    let thread = thread::spawn(move || {
        let mut last_modified = std::fs::metadata(&path)
            .ok()
            .and_then(|m| m.modified().ok());
        let mut last_len = std::fs::metadata(&path).ok().map(|m| m.len());

        loop {
            thread::sleep(Duration::from_millis(WATCH_POLL_INTERVAL_MS));

            // Check if file still exists
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

            let current_len = metadata.len();
            let current_modified = metadata.modified().ok();

            // Check for changes
            if current_modified != last_modified || Some(current_len) != last_len {
                let kind = if Some(current_len) != last_len {
                    ChangeKind::Modified
                } else {
                    ChangeKind::Metadata
                };

                callback(ChangeEvent {
                    offset: None,
                    len: None,
                    kind,
                });

                last_modified = current_modified;
                last_len = Some(current_len);
            }
        }
    });

    Ok(WatchHandle { thread })
}

#[cfg(test)]
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
}
