//! Property tests for bounds-checking on the byte-level mmap API.
//!
//! These tests shake out edge cases in `as_slice`, `as_slice_mut`,
//! `read_into`, `update_region`, and `flush_range` by generating
//! random `(offset, len)` pairs against random file sizes and
//! verifying that:
//!
//! 1. Every in-bounds request either succeeds or fails for a documented
//!    reason (e.g., `as_slice` on RW returns `InvalidMode`, not
//!    `OutOfBounds`).
//! 2. Every out-of-bounds request fails with `MmapIoError::OutOfBounds`
//!    carrying the requested `(offset, len, total)`.
//! 3. Zero-length requests are always accepted (whether they touch
//!    anything or not).
//! 4. `read_into` does not write past the end of its destination
//!    buffer when bounds checks pass.
//! 5. `update_region` round-trips its bytes via `read_into` for valid
//!    ranges (data integrity).
//!
//! These properties are run with at least 1000 cases per `proptest!`
//! invocation. Set `PROPTEST_CASES=10000` in the environment for the
//! deep-test sweep.

use mmap_io::{errors::MmapIoError, MemoryMappedFile};
use proptest::prelude::*;
use std::path::PathBuf;

/// Test file sizes. Kept small enough that 1000+ cases run in a
/// reasonable time on CI, large enough to span at least a few pages on
/// every supported platform.
const MIN_FILE: u64 = 64;
const MAX_FILE: u64 = 64 * 1024; // 64 KiB

/// Generate a unique-per-call temp path. We avoid `tempfile`'s
/// per-handle teardown because that interacts poorly with our explicit
/// mmap drop ordering on Windows.
fn tmp_path(tag: &str, seed: u64) -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!(
        "mmap_io_proptest_bounds_{}_{}_{}",
        tag,
        std::process::id(),
        seed
    ));
    p
}

/// Build a fresh RW mapping of the given size, filled with zeroes.
fn rw_mmap(size: u64, tag: &str, seed: u64) -> (MemoryMappedFile, PathBuf) {
    let path = tmp_path(tag, seed);
    let _ = std::fs::remove_file(&path);
    let mmap = MemoryMappedFile::create_rw(&path, size).expect("create_rw");
    (mmap, path)
}

/// Build a fresh RO mapping by creating an RW file, writing a known
/// pattern, dropping, then re-opening read-only.
fn ro_mmap(size: u64, tag: &str, seed: u64) -> (MemoryMappedFile, PathBuf) {
    let path = tmp_path(tag, seed);
    let _ = std::fs::remove_file(&path);
    {
        let rw = MemoryMappedFile::create_rw(&path, size).expect("create_rw");
        // Fill with a deterministic pattern so we can verify reads.
        let payload: Vec<u8> = (0..size).map(|i| (i & 0xFF) as u8).collect();
        if !payload.is_empty() {
            rw.update_region(0, &payload).expect("update_region");
        }
        rw.flush().expect("flush");
        drop(rw);
    }
    let mmap = MemoryMappedFile::open_ro(&path).expect("open_ro");
    (mmap, path)
}

