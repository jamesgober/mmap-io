//! Test for the new touch_pages functionality

use mmap_io::MemoryMappedFile;
use std::fs;
use std::time::Instant;

mod common;
use common::tmp_path;

#[test]
fn test_touch_pages_basic() {
    let path = tmp_path("touch_basic");
    let _ = fs::remove_file(&path);

    // Create a 1MB file
    let mmap = MemoryMappedFile::create_rw(&path, 1024 * 1024).expect("create");

    // Fill with some data
    let data = vec![0xAB; 4096];
    for i in 0..256 {
        mmap.update_region(i * 4096, &data).expect("write");
    }
    mmap.flush().expect("flush");

    // Test touching all pages
    let start = Instant::now();
    mmap.touch_pages().expect("touch_pages");
    let touch_duration = start.elapsed();

    println!("Touch pages took: {touch_duration:?}");

    // Verify we can still read the data
    let mut buf = vec![0u8; 4096];
    mmap.read_into(0, &mut buf).expect("read");
    assert_eq!(buf[0], 0xAB);

    fs::remove_file(&path).expect("cleanup");
}

#[test]
fn test_touch_pages_range() {
    let path = tmp_path("touch_range");
    let _ = fs::remove_file(&path);

    // Create a 1MB file
    let mmap = MemoryMappedFile::create_rw(&path, 1024 * 1024).expect("create");

    // Test touching a specific range
    let start = Instant::now();
    mmap.touch_pages_range(0, 64 * 1024)
        .expect("touch_pages_range");
    let touch_duration = start.elapsed();

    println!("Touch pages range took: {touch_duration:?}");

    // Test bounds checking
    let result = mmap.touch_pages_range(1024 * 1024, 1);
    assert!(result.is_err());

    fs::remove_file(&path).expect("cleanup");
}

#[test]
fn test_microflush_optimization() {
    let path = tmp_path("microflush");
    let _ = fs::remove_file(&path);

    let mmap = MemoryMappedFile::create_rw(&path, 64 * 1024).expect("create");

    // Test small flush (should trigger microflush optimization)
    let small_data = vec![0xCD; 512]; // 512 bytes, smaller than page size
    mmap.update_region(0, &small_data).expect("write");

    let start = Instant::now();
    mmap.flush_range(0, 512).expect("flush_range");
    let flush_duration = start.elapsed();

    println!("Microflush took: {flush_duration:?}");

    fs::remove_file(&path).expect("cleanup");
}

#[cfg(feature = "async")]
#[tokio::test(flavor = "multi_thread")]
async fn test_time_based_flushing() {
    use mmap_io::{flush::FlushPolicy, MmapMode};
    //use std::time::Duration;

    let path = tmp_path("time_flush");
    let _ = fs::remove_file(&path);

    // Create with time-based flushing every 100ms
    let mmap = MemoryMappedFile::builder(&path)
        .mode(MmapMode::ReadWrite)
        .size(4096)
        .flush_policy(FlushPolicy::EveryMillis(100))
        .create()
        .expect("create with time policy");

    // Write some data
    mmap.update_region(0, b"time-based flush test")
        .expect("write");

    // The background flusher is covered by tests/time_based_flush.rs;
    // here an explicit flush must work alongside the policy.
    mmap.flush().expect("manual flush");

    // Open a new read-only mapping to verify
    let ro_mmap = MemoryMappedFile::open_ro(&path).expect("open ro");
    let mut buf = vec![0u8; 22];
    ro_mmap.read_into(0, &mut buf).expect("read");
    assert_eq!(&buf[..21], b"time-based flush test");

    fs::remove_file(&path).expect("cleanup");
}
