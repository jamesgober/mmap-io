//! `resize`: every grow/shrink pair across the page and granularity
//! boundaries, invalid sizes, mode errors, and what survives.

use std::fs;

use mmap_io::{MemoryMappedFile, MmapIoError, MmapMode};

use crate::common::{boundary_sizes, pattern, read_file, tmp_path};

const MAX_SIZE: u64 = if cfg!(target_pointer_width = "64") {
    128 << 40
} else {
    2 << 30
};

#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
#[test]
fn every_grow_and_shrink_pair_preserves_the_common_prefix() {
    let sizes = boundary_sizes();
    for &from in &sizes {
        for &to in &sizes {
            let path = tmp_path("pair.bin");
            let m = MemoryMappedFile::create_rw(&path, from).unwrap();
            let data = pattern(from as usize, (from % 251) as u8);
            m.update_region(0, &data).unwrap();
            m.resize(to).unwrap_or_else(|e| panic!("{from}->{to}: {e}"));

            let ctx = format!("{from}->{to}");
            assert_eq!(m.len(), to, "{ctx}");
            assert_eq!(fs::metadata(&path).unwrap().len(), to, "{ctx}: file length");
            let keep = from.min(to) as usize;
            let mut expect = data[..keep].to_vec();
            expect.resize(to as usize, 0);
            assert_eq!(m.as_slice(0, to).unwrap(), &expect[..], "{ctx}");
            assert!(
                matches!(
                    m.as_slice(to, 1),
                    Err(MmapIoError::OutOfBounds { offset, len: 1, total })
                        if offset == to && total == to
                ),
                "{ctx}: one past the new end"
            );
            m.update_region(to - 1, &[0xEE]).unwrap();
            expect[to as usize - 1] = 0xEE;
            m.flush().unwrap();
            drop(m);
            assert_eq!(read_file(&path), expect, "{ctx}: on disk");
        }
    }
}

#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
#[test]
fn shrinking_discards_the_tail_and_regrowing_exposes_zeros() {
    let path = tmp_path("regrow.bin");
    let size = 3 * 4096 + 100;
    let m = MemoryMappedFile::create_rw(&path, size).unwrap();
    m.update_region(0, &pattern(size as usize, 1)).unwrap();
    m.resize(10).unwrap();
    m.resize(size).unwrap();
    assert_eq!(m.as_slice(0, 10).unwrap(), &pattern(10, 1)[..]);
    let tail = m.as_slice(10, size - 10).unwrap();
    assert!(tail.iter().all(|&b| b == 0), "regrown tail is not zero");
}

#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
#[test]
fn resizing_to_the_same_size_is_a_no_op() {
    let path = tmp_path("same.bin");
    let m = MemoryMappedFile::create_rw(&path, 5000).unwrap();
    m.update_region(0, b"same").unwrap();
    let pending = m.pending_bytes();
    for _ in 0..3 {
        m.resize(5000).unwrap();
    }
    assert_eq!(m.len(), 5000);
    assert_eq!(m.as_slice(0, 4).unwrap(), b"same");
    assert_eq!(m.pending_bytes(), pending);
}

#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
#[test]
fn invalid_sizes_leave_the_mapping_untouched() {
    let path = tmp_path("bad.bin");
    let m = MemoryMappedFile::create_rw(&path, 4096).unwrap();
    m.update_region(0, b"intact").unwrap();
    for bad in [0, MAX_SIZE + 1, u64::MAX] {
        assert!(
            matches!(m.resize(bad), Err(MmapIoError::ResizeFailed(_))),
            "resize({bad})"
        );
        assert_eq!(m.len(), 4096);
        assert_eq!(fs::metadata(&path).unwrap().len(), 4096);
        assert_eq!(m.as_slice(0, 6).unwrap(), b"intact");
    }
}

