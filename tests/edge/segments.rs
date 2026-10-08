//! `Segment` and `SegmentMut`: accessors, validation at construction
//! and on access, behavior across resizes, and mode errors.

use std::sync::Arc;

use mmap_io::segment::{Segment, SegmentMut};
use mmap_io::{MemoryMappedFile, MmapIoError};

use crate::common::{pattern, tmp_path};

#[test]
fn accessors_report_construction_values() {
    let path = tmp_path("seg.bin");
    let m = Arc::new(MemoryMappedFile::create_rw(&path, 1000).unwrap());
    for (offset, len) in [
        (0, 0),
        (0, 1000),
        (999, 1),
        (1000, 0),
        (u64::MAX, 0),
        (5, 7),
    ] {
        let s = Segment::new(Arc::clone(&m), offset, len).unwrap();
        assert_eq!((s.offset(), s.len(), s.is_empty()), (offset, len, len == 0));
        assert_eq!(s.parent().len(), 1000);
        assert!(s.is_valid());
        let c = s.clone();
        assert_eq!((c.offset(), c.len()), (offset, len));
        assert!(format!("{s:?}").contains("Segment"));

        let s = SegmentMut::new(Arc::clone(&m), offset, len).unwrap();
        assert_eq!((s.offset(), s.len(), s.is_empty()), (offset, len, len == 0));
        assert_eq!(s.parent().len(), 1000);
        assert!(s.is_valid());
        let c = s.clone();
        assert_eq!((c.offset(), c.len()), (offset, len));
        assert!(format!("{s:?}").contains("SegmentMut"));
    }
}

#[test]
fn segment_write_rules() {
    let path = tmp_path("segw.bin");
    let m = Arc::new(MemoryMappedFile::create_rw(&path, 100).unwrap());
    let s = SegmentMut::new(Arc::clone(&m), 10, 20).unwrap();

    // Shorter than the segment: the rest is left alone.
    m.update_region(10, &[9; 20]).unwrap();
    s.write(b"abc").unwrap();
    let mut expect = [9u8; 20];
    expect[..3].copy_from_slice(b"abc");
    assert_eq!(m.as_slice(10, 20).unwrap(), &expect[..]);

    // Exactly the segment length.
    s.write(&[1; 20]).unwrap();
    assert_eq!(m.as_slice(10, 20).unwrap(), &[1; 20][..]);
    assert_eq!(
        m.as_slice(30, 1).unwrap(),
        &[0][..],
        "wrote past the segment"
    );

    // Longer: segment-relative error fields, nothing written.
    assert!(matches!(
        s.write(&[2; 21]),
        Err(MmapIoError::OutOfBounds {
            offset: 0,
            len: 21,
            total: 20
        })
    ));
    assert_eq!(m.as_slice(10, 20).unwrap(), &[1; 20][..]);

    // Empty data is fine, also on an empty segment past the end.
    s.write(&[]).unwrap();
    let empty = SegmentMut::new(Arc::clone(&m), 500, 0).unwrap();
    empty.write(&[]).unwrap();
    assert!(matches!(
        empty.write(&[1]),
        Err(MmapIoError::OutOfBounds {
            offset: 0,
            len: 1,
            total: 0
        })
    ));

    // as_slice_mut writes through.
    {
        let mut g = s.as_slice_mut().unwrap();
        assert_eq!(g.len(), 20);
        g.as_mut().copy_from_slice(&pattern(20, 1));
    }
    let ro = Segment::new(Arc::clone(&m), 10, 20).unwrap();
    assert_eq!(ro.as_slice().unwrap(), &pattern(20, 1)[..]);
}

