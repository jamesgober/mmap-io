//! Regression tests for lock-lifetime and resize-race soundness bugs.
//!
//! Each test pins one observable property:
//!
//! - Items yielded by `chunks()` / `pages()` keep the mapping pinned
//!   after the iterator itself is dropped (a yielded chunk used to
//!   borrow memory that `resize()` could unmap).
//! - Atomic views refuse read-only and copy-on-write mappings, whose
//!   pages are not writable.
//! - A shrinking `resize()` does not truncate the file while a view
//!   into the doomed tail is still alive.
//! - Accessors validate against the length protected by the lock they
//!   hold, so a concurrent `resize()` yields `OutOfBounds`, never a
//!   panic or an out-of-range access.
//! - Taking a second read view on the same thread while a writer is
//!   queued does not deadlock.

use mmap_io::{MemoryMappedFile, MmapIoError};
use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::{Duration, Instant};

mod common;
use common::tmp_path;

fn on_disk_len(path: &Path) -> u64 {
    fs::metadata(path).expect("metadata").len()
}

/// Spawn `resize(new_size)` on another thread. The returned flag flips
/// to `true` once `resize` has returned.
fn resize_in_background(
    mmap: &Arc<MemoryMappedFile>,
    new_size: u64,
) -> (thread::JoinHandle<()>, Arc<AtomicBool>) {
    let done = Arc::new(AtomicBool::new(false));
    let done_t = Arc::clone(&done);
    let m = Arc::clone(mmap);
    let h = thread::spawn(move || {
        m.resize(new_size).expect("resize");
        done_t.store(true, Ordering::SeqCst);
    });
    (h, done)
}

#[cfg(feature = "iterator")]
#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
#[test]
fn yielded_chunk_pins_mapping_after_iterator_drops() {
    let path = tmp_path("chunk_pins");
    let _ = fs::remove_file(&path);
    let mmap = Arc::new(MemoryMappedFile::create_rw(&path, 64 * 1024).expect("create"));
    mmap.update_region(60 * 1024, &[0x5A; 4096]).expect("seed");

    // `last()` consumes and drops the iterator; only the chunk lives on.
    let chunk = mmap.chunks(4096).last().expect("at least one chunk");

    // Growing remaps the file, so the chunk's old address would dangle.
    let (h, done) = resize_in_background(&mmap, 1024 * 1024);
    thread::sleep(Duration::from_millis(200));
    assert!(
        !done.load(Ordering::SeqCst),
        "resize() completed while a yielded chunk was still alive"
    );
    assert!(chunk.iter().all(|&b| b == 0x5A));
    drop(chunk);

    h.join().expect("resize thread");
    assert!(done.load(Ordering::SeqCst));
    assert_eq!(mmap.len(), 1024 * 1024);

    drop(mmap);
    let _ = fs::remove_file(&path);
}

#[cfg(feature = "iterator")]
#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
#[test]
fn yielded_page_pins_mapping_after_iterator_drops() {
    let path = tmp_path("page_pins");
    let _ = fs::remove_file(&path);
    let ps = mmap_io::utils::page_size() as u64;
    let mmap = Arc::new(MemoryMappedFile::create_rw(&path, ps * 4).expect("create"));

    let pages: Vec<_> = mmap.pages().collect();
    assert_eq!(pages.len(), 4);

    let (h, done) = resize_in_background(&mmap, ps * 64);
    thread::sleep(Duration::from_millis(200));
    assert!(
        !done.load(Ordering::SeqCst),
        "resize() completed while yielded pages were still alive"
    );
    drop(pages);
    h.join().expect("resize thread");
    assert_eq!(mmap.len(), ps * 64);

    drop(mmap);
    let _ = fs::remove_file(&path);
}

