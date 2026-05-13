//! Integration tests for all new features.

#[cfg(all(
    feature = "advise",
    feature = "iterator",
    feature = "cow",
    feature = "locking",
    feature = "atomic",
    feature = "watch"
))]
#[allow(clippy::permissions_set_readonly_false)]
mod all_features {
    use mmap_io::{create_mmap, ChangeEvent, MemoryMappedFile, MmapAdvice};
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::thread;
    use std::time::Duration;

    fn tmp_path(name: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "mmap_io_integration_test_{}_{}",
            name,
            std::process::id()
        ));
        p
    }

    #[test]
    fn test_all_features_integration() {
        let path = tmp_path("all_features");
        let _ = fs::remove_file(&path);

        // Create a file with some initial data
        let mmap = create_mmap(&path, 8192).expect("create");

        // Test advise
        mmap.advise(0, 4096, MmapAdvice::Sequential)
            .expect("advise sequential");
        mmap.advise(4096, 4096, MmapAdvice::Random)
            .expect("advise random");

        // Test iterator - write pattern using chunks_mut
        mmap.chunks_mut(1024)
            .for_each_mut(|offset, chunk| {
                let value = (offset / 1024) as u8;
                chunk.fill(value);
                Ok(())
            })
            .expect("for_each_mut");

        mmap.flush().expect("flush");

        // Test iterator - zero-copy read and verify using chunks
        let chunks: Vec<Vec<u8>> = mmap.chunks(1024).map(|s| s.as_slice().to_vec()).collect();

        assert_eq!(chunks.len(), 8);
        for (i, chunk) in chunks.iter().enumerate() {
            assert!(chunk.iter().all(|&b| b == i as u8));
        }

        // Test atomic operations
        let atomic = mmap.atomic_u64(0).expect("atomic u64");
        atomic.store(0x1234567890ABCDEF, Ordering::SeqCst);
        assert_eq!(atomic.load(Ordering::SeqCst), 0x1234567890ABCDEF);
        // Drop the view before any subsequent write op on this mmap:
        // the view holds the read lock; update_region/flush need the
        // write lock and would deadlock the test thread otherwise.
        drop(atomic);

        // Test locking (may fail without privileges)
        let _ = mmap.lock(0, 4096);
        let _ = mmap.unlock(0, 4096);

        // Test native watch (since 0.9.9: inotify/FSEvents/RDCW).
        // For reliable cross-platform detection we modify the file
        // via the std::fs API rather than via the mmap; mmap writes
        // go through the page cache and only reach the FS watcher
        // at OS-decided writeback time, which is too unreliable for
        // a tight test deadline.
        let changed = Arc::new(AtomicBool::new(false));
        let changed_clone = Arc::clone(&changed);

        let _handle = mmap
            .watch(move |_event: ChangeEvent| {
                changed_clone.store(true, Ordering::SeqCst);
            })
            .expect("watch");

        // Give the OS watcher a moment to register the subscription.
        thread::sleep(Duration::from_millis(200));

        // External modification through the std::fs API: this is
        // what real-world FS watchers are designed to observe and
        // what every supported backend (inotify / FSEvents / RDCW)
        // reliably reports.
        use std::io::Write;
        {
            let mut f = std::fs::OpenOptions::new()
                .write(true)
                .open(&path)
                .expect("reopen for external write");
            f.write_all(b"watched change").expect("external write");
            f.sync_all().expect("external sync");
        }

        // Native backends are fast; 3 seconds is a generous deadline
        // for slow CI.
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        while std::time::Instant::now() < deadline {
            if changed.load(Ordering::SeqCst) {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }

        assert!(
            changed.load(Ordering::SeqCst),
            "watcher must detect the external change"
        );

        // Clean up
        drop(mmap);
        fs::remove_file(&path).expect("cleanup");
    }

    #[test]
    fn test_cow_mode_integration() {
        let path = tmp_path("cow_integration");
        let _ = fs::remove_file(&path);

        // Create initial file
        let mmap = create_mmap(&path, 4096).expect("create");
        mmap.update_region(0, b"original data").expect("write");
        mmap.flush().expect("flush");
        drop(mmap);

        // Open in COW mode
        let cow_mmap = MemoryMappedFile::open_cow(&path).expect("open cow");

        // Read original data
        let mut buf = vec![0u8; 13];
        cow_mmap.read_into(0, &mut buf).expect("read");
        assert_eq!(&buf, b"original data");

        // Test advise on COW
        cow_mmap
            .advise(0, 4096, MmapAdvice::WillNeed)
            .expect("advise cow");

        // Test iterator on COW (zero-copy; each item is MappedSlice).
        let page_count = cow_mmap.pages().count();
        assert!(page_count > 0);

        // Test atomic on COW (read-only)
        let atomic = cow_mmap.atomic_u64(16).expect("atomic cow");
        let _ = atomic.load(Ordering::SeqCst);

        // Clean up
        fs::remove_file(&path).expect("cleanup");
    }

    #[test]
    fn test_concurrent_features() {
        let path = tmp_path("concurrent_features");
        let _ = fs::remove_file(&path);

        let mmap = Arc::new(create_mmap(&path, 8192).expect("create"));

        // Initialize with atomic values
        for i in 0..8 {
            let atomic = mmap.atomic_u64(i * 8).expect("atomic");
            atomic.store(i, Ordering::SeqCst);
        }

        // Spawn threads that use different features
        let handles: Vec<_> = (0..4)
            .map(|thread_id| {
                let mmap = Arc::clone(&mmap);
                thread::spawn(move || {
                    // Each thread works on different parts
                    let offset = thread_id * 2048;

                    // Advise on thread's region
                    mmap.advise(offset, 2048, MmapAdvice::Sequential)
                        .expect("thread advise");

                    // Read using iterator
                    let mut iter = mmap.chunks(512);
                    for _ in 0..thread_id {
                        iter.next(); // Skip to thread's region
                    }

                    if let Some(chunk) = iter.next() {
                        assert_eq!(chunk.len(), 512);
                    }

                    // Atomic increment
                    if thread_id < 8 {
                        let atomic = mmap.atomic_u64(thread_id * 8).expect("thread atomic");
                        atomic.fetch_add(100, Ordering::SeqCst);
                    }
                })
            })
            .collect();

        // Wait for all threads
        for handle in handles {
            handle.join().expect("thread join");
        }

        // Verify atomic increments
        for i in 0..4 {
            let atomic = mmap.atomic_u64(i * 8).expect("verify atomic");
            assert_eq!(atomic.load(Ordering::SeqCst), i + 100);
        }

        fs::remove_file(&path).expect("cleanup");
    }

    #[test]
    fn test_page_aligned_operations() {
        use mmap_io::utils::page_size;

        let path = tmp_path("page_aligned");
        let _ = fs::remove_file(&path);

        let ps = page_size();
        let file_size = ps * 4; // 4 pages

        let mmap = create_mmap(&path, file_size as u64).expect("create");

        // Advise each page differently
        mmap.advise(0, ps as u64, MmapAdvice::Sequential)
            .expect("advise page 0");
        mmap.advise(ps as u64, ps as u64, MmapAdvice::Random)
            .expect("advise page 1");
        mmap.advise((ps * 2) as u64, ps as u64, MmapAdvice::WillNeed)
            .expect("advise page 2");
        mmap.advise((ps * 3) as u64, ps as u64, MmapAdvice::DontNeed)
            .expect("advise page 3");

        // Lock first page (may fail without privileges)
        let _ = mmap.lock(0, ps as u64);

        // Verify page sizes via the zero-copy iterator.
        for (i, page_data) in mmap.pages().enumerate() {
            if i < 3 {
                assert_eq!(page_data.len(), ps);
            }
        }

        // Place atomics at page boundaries
        for i in 0..4 {
            let offset = (i * ps) as u64;
            if offset + 8 <= file_size as u64 {
                let atomic = mmap.atomic_u64(offset).expect("page boundary atomic");
                atomic.store(0xDEADBEEF00000000 | i as u64, Ordering::SeqCst);
            }
        }

        // Unlock if we locked
        let _ = mmap.unlock(0, ps as u64);

        fs::remove_file(&path).expect("cleanup");
    }
}

