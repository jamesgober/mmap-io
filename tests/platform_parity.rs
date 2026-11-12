#![cfg(test)]
//! Platform parity tests for flush visibility across OSes.
//!
//! Contract: After calling flush() or flush_range(), a fresh read of the
//! file must observe the written bytes on all supported platforms. The
//! file is read with `std::fs`, since a second mapping of a file that a
//! read-write mapping maps is refused (since 1.2.0).

use mmap_io::{MemoryMappedFile, MmapMode};
use std::fs;

mod common;
use common::tmp_path;

#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
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

    // Read the file back and verify both segments
    let file = fs::read(&path).expect("read");
    assert_eq!(&file[..10], b"ABCDEFGHIJ");
    assert_eq!(&file[100..106], b"klmnop");

    let _ = fs::remove_file(&path);
}

#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
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
    let file = fs::read(&path).expect("read");
    assert_eq!(&file[200..210], b"RANGE-ONLY");
    // For non-flushed ranges, do not assert visibility.

    // Now do a full flush to persist the rest and validate
    mmap.flush().expect("flush all");
    let file = fs::read(&path).expect("read after full flush");
    assert_eq!(&file[10..22], b"XXXXYYYYZZZZ");
    assert_eq!(&file[1000..1012], b"NO-FLUSH-YET");

    // A read-only mapping opened after the writer is gone sees it all.
    drop(mmap);
    let ro = MemoryMappedFile::open_ro(&path).expect("open_ro");
    assert_eq!(ro.as_slice(200, 10).expect("slice"), b"RANGE-ONLY");

    let _ = fs::remove_file(&path);
}