#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
#[test]
fn read_only_mappings_cannot_be_resized() {
    let path = tmp_path("ro.bin");
    drop(MemoryMappedFile::create_rw(&path, 4096).unwrap());
    let ro = MemoryMappedFile::open_ro(&path).unwrap();
    for size in [0, 1, 4096, 8192, u64::MAX] {
        assert!(
            matches!(ro.resize(size), Err(MmapIoError::InvalidMode(_))),
            "resize({size}) on RO"
        );
    }
    assert_eq!(ro.len(), 4096);
    assert_eq!(fs::metadata(&path).unwrap().len(), 4096);
    assert_eq!(ro.mode(), MmapMode::ReadOnly);
}

#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
#[test]
fn many_alternating_resizes_keep_len_and_file_in_step() {
    let path = tmp_path("churn.bin");
    let m = MemoryMappedFile::create_rw(&path, 1).unwrap();
    let mut size = 1u64;
    for i in 0..200u64 {
        // Deterministic walk over sizes from 1 byte to ~1 MiB.
        size = size
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407 + i)
            % (1 << 20)
            + 1;
        m.resize(size).unwrap();
        assert_eq!(m.len(), size);
        m.update_region(size - 1, &[i as u8]).unwrap();
        assert_eq!(m.as_slice(size - 1, 1).unwrap(), &[i as u8]);
    }
    m.flush().unwrap();
    assert_eq!(fs::metadata(&path).unwrap().len(), size);
}

/// A shrink to below a page keeps the first byte addressable, and the
/// segment, iterator, and reader views built afterwards see the new
/// length.
#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
#[test]
fn views_created_after_a_resize_see_the_new_length() {
    let path = tmp_path("views.bin");
    let m = MemoryMappedFile::create_rw(&path, 3 * 4096).unwrap();
    m.resize(1).unwrap();
    assert_eq!(m.as_slice(0, 1).unwrap().len(), 1);
    let mut out = Vec::new();
    std::io::Read::read_to_end(&mut m.reader(), &mut out).unwrap();
    assert_eq!(out, [0]);
    #[cfg(feature = "iterator")]
    assert_eq!(m.chunks(4096).map(|c| c.len()).collect::<Vec<_>>(), [1]);
    m.resize(4097).unwrap();
    #[cfg(feature = "iterator")]
    assert_eq!(
        m.pages().len(),
        (4097_usize).div_ceil(mmap_io::utils::page_size())
    );
}

/// Windows refuses to truncate a file that another view maps. The
/// documented behavior is an `Io` error with the mapping restored at
/// its old size and contents.
#[cfg(windows)]
#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
#[test]
fn shrink_with_a_second_mapping_open_fails_and_restores_on_windows() {
    let path = tmp_path("second.bin");
    let m = MemoryMappedFile::create_rw(&path, 8192).unwrap();
    m.update_region(0, &pattern(8192, 8)).unwrap();
    let other = MemoryMappedFile::open_ro(&path).unwrap();
    assert!(matches!(m.resize(4096), Err(MmapIoError::Io(_))));
    assert_eq!(m.len(), 8192);
    assert_eq!(m.as_slice(0, 8192).unwrap(), &pattern(8192, 8)[..]);
    assert_eq!(other.as_slice(0, 8192).unwrap(), &pattern(8192, 8)[..]);
    drop(other);
    m.resize(4096).unwrap();
    assert_eq!(m.len(), 4096);
}

#[cfg(target_pointer_width = "32")]
#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
#[test]
fn sizes_above_two_gib_are_rejected_on_32_bit() {
    let path = tmp_path("big32.bin");
    let m = MemoryMappedFile::create_rw(&path, 4096).unwrap();
    for size in [
        (2u64 << 30) + 1,
        u64::from(u32::MAX),
        u64::from(u32::MAX) + 1,
    ] {
        assert!(matches!(m.resize(size), Err(MmapIoError::ResizeFailed(_))));
        assert!(matches!(
            MemoryMappedFile::create_rw(path.sibling("x"), size),
            Err(MmapIoError::ResizeFailed(_))
        ));
        assert!(matches!(
            mmap_io::AnonymousMmap::new(size),
            Err(MmapIoError::ResizeFailed(_))
        ));
    }
}
