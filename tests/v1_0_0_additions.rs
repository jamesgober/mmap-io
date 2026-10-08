//! Tests for 1.0.0 additive surface: AnonymousMmap (F1) and
//! is_hugepage_backed (F4).

use mmap_io::{AnonymousMmap, MemoryMappedFile, MmapIoError};

// ---------------------------------------------------------------------
// F1: AnonymousMmap
// ---------------------------------------------------------------------

#[test]
fn anonymous_new_rejects_zero_size() {
    let err = AnonymousMmap::new(0).unwrap_err();
    assert!(matches!(err, MmapIoError::ResizeFailed(_)));
}

#[test]
fn anonymous_new_rejects_oversized() {
    let oversized = if cfg!(target_pointer_width = "64") {
        // 256 TB > 128 TB cap on 64-bit
        256u64 * (1 << 40)
    } else {
        // 4 GB > 2 GB cap on 32-bit
        4u64 * (1 << 30)
    };
    let err = AnonymousMmap::new(oversized).unwrap_err();
    assert!(matches!(err, MmapIoError::ResizeFailed(_)));
}

#[test]
fn anonymous_new_succeeds_and_reports_len() {
    let mmap = AnonymousMmap::new(4096).expect("4 KiB anon");
    assert_eq!(mmap.len(), 4096);
    assert!(!mmap.is_empty());
}

#[test]
fn anonymous_pages_are_zero_initialized() {
    let mmap = AnonymousMmap::new(8192).expect("anon");
    let mut buf = [0xFFu8; 64];
    mmap.read_into(0, &mut buf).expect("read");
    assert!(
        buf.iter().all(|&b| b == 0),
        "anonymous pages must be zero-initialized: {buf:?}"
    );
}

#[test]
fn anonymous_update_region_roundtrip() {
    let mmap = AnonymousMmap::new(4096).expect("anon");
    let data = b"hello, anonymous world";
    mmap.update_region(100, data).expect("write");
    let mut buf = vec![0u8; data.len()];
    mmap.read_into(100, &mut buf).expect("read");
    assert_eq!(&buf, data);
}

#[test]
fn anonymous_read_into_oob_errors() {
    let mmap = AnonymousMmap::new(64).expect("anon");
    let mut buf = [0u8; 32];
    let err = mmap.read_into(50, &mut buf).unwrap_err();
    assert!(matches!(err, MmapIoError::OutOfBounds { .. }));
}

#[test]
fn anonymous_update_region_oob_errors() {
    let mmap = AnonymousMmap::new(64).expect("anon");
    let err = mmap.update_region(50, &[0u8; 32]).unwrap_err();
    assert!(matches!(err, MmapIoError::OutOfBounds { .. }));
}

#[test]
fn anonymous_offset_at_end_with_zero_len_ok() {
    let mmap = AnonymousMmap::new(64).expect("anon");
    let mut empty: [u8; 0] = [];
    mmap.read_into(64, &mut empty)
        .expect("zero-len at boundary is allowed");
}

#[test]
fn anonymous_offset_past_end_errors() {
    let mmap = AnonymousMmap::new(64).expect("anon");
    // Since 1.1.0 a zero-length request is accepted at any offset (the
    // crate-wide rule); a non-empty one past the end still errors.
    mmap.read_into(65, &mut [0u8; 0])
        .expect("zero-length past the end is a no-op");
    let err = mmap.read_into(65, &mut [0u8; 1]).unwrap_err();
    assert!(matches!(err, MmapIoError::OutOfBounds { .. }));
}

#[test]
fn anonymous_as_slice_reads_match_writes() {
    let mmap = AnonymousMmap::new(4096).expect("anon");
    mmap.update_region(0, b"abcdef").expect("write");
    let slice = mmap.as_slice(0, 6).expect("as_slice");
    assert_eq!(&*slice, b"abcdef");
}

