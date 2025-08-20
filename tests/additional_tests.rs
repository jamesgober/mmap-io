//! Additional unit tests for comprehensive coverage.

use mmap_io::{
    create_mmap, load_mmap,
    segment::{Segment, SegmentMut},
    utils::{align_up, ensure_in_bounds, page_size},
    MmapIoError, MmapMode,
};
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

fn tmp_path(name: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!("mmap_io_test_{}_{}", name, std::process::id()));
    p
}

#[test]
fn test_resize_operations() {
    let path = tmp_path("resize_operations");
    let _ = fs::remove_file(&path);

    // Create and resize
    let mmap = create_mmap(&path, 1024).expect("create");
    assert_eq!(mmap.len(), 1024);

    // Write data before resize
    mmap.update_region(0, b"before resize").expect("write");

    // Resize larger
    mmap.resize(2048).expect("resize larger");
    assert_eq!(mmap.len(), 2048);

    // Verify data persisted
    let mut buf = [0u8; 13];
    mmap.read_into(0, &mut buf).expect("read");
    assert_eq!(&buf, b"before resize");

    // Resize smaller
    mmap.resize(512).expect("resize smaller");
    assert_eq!(mmap.len(), 512);

    // Cleanup
    fs::remove_file(&path).expect("delete");
}

#[test]
fn test_flush_range() {
    let path = tmp_path("flush_range");
    let _ = fs::remove_file(&path);

    let mmap = create_mmap(&path, 4096).expect("create");

    // Write data in different regions
    mmap.update_region(0, b"start").expect("write start");
    mmap.update_region(1000, b"middle").expect("write middle");
    mmap.update_region(4000, b"end").expect("write end");

    // Flush specific ranges
    mmap.flush_range(0, 100).expect("flush start");
    mmap.flush_range(1000, 100).expect("flush middle");
    mmap.flush_range(4000, 96).expect("flush end");

    // Cleanup
    fs::remove_file(&path).expect("delete");
}

#[test]
fn test_segment_operations() {
    let path = tmp_path("segment_operations");
    let _ = fs::remove_file(&path);

    let mmap = Arc::new(create_mmap(&path, 1024).expect("create"));

    // Create segments
    let seg1 = Segment::new(mmap.clone(), 0, 100).expect("segment 1");
    let seg2 = Segment::new(mmap.clone(), 100, 200).expect("segment 2");

    assert_eq!(seg1.len(), 100);
    assert_eq!(seg1.offset(), 0);
    assert!(!seg1.is_empty());

    assert_eq!(seg2.len(), 200);
    assert_eq!(seg2.offset(), 100);

    // Mutable segment
    let seg_mut = SegmentMut::new(mmap.clone(), 500, 100).expect("mutable segment");
    seg_mut.write(b"segment data").expect("write to segment");

    // Verify through main mmap
    let mut buf = [0u8; 12];
    mmap.read_into(500, &mut buf).expect("read");
    assert_eq!(&buf, b"segment data");

    // Cleanup
    fs::remove_file(&path).expect("delete");
}

#[test]
fn test_segment_bounds_checking() {
    let path = tmp_path("segment_bounds");
    let _ = fs::remove_file(&path);

    let mmap = Arc::new(create_mmap(&path, 1024).expect("create"));

    // Valid segment
    assert!(Segment::new(mmap.clone(), 0, 1024).is_ok());

    // Out of bounds segments
    assert!(Segment::new(mmap.clone(), 1024, 1).is_err());
    assert!(Segment::new(mmap.clone(), 0, 1025).is_err());
    assert!(Segment::new(mmap.clone(), 500, 600).is_err());

    // Cleanup
    fs::remove_file(&path).expect("delete");
}

#[test]
fn test_utils_functions() {
    // Test page_size
    let ps = page_size();
    assert!(ps > 0);
    assert!(ps.is_power_of_two());

    // Test align_up
    assert_eq!(align_up(0, 4096), 0);
    assert_eq!(align_up(1, 4096), 4096);
    assert_eq!(align_up(4096, 4096), 4096);
    assert_eq!(align_up(4097, 4096), 8192);

    // Test with non-power-of-2
    assert_eq!(align_up(10, 3), 12);
    assert_eq!(align_up(9, 3), 9);

    // Test ensure_in_bounds
    assert!(ensure_in_bounds(0, 100, 100).is_ok());
    assert!(ensure_in_bounds(0, 101, 100).is_err());
    assert!(ensure_in_bounds(50, 50, 100).is_ok());
    assert!(ensure_in_bounds(50, 51, 100).is_err());
    assert!(ensure_in_bounds(100, 0, 100).is_ok());
    assert!(ensure_in_bounds(101, 0, 100).is_err());
}

#[test]
fn test_error_display() {
    use std::io;

    // Test error formatting
    let io_err = MmapIoError::Io(io::Error::new(io::ErrorKind::NotFound, "file not found"));
    assert!(io_err.to_string().contains("I/O error"));

    let bounds_err = MmapIoError::OutOfBounds {
        offset: 100,
        len: 50,
        total: 120,
    };
    let err_str = bounds_err.to_string();
    assert!(err_str.contains("100"));
    assert!(err_str.contains("50"));
    assert!(err_str.contains("120"));

    let mode_err = MmapIoError::InvalidMode("test mode error");
    assert!(mode_err.to_string().contains("test mode error"));
}

#[test]
fn test_readonly_write_fails() {
    let path = tmp_path("readonly_write");
    let _ = fs::remove_file(&path);

    // Create file first
    let mmap = create_mmap(&path, 1024).expect("create");
    drop(mmap);

    // Open read-only
    let ro_mmap = load_mmap(&path, MmapMode::ReadOnly).expect("open ro");

    // Write operations should fail
    assert!(ro_mmap.update_region(0, b"fail").is_err());
    assert!(ro_mmap.as_slice_mut(0, 10).is_err());
    assert!(ro_mmap.resize(2048).is_err());

    // Read operations should work
    assert!(ro_mmap.as_slice(0, 10).is_ok());

    // Cleanup
    fs::remove_file(&path).expect("delete");
}

#[test]
fn test_empty_operations() {
    let path = tmp_path("empty_operations");
    let _ = fs::remove_file(&path);

    let mmap = create_mmap(&path, 1024).expect("create");

    // Empty write is ok
    assert!(mmap.update_region(0, b"").is_ok());

    // Empty flush range is ok
    assert!(mmap.flush_range(0, 0).is_ok());

    // Empty read
    let mut buf = [];
    assert!(mmap.read_into(0, &mut buf).is_ok());

    // Cleanup
    fs::remove_file(&path).expect("delete");
}

#[cfg(feature = "async")]
#[tokio::test]
async fn test_async_operations() {
    use mmap_io::manager::r#async::{copy_mmap_async, create_mmap_async, delete_mmap_async};

    let src = tmp_path("async_src");
    let dst = tmp_path("async_dst");
    let _ = fs::remove_file(&src);
    let _ = fs::remove_file(&dst);

    // Create async
    let mmap = create_mmap_async(&src, 1024).await.expect("create async");
    mmap.update_region(0, b"async data").expect("write");
    mmap.flush().expect("flush");
    drop(mmap);

    // Copy async
    copy_mmap_async(&src, &dst).await.expect("copy async");

    // Verify copy
    let copied = load_mmap(&dst, MmapMode::ReadOnly).expect("open copy");
    let data = copied.as_slice(0, 10).expect("read");
    assert_eq!(data, b"async data");

    // Delete async
    delete_mmap_async(&src).await.expect("delete src");
    delete_mmap_async(&dst).await.expect("delete dst");
}