// Test that features can be used independently
#[cfg(feature = "advise")]
#[test]
fn test_advise_only() {
    use mmap_io::{create_mmap, MmapAdvice};
    let path = "test_advise_only.tmp";
    let _ = std::fs::remove_file(path);

    let mmap = create_mmap(path, 4096).expect("create");
    mmap.advise(0, 4096, MmapAdvice::Normal).expect("advise");

    std::fs::remove_file(path).expect("cleanup");
}

#[cfg(feature = "iterator")]
#[test]
fn test_iterator_only() {
    use mmap_io::create_mmap;
    let path = "test_iterator_only.tmp";
    let _ = std::fs::remove_file(path);

    let mmap = create_mmap(path, 4096).expect("create");
    let count = mmap.chunks(1024).count();
    assert_eq!(count, 4);

    std::fs::remove_file(path).expect("cleanup");
}

#[cfg(feature = "atomic")]
#[test]
fn test_atomic_only() {
    use mmap_io::create_mmap;
    use std::sync::atomic::Ordering;

    let path = "test_atomic_only.tmp";
    let _ = std::fs::remove_file(path);

    let mmap = create_mmap(path, 64).expect("create");
    let atomic = mmap.atomic_u64(0).expect("atomic");
    atomic.store(42, Ordering::SeqCst);

    std::fs::remove_file(path).expect("cleanup");
}
