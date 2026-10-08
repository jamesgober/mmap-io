//! `pending_bytes` accounting on every write path, and the automatic
//! flushes of each `FlushPolicy`.

use mmap_io::flush::FlushPolicy;
use mmap_io::{MemoryMappedFile, MmapIoError, MmapMode};

use crate::common::{tmp_path, wait_for_background_flush, TmpPath};

fn with_policy(policy: FlushPolicy, size: u64) -> (TmpPath, MemoryMappedFile) {
    let path = tmp_path("policy.bin");
    let m = MemoryMappedFile::builder(&path)
        .mode(MmapMode::ReadWrite)
        .size(size)
        .flush_policy(policy)
        .create()
        .unwrap();
    (path, m)
}

#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
#[test]
fn every_write_path_adds_to_pending_bytes() {
    let (_p, m) = with_policy(FlushPolicy::Manual, 64 * 1024);
    assert_eq!(m.pending_bytes(), 0);

    m.update_region(0, &[1; 10]).unwrap();
    assert_eq!(m.pending_bytes(), 10);
    // Empty and rejected writes count nothing.
    m.update_region(0, &[]).unwrap();
    m.update_region(u64::MAX, &[]).unwrap();
    assert!(m.update_region(64 * 1024, &[1]).is_err());
    assert_eq!(m.pending_bytes(), 10);

    // A mutable slice counts its full length on drop, written or not.
    let s = m.as_slice_mut(100, 50).unwrap();
    assert_eq!(m.pending_bytes(), 10, "counted before drop");
    drop(s);
    assert_eq!(m.pending_bytes(), 60);
    drop(m.as_slice_mut(u64::MAX, 0).unwrap());
    assert_eq!(m.pending_bytes(), 60);
    assert!(m.as_slice_mut(64 * 1024, 1).is_err());
    assert_eq!(m.pending_bytes(), 60);

    // Segment writes go through the same paths.
    let parent = std::sync::Arc::new(m.clone());
    let seg = mmap_io::segment::SegmentMut::new(parent, 1000, 30).unwrap();
    seg.write(&[2; 5]).unwrap();
    assert_eq!(m.pending_bytes(), 65);
    drop(seg.as_slice_mut().unwrap());
    assert_eq!(m.pending_bytes(), 95);

    // The raw mutable pointer counts the whole mapping.
    // SAFETY: the pointer is not dereferenced.
    let _ = unsafe { m.as_mut_ptr() }.unwrap();
    assert_eq!(m.pending_bytes(), 95 + 64 * 1024);

    m.flush().unwrap();
    assert_eq!(m.pending_bytes(), 0);
}

#[cfg(feature = "iterator")]
#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
#[test]
fn chunks_mut_counts_the_bytes_it_handed_out() {
    let (_p, m) = with_policy(FlushPolicy::Manual, 10_000);
    m.chunks_mut(3000).for_each_mut(|_, _| Ok(())).unwrap();
    assert_eq!(m.pending_bytes(), 10_000);
    m.flush().unwrap();
    // Stopping at the second chunk counts the two chunks handed out.
    let mut calls = 0;
    let r = m.chunks_mut(3000).for_each_mut(|_, _| {
        calls += 1;
        if calls == 2 {
            Err(MmapIoError::FlushFailed("stop".into()))
        } else {
            Ok(())
        }
    });
    assert!(matches!(r, Err(MmapIoError::FlushFailed(ref s)) if s == "stop"));
    assert_eq!(m.pending_bytes(), 6000);
    m.flush().unwrap();
    m.chunks_mut(0).for_each_mut(|_, _| Ok(())).unwrap();
    assert_eq!(m.pending_bytes(), 0);
}

#[cfg(feature = "atomic")]
#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
#[test]
fn atomic_views_count_their_size_on_drop() {
    let (_p, m) = with_policy(FlushPolicy::Manual, 4096);
    drop(m.atomic_u64(0).unwrap());
    assert_eq!(m.pending_bytes(), 8);
    drop(m.atomic_u32(8).unwrap());
    assert_eq!(m.pending_bytes(), 12);
    drop(m.atomic_u64_slice(16, 10).unwrap());
    assert_eq!(m.pending_bytes(), 92);
    drop(m.atomic_u32_slice(4096, 0).unwrap());
    assert_eq!(m.pending_bytes(), 92);
    assert!(m.atomic_u64(4096).is_err());
    assert_eq!(m.pending_bytes(), 92);
}

#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
#[test]
fn flush_range_resets_pending_only_for_the_whole_mapping() {
    let (_p, m) = with_policy(FlushPolicy::Manual, 8192);
    m.update_region(0, &[1; 100]).unwrap();
    m.flush_range(0, 100).unwrap();
    assert_eq!(m.pending_bytes(), 100, "partial range leaves the counter");
    m.flush_range(1, 8191).unwrap();
    assert_eq!(m.pending_bytes(), 100);
    m.flush_range(0, 8191).unwrap();
    assert_eq!(m.pending_bytes(), 100);
    m.flush_range(8192, 0).unwrap();
    assert_eq!(m.pending_bytes(), 100);
    m.flush_range(0, 8192).unwrap();
    assert_eq!(m.pending_bytes(), 0, "whole range resets");
    // A whole-range flush with nothing pending is fine too.
    m.flush_range(0, 8192).unwrap();
    m.flush().unwrap();
}

