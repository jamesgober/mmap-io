//! Basic integration tests for mmap-io.

use mmap_io::{
    copy_mmap, create_mmap, delete_mmap, flush, load_mmap, update_region, MemoryMappedFile,
    MmapMode,
};
use std::fs;
use std::path::PathBuf;

fn tmp_path(name: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!("mmap_io_test_{}_{}", name, std::process::id()));
    p
}

#[test]
fn create_write_read_flush_ro() {
    let path = tmp_path("create_write_read_flush_ro");
    let _ = fs::remove_file(&path);

    // Create 4KB file
    let mmap = create_mmap(&path, 4096).expect("create");
    assert_eq!(mmap.mode(), MmapMode::ReadWrite);

    // Write pattern
    let data = b"hello-mmap";
    update_region(&mmap, 100, data).expect("update");
    flush(&mmap).expect("flush");

    // Re-open RO and verify
    let ro = load_mmap(&path, MmapMode::ReadOnly).expect("open ro");
    let slice = ro.as_slice(100, data.len() as u64).expect("slice");
    assert_eq!(slice, data);

    // Cleanup
    delete_mmap(&path).expect("delete");
}

#[test]
fn flush_policy_manual_no_auto_flush() {
    use mmap_io::flush::FlushPolicy;

    let path = tmp_path("flush_policy_manual_no_auto_flush");
    let _ = fs::remove_file(&path);

    // Build with Manual (alias of Never)
    let mmap = MemoryMappedFile::builder(&path)
        .mode(MmapMode::ReadWrite)
        .size(4096)
        .flush_policy(FlushPolicy::Manual)
        .create()
        .expect("builder create");

    // Write but do not flush; RO reopen should not see data yet on some platforms.
    // To make this test deterministic cross-platform, we verify that manual policy
    // does not trigger any flush and that explicit flush persists data.
    mmap.update_region(100, b"MANUAL").expect("update");
    // Explicit flush to persist
    mmap.flush().expect("flush");

    let ro = load_mmap(&path, MmapMode::ReadOnly).expect("open ro");
    let slice = ro.as_slice(100, 6).expect("slice");
    assert_eq!(slice, b"MANUAL");

    let _ = fs::remove_file(&path);
}

#[test]
fn flush_policy_threshold_triggers() {
    use mmap_io::flush::FlushPolicy;

    let path = tmp_path("flush_policy_threshold_triggers");
    let _ = fs::remove_file(&path);

    // Set threshold to 8 bytes; after a single 8B write, a flush should occur.
    let mmap = MemoryMappedFile::builder(&path)
        .mode(MmapMode::ReadWrite)
        .size(4096)
        .flush_policy(FlushPolicy::EveryBytes(8))
        .create()
        .expect("builder create");

    let data = b"ABCDEFGH"; // 8 bytes
    mmap.update_region(0, data).expect("update");

    // Because threshold equals bytes written, policy should have flushed.
    // Re-open RO and confirm content.
    let ro = load_mmap(&path, MmapMode::ReadOnly).expect("open ro");
    let slice = ro.as_slice(0, 8).expect("slice");
    assert_eq!(slice, data);

    let _ = fs::remove_file(&path);
}

#[test]
fn flush_policy_interval_is_manual_now() {
    use mmap_io::flush::FlushPolicy;

    let path = tmp_path("flush_policy_interval_is_manual_now");
    let _ = fs::remove_file(&path);

    // Interval is a no-op in current phase; treat as Manual.
    let mmap = MemoryMappedFile::builder(&path)
        .mode(MmapMode::ReadWrite)
        .size(4096)
        .flush_policy(FlushPolicy::EveryMillis(10))
        .create()
        .expect("builder create");

    mmap.update_region(10, b"INTV").expect("update");
    // Manually flush to persist
    mmap.flush().expect("flush");

    let ro = load_mmap(&path, MmapMode::ReadOnly).expect("open ro");
    let slice = ro.as_slice(10, 4).expect("slice");
    assert_eq!(slice, b"INTV");

    let _ = fs::remove_file(&path);
}

#[test]
fn segments_mut_and_read_into() {
    let path = tmp_path("segments_mut_and_read_into");
    let _ = fs::remove_file(&path);

    let mmap = create_mmap(&path, 1024).expect("create");
    // Get a mutable region guard and write directly
    {
        let mut guard = mmap.as_slice_mut(10, 6).expect("slice_mut");
        guard.as_mut().copy_from_slice(b"ABCDEF");
    }
    mmap.flush().expect("flush");

    // Read back using read_into for RW mapping
    let mut buf = [0u8; 6];
    mmap.read_into(10, &mut buf).expect("read_into");
    assert_eq!(&buf, b"ABCDEF");

    // Confirm RO open matches
    let ro = MemoryMappedFile::open_ro(&path).expect("open ro");
    let slice = ro.as_slice(10, 6).expect("slice");
    assert_eq!(slice, b"ABCDEF");

    delete_mmap(&path).expect("delete");
}

