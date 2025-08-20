#![cfg(feature = "async")]
//! Async-only flushing tests: async write auto-flushes without explicit flush()

use mmap_io::MemoryMappedFile;
use std::fs;
use std::path::PathBuf;

fn tmp_path(name: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!(
        "mmap_io_async_test_{}_{}",
        name,
        std::process::id()
    ));
    p
}

#[tokio::test(flavor = "multi_thread")]
async fn async_update_region_auto_flushes() {
    let path = tmp_path("async_update_region_auto_flushes");
    let _ = fs::remove_file(&path);

    // Create RW mapping
    let mmap = MemoryMappedFile::create_rw(&path, 4096).expect("create_rw");

    // Perform async write; this should auto-flush due to async-only flushing semantics
    mmap.update_region_async(128, b"ASYNC-FLUSH")
        .await
        .expect("update_region_async");

    // Reopen read-only and verify data persisted without explicit flush
    let ro = MemoryMappedFile::open_ro(&path).expect("open_ro");
    let slice = ro.as_slice(128, 11).expect("slice");
    assert_eq!(slice, b"ASYNC-FLUSH");

    let _ = fs::remove_file(&path);
}

#[tokio::test(flavor = "multi_thread")]
async fn async_explicit_flush_still_works() {
    let path = tmp_path("async_explicit_flush_still_works");
    let _ = fs::remove_file(&path);

    let mmap = MemoryMappedFile::create_rw(&path, 1024).expect("create_rw");

    mmap.update_region_async(0, b"XYZ")
        .await
        .expect("update_region_async");
    // extra explicit async flush is a no-op but should succeed
    mmap.flush_async().await.expect("flush_async");

    let ro = MemoryMappedFile::open_ro(&path).expect("open_ro");
    let slice = ro.as_slice(0, 3).expect("slice");
    assert_eq!(slice, b"XYZ");

    let _ = fs::remove_file(&path);
}
