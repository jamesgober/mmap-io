//! Edge-case range validation for every API that hands an
//! `(offset, len)` pair to a kernel syscall or to the raw mapping layer.
//!
//! RUSTSEC-2026-0186 reported that `memmap2` before 0.9.11 did not
//! validate `offset` / `len` in `flush_range` and `advise_range`,
//! producing out-of-bounds pointers that were passed to `msync` /
//! `madvise`. Since 1.1.0 mmap-io maps memory through its own
//! `mmap_io::raw` layer, which checks every range before pointer math,
//! and the managed API validates every range again under the mapping
//! lock. These tests pin the managed-API side so a future refactor
//! cannot drop it and silently rely on the layer below.
//!
//! Every hostile range below must return `MmapIoError::OutOfBounds`
//! (never panic, never succeed), and every boundary-exact range must
//! succeed.

use mmap_io::{errors::MmapIoError, MemoryMappedFile};

mod common;
use common::tmp_path;

const SIZE: u64 = 64 * 1024;

/// Ranges that must be rejected for a mapping of `total` bytes.
fn hostile_ranges(total: u64) -> Vec<(u64, u64)> {
    vec![
        // One byte past the end.
        (0, total + 1),
        (total, 1),
        (total - 1, 2),
        // Offset entirely past the end.
        (total + 1, 1),
        (total * 2, 4096),
        // offset + len overflows u64; a wrapping add would yield a
        // small, "in-bounds" end and is exactly the class of bug the
        // advisory describes.
        (u64::MAX, 1),
        (1, u64::MAX),
        (u64::MAX, u64::MAX),
        (total - 1, u64::MAX),
        (u64::MAX - 1, 2),
        // Values that truncate to small numbers when cast to usize on
        // 32-bit targets (and to i64-negative values in libc on 64-bit).
        (1u64 << 32, 1),
        (0, (1u64 << 32) + 1),
        (i64::MAX as u64, 1),
        ((i64::MAX as u64) + 1, 1),
    ]
}

/// Ranges that sit exactly on the mapping boundary and must succeed.
fn boundary_ranges(total: u64) -> Vec<(u64, u64)> {
    vec![(0, total), (total - 1, 1), (0, 1), (total / 2, total / 2)]
}

fn assert_oob(res: Result<(), MmapIoError>, op: &str, offset: u64, len: u64) {
    match res {
        Err(MmapIoError::OutOfBounds { .. }) => {}
        other => panic!("{op}({offset}, {len}) expected OutOfBounds, got {other:?}"),
    }
}

fn create_dirty(name: &str) -> (common::TmpPath, MemoryMappedFile) {
    let path = tmp_path(name);
    let _ = std::fs::remove_file(&path);
    let mmap = MemoryMappedFile::create_rw(&path, SIZE).expect("create_rw");
    // Dirty the mapping so flush_range cannot short-circuit on an
    // empty write accumulator before reaching its bounds check.
    mmap.update_region(0, &[0xAB; 128]).expect("update_region");
    (path, mmap)
}

#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
#[test]
fn flush_range_rejects_hostile_ranges() {
    let (path, mmap) = create_dirty("flush_hostile");
    for (offset, len) in hostile_ranges(SIZE) {
        assert_oob(mmap.flush_range(offset, len), "flush_range", offset, len);
    }
    drop(mmap);
    let _ = std::fs::remove_file(&path);
}

#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
#[test]
fn flush_range_accepts_boundary_ranges() {
    let (path, mmap) = create_dirty("flush_boundary");
    for (offset, len) in boundary_ranges(SIZE) {
        mmap.update_region(offset, &[0xCD]).expect("re-dirty");
        mmap.flush_range(offset, len)
            .unwrap_or_else(|e| panic!("flush_range({offset}, {len}) failed: {e:?}"));
    }
    drop(mmap);
    let _ = std::fs::remove_file(&path);
}

#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
#[test]
fn flush_range_zero_length_is_noop_everywhere() {
    // Zero-length requests are documented as always accepted. They must
    // never reach the syscall, even with a nonsensical offset.
    let (path, mmap) = create_dirty("flush_zero");
    for offset in [0, SIZE, SIZE + 1, u64::MAX] {
        mmap.flush_range(offset, 0)
            .unwrap_or_else(|e| panic!("flush_range({offset}, 0) failed: {e:?}"));
    }
    drop(mmap);
    let _ = std::fs::remove_file(&path);
}

