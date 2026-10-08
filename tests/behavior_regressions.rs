//! Regression tests for behavior fixes: flush accounting, resize on
//! Windows, builder `open()` parity, segment bounds, non-truncating
//! `open_or_create`, page alignment in `advise`, `MmapReader::seek`
//! edge cases, the zero-length range rule, and the `MmapIoError`
//! `Display` strings.

use mmap_io::flush::FlushPolicy;
use mmap_io::segment::SegmentMut;
use mmap_io::{MemoryMappedFile, MmapIoError, MmapMode};
use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

mod common;
use common::tmp_path;

fn read_file(path: &Path) -> Vec<u8> {
    fs::read(path).expect("read file")
}

// ---------------------------------------------------------------------
// Flush accounting
// ---------------------------------------------------------------------

#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
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

#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
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
#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
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
#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
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

#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
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

#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
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

#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
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
// Builder parity
// ---------------------------------------------------------------------

fn wait_until(timeout: Duration, mut pred: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if pred() {
            return true;
        }
        thread::sleep(Duration::from_millis(10));
    }
    pred()
}

#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
#[test]
fn builder_open_starts_time_based_flusher() {
    let path = tmp_path("builder_open_millis");
    let _ = fs::remove_file(&path);
    fs::write(&path, vec![0u8; 4096]).expect("seed file");
    let mmap = MemoryMappedFile::builder(&path)
        .mode(MmapMode::ReadWrite)
        .flush_policy(FlushPolicy::EveryMillis(20))
        .open()
        .expect("open");
    mmap.update_region(0, b"hello").expect("write");
    assert!(
        wait_until(Duration::from_secs(3), || mmap.pending_bytes() == 0),
        "EveryMillis flusher never ran for a builder-opened mapping"
    );
    drop(mmap);
    let _ = fs::remove_file(&path);
}

#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
#[test]
fn builder_open_or_create_existing_file_defaults_to_read_write() {
    let path = tmp_path("builder_ooc_rw");
    let _ = fs::remove_file(&path);
    fs::write(&path, vec![1u8; 4096]).expect("seed file");
    let mmap = MemoryMappedFile::builder(&path)
        .size(1 << 20)
        .flush_policy(FlushPolicy::EveryMillis(20))
        .open_or_create()
        .expect("open_or_create");
    assert_eq!(mmap.mode(), MmapMode::ReadWrite);
    assert_eq!(mmap.len(), 4096, "existing file keeps its length");
    mmap.update_region(0, b"x").expect("write");
    assert!(wait_until(Duration::from_secs(3), || mmap.pending_bytes() == 0));
    drop(mmap);
    let _ = fs::remove_file(&path);
}

// ---------------------------------------------------------------------
// open_or_create / create_mmap_async
// ---------------------------------------------------------------------

#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
#[test]
fn open_or_create_preserves_existing_data() {
    let path = tmp_path("ooc_preserve");
    let _ = fs::remove_file(&path);
    fs::write(&path, b"keep me").expect("seed file");
    let mmap = MemoryMappedFile::open_or_create(&path, 4096).expect("open_or_create");
    assert_eq!(mmap.len(), 7);
    assert_eq!(mmap.as_slice(0, 7).expect("slice"), b"keep me");
    drop(mmap);
    assert_eq!(read_file(&path), b"keep me");
    let _ = fs::remove_file(&path);
}

#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
#[test]
fn open_or_create_sizes_existing_empty_file() {
    let path = tmp_path("ooc_empty");
    let _ = fs::remove_file(&path);
    fs::write(&path, b"").expect("seed empty file");
    let mmap = MemoryMappedFile::open_or_create(&path, 4096).expect("open_or_create");
    assert_eq!(mmap.len(), 4096);
    drop(mmap);
    let _ = fs::remove_file(&path);
}

#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
#[test]
fn open_or_create_rejects_zero_default_for_new_file() {
    let path = tmp_path("ooc_zero");
    let _ = fs::remove_file(&path);
    assert!(matches!(
        MemoryMappedFile::open_or_create(&path, 0),
        Err(MmapIoError::ResizeFailed(_))
    ));
    assert!(
        !path.exists(),
        "a failed open_or_create must not leave a file"
    );
}

#[cfg(feature = "async")]
#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
#[tokio::test(flavor = "multi_thread")]
async fn create_mmap_async_validates_before_truncating() {
    use mmap_io::manager::r#async::create_mmap_async;
    let path = tmp_path("async_create_validate");
    let _ = fs::remove_file(&path);
    fs::write(&path, b"precious").expect("seed file");
    let r = create_mmap_async(&path, 0).await;
    assert!(matches!(r, Err(MmapIoError::ResizeFailed(_))));
    assert_eq!(
        read_file(&path),
        b"precious",
        "file truncated before validation"
    );
    let _ = fs::remove_file(&path);
}

// ---------------------------------------------------------------------
// SegmentMut bounds
// ---------------------------------------------------------------------

#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
#[test]
fn segment_mut_write_rejects_data_longer_than_segment() {
    let path = tmp_path("segment_write_bounds");
    let _ = fs::remove_file(&path);
    let mmap = Arc::new(MemoryMappedFile::create_rw(&path, 1024).expect("create"));
    let seg = SegmentMut::new(Arc::clone(&mmap), 0, 4).expect("segment");
    match seg.write(b"too long") {
        Err(MmapIoError::OutOfBounds { offset, len, total }) => {
            assert_eq!((offset, len, total), (0, 8, 4));
        }
        other => panic!("expected OutOfBounds, got {other:?}"),
    }
    // The bytes past the segment must be untouched.
    assert_eq!(mmap.as_slice(4, 4).expect("slice"), &[0u8; 4]);
    seg.write(b"ok").expect("short write fits");
    drop(seg);
    drop(mmap);
    let _ = fs::remove_file(&path);
}

