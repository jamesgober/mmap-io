//! Comprehensive test for all the new critical features

use mmap_io::{flush::FlushPolicy, MemoryMappedFile, MmapMode, TouchHint};
use std::fs;
use std::path::PathBuf;
use std::time::Instant;

fn tmp_path(name: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!(
        "mmap_io_critical_test_{}_{}",
        name,
        std::process::id()
    ));
    p
}

#[cfg_attr(not(target_os = "linux"), ignore)]
#[test]
fn test_hugepages_fallback_behavior() {
    let path = tmp_path("hugepages");
    let _ = fs::remove_file(&path);

    // Test huge pages with fallback - should not fail even if huge pages aren't available
    #[cfg(feature = "hugepages")]
    {
        let mmap = MemoryMappedFile::builder(&path)
            .mode(MmapMode::ReadWrite)
            .size(4 * 1024 * 1024) // 4MB to potentially trigger huge page optimization
            .huge_pages(true)
            .create()
            .expect("huge pages with fallback should succeed");

        // Write some data to verify mapping works
        mmap.update_region(0, b"huge page test").expect("write");
        mmap.flush().expect("flush");

        // Verify data
        let mut buf = vec![0u8; 14];
        mmap.read_into(0, &mut buf).expect("read");
        assert_eq!(&buf, b"huge page test");
    }

    // Ignore NotFound since the file may not have been created when the
    // `hugepages` feature is disabled on the CI runner.
    let _ = fs::remove_file(&path);
}

#[test]
fn test_touch_hint_eager() {
    let path = tmp_path("touch_eager");
    let _ = fs::remove_file(&path);

    // Test eager touching
    let start = Instant::now();
    let mmap = MemoryMappedFile::builder(&path)
        .mode(MmapMode::ReadWrite)
        .size(1024 * 1024) // 1MB
        .touch_hint(TouchHint::Eager)
        .create()
        .expect("create with eager touch");
    let creation_time = start.elapsed();

    println!("Creation with eager touch took: {:?}", creation_time);

    // Subsequent access should be faster (no page faults)
    let start = Instant::now();
    let data = vec![0x42; 4096];
    for i in 0..256 {
        mmap.update_region(i * 4096, &data).expect("write");
    }
    let write_time = start.elapsed();

    println!("Writing after eager touch took: {:?}", write_time);

    fs::remove_file(&path).expect("cleanup");
}

#[test]
fn test_touch_hint_never() {
    let path = tmp_path("touch_never");
    let _ = fs::remove_file(&path);

    // Test no touching (default)
    let start = Instant::now();
    let mmap = MemoryMappedFile::builder(&path)
        .mode(MmapMode::ReadWrite)
        .size(1024 * 1024) // 1MB
        .touch_hint(TouchHint::Never)
        .create()
        .expect("create with no touch");
    let creation_time = start.elapsed();

    println!("Creation with no touch took: {:?}", creation_time);

    // First access may have page faults
    let start = Instant::now();
    let data = vec![0x42; 4096];
    for i in 0..256 {
        mmap.update_region(i * 4096, &data).expect("write");
    }
    let write_time = start.elapsed();

    println!("First write (with page faults) took: {:?}", write_time);

    fs::remove_file(&path).expect("cleanup");
}

#[test]
fn test_time_based_flushing_policy() {
    let path = tmp_path("time_flush");
    let _ = fs::remove_file(&path);

    // Test time-based flushing policy setting
    let mmap = MemoryMappedFile::builder(&path)
        .mode(MmapMode::ReadWrite)
        .size(4096)
        .flush_policy(FlushPolicy::EveryMillis(100))
        .create()
        .expect("create with time policy");

    // Write some data
    mmap.update_region(0, b"time-based test").expect("write");

    // Manual flush to ensure data is persisted
    mmap.flush().expect("flush");

    // Verify data persisted
    let ro_mmap = MemoryMappedFile::open_ro(&path).expect("open ro");
    let mut buf = vec![0u8; 15];
    ro_mmap.read_into(0, &mut buf).expect("read");
    assert_eq!(&buf, b"time-based test");

    fs::remove_file(&path).expect("cleanup");
}

#[test]
fn test_microflush_optimization() {
    let path = tmp_path("microflush");
    let _ = fs::remove_file(&path);

    let mmap = MemoryMappedFile::create_rw(&path, 64 * 1024).expect("create");

    // Test microflush (small range)
    let small_data = vec![0xAB; 256]; // 256 bytes, much smaller than page size
    mmap.update_region(0, &small_data).expect("write");

    let start = Instant::now();
    mmap.flush_range(0, 256).expect("microflush");
    let microflush_time = start.elapsed();

    println!("Microflush optimization took: {:?}", microflush_time);

    // Verify data was flushed
    let ro_mmap = MemoryMappedFile::open_ro(&path).expect("open ro");
    let mut buf = vec![0u8; 256];
    ro_mmap.read_into(0, &mut buf).expect("read");
    assert_eq!(buf[0], 0xAB);

    fs::remove_file(&path).expect("cleanup");
}

#[test]
fn test_comprehensive_feature_combination() {
    let path = tmp_path("comprehensive");
    let _ = fs::remove_file(&path);

    // Test combination of multiple features
    #[allow(unused_mut)]
    let mut builder = MemoryMappedFile::builder(&path)
        .mode(MmapMode::ReadWrite)
        .size(2 * 1024 * 1024) // 2MB
        .flush_policy(FlushPolicy::EveryBytes(64 * 1024))
        .touch_hint(TouchHint::Eager);

    #[cfg(feature = "hugepages")]
    {
        builder = builder.huge_pages(true);
    }

    let mmap = builder.create().expect("create comprehensive mapping");

    // Test various operations
    let data = vec![0xCD; 8192]; // 8KB chunks
    for i in 0..16 {
        mmap.update_region(i * 8192, &data).expect("write chunk");
    }

    // Test touch pages explicitly
    mmap.touch_pages().expect("touch pages");

    // Test range flushing
    mmap.flush_range(0, 64 * 1024).expect("flush range");

    // Test full flush
    mmap.flush().expect("full flush");

    // Verify all data
    let mut buf = vec![0u8; 8192];
    for i in 0..16 {
        mmap.read_into(i * 8192, &mut buf).expect("read chunk");
        assert_eq!(buf[0], 0xCD);
        assert_eq!(buf[8191], 0xCD);
    }

    println!("Comprehensive feature test completed successfully");

    fs::remove_file(&path).expect("cleanup");
}

fn main() {
    println!("Running critical feature tests...");

    test_hugepages_fallback_behavior();
    println!("✓ Huge pages fallback behavior test passed");

    test_touch_hint_eager();
    println!("✓ TouchHint::Eager test passed");

    test_touch_hint_never();
    println!("✓ TouchHint::Never test passed");

    test_time_based_flushing_policy();
    println!("✓ Time-based flushing policy test passed");

    test_microflush_optimization();
    println!("✓ Microflush optimization test passed");

    test_comprehensive_feature_combination();
    println!("✓ Comprehensive feature combination test passed");

    println!("\nAll critical feature tests passed! 🎉");
}
