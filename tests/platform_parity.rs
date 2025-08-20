#![cfg(all(test))]
//! Platform parity tests for flush visibility across OSes.
//!
//! Contract: After calling flush() or flush_range(), a newly opened read-only mapping
//! must observe the written bytes persisted on disk on all supported platforms.

use mmap_io::{MemoryMappedFile, MmapMode};
use std::fs;
use std::path::PathBuf;

fn tmp_path(name: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!(
        "mmap_io_platform_parity_{}_{}",
        name,
        std::process::id()
    ));
    p
}

#[test]
fn parity_flush_visibility_full_file() {
    let path = tmp_path("parity_flush_visibility_full_file");
    let _ = fs::remove_file(&path);

    // Create a new RW mapping
    let mmap = MemoryMappedFile::create_rw(&path, 4096).expect("create_rw");
    assert_eq!(mmap.mode(), MmapMode::ReadWrite);

    // Write two segments
    mmap.update_region(0, b"ABCDEFGHIJ").expect("write-1");
    mmap.update_region(100, b"klmnop").expect("write-2");

    // Full flush should make all changes visible
    mmap.flush().expect("flush");

    // Re-open RO and verify both segments
    let ro = MemoryMappedFile::open_ro(&path).expect("open_ro");
    let s1 = ro.as_slice(0, 10).expect("slice s1");
    let s2 = ro.as_slice(100, 6).expect("slice s2");
    assert_eq!(s1, b"ABCDEFGHIJ");
    assert_eq!(s2, b"klmnop");

    let _ = fs::remove_file(&path);
}

#[test]
fn parity_flush_visibility_range() {
    let path = tmp_path("parity_flush_visibility_range");
    let _ = fs::remove_file(&path);

    // Create a new RW mapping
    let mmap = MemoryMappedFile::create_rw(&path, 4096).expect("create_rw");

    // Write three regions
    mmap.update_region(10, b"XXXXYYYYZZZZ").expect("write-xyz");
    mmap.update_region(200, b"RANGE-ONLY").expect("write-range");
    mmap.update_region(1000, b"NO-FLUSH-YET")
        .expect("write-no-flush");

    // Flush only the [200, 200 + len) region
    mmap.flush_range(200, "RANGE-ONLY".len() as u64)
        .expect("flush_range");

    // Re-open RO and verify:
    // - The flushed range must be visible
    // - Other ranges might or might not be visible depending on platform caching,
    //   but our contract guarantees visibility for flushed regions.
    let ro = MemoryMappedFile::open_ro(&path).expect("open_ro");

    let flushed = ro
        .as_slice(200, "RANGE-ONLY".len() as u64)
        .expect("slice flushed");
    assert_eq!(flushed, b"RANGE-ONLY");

    // For non-flushed ranges, do not assert visibility; just ensure access is valid.
    let _ = ro.as_slice(10, 12).expect("slice non-flushed 1");
    let _ = ro.as_slice(1000, 12).expect("slice non-flushed 2");

    // Now do a full flush to persist the rest and validate
    mmap.flush().expect("flush all");
    let ro2 = MemoryMappedFile::open_ro(&path).expect("open_ro2");
    let s1 = ro2.as_slice(10, 12).expect("slice after full flush");
    let s2 = ro2.as_slice(1000, 12).expect("slice after full flush 2");
    assert_eq!(s1, b"XXXXYYYYZZZZ");
    assert_eq!(s2, b"NO-FLUSH-YET");

    let _ = fs::remove_file(&path);
}