#[cfg(feature = "atomic")]
#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
#[test]
fn atomic_views_reject_read_only_mapping() {
    let path = tmp_path("atomic_ro");
    let _ = fs::remove_file(&path);
    {
        let rw = MemoryMappedFile::create_rw(&path, 64).expect("create");
        rw.update_region(0, &42u64.to_ne_bytes()).expect("seed");
        rw.flush().expect("flush");
    }
    let ro = MemoryMappedFile::open_ro(&path).expect("open_ro");
    assert!(matches!(
        ro.atomic_u64(0),
        Err(mmap_io::MmapIoError::InvalidMode(_))
    ));
    assert!(matches!(
        ro.atomic_u32(0),
        Err(mmap_io::MmapIoError::InvalidMode(_))
    ));
    assert!(matches!(
        ro.atomic_u64_slice(0, 2),
        Err(mmap_io::MmapIoError::InvalidMode(_))
    ));
    assert!(matches!(
        ro.atomic_u32_slice(0, 2),
        Err(mmap_io::MmapIoError::InvalidMode(_))
    ));
    drop(ro);
    let _ = fs::remove_file(&path);
}

// A default copy-on-write mapping is read-only (as in 1.0), so its pages
// may not be writable and an atomic view, whose safe `store` would write
// them, is refused.
#[cfg(all(feature = "atomic", feature = "cow"))]
#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
#[test]
fn atomic_views_reject_copy_on_write_mapping() {
    let path = tmp_path("atomic_cow_ro");
    let _ = fs::remove_file(&path);
    {
        let rw = MemoryMappedFile::create_rw(&path, 64).expect("create");
        rw.flush().expect("flush");
    }
    let cow = MemoryMappedFile::open_cow(&path).expect("open_cow");
    assert!(matches!(
        cow.atomic_u64(0),
        Err(mmap_io::MmapIoError::InvalidMode(_))
    ));
    assert!(matches!(
        cow.atomic_u32_slice(0, 1),
        Err(mmap_io::MmapIoError::InvalidMode(_))
    ));
    drop(cow);
    let _ = fs::remove_file(&path);
}

// A copy-on-write mapping opened writable (1.1 opt-in) is mapped with
// private writable pages, so atomic views on it are sound and allowed;
// the stores never reach the file.
#[cfg(all(feature = "atomic", feature = "cow"))]
#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
#[test]
fn atomic_views_on_writable_copy_on_write_mapping_stay_private() {
    use std::sync::atomic::Ordering;
    let path = tmp_path("atomic_cow");
    let _ = fs::remove_file(&path);
    {
        let rw = MemoryMappedFile::create_rw(&path, 64).expect("create");
        rw.flush().expect("flush");
    }
    let cow = MemoryMappedFile::open_cow_writable(&path).expect("open_cow_writable");
    cow.atomic_u64(0)
        .expect("u64 view")
        .store(u64::MAX, Ordering::SeqCst);
    cow.atomic_u32_slice(8, 1).expect("u32 view")[0].store(7, Ordering::SeqCst);
    assert_eq!(
        cow.atomic_u64(0).expect("again").load(Ordering::SeqCst),
        u64::MAX
    );
    drop(cow);
    assert_eq!(fs::read(&path).expect("read"), vec![0u8; 64]);
    let _ = fs::remove_file(&path);
}

#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
#[test]
fn shrink_does_not_truncate_file_under_live_view() {
    let path = tmp_path("shrink_live_view");
    let _ = fs::remove_file(&path);
    let size = 64 * 1024;
    let mmap = Arc::new(MemoryMappedFile::create_rw(&path, size).expect("create"));
    mmap.update_region(size - 4096, &[0x77; 4096])
        .expect("seed");

    // Hold a view into the tail that the shrink would cut off.
    let tail = mmap.as_slice(size - 4096, 4096).expect("tail view");

    let (h, done) = resize_in_background(&mmap, 4096);
    thread::sleep(Duration::from_millis(200));
    assert!(!done.load(Ordering::SeqCst), "resize ran under a live view");
    assert_eq!(
        on_disk_len(&path),
        size,
        "file was truncated while a view into its tail was alive"
    );
    // Touching the tail must still be valid (SIGBUS before the fix).
    assert!(tail.iter().all(|&b| b == 0x77));
    drop(tail);

    h.join().expect("resize thread");
    assert_eq!(mmap.len(), 4096);
    assert_eq!(on_disk_len(&path), 4096);

    drop(mmap);
    let _ = fs::remove_file(&path);
}