#[test]
fn segments_on_read_only_parents_read_but_do_not_write() {
    let path = tmp_path("segro.bin");
    std::fs::write(&path, pattern(64, 2)).unwrap();
    let m = Arc::new(MemoryMappedFile::open_ro(&path).unwrap());
    let s = Segment::new(Arc::clone(&m), 8, 8).unwrap();
    assert_eq!(s.as_slice().unwrap(), &pattern(64, 2)[8..16]);
    let w = SegmentMut::new(Arc::clone(&m), 8, 8).unwrap();
    assert!(matches!(w.write(b"x"), Err(MmapIoError::InvalidMode(_))));
    assert!(matches!(w.as_slice_mut(), Err(MmapIoError::InvalidMode(_))));
    // An oversized write is a range error before the mode is checked.
    assert!(matches!(
        w.write(&[0; 9]),
        Err(MmapIoError::OutOfBounds { .. })
    ));
}

#[test]
fn validity_follows_every_resize() {
    let path = tmp_path("segrs.bin");
    let m = Arc::new(MemoryMappedFile::create_rw(&path, 4096).unwrap());
    let tail = Segment::new(Arc::clone(&m), 4000, 96).unwrap();
    let tail_mut = SegmentMut::new(Arc::clone(&m), 4000, 96).unwrap();
    let head = Segment::new(Arc::clone(&m), 0, 10).unwrap();

    for (size, tail_ok) in [
        (4095, false),
        (4096, true),
        (4000, false),
        (1, false),
        (8192, true),
    ] {
        m.resize(size).unwrap();
        assert_eq!(tail.is_valid(), tail_ok, "size {size}");
        assert_eq!(tail_mut.is_valid(), tail_ok, "size {size}");
        if tail_ok {
            assert_eq!(tail.as_slice().unwrap().len(), 96);
            tail_mut.write(b"ok").unwrap();
        } else {
            assert!(
                matches!(
                    tail.as_slice(),
                    Err(MmapIoError::OutOfBounds { offset: 4000, len: 96, total }) if total == size
                ),
                "size {size}"
            );
            assert!(matches!(
                tail_mut.as_slice_mut(),
                Err(MmapIoError::OutOfBounds { offset: 4000, len: 96, total }) if total == size
            ));
            // A write of the full segment length no longer fits either.
            assert!(matches!(
                tail_mut.write(&[0; 96]),
                Err(MmapIoError::OutOfBounds { offset: 4000, len: 96, total }) if total == size
            ));
        }
        assert_eq!(head.is_valid(), size >= 10, "size {size}");
    }
}

#[test]
fn hostile_segment_ranges_are_rejected_with_exact_fields() {
    let path = tmp_path("seghostile.bin");
    let m = Arc::new(MemoryMappedFile::create_rw(&path, 100).unwrap());
    for (offset, len) in [
        (0, 101),
        (100, 1),
        (101, 1),
        (u64::MAX, 1),
        (1, u64::MAX),
        (u64::MAX, u64::MAX),
        (99, 2),
    ] {
        assert!(matches!(
            Segment::new(Arc::clone(&m), offset, len),
            Err(MmapIoError::OutOfBounds { offset: o, len: l, total: 100 }) if o == offset && l == len
        ));
        assert!(matches!(
            SegmentMut::new(Arc::clone(&m), offset, len),
            Err(MmapIoError::OutOfBounds { offset: o, len: l, total: 100 }) if o == offset && l == len
        ));
    }
}

/// `SegmentMut::write` documents `OutOfBounds` "if the segment no longer
/// fits in the parent (e.g. after a shrinking resize)". It only checks
/// the bytes being written, so a short write into a segment whose tail
/// was cut off still succeeds.
#[test]
#[ignore = "BUG: SegmentMut::write succeeds on a segment that no longer fits its parent if the data still fits"]
fn segment_write_rejects_a_segment_cut_by_a_shrink() {
    let path = tmp_path("segcut.bin");
    let m = Arc::new(MemoryMappedFile::create_rw(&path, 4096).unwrap());
    let seg = SegmentMut::new(Arc::clone(&m), 4000, 96).unwrap();
    m.resize(4050).unwrap();
    assert!(!seg.is_valid());
    assert!(matches!(
        seg.write(b"short"),
        Err(MmapIoError::OutOfBounds { .. })
    ));
}
