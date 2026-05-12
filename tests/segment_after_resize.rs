//! Regression tests for the H6 audit finding: segment bounds must be
//! re-validated on every access because the parent mapping can be
//! resized between segment construction and use.

use mmap_io::{
    create_mmap,
    segment::{Segment, SegmentMut},
    MmapIoError,
};
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

fn tmp_path(name: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!(
        "mmap_io_segment_resize_test_{}_{}",
        name,
        std::process::id()
    ));
    p
}

#[test]
fn segment_remains_valid_when_parent_grows() {
    let path = tmp_path("seg_grow");
    let _ = fs::remove_file(&path);

    let mmap = Arc::new(create_mmap(&path, 1024).expect("create"));

    // Make a segment over the back half: [512, 1024).
    let seg = Segment::new(mmap.clone(), 512, 512).expect("segment");
    assert!(seg.is_valid());

    // Grow the parent. Segment range still within bounds.
    mmap.resize(4096).expect("grow");
    assert!(seg.is_valid());

    // Bounds-respecting access works on RW since 0.9.7 (as_slice
    // returns a MappedSlice<'_> that holds the read guard).
    {
        let s = seg.as_slice().expect("RW as_slice should succeed");
        assert_eq!(s.len() as u64, seg.len());
    }

    let _ = fs::remove_file(&path);
}

#[test]
fn segment_returns_oob_after_parent_shrinks_past_range() {
    let path = tmp_path("seg_shrink_past");
    let _ = fs::remove_file(&path);

    let mmap = Arc::new(create_mmap(&path, 1024).expect("create"));

    // Segment over [900, 1024).
    let seg = Segment::new(mmap.clone(), 900, 124).expect("segment");
    assert!(seg.is_valid());

    // Shrink the parent below the segment's range.
    mmap.resize(500).expect("shrink");

    assert!(
        !seg.is_valid(),
        "segment must report invalid after shrink past its range"
    );

    // as_slice MUST now return OutOfBounds, not silently succeed and
    // not panic.
    match seg.as_slice() {
        Err(MmapIoError::OutOfBounds { .. }) => {}
        Err(e) => panic!("expected OutOfBounds, got {e}"),
        Ok(_) => panic!("expected OutOfBounds, got Ok"),
    }

    let _ = fs::remove_file(&path);
}

#[test]
fn segment_mut_returns_oob_after_parent_shrinks_past_range() {
    let path = tmp_path("seg_mut_shrink_past");
    let _ = fs::remove_file(&path);

    let mmap = Arc::new(create_mmap(&path, 1024).expect("create"));

    let seg = SegmentMut::new(mmap.clone(), 900, 124).expect("segment");
    assert!(seg.is_valid());

    mmap.resize(500).expect("shrink");
    assert!(!seg.is_valid());

    // write() MUST surface OutOfBounds, not corrupt memory or panic.
    let err = seg.write(b"should fail").expect_err("expected OutOfBounds");
    assert!(
        matches!(err, MmapIoError::OutOfBounds { .. }),
        "expected OutOfBounds, got {err}"
    );

    // as_slice_mut() MUST also surface OutOfBounds. Use match because
    // MappedSliceMut does not implement Debug (cannot use expect_err).
    match seg.as_slice_mut() {
        Err(MmapIoError::OutOfBounds { .. }) => {}
        Err(e) => panic!("expected OutOfBounds, got {e}"),
        Ok(_) => panic!("expected OutOfBounds, got Ok"),
    }

    let _ = fs::remove_file(&path);
}

#[test]
fn segment_mut_write_succeeds_when_parent_still_covers_range() {
    let path = tmp_path("seg_mut_partial_shrink");
    let _ = fs::remove_file(&path);

    let mmap = Arc::new(create_mmap(&path, 4096).expect("create"));

    let seg = SegmentMut::new(mmap.clone(), 100, 50).expect("segment");

    // Shrink, but keep enough room for the segment.
    mmap.resize(2048).expect("partial shrink");
    assert!(seg.is_valid());

    // Write should still succeed.
    seg.write(b"data").expect("write after partial shrink");

    let _ = fs::remove_file(&path);
}