// ---------------------------------------------------------------------
// advise alignment
// ---------------------------------------------------------------------

#[cfg(feature = "advise")]
#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
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

// ---------------------------------------------------------------------
// MmapReader / utils
// ---------------------------------------------------------------------

#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
#[test]
fn reader_seek_rejects_negative_and_overflowing_positions() {
    let path = tmp_path("reader_seek");
    let _ = fs::remove_file(&path);
    let mmap = MemoryMappedFile::create_rw(&path, 100).expect("create");
    let mut r = mmap.reader();

    let e = r.seek(SeekFrom::End(i64::MIN)).expect_err("End(i64::MIN)");
    assert_eq!(e.kind(), std::io::ErrorKind::InvalidInput);
    assert_eq!(r.position(), 0, "failed seek leaves the cursor unchanged");

    let e = r.seek(SeekFrom::Current(-1)).expect_err("before start");
    assert_eq!(e.kind(), std::io::ErrorKind::InvalidInput);

    assert_eq!(r.seek(SeekFrom::End(-10)).expect("end-10"), 90);
    assert_eq!(r.seek(SeekFrom::Current(-90)).expect("to 0"), 0);
    assert_eq!(r.seek(SeekFrom::End(5)).expect("past end"), 105);
    let mut buf = [0u8; 4];
    assert_eq!(r.read(&mut buf).expect("read at EOF"), 0);

    r.set_position(u64::MAX);
    let e = r.seek(SeekFrom::Current(1)).expect_err("overflow");
    assert_eq!(e.kind(), std::io::ErrorKind::InvalidInput);
    assert_eq!(
        r.seek(SeekFrom::Current(i64::MIN)).expect("back"),
        u64::MAX - (1u64 << 63)
    );

    drop(mmap);
    let _ = fs::remove_file(&path);
}

#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
#[test]
fn align_up_saturates_instead_of_overflowing() {
    use mmap_io::utils::align_up;
    assert_eq!(align_up(u64::MAX, 4096), u64::MAX);
    assert_eq!(align_up(u64::MAX - 1, 3), u64::MAX);
    assert_eq!(align_up(u64::MAX - 4095, 4096), u64::MAX - 4095);
    assert_eq!(align_up(1, 4096), 4096);
}

// ---------------------------------------------------------------------
// Zero-length range rule
// ---------------------------------------------------------------------

#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
#[test]
fn zero_length_requests_are_accepted_at_any_offset() {
    let path = tmp_path("zero_len_rule");
    let _ = fs::remove_file(&path);
    let size = 4096u64;
    let mmap = MemoryMappedFile::create_rw(&path, size).expect("create");
    for off in [0, size, size + 1, u64::MAX] {
        assert!(mmap.as_slice(off, 0).expect("as_slice").is_empty());
        assert!(mmap.as_slice_mut(off, 0).expect("as_slice_mut").is_empty());
        mmap.read_into(off, &mut []).expect("read_into");
        mmap.update_region(off, &[]).expect("update_region");
        mmap.flush_range(off, 0).expect("flush_range");
        mmap.touch_pages_range(off, 0).expect("touch_pages_range");
        mmap.prefetch_range(off, 0).expect("prefetch_range");
        let seg = SegmentMut::new(Arc::new(mmap.clone()), off, 0).expect("segment");
        assert!(seg.is_valid());
    }
    drop(mmap);

    let ro = MemoryMappedFile::open_ro(&path).expect("open_ro");
    assert!(ro.as_slice_bytes(size + 10, 0).expect("bytes").is_empty());
    drop(ro);
    let _ = fs::remove_file(&path);
}

// ---------------------------------------------------------------------
// Error Display
// ---------------------------------------------------------------------

#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
#[test]
fn error_display_strings_are_stable() {
    use std::error::Error as _;
    let io = MmapIoError::from(std::io::Error::other("boom"));
    assert_eq!(io.to_string(), "I/O error: boom");
    assert!(io.source().is_some(), "Io keeps its source");

    let cases: Vec<(MmapIoError, &str)> = vec![
        (
            MmapIoError::InvalidMode("nope"),
            "invalid access mode: nope",
        ),
        (
            MmapIoError::OutOfBounds {
                offset: 1,
                len: 2,
                total: 3,
            },
            "range out of bounds: offset=1, len=2, total=3",
        ),
        (MmapIoError::FlushFailed("f".into()), "flush failed: f"),
        (MmapIoError::ResizeFailed("r".into()), "resize failed: r"),
        (MmapIoError::AdviceFailed("a".into()), "advice failed: a"),
        (MmapIoError::LockFailed("l".into()), "lock failed: l"),
        (MmapIoError::UnlockFailed("u".into()), "unlock failed: u"),
        (
            MmapIoError::Misaligned {
                required: 8,
                offset: 3,
            },
            "atomic alignment error: required=8, offset=3",
        ),
        (MmapIoError::WatchFailed("w".into()), "watch failed: w"),
    ];
    for (err, want) in cases {
        assert_eq!(err.to_string(), want);
        assert!(err.source().is_none(), "{want} has no source");
    }
}

#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
#[test]
fn reader_reads_whole_file() {
    let path = tmp_path("reader_whole");
    let _ = fs::remove_file(&path);
    let mmap = MemoryMappedFile::create_rw(&path, 10).expect("create");
    mmap.update_region(0, b"0123456789").expect("write");
    let mut out = Vec::new();
    mmap.reader().read_to_end(&mut out).expect("read_to_end");
    assert_eq!(out, b"0123456789");
    drop(mmap);
    let _ = fs::remove_file(&path);
}
