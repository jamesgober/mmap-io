//! Non-blocking accessors (1.1.0): `try_as_slice`, `try_as_slice_mut`
//! and `try_update_region` report "would block" instead of waiting, and
//! otherwise behave like their blocking counterparts.

use std::sync::{mpsc, Arc};
use std::thread;

use mmap_io::flush::FlushPolicy;
use mmap_io::{AnonymousMmap, MemoryMappedFile, MmapIoError};

fn rw(dir: &tempfile::TempDir, len: u64) -> MemoryMappedFile {
    MemoryMappedFile::create_rw(dir.path().join("t.bin"), len).expect("create_rw")
}

#[test]
fn uncontended_calls_behave_like_blocking_ones() {
    let dir = tempfile::tempdir().expect("tempdir");
    let m = rw(&dir, 4096);
    assert!(m.try_update_region(10, b"abc").expect("write"));
    assert_eq!(&*m.try_as_slice(10, 3).expect("ok").expect("some"), b"abc");
    {
        let mut w = m.try_as_slice_mut(20, 2).expect("ok").expect("some");
        w.copy_from_slice(b"zz");
    }
    assert_eq!(&*m.as_slice(20, 2).expect("slice"), b"zz");
    assert_eq!(m.pending_bytes(), 5);
}

#[test]
fn validation_matches_blocking_versions() {
    let dir = tempfile::tempdir().expect("tempdir");
    let m = rw(&dir, 100);
    for (off, len) in [(100u64, 1u64), (0, 101), (u64::MAX, 1), (99, 2)] {
        assert!(matches!(
            m.try_as_slice(off, len),
            Err(MmapIoError::OutOfBounds { .. })
        ));
        assert!(matches!(
            m.try_as_slice_mut(off, len),
            Err(MmapIoError::OutOfBounds { .. })
        ));
        assert!(matches!(
            m.try_update_region(off, &vec![0; len as usize]),
            Err(MmapIoError::OutOfBounds { .. })
        ));
    }
    // Zero-length requests are accepted at any offset.
    assert!(m
        .try_as_slice(1000, 0)
        .expect("ok")
        .expect("some")
        .is_empty());
    assert!(m
        .try_as_slice_mut(1000, 0)
        .expect("ok")
        .expect("some")
        .is_empty());
    assert!(m.try_update_region(u64::MAX, &[]).expect("ok"));
    // After a shrink the new length is what counts.
    m.resize(50).expect("shrink");
    assert!(matches!(
        m.try_update_region(49, b"xy"),
        Err(MmapIoError::OutOfBounds { .. })
    ));
    assert!(m.try_update_region(48, b"xy").expect("ok"));
}

#[test]
fn read_only_mappings() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("ro.bin");
    std::fs::write(&path, b"read-only").expect("write");
    let ro = MemoryMappedFile::open_ro(&path).expect("open_ro");
    assert_eq!(&*ro.try_as_slice(0, 4).expect("ok").expect("some"), b"read");
    assert!(matches!(
        ro.try_as_slice_mut(0, 1),
        Err(MmapIoError::InvalidMode(_))
    ));
    assert!(matches!(
        ro.try_update_region(0, b"x"),
        Err(MmapIoError::InvalidMode(_))
    ));
    // Empty data is accepted in any mode, like update_region.
    assert!(ro.try_update_region(0, &[]).expect("empty"));
}

#[test]
fn same_thread_reader_makes_writes_report_would_block() {
    let dir = tempfile::tempdir().expect("tempdir");
    let m = rw(&dir, 4096);
    let view = m.as_slice(0, 16).expect("view");
    // Blocking versions would deadlock here.
    assert!(!m.try_update_region(100, b"x").expect("would block"));
    assert!(m.try_as_slice_mut(100, 1).expect("would block").is_none());
    // Readers do not block readers.
    assert!(m.try_as_slice(100, 8).expect("ok").is_some());
    drop(view);
    assert!(m.try_update_region(100, b"x").expect("free"));
}

#[test]
fn same_thread_writer_makes_everything_report_would_block() {
    let dir = tempfile::tempdir().expect("tempdir");
    let m = rw(&dir, 4096);
    let w = m.as_slice_mut(0, 16).expect("writer");
    assert!(m.try_as_slice(100, 8).expect("ok").is_none());
    assert!(m.try_as_slice_mut(100, 8).expect("ok").is_none());
    assert!(!m.try_update_region(100, b"x").expect("ok"));
    // Nothing was written by the refused call.
    drop(w);
    assert_eq!(m.as_slice(100, 1).expect("slice")[0], 0);
}

#[cfg(feature = "iterator")]
#[test]
fn iterator_items_and_atomics_block_writers_without_deadlock() {
    let dir = tempfile::tempdir().expect("tempdir");
    let m = rw(&dir, 8192);
    let mut it = m.chunks(4096);
    let first = it.next().expect("chunk");
    assert!(!m.try_update_region(5000, b"x").expect("would block"));
    drop((first, it));
    assert!(m.try_update_region(5000, b"x").expect("free"));
    #[cfg(feature = "atomic")]
    {
        let a = m.atomic_u64(0).expect("atomic");
        assert!(!m.try_update_region(5000, b"y").expect("would block"));
        // Overlap with an atomic view is an error, not "would block".
        assert!(matches!(
            m.try_as_slice(0, 8),
            Err(MmapIoError::InvalidMode(_))
        ));
        drop(a);
    }
}