#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
#[test]
fn read_only_mappings_never_have_pending_bytes() {
    let path = tmp_path("ro.bin");
    drop(MemoryMappedFile::create_rw(&path, 4096).unwrap());
    let ro = MemoryMappedFile::open_ro(&path).unwrap();
    assert_eq!(ro.pending_bytes(), 0);
    assert!(ro.update_region(0, b"x").is_err());
    assert!(ro.as_slice_mut(0, 1).is_err());
    // SAFETY: the pointer is not used.
    assert!(matches!(
        unsafe { ro.as_mut_ptr() },
        Err(MmapIoError::InvalidMode(_))
    ));
    ro.flush().unwrap();
    assert_eq!(ro.pending_bytes(), 0);
}

#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
#[test]
fn manual_and_never_policies_do_not_flush_on_their_own() {
    for policy in [
        FlushPolicy::Never,
        FlushPolicy::Manual,
        FlushPolicy::EveryMillis(0),
    ] {
        let (_p, m) = with_policy(policy, 4096);
        for i in 0..50u64 {
            m.update_region(i, &[1]).unwrap();
        }
        assert_eq!(m.pending_bytes(), 50, "{policy:?}");
    }
}

#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
#[test]
fn always_flushes_after_every_update_region() {
    let (_p, m) = with_policy(FlushPolicy::Always, 4096);
    for i in 0..10u64 {
        m.update_region(i * 10, &[1; 10]).unwrap();
        assert_eq!(m.pending_bytes(), 0);
    }
    // Other write paths are counted but not flushed until the next
    // update_region.
    drop(m.as_slice_mut(0, 100).unwrap());
    assert_eq!(m.pending_bytes(), 100);
    m.update_region(0, &[2]).unwrap();
    assert_eq!(m.pending_bytes(), 0);
    // An empty write does not evaluate the policy.
    drop(m.as_slice_mut(0, 7).unwrap());
    m.update_region(0, &[]).unwrap();
    assert_eq!(m.pending_bytes(), 7);
}

#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
#[test]
fn every_bytes_flushes_when_the_threshold_is_reached() {
    // (threshold, write sizes, pending after each write)
    let cases: &[(usize, &[usize], &[u64])] = &[
        (10, &[3, 3, 3, 1], &[3, 6, 9, 0]),
        (10, &[10, 10], &[0, 0]),
        (10, &[11, 1], &[0, 1]),
        (10, &[9, 2, 9], &[9, 0, 9]),
        (1, &[1, 5, 1], &[0, 0, 0]),
        // Zero disables the threshold.
        (0, &[1, 100, 1000], &[1, 101, 1101]),
        (usize::MAX, &[4096], &[4096]),
    ];
    for &(n, writes, pending) in cases {
        let (_p, m) = with_policy(FlushPolicy::EveryBytes(n), 64 * 1024);
        let mut off = 0u64;
        for (w, want) in writes.iter().zip(pending) {
            m.update_region(off, &vec![7u8; *w]).unwrap();
            off += *w as u64;
            assert_eq!(m.pending_bytes(), *want, "EveryBytes({n}) {writes:?}");
        }
    }
}

#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
#[test]
fn every_bytes_counts_bytes_from_other_paths_at_the_next_update() {
    let (_p, m) = with_policy(FlushPolicy::EveryBytes(100), 4096);
    drop(m.as_slice_mut(0, 99).unwrap());
    assert_eq!(m.pending_bytes(), 99);
    m.update_region(0, &[1]).unwrap();
    assert_eq!(m.pending_bytes(), 0);
}

#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
#[test]
fn every_writes_flushes_on_the_nth_update_region() {
    for (w, writes, expect_zero_at) in [
        (1usize, 5usize, vec![0, 1, 2, 3, 4]),
        (3, 7, vec![2, 5]),
        (4, 4, vec![3]),
        (0, 5, vec![]),
        (usize::MAX, 5, vec![]),
    ] {
        let (_p, m) = with_policy(FlushPolicy::EveryWrites(w), 4096);
        for i in 0..writes {
            m.update_region(i as u64, &[1]).unwrap();
            let zero = m.pending_bytes() == 0;
            assert_eq!(
                zero,
                expect_zero_at.contains(&i),
                "EveryWrites({w}) after write {i}"
            );
        }
    }
}

#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
#[test]
fn every_writes_does_not_count_empty_or_rejected_writes() {
    let (_p, m) = with_policy(FlushPolicy::EveryWrites(2), 4096);
    m.update_region(0, &[1]).unwrap();
    m.update_region(0, &[]).unwrap();
    assert!(m.update_region(4096, &[1]).is_err());
    assert_eq!(m.pending_bytes(), 1, "only one counted write so far");
    m.update_region(1, &[1]).unwrap();
    assert_eq!(m.pending_bytes(), 0);
}

#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
#[test]
fn every_millis_flushes_in_the_background() {
    let (path, m) = with_policy(FlushPolicy::EveryMillis(5), 8192);
    for round in 0..3u8 {
        m.update_region(0, &[round; 64]).unwrap();
        wait_for_background_flush(&m);
        assert_eq!(&crate::common::read_file(&path)[..64], &[round; 64]);
    }
    // Writes from every path are picked up, not only update_region.
    drop(m.as_slice_mut(100, 10).unwrap());
    wait_for_background_flush(&m);
}

#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
#[test]
fn every_millis_flusher_stops_when_the_last_clone_drops() {
    let (_p, m) = with_policy(FlushPolicy::EveryMillis(1), 4096);
    let clones: Vec<_> = (0..8).map(|_| m.clone()).collect();
    for (i, c) in clones.iter().enumerate() {
        c.update_region(i as u64, &[1]).unwrap();
    }
    drop(clones);
    m.update_region(100, &[1]).unwrap();
    wait_for_background_flush(&m);
    let start = std::time::Instant::now();
    drop(m);
    assert!(
        start.elapsed() < std::time::Duration::from_secs(5),
        "drop waited too long for the flusher"
    );
}
