//! The crate-wide range rule, checked for every `(offset, len)` API
//! against every mapping mode, at sizes around the page and
//! allocation-granularity boundaries, and again after growing and
//! shrinking `resize` calls:
//!
//! - a zero-length request is accepted at any offset and does nothing;
//! - otherwise `offset + len <= len()` (without overflow) or the call
//!   fails with `OutOfBounds { offset, len, total }`, exact fields.

use mmap_io::segment::{Segment, SegmentMut};
use mmap_io::{MemoryMappedFile, MmapIoError, MmapMode};
use std::sync::Arc;

use crate::common::{boundary_sizes, granularity, page, pattern, tmp_path, TmpPath};

/// Offsets worth probing for a mapping of `size` bytes.
fn offsets(size: u64) -> Vec<u64> {
    let p = page();
    let mut v = vec![
        0,
        1,
        size.saturating_sub(1),
        size,
        size + 1,
        p - 1,
        p,
        p + 1,
        granularity(),
        u64::from(u32::MAX),
        u64::from(u32::MAX) + 1,
        i64::MAX as u64,
        i64::MAX as u64 + 1,
        u64::MAX - 1,
        u64::MAX,
    ];
    v.sort_unstable();
    v.dedup();
    v
}

/// Lengths worth probing at `offset` for a mapping of `size` bytes.
fn lengths(size: u64, offset: u64) -> Vec<u64> {
    let mut v = vec![
        0,
        1,
        2,
        page(),
        size.saturating_sub(1),
        size,
        size + 1,
        u64::from(u32::MAX) + 1,
        u64::MAX - 1,
        u64::MAX,
    ];
    if offset <= size {
        // Exactly to the end, and one byte past it.
        v.push(size - offset);
        v.push(size - offset + 1);
    }
    v.sort_unstable();
    v.dedup();
    v
}

/// Expected outcome for a range request.
fn valid(offset: u64, len: u64, size: u64) -> bool {
    len == 0 || offset.checked_add(len).is_some_and(|end| end <= size)
}

fn expect_oob<T: std::fmt::Debug>(r: Result<T, MmapIoError>, ctx: &str, o: u64, l: u64, t: u64) {
    match r {
        Err(MmapIoError::OutOfBounds { offset, len, total }) => {
            assert_eq!((offset, len, total), (o, l, t), "{ctx}: error fields")
        }
        other => panic!("{ctx}: expected OutOfBounds, got {other:?}"),
    }
}

/// Buffers above this size are not allocated for `read_into` /
/// `update_region`; their validity is still covered by the slice APIs.
const MAX_BUF: u64 = 4 << 20;