/// Predicate: would the request `(offset, len)` against a file of
/// `total` bytes be in-bounds?
fn is_in_bounds(offset: u64, len: u64, total: u64) -> bool {
    offset
        .checked_add(len)
        .map(|end| end <= total)
        .unwrap_or(false)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(1024))]

    /// `as_slice` on an RO mapping must accept every in-bounds request
    /// and return a slice of the requested length, OR reject with
    /// OutOfBounds for any range that exceeds the file. No other error
    /// variant is reachable on the RO path.
    #[test]
    fn as_slice_ro_bounds(
        size in MIN_FILE..MAX_FILE,
        offset in 0u64..MAX_FILE * 2,
        len in 0u64..MAX_FILE * 2,
    ) {
        let (mmap, path) = ro_mmap(size, "as_slice_ro", offset.wrapping_add(len));
        let result = mmap.as_slice(offset, len);

        if is_in_bounds(offset, len, size) {
            let s = result.expect("in-bounds as_slice");
            prop_assert_eq!(s.len() as u64, len);
            // Verify content matches the deterministic pattern we
            // wrote during setup (offset i has byte (i & 0xFF)).
            for (i, byte) in s.iter().enumerate() {
                let want = ((offset as usize + i) & 0xFF) as u8;
                prop_assert_eq!(*byte, want, "byte mismatch at offset+{}", i);
            }
        } else {
            match result {
                Err(MmapIoError::OutOfBounds { offset: e_off, len: e_len, total }) => {
                    prop_assert_eq!(e_off, offset);
                    prop_assert_eq!(e_len, len);
                    prop_assert_eq!(total, size);
                }
                other => prop_assert!(
                    false,
                    "expected OutOfBounds, got {:?}",
                    other
                ),
            }
        }

        drop(mmap);
        let _ = std::fs::remove_file(&path);
    }

    /// `as_slice` on an RW mapping must always return `InvalidMode`
    /// (the documented behavior - RW callers use `read_into`). No
    /// bounds-related error is reachable on this path.
    #[test]
    fn as_slice_rw_invalid_mode(
        size in MIN_FILE..MAX_FILE,
        offset in 0u64..MAX_FILE * 2,
        len in 0u64..MAX_FILE * 2,
    ) {
        let (mmap, path) = rw_mmap(size, "as_slice_rw", offset.wrapping_add(len));
        let result = mmap.as_slice(offset, len);

        // The crate decides bounds first OR mode first depending on
        // the implementation. Either is acceptable as long as the
        // error is one of the documented variants.
        match result {
            Err(MmapIoError::InvalidMode(_)) => { /* expected */ }
            Err(MmapIoError::OutOfBounds { .. }) => {
                prop_assert!(
                    !is_in_bounds(offset, len, size),
                    "OutOfBounds returned for in-bounds RW as_slice request"
                );
            }
            Err(other) => prop_assert!(
                false,
                "unexpected error variant: {:?}",
                other
            ),
            Ok(_) => prop_assert!(
                false,
                "RW as_slice unexpectedly returned Ok"
            ),
        }

        drop(mmap);
        let _ = std::fs::remove_file(&path);
    }

    /// `as_slice_mut` on an RW mapping mirrors `as_slice` on RO: it
    /// accepts every in-bounds request and rejects every OOB one with
    /// OutOfBounds.
    #[test]
    fn as_slice_mut_bounds(
        size in MIN_FILE..MAX_FILE,
        offset in 0u64..MAX_FILE * 2,
        len in 0u64..MAX_FILE * 2,
    ) {
        let (mmap, path) = rw_mmap(size, "as_slice_mut", offset.wrapping_add(len));
        // MappedSliceMut does not implement Debug, so we can't unwrap;
        // pattern-match the success path instead.
        {
            let result = mmap.as_slice_mut(offset, len);
            if is_in_bounds(offset, len, size) {
                match result {
                    Ok(mut guard) => {
                        // Verify the mut slice has the right length.
                        prop_assert_eq!(guard.as_mut().len() as u64, len);
                    }
                    Err(e) => prop_assert!(
                        false,
                        "expected Ok for in-bounds as_slice_mut, got {:?}",
                        e
                    ),
                }
            } else {
                match result {
                    Err(MmapIoError::OutOfBounds { offset: e_off, len: e_len, total }) => {
                        prop_assert_eq!(e_off, offset);
                        prop_assert_eq!(e_len, len);
                        prop_assert_eq!(total, size);
                    }
                    Ok(_) => prop_assert!(false, "expected OutOfBounds, got Ok"),
                    Err(other) => prop_assert!(
                        false,
                        "expected OutOfBounds, got {:?}",
                        other
                    ),
                }
            }
        } // result/guard dropped here, releasing the write lock

        drop(mmap);
        let _ = std::fs::remove_file(&path);
    }

    /// `read_into` honors the destination buffer's length. The buffer
    /// is fully written for in-bounds reads; OOB returns OutOfBounds.
    /// We also verify that a one-byte sentinel placed past the read
    /// region is NOT modified (no buffer overrun).
    #[test]
    fn read_into_bounds_and_no_overrun(
        size in MIN_FILE..MAX_FILE,
        offset in 0u64..MAX_FILE * 2,
        len in 0usize..(MAX_FILE * 2) as usize,
    ) {
        let (mmap, path) = ro_mmap(size, "read_into", offset.wrapping_add(len as u64));

        // Destination is `len + 1` bytes; we read `len` and check the
        // last byte stays as the sentinel.
        let sentinel = 0xCDu8;
        let mut dst = vec![sentinel; len + 1];
        let result = mmap.read_into(offset, &mut dst[..len]);

        if is_in_bounds(offset, len as u64, size) {
            result.expect("in-bounds read_into");
            prop_assert_eq!(dst[len], sentinel, "read_into wrote past end of buffer");
            // Verify the read content matches the deterministic pattern.
            for (i, byte) in dst[..len].iter().enumerate() {
                let want = ((offset as usize + i) & 0xFF) as u8;
                prop_assert_eq!(*byte, want, "byte mismatch at offset+{}", i);
            }
        } else if len > 0 {
            // Zero-length read is fine even past the end; nonzero
            // must error.
            prop_assert!(
                matches!(result, Err(MmapIoError::OutOfBounds { .. })),
                "expected OutOfBounds, got {:?}",
                result
            );
            prop_assert_eq!(dst[len], sentinel, "read_into wrote past end of buffer on error path");
        }

        drop(mmap);
        let _ = std::fs::remove_file(&path);
    }

    /// `update_region` rejects out-of-bounds writes. For in-bounds
    /// writes, the data must round-trip through `read_into`.
    #[test]
    fn update_region_round_trip(
        size in MIN_FILE..MAX_FILE,
        offset in 0u64..MAX_FILE * 2,
        data in proptest::collection::vec(any::<u8>(), 0..1024),
    ) {
        let (mmap, path) = rw_mmap(size, "update_round_trip", offset.wrapping_add(data.len() as u64));
        let len = data.len() as u64;
        let result = mmap.update_region(offset, &data);

        if is_in_bounds(offset, len, size) {
            result.expect("in-bounds update_region");
            // Read back and verify.
            if !data.is_empty() {
                let mut buf = vec![0u8; data.len()];
                mmap.read_into(offset, &mut buf).expect("read_into after update");
                prop_assert_eq!(&buf, &data);
            }
        } else if !data.is_empty() {
            prop_assert!(
                matches!(result, Err(MmapIoError::OutOfBounds { .. })),
                "expected OutOfBounds, got {:?}",
                result
            );
        }

        drop(mmap);
        let _ = std::fs::remove_file(&path);
    }

    /// `flush_range` bounds: in-bounds ranges succeed (assuming
    /// FlushPolicy doesn't introduce additional errors, which it
    /// doesn't), OOB returns OutOfBounds. Zero-length is always OK.
    #[test]
    fn flush_range_bounds(
        size in MIN_FILE..MAX_FILE,
        offset in 0u64..MAX_FILE * 2,
        len in 0u64..MAX_FILE * 2,
    ) {
        let (mmap, path) = rw_mmap(size, "flush_range", offset.wrapping_add(len));

        // Dirty a small region first so flush_range has something to
        // actually do, though its bounds behavior is the same either way.
        if size >= 4 {
            let _ = mmap.update_region(0, b"hi!\0");
        }

        let result = mmap.flush_range(offset, len);

        if is_in_bounds(offset, len, size) || len == 0 {
            result.expect("in-bounds flush_range");
        } else {
            prop_assert!(
                matches!(result, Err(MmapIoError::OutOfBounds { .. })),
                "expected OutOfBounds for flush_range, got {:?}",
                result
            );
        }

        drop(mmap);
        let _ = std::fs::remove_file(&path);
    }

    /// Boundary-condition focus: tests with offset/len picked to land
    /// exactly at file boundaries. This is where overflow bugs (e.g.,
    /// `offset + len` wrapping) historically lurked.
    #[test]
    fn boundary_conditions(
        size in MIN_FILE..MAX_FILE,
        edge_pick in 0u8..8,
    ) {
        let (mmap, path) = rw_mmap(size, "boundary", edge_pick as u64);

        // Picks: 0/0, 0/size, size/0, size-1/1, size/1, (size-1)/2,
        //        u64::MAX-size/size+1 (wraps), u64::MAX/0
        let (off, len) = match edge_pick {
            0 => (0u64, 0u64),
            1 => (0, size),
            2 => (size, 0),
            3 => (size - 1, 1),
            4 => (size, 1),       // off-by-one OOB
            5 => (size - 1, 2),   // off-by-one OOB
            6 => (u64::MAX - size + 2, size),  // wrapping add OOB
            _ => (u64::MAX, 0),
        };

        let in_bounds = is_in_bounds(off, len, size);
        let result_len = len;

        // Round-trip via read_into for in-bounds; expect OOB otherwise.
        let mut buf = vec![0u8; len.min(1024) as usize];
        let truncated_len = buf.len() as u64;
        let read_result = mmap.read_into(off, &mut buf);

        if in_bounds && truncated_len == len {
            read_result.expect("boundary in-bounds read");
        } else if len > 0 && truncated_len > 0 {
            // The buffer may be smaller than `len`; we read
            // truncated_len bytes only. If THAT slice is in bounds,
            // the read succeeds.
            let trunc_in_bounds = is_in_bounds(off, truncated_len, size);
            if trunc_in_bounds {
                read_result.expect("boundary truncated read");
            } else {
                prop_assert!(
                    matches!(read_result, Err(MmapIoError::OutOfBounds { .. })),
                    "expected OutOfBounds, got {:?}",
                    read_result
                );
            }
        }

        // Avoid unused warning when buf is empty.
        let _ = (result_len, &buf);

        drop(mmap);
        let _ = std::fs::remove_file(&path);
    }
}
