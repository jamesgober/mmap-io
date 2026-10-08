//! Regression tests for behavior fixes: flush accounting, resize on
//! Windows, builder `open()` parity, segment bounds, non-truncating
//! `open_or_create`, page alignment in `advise`, `MmapReader::seek`
//! edge cases, the zero-length range rule, and the `MmapIoError`
//! `Display` strings.

use mmap_io::MemoryMappedFile;
use std::fs;
use std::path::PathBuf;

fn tmp_path(name: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!("mmap_io_behavior_{}_{}", name, std::process::id()));
    p
}

// ---------------------------------------------------------------------
// Resize
// ---------------------------------------------------------------------

#[test]
fn shrink_truncates_file_and_regrow_reads_zeros() {
    let path = tmp_path("shrink_regrow");
    let _ = fs::remove_file(&path);
    let mmap = MemoryMappedFile::create_rw(&path, 8192).expect("create");
    mmap.update_region(0, &[0xAA; 8192]).expect("fill");
    mmap.flush().expect("flush");

    mmap.resize(4096).expect("shrink");
    assert_eq!(mmap.len(), 4096);
    assert_eq!(
        fs::metadata(&path).expect("meta").len(),
        4096,
        "shrink must truncate the backing file"
    );

    mmap.resize(8192).expect("grow after shrink");
    assert_eq!(mmap.len(), 8192);
    let mut head = vec![0u8; 4096];
    mmap.read_into(0, &mut head).expect("read head");
    assert!(head.iter().all(|&b| b == 0xAA), "surviving bytes preserved");
    let mut tail = vec![0xFFu8; 4096];
    mmap.read_into(4096, &mut tail).expect("read tail");
    assert!(
        tail.iter().all(|&b| b == 0),
        "bytes cut off by the shrink must not come back"
    );
    drop(mmap);
    let _ = fs::remove_file(&path);
}
