#![cfg(feature = "async")]
//! Async-only flushing tests: async write auto-flushes without explicit flush()

use mmap_io::MemoryMappedFile;
use std::fs;

mod common;
use common::tmp_path;

#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
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

    // Read the file back (a second mapping of it is refused while the
    // writer lives) and verify data persisted without explicit flush.
    assert_eq!(&common::read_file(&path)[128..139], b"ASYNC-FLUSH");

    let _ = fs::remove_file(&path);
}

#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
#[tokio::test(flavor = "multi_thread")]
async fn async_explicit_flush_still_works() {
    let path = tmp_path("async_explicit_flush_still_works");
    let _ = fs::remove_file(&path);

    let mmap = MemoryMappedFile::create_rw(&path, 1024).expect("create_rw");

    mmap.update_region_async(0, b"XYZ")
        .await
        .expect("update_region_async");
    // extra explicit async flush flushes again and should succeed
    mmap.flush_async().await.expect("flush_async");

    assert_eq!(&common::read_file(&path)[..3], b"XYZ");

    let _ = fs::remove_file(&path);
}