#[test]
fn other_thread_writer_is_reported_then_released() {
    let dir = tempfile::tempdir().expect("tempdir");
    let m = Arc::new(rw(&dir, 4096));
    let (held_tx, held_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let holder = {
        let m = Arc::clone(&m);
        thread::spawn(move || {
            let mut w = m.as_slice_mut(0, 4).expect("writer");
            w.copy_from_slice(b"held");
            held_tx.send(()).expect("send");
            release_rx.recv().expect("recv");
        })
    };
    held_rx.recv().expect("held");
    assert!(m.try_as_slice(0, 4).expect("ok").is_none());
    assert!(!m.try_update_region(8, b"x").expect("ok"));
    release_tx.send(()).expect("release");
    holder.join().expect("join");
    assert_eq!(&*m.try_as_slice(0, 4).expect("ok").expect("some"), b"held");
    assert!(m.try_update_region(8, b"x").expect("free"));
}

#[test]
fn flush_policy_runs_without_releasing_the_lock() {
    let dir = tempfile::tempdir().expect("tempdir");
    let m = MemoryMappedFile::builder(dir.path().join("p.bin"))
        .size(4096)
        .flush_policy(FlushPolicy::Always)
        .create()
        .expect("create");
    assert!(m.try_update_region(0, b"durable").expect("write"));
    assert_eq!(m.pending_bytes(), 0, "Always policy flushed");
    let m = MemoryMappedFile::builder(dir.path().join("q.bin"))
        .size(4096)
        .flush_policy(FlushPolicy::EveryBytes(10))
        .create()
        .expect("create");
    assert!(m.try_update_region(0, b"12345").expect("write"));
    assert_eq!(m.pending_bytes(), 5);
    assert!(m.try_update_region(5, b"67890").expect("write"));
    assert_eq!(m.pending_bytes(), 0, "threshold reached and flushed");
}

#[cfg(feature = "cow")]
#[test]
fn copy_on_write_mappings() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("c.bin");
    std::fs::write(&path, vec![1u8; 4096]).expect("write");
    // Default COW is read-only: refused before the lock, as on RO.
    let ro_cow = MemoryMappedFile::open_cow(&path).expect("cow");
    let held = ro_cow.as_slice(0, 4).expect("view");
    assert!(matches!(
        ro_cow.try_update_region(10, b"x"),
        Err(MmapIoError::InvalidMode(_))
    ));
    assert!(matches!(
        ro_cow.try_as_slice_mut(10, 1),
        Err(MmapIoError::InvalidMode(_))
    ));
    assert!(ro_cow.try_as_slice(0, 4).expect("try view").is_some());
    drop(held);
    drop(ro_cow);

    let cow = MemoryMappedFile::open_cow_writable(&path).expect("cow");
    let view = cow.as_slice(0, 4).expect("view");
    assert!(!cow.try_update_region(10, b"x").expect("would block"));
    drop(view);
    assert!(cow.try_update_region(10, b"x").expect("free"));
    assert_eq!(cow.pending_bytes(), 0);
    assert_eq!(std::fs::read(&path).expect("read"), vec![1u8; 4096]);
}

#[test]
fn anonymous_mappings() {
    let m = AnonymousMmap::new(4096).expect("anon");
    assert!(m.try_update_region(0, b"anon").expect("write"));
    assert_eq!(&*m.try_as_slice(0, 4).expect("ok").expect("some"), b"anon");
    let view = m.as_slice(0, 4).expect("view");
    assert!(!m.try_update_region(8, b"x").expect("would block"));
    assert!(m.try_as_mut_slice(8, 1).expect("ok").is_none());
    assert!(m.try_as_slice(8, 1).expect("ok").is_some());
    drop(view);
    let w = m.as_mut_slice(0, 1).expect("writer");
    assert!(m.try_as_slice(8, 1).expect("ok").is_none());
    drop(w);
    assert!(matches!(
        m.try_update_region(4095, b"xy"),
        Err(MmapIoError::OutOfBounds { .. })
    ));
    assert!(matches!(
        m.try_as_mut_slice(4096, 1),
        Err(MmapIoError::OutOfBounds { .. })
    ));
}

#[test]
fn concurrent_try_writers_never_lose_successful_writes() {
    let dir = tempfile::tempdir().expect("tempdir");
    let m = Arc::new(rw(&dir, 8 * 64));
    let handles: Vec<_> = (0..8u8)
        .map(|t| {
            let m = Arc::clone(&m);
            thread::spawn(move || {
                let mut written = 0;
                while written < 100 {
                    if m.try_update_region(u64::from(t) * 64, &[t; 64])
                        .expect("write")
                    {
                        written += 1;
                    } else {
                        thread::yield_now();
                    }
                    if written == 0 {
                        continue;
                    }
                    if let Some(s) = m.try_as_slice(u64::from(t) * 64, 64).expect("read") {
                        assert!(s.iter().all(|&b| b == t));
                    }
                }
            })
        })
        .collect();
    for h in handles {
        h.join().expect("join");
    }
}