#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
#[test]
fn flush_range_after_shrink_rejects_stale_range() {
    // A range that was valid before `resize` shrank the file must be
    // rejected afterwards; the check has to use the current length,
    // not the length at open time.
    let (path, mmap) = create_dirty("flush_shrink");
    let half = SIZE / 2;
    mmap.resize(half).expect("shrink");
    mmap.update_region(0, &[1]).expect("re-dirty");
    assert_oob(mmap.flush_range(half, 1), "flush_range", half, 1);
    assert_oob(mmap.flush_range(0, SIZE), "flush_range", 0, SIZE);
    mmap.flush_range(0, half).expect("in-bounds after shrink");
    drop(mmap);
    let _ = std::fs::remove_file(&path);
}

#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
#[test]
fn flush_range_on_read_only_and_cow_still_validates() {
    let (path, mmap) = create_dirty("flush_ro");
    mmap.flush().expect("flush");
    drop(mmap);

    let ro = MemoryMappedFile::open_ro(&path).expect("open_ro");
    for (offset, len) in hostile_ranges(SIZE) {
        assert_oob(ro.flush_range(offset, len), "ro.flush_range", offset, len);
    }
    drop(ro);

    #[cfg(feature = "cow")]
    {
        let cow = MemoryMappedFile::open_cow(&path).expect("open_cow");
        for (offset, len) in hostile_ranges(SIZE) {
            assert_oob(cow.flush_range(offset, len), "cow.flush_range", offset, len);
        }
    }
    let _ = std::fs::remove_file(&path);
}

#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
#[test]
fn touch_pages_range_rejects_hostile_ranges() {
    let (path, mmap) = create_dirty("touch_hostile");
    for (offset, len) in hostile_ranges(SIZE) {
        assert_oob(
            mmap.touch_pages_range(offset, len),
            "touch_pages_range",
            offset,
            len,
        );
    }
    for (offset, len) in boundary_ranges(SIZE) {
        mmap.touch_pages_range(offset, len)
            .unwrap_or_else(|e| panic!("touch_pages_range({offset}, {len}) failed: {e:?}"));
    }
    drop(mmap);
    let _ = std::fs::remove_file(&path);
}

#[cfg(feature = "advise")]
#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
#[test]
fn advise_rejects_hostile_ranges_for_every_advice() {
    use mmap_io::advise::MmapAdvice;

    let (path, mmap) = create_dirty("advise_hostile");
    let advices = [
        MmapAdvice::Normal,
        MmapAdvice::Random,
        MmapAdvice::Sequential,
        MmapAdvice::WillNeed,
        MmapAdvice::DontNeed,
    ];
    for advice in advices {
        for (offset, len) in hostile_ranges(SIZE) {
            assert_oob(mmap.advise(offset, len, advice), "advise", offset, len);
        }
    }
    // DontNeed is excluded from the success sweep: on Linux it drops
    // the page contents of a private mapping and is not a pure hint.
    for advice in [
        MmapAdvice::Normal,
        MmapAdvice::Sequential,
        MmapAdvice::WillNeed,
    ] {
        for (offset, len) in boundary_ranges(SIZE) {
            mmap.advise(offset, len, advice)
                .unwrap_or_else(|e| panic!("advise({offset}, {len}) failed: {e:?}"));
        }
    }
    drop(mmap);
    let _ = std::fs::remove_file(&path);
}

#[cfg(feature = "locking")]
#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
#[test]
fn lock_and_unlock_reject_hostile_ranges() {
    let (path, mmap) = create_dirty("lock_hostile");
    for (offset, len) in hostile_ranges(SIZE) {
        assert_oob(mmap.lock(offset, len), "lock", offset, len);
        assert_oob(mmap.unlock(offset, len), "unlock", offset, len);
    }
    drop(mmap);
    let _ = std::fs::remove_file(&path);
}

#[cfg(any(target_os = "linux", target_os = "android"))]
#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
#[test]
fn prefetch_range_rejects_hostile_ranges() {
    let (path, mmap) = create_dirty("prefetch_hostile");
    for (offset, len) in hostile_ranges(SIZE) {
        assert_oob(
            mmap.prefetch_range(offset, len),
            "prefetch_range",
            offset,
            len,
        );
    }
    drop(mmap);
    let _ = std::fs::remove_file(&path);
}

#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
#[test]
fn read_and_write_paths_reject_hostile_ranges() {
    let (path, mmap) = create_dirty("rw_hostile");
    for (offset, len) in hostile_ranges(SIZE) {
        // Only allocate a buffer when the length is small enough to be
        // realistic; the offset alone must trigger rejection otherwise.
        if len <= SIZE + 1 {
            let mut buf = vec![0u8; len as usize];
            assert_oob(mmap.read_into(offset, &mut buf), "read_into", offset, len);
            assert_oob(
                mmap.update_region(offset, &buf),
                "update_region",
                offset,
                len,
            );
        }
        assert_oob(
            mmap.as_slice_mut(offset, len).map(|_| ()),
            "as_slice_mut",
            offset,
            len,
        );
    }
    drop(mmap);
    let _ = std::fs::remove_file(&path);
}
