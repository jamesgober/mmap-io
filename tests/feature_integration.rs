//! Integration tests for all new features.

#[cfg(all(
    feature = "advise",
    feature = "iterator",
    feature = "cow",
    feature = "locking",
    feature = "atomic",
    feature = "watch"
))]
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
                Ok::<(), std::io::Error>(())
            })
            .expect("chunks_mut")
            .expect("for_each_mut");

        mmap.flush().expect("flush");

        // Test iterator - read and verify using chunks
        let chunks: Vec<_> = mmap
            .chunks(1024)
            .collect::<Result<Vec<_>, _>>()
            .expect("collect chunks");

        assert_eq!(chunks.len(), 8);
        for (i, chunk) in chunks.iter().enumerate() {
            assert!(chunk.iter().all(|&b| b == i as u8));
        }

        // Test atomic operations
        let atomic = mmap.atomic_u64(0).expect("atomic u64");
        atomic.store(0x1234567890ABCDEF, Ordering::SeqCst);
        assert_eq!(atomic.load(Ordering::SeqCst), 0x1234567890ABCDEF);

        // Test locking (may fail without privileges)
        let _ = mmap.lock(0, 4096);
        let _ = mmap.unlock(0, 4096);

        // Test watch
        let changed = Arc::new(AtomicBool::new(false));
        let changed_clone = Arc::clone(&changed);

        let _handle = mmap
            .watch(move |_event: ChangeEvent| {
                changed_clone.store(true, Ordering::SeqCst);
            })
            .expect("watch");

        // Give watcher time to start (allow 3 polling intervals at 100ms each)
        thread::sleep(Duration::from_millis(300));

        // Make a change
        mmap.update_region(100, b"watched change").expect("update");
        // Ensure durability and bump timestamps for watch parity across platforms
        mmap.flush().expect("flush for watch visibility");
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

        // Wait for change detection (allow 12 polling cycles at 100ms each)
        thread::sleep(Duration::from_millis(1200));

        // The change should be detected
        assert!(changed.load(Ordering::SeqCst), "Change should be detected");

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

        // Test iterator on COW
        let pages: Vec<_> = cow_mmap
            .pages()
            .collect::<Result<Vec<_>, _>>()
            .expect("collect pages");
        assert!(!pages.is_empty());

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
            atomic.store(i as u64, Ordering::SeqCst);
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

                    if let Some(Ok(chunk)) = iter.next() {
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
            assert_eq!(atomic.load(Ordering::SeqCst), i as u64 + 100);
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

        // Write pattern to each page
        for (i, page) in mmap.pages().enumerate() {
            if let Ok(page_data) = page {
                // Verify page size
                if i < 3 {
                    assert_eq!(page_data.len(), ps);
                }
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
    let _chunks: Vec<_> = mmap
        .chunks(1024)
        .collect::<Result<Vec<_>, _>>()
        .expect("chunks");

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