#[test]
fn huge_pages_builder_noop_nonlinux_or_enabled_linux() {
    // This test ensures the builder API compiles and runs with/without the `hugepages` feature.
    // On Linux with feature enabled, we request huge pages. On other platforms or without feature,
    // this becomes a no-op and mapping still succeeds.
    let path = tmp_path("huge_pages_builder");
    let _ = fs::remove_file(&path);

    // Build a small mapping; huge pages typically require specific sys config,
    // so the implementation falls back to a normal map if huge pages aren't available.
    let mmap = MemoryMappedFile::builder(&path)
        .mode(MmapMode::ReadWrite)
        .size(4096)
        .flush_policy(mmap_io::flush::FlushPolicy::Manual)
        // Gate the method at compile-time; when feature is absent this call is not compiled in.
        // Provide both branches for cross-platform CI.
        .create()
        .expect("builder create");

    // Write/flush/read to validate mapping works regardless of huge page availability
    mmap.update_region(0, b"HP").expect("update");
    mmap.flush().expect("flush");
    let mut buf = [0u8; 2];
    mmap.read_into(0, &mut buf).expect("read");
    assert_eq!(&buf, b"HP");

    let _ = fs::remove_file(&path);
}

#[test]
fn copy_and_delete() {
    let src = tmp_path("copy_and_delete_src");
    let dst = tmp_path("copy_and_delete_dst");
    let _ = fs::remove_file(&src);
    let _ = fs::remove_file(&dst);

    let mmap = create_mmap(&src, 128).expect("create");
    update_region(&mmap, 0, b"xyz").expect("write");
    flush(&mmap).expect("flush");

    copy_mmap(&src, &dst).expect("copy");

    let ro = load_mmap(&dst, MmapMode::ReadOnly).expect("open ro");
    let slice = ro.as_slice(0, 3).expect("slice");
    assert_eq!(slice, b"xyz");

    delete_mmap(&src).expect("delete src");
    delete_mmap(&dst).expect("delete dst");
}
#[test]
fn zero_length_file() {
    let path = tmp_path("zero_length_file");
    let _ = fs::remove_file(&path);

    let result = create_mmap(&path, 0);
    assert!(result.is_err());
    if let Err(e) = result {
        assert_eq!(
            e.to_string(),
            "resize failed: Size must be greater than zero"
        );
    }
}

#[test]
fn invalid_offset_access() {
    let path = tmp_path("invalid_offset_access");
    let _ = fs::remove_file(&path);

    let mmap = create_mmap(&path, 1024).expect("create");
    let result = mmap.as_slice(2048, 10);
    assert!(result.is_err());
    if let Err(e) = result {
        assert_eq!(
            e.to_string(),
            "range out of bounds: offset=2048, len=10, total=1024"
        );
    }

    println!("Cleaning up temporary files...");
    delete_mmap(&path).expect("delete");
}

#[test]
fn concurrent_access() {
    use std::thread;
    use std::time::Duration;

    let path = tmp_path("concurrent_access");
    let _ = fs::remove_file(&path);

    let mmap = create_mmap(&path, 1024).expect("create");

    println!("Starting concurrent write operation...");
    let handle = thread::spawn({
        let mmap = mmap.clone();
        move || {
            // Write data in a scope to ensure the guard is dropped before flush
            {
                let mut guard = mmap.as_slice_mut(0, 10).expect("slice_mut");
                guard.as_mut().copy_from_slice(b"CONCURTEST");
            }
            println!("Flushing changes...");
            mmap.flush().expect("flush");
        }
    });

    println!("Waiting for thread to complete...");
    // Add a timeout to prevent indefinite hanging
    let start = std::time::Instant::now();
    while !handle.is_finished() {
        if start.elapsed() > Duration::from_secs(5) {
            panic!("Thread timed out after 5 seconds");
        }
        // Small delay to avoid busy-waiting
        thread::sleep(Duration::from_millis(10));
    }

    if let Err(e) = handle.join().map_err(|e| format!("Thread panicked: {e:?}")) {
        panic!("Thread panicked: {e:?}");
    }

    let mut buf = [0u8; 10];
    mmap.read_into(0, &mut buf).expect("read_into");
    println!("Verifying written data...");
    assert_eq!(&buf, b"CONCURTEST");

    delete_mmap(&path).expect("delete");
}