/// Hammer readers against a thread that flips the mapping between a
/// large and a small size. Every read near the end of the large size
/// must either succeed or fail with `OutOfBounds`; a stale cached
/// length used to make the guarded slice index past the new mapping
/// and panic.
#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
#[test]
fn concurrent_resize_never_panics_readers() {
    let path = tmp_path("resize_race");
    let _ = fs::remove_file(&path);
    let big: u64 = 256 * 1024;
    let small: u64 = 4096;
    let mmap = Arc::new(MemoryMappedFile::create_rw(&path, big).expect("create"));

    let stop = Arc::new(AtomicBool::new(false));
    let resizer = {
        let m = Arc::clone(&mmap);
        let stop = Arc::clone(&stop);
        thread::spawn(move || {
            let mut grow = false;
            while !stop.load(Ordering::Relaxed) {
                let target = if grow { big } else { small };
                m.resize(target).expect("resize");
                grow = !grow;
            }
        })
    };

    let readers: Vec<_> = (0..3)
        .map(|i| {
            let m = Arc::clone(&mmap);
            let stop = Arc::clone(&stop);
            thread::spawn(move || {
                let mut buf = [0u8; 64];
                let off = big - 128;
                while !stop.load(Ordering::Relaxed) {
                    let r = match i {
                        0 => m.read_into(off, &mut buf),
                        1 => m.as_slice(off, 64).map(|s| {
                            buf.copy_from_slice(&s);
                        }),
                        _ => m.update_region(off, &buf),
                    };
                    match r {
                        Ok(()) | Err(MmapIoError::OutOfBounds { .. }) => {}
                        Err(e) => panic!("unexpected error: {e:?}"),
                    }
                }
            })
        })
        .collect();

    thread::sleep(Duration::from_millis(500));
    stop.store(true, Ordering::Relaxed);
    resizer.join().expect("resizer panicked");
    for r in readers {
        r.join().expect("reader panicked under concurrent resize");
    }

    drop(mmap);
    let _ = fs::remove_file(&path);
}

/// Run `f` on a helper thread and fail if it does not finish in time.
/// A deadlocked helper is leaked; the test still fails.
fn finishes_within<F: FnOnce() + Send + 'static>(timeout: Duration, f: F) -> bool {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        f();
        let _ = tx.send(());
    });
    rx.recv_timeout(timeout).is_ok()
}

#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
#[test]
fn second_read_view_on_same_thread_does_not_deadlock_behind_writer() {
    let path = tmp_path("recursive_read");
    let _ = fs::remove_file(&path);
    let mmap = Arc::new(MemoryMappedFile::create_rw(&path, 4096).expect("create"));

    let m = Arc::clone(&mmap);
    let ok = finishes_within(Duration::from_secs(5), move || {
        let first = m.as_slice(0, 16).expect("first view");

        // Queue a writer behind the read guard we hold.
        let w = Arc::clone(&m);
        let writer = thread::spawn(move || {
            w.update_region(0, b"writer").expect("write");
        });
        thread::sleep(Duration::from_millis(100));

        // A fair RwLock blocks new readers once a writer is queued;
        // the crate's read paths must still let this thread in.
        let second = m.as_slice(16, 16).expect("second view");
        let mut buf = [0u8; 8];
        m.read_into(0, &mut buf).expect("read_into");
        drop(second);
        drop(first);
        writer.join().expect("writer");
    });
    assert!(
        ok,
        "same-thread read view deadlocked behind a queued writer"
    );

    let _ = fs::remove_file(&path);
}

#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
#[test]
fn resize_blocks_until_slice_drops_then_completes() {
    let path = tmp_path("resize_waits");
    let _ = fs::remove_file(&path);
    let mmap = Arc::new(MemoryMappedFile::create_rw(&path, 8192).expect("create"));
    let view = mmap.as_slice(0, 8192).expect("view");
    let start = Instant::now();
    let (h, done) = resize_in_background(&mmap, 16384);
    thread::sleep(Duration::from_millis(100));
    assert!(!done.load(Ordering::SeqCst));
    drop(view);
    h.join().expect("resize");
    assert!(start.elapsed() >= Duration::from_millis(100));
    assert_eq!(mmap.len(), 16384);
    drop(mmap);
    let _ = fs::remove_file(&path);
}