/// Run every range API over the offset/length table and compare with
/// `model`, the expected content of the mapping.
fn check_all(m: &MemoryMappedFile, model: &[u8], label: &str) {
    let size = model.len() as u64;
    assert_eq!(m.len(), size, "{label}");
    let rw = m.mode() == MmapMode::ReadWrite;
    let parent = Arc::new(m.clone());
    for offset in offsets(size) {
        for len in lengths(size, offset) {
            let ctx = format!("{label} size={size} offset={offset} len={len}");
            let ok = valid(offset, len, size);
            let range = if ok && len > 0 {
                Some(offset as usize..(offset + len) as usize)
            } else {
                None
            };

            // as_slice
            match m.as_slice(offset, len) {
                Ok(s) => {
                    assert!(ok, "{ctx}: as_slice accepted");
                    match &range {
                        Some(r) => assert_eq!(&*s, &model[r.clone()], "{ctx}: as_slice bytes"),
                        None => assert!(s.is_empty(), "{ctx}: zero-length slice not empty"),
                    }
                }
                Err(e) => {
                    assert!(!ok, "{ctx}: as_slice rejected: {e}");
                    expect_oob(Err::<(), _>(e), &ctx, offset, len, size);
                }
            }

            // as_slice_bytes: RO/COW follow the rule, RW is refused.
            let r = m.as_slice_bytes(offset, len);
            if rw {
                assert!(
                    matches!(r, Err(MmapIoError::InvalidMode(_))),
                    "{ctx}: {r:?}"
                );
            } else if ok {
                let s = r.unwrap_or_else(|e| panic!("{ctx}: as_slice_bytes: {e}"));
                assert_eq!(s.len() as u64, len, "{ctx}");
            } else {
                expect_oob(r, &ctx, offset, len, size);
            }

            // read_into
            if len <= MAX_BUF {
                let mut buf = vec![0xA5u8; len as usize];
                let r = m.read_into(offset, &mut buf);
                if ok {
                    r.unwrap_or_else(|e| panic!("{ctx}: read_into: {e}"));
                    if let Some(rg) = &range {
                        assert_eq!(&buf[..], &model[rg.clone()], "{ctx}: read_into bytes");
                    }
                } else {
                    expect_oob(r, &ctx, offset, len, size);
                }
            }

            // Writes: put back the bytes that are already there, so the
            // model stays valid.
            if len <= MAX_BUF {
                let data = match &range {
                    Some(rg) => model[rg.clone()].to_vec(),
                    None => vec![0u8; len as usize],
                };
                let r = m.update_region(offset, &data);
                if len == 0 {
                    r.unwrap_or_else(|e| panic!("{ctx}: empty update_region: {e}"));
                } else if !rw {
                    assert!(
                        matches!(r, Err(MmapIoError::InvalidMode(_))),
                        "{ctx}: {r:?}"
                    );
                } else if ok {
                    r.unwrap_or_else(|e| panic!("{ctx}: update_region: {e}"));
                } else {
                    expect_oob(r, &ctx, offset, len, size);
                }
            }

            // as_slice_mut (RW only; mode is checked before the range).
            let r = m.as_slice_mut(offset, len);
            if !rw {
                assert!(matches!(r, Err(MmapIoError::InvalidMode(_))), "{ctx}");
            } else if ok {
                let mut s = r.unwrap_or_else(|e| panic!("{ctx}: as_slice_mut: {e}"));
                assert_eq!(s.len() as u64, len, "{ctx}");
                assert_eq!(s.is_empty(), len == 0);
                if let Some(rg) = &range {
                    assert_eq!(&s[..], &model[rg.clone()], "{ctx}: as_slice_mut bytes");
                    let copy = s.to_vec();
                    s.as_mut().copy_from_slice(&copy);
                }
            } else {
                expect_oob(r.map(|_| ()), &ctx, offset, len, size);
            }

            // flush_range (validated in every mode).
            let r = m.flush_range(offset, len);
            if ok {
                r.unwrap_or_else(|e| panic!("{ctx}: flush_range: {e}"));
            } else {
                expect_oob(r, &ctx, offset, len, size);
            }

            // touch_pages_range and prefetch_range.
            let r = m.touch_pages_range(offset, len);
            if ok {
                r.unwrap_or_else(|e| panic!("{ctx}: touch_pages_range: {e}"));
            } else {
                expect_oob(r, &ctx, offset, len, size);
            }
            let r = m.prefetch_range(offset, len);
            if ok {
                r.unwrap_or_else(|e| panic!("{ctx}: prefetch_range: {e}"));
            } else {
                expect_oob(r, &ctx, offset, len, size);
            }

            #[cfg(feature = "advise")]
            for advice in [
                mmap_io::MmapAdvice::Normal,
                mmap_io::MmapAdvice::Random,
                mmap_io::MmapAdvice::Sequential,
                mmap_io::MmapAdvice::WillNeed,
            ] {
                let r = m.advise(offset, len, advice);
                if ok {
                    r.unwrap_or_else(|e| panic!("{ctx}: advise({advice:?}): {e}"));
                } else {
                    expect_oob(r, &ctx, offset, len, size);
                }
            }

            #[cfg(feature = "locking")]
            {
                let r = m.lock(offset, len);
                if ok {
                    // Locking may need privileges; an unprivileged
                    // failure is LockFailed, never a range error.
                    match r {
                        Ok(()) => m.unlock(offset, len).unwrap(),
                        Err(MmapIoError::LockFailed(_)) => {}
                        Err(e) => panic!("{ctx}: lock: {e}"),
                    }
                } else {
                    expect_oob(r, &ctx, offset, len, size);
                    expect_oob(m.unlock(offset, len), &ctx, offset, len, size);
                }
            }

            #[cfg(feature = "bytes")]
            {
                let r = m.read_bytes(offset, len);
                if ok {
                    let b = r.unwrap_or_else(|e| panic!("{ctx}: read_bytes: {e}"));
                    assert_eq!(b.len() as u64, len);
                } else {
                    expect_oob(r, &ctx, offset, len, size);
                }
            }

            // Segments validate at construction and on every access.
            let seg = Segment::new(Arc::clone(&parent), offset, len);
            if ok {
                let seg = seg.unwrap_or_else(|e| panic!("{ctx}: Segment::new: {e}"));
                assert!(seg.is_valid());
                assert_eq!(seg.as_slice().unwrap().len() as u64, len);
            } else {
                expect_oob(seg, &ctx, offset, len, size);
            }
            let seg = SegmentMut::new(Arc::clone(&parent), offset, len);
            if ok {
                let seg = seg.unwrap_or_else(|e| panic!("{ctx}: SegmentMut::new: {e}"));
                assert!(seg.is_valid());
            } else {
                expect_oob(seg, &ctx, offset, len, size);
            }
        }
    }
    // The table only ever wrote back existing bytes.
    assert_eq!(
        m.as_slice(0, size).unwrap(),
        model,
        "{label}: content drifted"
    );
}

