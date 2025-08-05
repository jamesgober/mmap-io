//! Basic integration tests for mmap-io.

use mmap_io::{
    create_mmap, load_mmap, update_region, flush, copy_mmap, delete_mmap, MemoryMappedFile, MmapMode,
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
        assert_eq!(e.to_string(), "resize failed: Size must be greater than zero");
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
        assert_eq!(e.to_string(), "range out of bounds: offset=2048, len=10, total=1024");
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
        thread::sleep(Duration::from_millis(10));
    }
    
    if let Err(e) = handle.join().map_err(|e| format!("Thread panicked: {:?}", e)) {
        panic!("Thread panicked: {:?}", e);
    }

    let mut buf = [0u8; 10];
    mmap.read_into(0, &mut buf).expect("read_into");
    println!("Verifying written data...");
    assert_eq!(&buf, b"CONCURTEST");

    delete_mmap(&path).expect("delete");
}