#[test]
fn anonymous_as_mut_slice_writes_are_visible() {
    let mmap = AnonymousMmap::new(4096).expect("anon");
    {
        let mut s = mmap.as_mut_slice(10, 4).expect("as_mut_slice");
        s.copy_from_slice(b"YEAH");
    }
    let mut buf = [0u8; 4];
    mmap.read_into(10, &mut buf).expect("read");
    assert_eq!(&buf, b"YEAH");
}

#[test]
fn anonymous_concurrent_readers_do_not_block() {
    use std::sync::Arc;
    use std::thread;

    let mmap = Arc::new(AnonymousMmap::new(4096).expect("anon"));
    mmap.update_region(0, b"shared").expect("write");

    let handles: Vec<_> = (0..8)
        .map(|_| {
            let m = Arc::clone(&mmap);
            thread::spawn(move || {
                let mut buf = [0u8; 6];
                m.read_into(0, &mut buf).expect("read");
                assert_eq!(&buf, b"shared");
            })
        })
        .collect();
    for h in handles {
        h.join().expect("join");
    }
}

#[test]
fn anonymous_drop_releases_without_panic() {
    // Smoke test: drop a sizeable anon mapping and verify it doesn't
    // panic, double-free, or otherwise misbehave. A real leak check
    // would need valgrind/heaptrack; here we just exercise the path.
    for _ in 0..4 {
        let m = AnonymousMmap::new(64 * 1024).expect("anon");
        drop(m);
    }
}

#[test]
fn anonymous_debug_renders_len() {
    let mmap = AnonymousMmap::new(4096).expect("anon");
    let dbg = format!("{mmap:?}");
    assert!(dbg.contains("4096"), "expected len in debug output: {dbg}");
    assert!(dbg.contains("AnonymousMmap"));
}

#[test]
fn anonymous_raw_pointer_reads_match_slice() {
    let mmap = AnonymousMmap::new(4096).expect("anon");
    mmap.update_region(0, b"ABCD").expect("write");
    // SAFETY: pointer is used only while `mmap` is still in scope.
    // No concurrent mutable borrow exists; the pointer is dropped at
    // the end of this test.
    unsafe {
        let p = mmap.as_ptr();
        assert_eq!(*p, b'A');
        assert_eq!(*p.add(3), b'D');
    }
}

// ---------------------------------------------------------------------
// F4: is_hugepage_backed
// ---------------------------------------------------------------------

#[test]
fn is_hugepage_backed_returns_some_on_linux_none_elsewhere() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("h.bin");
    let mmap = MemoryMappedFile::create_rw(&path, 8192).expect("create_rw");

    let result = mmap.is_hugepage_backed();

    #[cfg(target_os = "linux")]
    {
        // Without explicit MAP_HUGETLB request the kernel will not back
        // a tiny file mapping with huge pages, so we expect Some(false)
        // or None (if /proc parse failed). Anything but Some(true).
        assert!(
            matches!(result, Some(false) | None),
            "expected Some(false) or None on Linux for regular mmap, got {result:?}"
        );
    }

    #[cfg(not(target_os = "linux"))]
    {
        assert_eq!(
            result, None,
            "is_hugepage_backed must return None on non-Linux platforms"
        );
    }
}

#[test]
fn is_hugepage_backed_works_for_each_mode() {
    let dir = tempfile::tempdir().expect("tempdir");
    let ro_path = dir.path().join("ro.bin");
    let rw_path = dir.path().join("rw.bin");

    // Create both files first so open_ro has something to open.
    {
        let m = MemoryMappedFile::create_rw(&ro_path, 4096).expect("create ro source");
        m.update_region(0, b"x").expect("seed");
        m.flush().expect("flush");
    }
    {
        let _ = MemoryMappedFile::create_rw(&rw_path, 4096).expect("create rw");
    }

    let ro = MemoryMappedFile::open_ro(&ro_path).expect("open_ro");
    let rw = MemoryMappedFile::open_rw(&rw_path).expect("open_rw");

    // Just verify the call does not panic on any of the variants;
    // the value is platform-dependent and asserted in the test above.
    let _ = ro.is_hugepage_backed();
    let _ = rw.is_hugepage_backed();
}
