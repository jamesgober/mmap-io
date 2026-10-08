//! Property tests for `mmap_io::raw` over `(file_len, offset, len)`:
//! in-bounds windows map and match the file byte for byte; anything
//! out of bounds (including overflowing and `u64::MAX` values) returns
//! `InvalidInput`. Nothing may panic.
//!
//! Every file-backed mapping maps a private temporary file, which is
//! the raw constructors' `# Safety` contract.

use std::fs::File;
use std::io::{self, Write};

use mmap_io::raw::{offset_granularity, RawMmapOptions};
use proptest::prelude::*;

fn pattern(len: usize, seed: u8) -> Vec<u8> {
    (0..len).map(|i| ((i % 253) as u8) ^ seed).collect()
}

fn temp_rw(contents: &[u8]) -> File {
    let mut f = tempfile::tempfile().expect("tempfile");
    f.write_all(contents).expect("write");
    f
}

/// File length up to three allocation granules plus a few bytes, so
/// every alignment boundary is reachable on every platform.
fn file_len_strategy() -> impl Strategy<Value = usize> {
    let g = offset_granularity().expect("granularity");
    prop_oneof![
        Just(0usize),
        Just(1usize),
        0..=8usize,
        (g - 2)..=(g + 2),
        0..=(3 * g + 3),
    ]
}

/// Offsets near the file and alignment edges, or arbitrary u64.
fn offset_strategy(file_len: usize) -> impl Strategy<Value = u64> {
    let n = file_len as u64;
    let g = offset_granularity().expect("granularity") as u64;
    prop_oneof![
        0..=n.saturating_add(2),
        Just(n),
        Just(n + 1),
        (0..4u64).prop_map(move |k| k * g),
        (0..4u64).prop_map(move |k| (k * g).saturating_sub(1)),
        any::<u64>(),
        Just(u64::MAX),
    ]
}

/// Lengths near the remaining bytes, arbitrary usize, or "to end".
fn len_strategy(file_len: usize) -> impl Strategy<Value = Option<usize>> {
    prop_oneof![
        Just(None),
        (0..=file_len.saturating_add(2)).prop_map(Some),
        any::<usize>().prop_map(Some),
        Just(Some(usize::MAX)),
        Just(Some(0)),
    ]
}

fn case_strategy() -> impl Strategy<Value = (usize, u64, Option<usize>, u8)> {
    file_len_strategy().prop_flat_map(|n| (Just(n), offset_strategy(n), len_strategy(n), 0u8..3))
}

fn expected(file_len: usize, offset: u64, len: Option<usize>) -> Option<(usize, usize)> {
    if offset > file_len as u64 {
        return None;
    }
    let start = offset as usize;
    let avail = file_len - start;
    match len {
        None => Some((start, avail)),
        Some(l) if l <= avail => Some((start, l)),
        Some(_) => None,
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
    #[test]
    fn map_matches_file_or_errors((file_len, offset, len, mode) in case_strategy()) {
        let data = pattern(file_len, 0x5A);
        let file = temp_rw(&data);
        let mut o = RawMmapOptions::new();
        o.offset(offset);
        if let Some(l) = len {
            o.len(l);
        }
        let want = expected(file_len, offset, len);
        // SAFETY: private temporary file.
        let got: io::Result<Vec<u8>> = unsafe {
            match mode {
                0 => o.map(&file).map(|m| m.to_vec()),
                1 => o.map_mut(&file).map(|m| m.to_vec()),
                _ => o.map_copy(&file).map(|m| m.to_vec()),
            }
        };
        match (want, got) {
            (Some((s, l)), Ok(bytes)) => prop_assert_eq!(&bytes[..], &data[s..s + l]),
            (None, Err(e)) => prop_assert_eq!(e.kind(), io::ErrorKind::InvalidInput),
            (w, g) => prop_assert!(false, "expected {:?}, got {:?}", w, g.map(|b| b.len())),
        }
    }

    #[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
    #[test]
    fn flush_range_ok_iff_in_bounds(
        window_off in 0u64..70_000,
        ranges in proptest::collection::vec((any::<usize>(), any::<usize>(), 0u8..4), 1..24),
    ) {
        let size = 200_000usize;
        let data = pattern(size, 1);
        let file = temp_rw(&data);
        // SAFETY: private temporary file.
        let mut m = unsafe { RawMmapOptions::new().offset(window_off).map_mut(&file) }
            .expect("map_mut");
        let wlen = m.len();
        let mut expect = data.clone();
        for (raw_off, raw_len, shape) in ranges {
            // Mix fully random values with values folded into or just
            // past the window so both outcomes are well covered.
            let (off, len) = match shape {
                0 => (raw_off, raw_len),
                1 => (raw_off % (wlen + 2), raw_len % (wlen + 2)),
                2 => (raw_off % (wlen + 1), raw_len % 9000),
                _ => (wlen.saturating_sub(raw_off % 3), raw_len % 4),
            };
            let in_bounds = off <= wlen && len <= wlen - off;
            if in_bounds {
                for b in &mut m[off..off + len] {
                    *b = b.wrapping_add(1);
                }
                let abs = window_off as usize + off;
                for b in &mut expect[abs..abs + len] {
                    *b = b.wrapping_add(1);
                }
            }
            let r = m.flush_range(off, len);
            prop_assert_eq!(r.is_ok(), in_bounds, "flush_range({}, {}) of {}", off, len, wlen);
            if let Err(e) = r {
                prop_assert_eq!(e.kind(), io::ErrorKind::InvalidInput);
            }
            prop_assert_eq!(m.flush_async_range(off, len).is_ok(), in_bounds);
        }
        drop(m);
        prop_assert!(read_all(&file) == expect);
    }
}

/// Re-read a temp file from the start through a cloned handle.
fn read_all(file: &File) -> Vec<u8> {
    use std::io::{Read, Seek};
    let mut f = file.try_clone().expect("clone handle");
    f.seek(io::SeekFrom::Start(0)).expect("seek");
    let mut out = Vec::new();
    f.read_to_end(&mut out).expect("read");
    out
}
