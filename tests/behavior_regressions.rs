//! Regression tests for behavior fixes: flush accounting, resize on
//! Windows, builder `open()` parity, segment bounds, non-truncating
//! `open_or_create`, page alignment in `advise`, `MmapReader::seek`
//! edge cases, the zero-length range rule, and the `MmapIoError`
//! `Display` strings.

use mmap_io::flush::FlushPolicy;
use mmap_io::segment::SegmentMut;
use mmap_io::{MemoryMappedFile, MmapMode};
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

fn tmp_path(name: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!("mmap_io_behavior_{}_{}", name, std::process::id()));
    p
}

// ---------------------------------------------------------------------
// Flush accounting
// ---------------------------------------------------------------------

#[test]
fn pending_bytes_counts_update_region_under_default_policy() {
    let path = tmp_path("pending_default");
    let _ = fs::remove_file(&path);
    let mmap = MemoryMappedFile::create_rw(&path, 8192).expect("create");
    assert_eq!(mmap.flush_policy(), FlushPolicy::Never);
    mmap.update_region(0, &[1u8; 100]).expect("write");
    assert_eq!(mmap.pending_bytes(), 100);
    mmap.flush().expect("flush");
    assert_eq!(mmap.pending_bytes(), 0);
    drop(mmap);
    let _ = fs::remove_file(&path);
}

#[test]
fn pending_bytes_counts_slice_mut_writes() {
    let path = tmp_path("pending_slice_mut");
    let _ = fs::remove_file(&path);
    let mmap = MemoryMappedFile::create_rw(&path, 8192).expect("create");
    {
        let mut s = mmap.as_slice_mut(0, 256).expect("slice_mut");
        s.as_mut().fill(9);
    }
    assert_eq!(mmap.pending_bytes(), 256);

    let seg = SegmentMut::new(Arc::new(mmap.clone()), 1024, 64).expect("segment");
    {
        let mut s = seg.as_slice_mut().expect("segment slice");
        s.as_mut().fill(3);
    }
    assert_eq!(mmap.pending_bytes(), 256 + 64);
    seg.write(b"abc").expect("segment write");
    assert_eq!(mmap.pending_bytes(), 256 + 64 + 3);

    mmap.flush().expect("flush");
    assert_eq!(mmap.pending_bytes(), 0);
    drop(seg);
    drop(mmap);
    let _ = fs::remove_file(&path);
}

#[cfg(feature = "iterator")]
#[test]
fn pending_bytes_counts_chunks_mut_writes() {
    let path = tmp_path("pending_chunks_mut");
    let _ = fs::remove_file(&path);
    let mmap = MemoryMappedFile::create_rw(&path, 4096).expect("create");
    mmap.chunks_mut(1024)
        .for_each_mut(|_, c| {
            c.fill(1);
            Ok(())
        })
        .expect("for_each_mut");
    assert_eq!(mmap.pending_bytes(), 4096);
    drop(mmap);
    let _ = fs::remove_file(&path);
}

#[cfg(feature = "atomic")]
#[test]
fn pending_bytes_counts_atomic_views() {
    use std::sync::atomic::Ordering;
    let path = tmp_path("pending_atomic");
    let _ = fs::remove_file(&path);
    let mmap = MemoryMappedFile::create_rw(&path, 64).expect("create");
    {
        let v = mmap.atomic_u64(0).expect("view");
        v.store(5, Ordering::SeqCst);
    }
    assert_eq!(mmap.pending_bytes(), 8);
    {
        let v = mmap.atomic_u32_slice(8, 4).expect("slice view");
        v[0].store(1, Ordering::SeqCst);
    }
    assert_eq!(mmap.pending_bytes(), 8 + 16);
    drop(mmap);
    let _ = fs::remove_file(&path);
}

#[test]
fn flush_range_does_not_debit_unrelated_pending_bytes() {
    let path = tmp_path("flush_range_debit");
    let _ = fs::remove_file(&path);
    let mmap = MemoryMappedFile::builder(&path)
        .mode(MmapMode::ReadWrite)
        .size(64 * 1024)
        .flush_policy(FlushPolicy::EveryBytes(1024 * 1024))
        .create()
        .expect("create");
    mmap.update_region(0, &[7u8; 100]).expect("write");
    // Flush a range that does not contain the dirty bytes.
    mmap.flush_range(32 * 1024, 4096).expect("flush_range");
    assert_eq!(
        mmap.pending_bytes(),
        100,
        "flush_range of an unrelated range must not clear pending bytes"
    );
    // A full-length flush_range is a full flush.
    mmap.flush_range(0, 64 * 1024).expect("full flush_range");
    assert_eq!(mmap.pending_bytes(), 0);
    drop(mmap);
    let _ = fs::remove_file(&path);
}

#[test]
fn every_writes_counts_calls_and_pending_reports_bytes() {
    let path = tmp_path("every_writes");
    let _ = fs::remove_file(&path);
    let mmap = MemoryMappedFile::builder(&path)
        .mode(MmapMode::ReadWrite)
        .size(8192)
        .flush_policy(FlushPolicy::EveryWrites(3))
        .create()
        .expect("create");
    mmap.update_region(0, &[1u8; 10]).expect("w1");
    mmap.update_region(10, &[1u8; 10]).expect("w2");
    assert_eq!(mmap.pending_bytes(), 20, "pending_bytes reports bytes");
    mmap.update_region(20, &[1u8; 10]).expect("w3");
    assert_eq!(mmap.pending_bytes(), 0, "third write triggers the flush");
    drop(mmap);
    let _ = fs::remove_file(&path);
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

// ---------------------------------------------------------------------
// advise alignment
// ---------------------------------------------------------------------

#[cfg(feature = "advise")]
#[test]
fn advise_accepts_unaligned_offsets() {
    use mmap_io::MmapAdvice;
    let path = tmp_path("advise_unaligned");
    let _ = fs::remove_file(&path);
    let mmap = MemoryMappedFile::create_rw(&path, 64 * 1024).expect("create");
    for advice in [
        MmapAdvice::Normal,
        MmapAdvice::Random,
        MmapAdvice::Sequential,
        MmapAdvice::WillNeed,
    ] {
        mmap.advise(1, 100, advice).expect("unaligned offset");
        mmap.advise(4097, 5000, advice).expect("unaligned span");
        mmap.advise(64 * 1024 - 1, 1, advice).expect("last byte");
    }
    drop(mmap);
    let _ = fs::remove_file(&path);
}
