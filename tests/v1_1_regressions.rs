//! Regression tests for bugs found by the 1.1 test-hardening pass.

use std::sync::Arc;
use std::thread;
use std::time::Duration;

use mmap_io::segment::SegmentMut;
use mmap_io::utils::{ensure_in_bounds, slice_range};
use mmap_io::{AnonymousMmap, MemoryMappedFile, MmapIoError};

fn oob<T>(r: Result<T, MmapIoError>) -> bool {
    matches!(r, Err(MmapIoError::OutOfBounds { .. }))
}

/// `offset + len` overflowing with `total == u64::MAX` used to saturate
/// to `u64::MAX`, pass the `end > total` check, and then overflow
/// (panic in debug, wrap in release) inside `slice_range`.
#[test]
fn bounds_helpers_reject_overflow_at_u64_max_total() {
    let max = u64::MAX;
    for (offset, len) in [
        (max, 1),
        (1, max),
        (max - 1, 2),
        (max / 2 + 1, max / 2 + 1),
        (max, max),
    ] {
        assert!(oob(ensure_in_bounds(offset, len, max)), "({offset}, {len})");
        assert!(oob(slice_range(offset, len, max)), "({offset}, {len})");
    }
    // In-range requests at the very top still pass the check.
    ensure_in_bounds(max - 1, 1, max).expect("last byte");
    ensure_in_bounds(max, 0, max).expect("empty at end");
    assert!(oob(ensure_in_bounds(1, 0, 0)));
}

/// `AnonymousMmap` follows the crate-wide zero-length rule.
#[test]
fn anonymous_zero_length_requests_are_accepted_anywhere() {
    let m = AnonymousMmap::new(64).expect("anon");
    for off in [64, 65, 1 << 40, u64::MAX] {
        m.read_into(off, &mut []).expect("read_into");
        m.update_region(off, &[]).expect("update_region");
        assert!(m.as_slice(off, 0).expect("as_slice").is_empty());
        assert!(m.as_mut_slice(off, 0).expect("as_mut_slice").is_empty());
        assert!(m
            .try_as_slice(off, 0)
            .expect("try")
            .expect("some")
            .is_empty());
        assert!(m
            .try_as_mut_slice(off, 0)
            .expect("try")
            .expect("some")
            .is_empty());
        assert!(m.try_update_region(off, &[]).expect("try"));
    }
    assert!(oob(m.read_into(64, &mut [0])));
    assert!(oob(m.as_slice(65, 1)));
}

/// A thread holding one `AnonymousMmap` view must be able to take a
/// second one while a writer is queued (fair `read()` deadlocked here).
#[test]
fn anonymous_second_view_does_not_deadlock_behind_queued_writer() {
    let m = Arc::new(AnonymousMmap::new(4096).expect("anon"));
    let first = m.as_slice(0, 8).expect("first view");
    let writer = {
        let m = Arc::clone(&m);
        thread::spawn(move || m.update_region(100, b"w").expect("write"))
    };
    // Give the writer time to queue on the lock.
    thread::sleep(Duration::from_millis(200));
    let (tx, rx) = std::sync::mpsc::channel();
    {
        let second = m.as_slice(8, 8).expect("second view");
        let mut buf = [0u8; 4];
        m.read_into(16, &mut buf).expect("read_into");
        assert!(m.try_as_slice(24, 4).expect("try").is_some());
        #[cfg(feature = "atomic")]
        {
            let a = m.atomic_u64(512).expect("atomic view");
            drop(a);
        }
        tx.send(second.len()).expect("send");
    }
    assert_eq!(
        rx.recv_timeout(Duration::from_secs(5))
            .expect("no deadlock"),
        8
    );
    drop(first);
    writer.join().expect("writer");
    let mut b = [0u8; 1];
    m.read_into(100, &mut b).expect("read");
    assert_eq!(&b, b"w");
}

/// `SegmentMut::write` must fail once the parent shrank below the
/// segment, even if the bytes written would still fit.
#[test]
fn segment_mut_write_after_shrink_is_out_of_bounds() {
    let dir = tempfile::tempdir().expect("tempdir");
    let parent =
        Arc::new(MemoryMappedFile::create_rw(dir.path().join("s.bin"), 8192).expect("create"));
    let seg = SegmentMut::new(Arc::clone(&parent), 4000, 200).expect("segment");
    seg.write(b"before").expect("fits before the shrink");
    parent.resize(4100).expect("shrink cuts the segment");
    assert!(!seg.is_valid());
    assert!(
        oob(seg.write(b"tiny")),
        "4000 + 4 fits, the segment does not"
    );
    assert!(oob(seg.as_slice_mut()));
    // Nothing was written past the check.
    assert_eq!(&*parent.as_slice(4000, 6).expect("slice"), b"before");
    // Empty writes stay no-ops under the zero-length rule.
    seg.write(&[]).expect("empty write");
    parent.resize(8192).expect("grow back");
    seg.write(b"after").expect("valid again");
}

/// `chunks_mut(0)` on a read-only mapping is an error, like any other
/// chunk size.
#[cfg(feature = "iterator")]
#[test]
fn chunks_mut_zero_on_read_only_is_invalid_mode() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("ro.bin");
    std::fs::write(&path, [0u8; 64]).expect("write");
    let ro = MemoryMappedFile::open_ro(&path).expect("open_ro");
    assert!(matches!(
        ro.chunks_mut(0).for_each_mut(|_, _| Ok(())),
        Err(MmapIoError::InvalidMode(_))
    ));
    assert!(matches!(
        ro.chunks_mut(0)
            .for_each_mut_legacy(|_, _| Ok::<(), std::io::Error>(())),
        Err(MmapIoError::InvalidMode(_))
    ));
    // On a writable mapping chunk size 0 is still a no-op.
    let rw = MemoryMappedFile::create_rw(dir.path().join("rw.bin"), 64).expect("rw");
    rw.chunks_mut(0)
        .for_each_mut(|_, _| panic!("no chunks"))
        .expect("zero chunk size");
}

/// `create_rw` must not leave a created (sparse, possibly huge) file
/// behind when sizing or mapping fails.
#[test]
fn create_rw_failure_leaves_no_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("huge.bin");
    // 100 TiB: rejected by `set_len` on most filesystems (ext4's limit
    // is 16 TiB) or by the mapping; either way an error must not leave
    // the file around. Succeeding is fine too (sparse file).
    match MemoryMappedFile::create_rw(&path, 100 << 40) {
        Ok(m) => drop(m),
        Err(_) => assert!(!path.exists(), "stray file left behind"),
    }
}

#[test]
fn mapped_slice_mut_debug_matches_mapped_slice() {
    let dir = tempfile::tempdir().expect("tempdir");
    let m = MemoryMappedFile::create_rw(dir.path().join("d.bin"), 16).expect("create");
    m.update_region(0, &[1, 2, 3]).expect("write");
    let expected = format!("{:?}", m.as_slice(0, 3).expect("slice"));
    let w = m.as_slice_mut(0, 3).expect("slice_mut");
    assert_eq!(format!("{w:?}"), expected);
    assert_eq!(expected, "[1, 2, 3]");
}
