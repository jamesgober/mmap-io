//! Tests for huge pages support (Linux-specific feature)

#![cfg(all(feature = "hugepages", target_os = "linux"))]

use mmap_io::{MemoryMappedFile, MmapMode};
use std::fs;
use tempfile::tempdir;

#[test]
fn test_hugepages_builder_create() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("hugepages_test.bin");

    // Create a file with huge pages enabled
    // Note: This may fall back to regular pages if huge pages are not configured
    let size = 2 * 1024 * 1024; // 2MB - common huge page size
    let result = MemoryMappedFile::builder(&path)
        .mode(MmapMode::ReadWrite)
        .size(size)
        .huge_pages(true)
        .create();

    // Should succeed even if huge pages aren't available (falls back gracefully)
    assert!(
        result.is_ok(),
        "Failed to create mapping with huge pages hint: {:?}",
        result
    );

    if let Ok(mmap) = result {
        assert_eq!(mmap.len(), size);
        assert_eq!(mmap.mode(), MmapMode::ReadWrite);

        // Write some data
        let data = b"Hello, huge pages!";
        mmap.update_region(0, data).unwrap();
        mmap.flush().unwrap();

        // Read it back
        let mut buf = vec![0u8; data.len()];
        mmap.read_into(0, &mut buf).unwrap();
        assert_eq!(&buf, data);
    }
}

#[test]
fn test_hugepages_builder_open() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("hugepages_open_test.bin");

    // First create a file
    let size = 2 * 1024 * 1024; // 2MB
    fs::write(&path, vec![0u8; size as usize]).unwrap();

    // Open with huge pages hint
    let result = MemoryMappedFile::builder(&path)
        .mode(MmapMode::ReadWrite)
        .huge_pages(true)
        .open();

    assert!(
        result.is_ok(),
        "Failed to open mapping with huge pages hint: {:?}",
        result
    );

    if let Ok(mmap) = result {
        assert_eq!(mmap.len(), size);
        assert_eq!(mmap.mode(), MmapMode::ReadWrite);

        // Test read/write
        let data = b"Huge pages open test";
        mmap.update_region(100, data).unwrap();
        mmap.flush().unwrap();

        let mut buf = vec![0u8; data.len()];
        mmap.read_into(100, &mut buf).unwrap();
        assert_eq!(&buf, data);
    }
}

#[test]
fn test_hugepages_fallback() {
    // This test verifies that the huge pages implementation gracefully
    // falls back to regular pages when huge pages are not available
    let dir = tempdir().unwrap();
    let path = dir.path().join("hugepages_fallback.bin");

    // Use a small size that's unlikely to use huge pages
    let size = 4096; // 4KB - smaller than typical huge page size

    let mmap = MemoryMappedFile::builder(&path)
        .mode(MmapMode::ReadWrite)
        .size(size)
        .huge_pages(true)
        .create()
        .expect("Should create mapping even without huge pages");

    assert_eq!(mmap.len(), size);

    // Verify basic functionality still works
    let test_data = b"Fallback test";
    mmap.update_region(0, test_data).unwrap();
    mmap.flush().unwrap();

    let mut buf = vec![0u8; test_data.len()];
    mmap.read_into(0, &mut buf).unwrap();
    assert_eq!(&buf, test_data);
}

#[test]
fn test_hugepages_large_file() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("hugepages_large.bin");

    // Use a size that's a multiple of common huge page sizes
    let size = 16 * 1024 * 1024; // 16MB

    let mmap = MemoryMappedFile::builder(&path)
        .mode(MmapMode::ReadWrite)
        .size(size)
        .huge_pages(true)
        .create()
        .expect("Should create large mapping");

    assert_eq!(mmap.len(), size);

    // Test writing at various offsets
    let test_cases = vec![
        (0, b"Start of file"),
        (1024 * 1024, b"At 1MB"),
        (8 * 1024 * 1024, b"At 8MB"),
        (size - 100, b"Near end"),
    ];

    for (offset, data) in test_cases {
        mmap.update_region(offset, data).unwrap();

        let mut buf = vec![0u8; data.len()];
        mmap.read_into(offset, &mut buf).unwrap();
        assert_eq!(&buf, data, "Data mismatch at offset {}", offset);
    }

    mmap.flush().unwrap();
}

#[test]
fn test_hugepages_disabled() {
    // Test that huge_pages(false) works correctly
    let dir = tempdir().unwrap();
    let path = dir.path().join("hugepages_disabled.bin");

    let size = 2 * 1024 * 1024;

    let mmap = MemoryMappedFile::builder(&path)
        .mode(MmapMode::ReadWrite)
        .size(size)
        .huge_pages(false) // Explicitly disable
        .create()
        .expect("Should create mapping without huge pages");

    assert_eq!(mmap.len(), size);

    // Verify functionality
    let data = b"No huge pages";
    mmap.update_region(0, data).unwrap();
    mmap.flush().unwrap();

    let mut buf = vec![0u8; data.len()];
    mmap.read_into(0, &mut buf).unwrap();
    assert_eq!(&buf, data);
}