fn rw_with(size: u64, seed: u8) -> (TmpPath, MemoryMappedFile, Vec<u8>) {
    let path = tmp_path("ranges.bin");
    let m = MemoryMappedFile::create_rw(&path, size).unwrap();
    let model = pattern(size as usize, seed);
    m.update_region(0, &model).unwrap();
    (path, m, model)
}

#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
#[test]
fn read_write_mapping_follows_the_range_rule() {
    for size in boundary_sizes() {
        let (_path, m, model) = rw_with(size, 1);
        check_all(&m, &model, "RW");
    }
}

#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
#[test]
fn read_only_mapping_follows_the_range_rule() {
    for size in boundary_sizes() {
        let (path, m, model) = rw_with(size, 2);
        m.flush().unwrap();
        drop(m);
        let ro = MemoryMappedFile::open_ro(&path).unwrap();
        check_all(&ro, &model, "RO");
    }
}

#[cfg(feature = "cow")]
#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
#[test]
fn copy_on_write_mapping_follows_the_range_rule_for_reads() {
    // Only the read-side APIs are meaningful for COW here; the write
    // checks inside `check_all` are skipped by not calling it.
    for size in boundary_sizes() {
        let (path, m, model) = rw_with(size, 3);
        m.flush().unwrap();
        drop(m);
        let cow = MemoryMappedFile::open_cow(&path).unwrap();
        for offset in offsets(size) {
            for len in lengths(size, offset) {
                let ctx = format!("COW size={size} offset={offset} len={len}");
                let ok = valid(offset, len, size);
                let r = cow.as_slice(offset, len);
                if ok {
                    let s = r.unwrap();
                    if len > 0 {
                        assert_eq!(&*s, &model[offset as usize..(offset + len) as usize]);
                    }
                } else {
                    expect_oob(r, &ctx, offset, len, size);
                }
                if len <= MAX_BUF {
                    let mut buf = vec![0u8; len as usize];
                    let r = cow.read_into(offset, &mut buf);
                    if ok {
                        r.unwrap();
                    } else {
                        expect_oob(r, &ctx, offset, len, size);
                    }
                }
                let r = cow.flush_range(offset, len);
                if ok {
                    r.unwrap();
                } else {
                    expect_oob(r, &ctx, offset, len, size);
                }
                let r = cow.touch_pages_range(offset, len);
                if ok {
                    r.unwrap();
                } else {
                    expect_oob(r, &ctx, offset, len, size);
                }
            }
        }
    }
}

#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
#[test]
fn range_rule_holds_after_growing_and_shrinking() {
    let p = page();
    let g = granularity();
    // (initial, resized) pairs crossing page and granularity edges.
    let pairs = [
        (1, p + 1),
        (p - 1, p),
        (p, p - 1),
        (p + 1, 1),
        (g, g + 1),
        (g + 1, g - 1),
        (3 * g + 7, p),
        (p, 3 * g + 7),
    ];
    for (from, to) in pairs {
        let (_path, m, mut model) = rw_with(from, 4);
        m.resize(to).unwrap();
        model.resize(to as usize, 0);
        check_all(&m, &model, &format!("RW {from}->{to}"));
    }
}

#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
#[test]
fn a_multi_mebibyte_mapping_follows_the_range_rule() {
    let (_path, m, model) = rw_with((3 << 20) + 5, 5);
    check_all(&m, &model, "RW 3MiB+5");
}

#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
#[test]
fn whole_mapping_operations_cover_every_byte() {
    for size in boundary_sizes() {
        let (_path, m, model) = rw_with(size, 6);
        m.touch_pages().unwrap();
        m.flush().unwrap();
        #[cfg(feature = "locking")]
        match m.lock_all() {
            Ok(()) => m.unlock_all().unwrap(),
            Err(MmapIoError::LockFailed(_)) => {}
            Err(e) => panic!("lock_all: {e}"),
        }
        assert_eq!(m.as_slice(0, size).unwrap(), &model[..]);
    }
}
