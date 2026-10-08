//! Tests for size limit validation

use mmap_io::{MemoryMappedFile, MmapIoError};
use std::fs;

mod common;
use common::tmp_path;

#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
#[test]
fn test_max_size_validation_create() {
    let path = tmp_path("max_size_create");
    let _ = fs::remove_file(&path);

    // Test that MAX_MMAP_SIZE is enforced
    // Use a size that's definitely too large (u64::MAX)
    let huge_size = u64::MAX;
    let result = MemoryMappedFile::create_rw(&path, huge_size);

    assert!(result.is_err());
    match result {
        Err(MmapIoError::ResizeFailed(msg)) => {
            assert!(msg.contains("exceeds maximum safe limit"));
        }
        _ => panic!("Expected ResizeFailed error for oversized mmap"),
    }

    // Clean up
    let _ = fs::remove_file(&path);
}

#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
#[test]
fn test_max_size_validation_resize() {
    let path = tmp_path("max_size_resize");
    let _ = fs::remove_file(&path);

    // Create a normal-sized file
    let mmap = MemoryMappedFile::create_rw(&path, 1024).expect("create normal size");

    // Try to resize to an excessive size
    let huge_size = u64::MAX;
    let result = mmap.resize(huge_size);

    assert!(result.is_err());
    match result {
        Err(MmapIoError::ResizeFailed(msg)) => {
            assert!(msg.contains("exceeds maximum safe limit"));
        }
        _ => panic!("Expected ResizeFailed error for oversized resize"),
    }

    // Clean up
    drop(mmap);
    let _ = fs::remove_file(&path);
}

#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
#[test]
fn test_max_size_validation_builder() {
    let path = tmp_path("max_size_builder");
    let _ = fs::remove_file(&path);

    // Test builder with excessive size
    let huge_size = u64::MAX;
    let result = MemoryMappedFile::builder(&path)
        .mode(mmap_io::MmapMode::ReadWrite)
        .size(huge_size)
        .create();

    assert!(result.is_err());
    match result {
        Err(MmapIoError::ResizeFailed(msg)) => {
            assert!(msg.contains("exceeds maximum safe limit"));
        }
        _ => panic!("Expected ResizeFailed error for oversized builder"),
    }

    // Clean up
    let _ = fs::remove_file(&path);
}

#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
#[test]
fn test_normal_size_still_works() {
    let path = tmp_path("normal_size");
    let _ = fs::remove_file(&path);

    // Test that normal sizes still work fine
    let mut normal_sizes = vec![
        1024,        // 1 KB
        1024 * 1024, // 1 MB
    ];
    if cfg!(target_pointer_width = "64") {
        normal_sizes.push(1024 * 1024 * 1024); // 1 GB
        normal_sizes.push(10 * 1024 * 1024 * 1024); // 10 GB
    } else {
        // 32-bit targets cap mappings at 2 GiB and have a few GiB of
        // address space in total.
        normal_sizes.push(256 * 1024 * 1024);
    }

    for size in normal_sizes {
        // Create with normal size should work
        let mmap = MemoryMappedFile::create_rw(&path, size).expect("create normal size");
        assert_eq!(mmap.len(), size);

        // Clean up for next iteration
        drop(mmap);
        let _ = fs::remove_file(&path);
    }
}

#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
#[test]
fn test_zero_size_validation() {
    let path = tmp_path("zero_size");
    let _ = fs::remove_file(&path);

    // Test that zero size is still rejected
    let result = MemoryMappedFile::create_rw(&path, 0);

    assert!(result.is_err());
    match result {
        Err(MmapIoError::ResizeFailed(msg)) => {
            assert!(msg.contains("must be greater than zero"));
        }
        _ => panic!("Expected ResizeFailed error for zero size"),
    }

    // Clean up
    let _ = fs::remove_file(&path);
}
