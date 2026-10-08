//! Checked offset and length arithmetic for the raw mapping layer.
//!
//! Every function here is pure: no syscalls, no pointers, no `unsafe`.
//! The platform backends only ever see values that went through these
//! functions, so every `(offset, len)` pair handed to `mmap`,
//! `MapViewOfFile`, `msync` or `FlushViewOfFile` has been validated
//! with checked arithmetic first. This is the bug class behind
//! RUSTSEC-2026-0186 (unchecked `offset + len` in `flush_range` /
//! `advise_range`), so the checks live in one small, separately
//! tested place.
//!
//! The functions take the platform limits (`max_len`, alignment,
//! page size) as parameters instead of reading them from the OS so
//! the unit tests can exercise 32-bit limits on a 64-bit host and so
//! the whole module runs under Miri.

use std::io;

/// Layout of one OS mapping that covers a caller's window.
///
/// The OS requires the file offset of a mapping to be a multiple of
/// its offset granularity (page size on Unix, allocation granularity
/// on Windows). The raw layer maps from `aligned_offset`, which is the
/// caller's offset rounded down, and hides the first `delta` bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Layout {
    /// File offset handed to the OS. A multiple of the granularity.
    pub(crate) aligned_offset: u64,
    /// Bytes between the start of the OS mapping and the caller's
    /// window. Always strictly less than the granularity.
    pub(crate) delta: usize,
    /// Length of the caller's window.
    pub(crate) len: usize,
    /// Length handed to the OS: `delta + len`. Never above the
    /// `max_len` passed to [`layout`].
    pub(crate) map_len: usize,
}

/// Largest mapping length the raw layer accepts on this target.
///
/// `slice::from_raw_parts` requires the total size to be at most
/// `isize::MAX` bytes, which matters on 32-bit targets where a 3 GiB
/// file is ordinary.
pub(crate) const MAX_LEN: usize = isize::MAX as usize;

fn invalid(msg: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, msg)
}

/// Resolve the window length for a file-backed mapping.
///
/// * `file_len`: current size of the file in bytes.
/// * `offset`: caller-requested start of the window.
/// * `len`: caller-requested length, or `None` for "to end of file".
/// * `max_len`: target limit (normally [`MAX_LEN`]).
///
/// # Errors
///
/// `InvalidInput` when `offset > file_len`, when `offset + len`
/// overflows or exceeds `file_len` (mapping past end of file invites
/// `SIGBUS` on Unix and fails on Windows), or when the resulting
/// length does not fit in `max_len`.
pub(crate) fn resolve_len(
    file_len: u64,
    offset: u64,
    len: Option<usize>,
    max_len: usize,
) -> io::Result<usize> {
    let available = file_len.checked_sub(offset).ok_or_else(|| {
        invalid(format!(
            "mapping offset {offset} is past the end of the file ({file_len} bytes)"
        ))
    })?;
    let len = match len {
        Some(requested) => {
            // usize is at most 64 bits on every Rust target, so this
            // conversion only fails on a hypothetical 128-bit target;
            // treat that as out of range instead of truncating.
            let requested_u64 = u64::try_from(requested)
                .map_err(|_| invalid(format!("mapping length {requested} does not fit in u64")))?;
            if requested_u64 > available {
                return Err(invalid(format!(
                    "mapping range offset {offset} + length {requested} extends past the \
                     end of the file ({file_len} bytes)"
                )));
            }
            requested
        }
        None => usize::try_from(available).map_err(|_| {
            invalid(format!(
                "file window of {available} bytes does not fit in this target's address space"
            ))
        })?,
    };
    if len > max_len {
        return Err(invalid(format!(
            "mapping length {len} exceeds the largest addressable slice ({max_len} bytes)"
        )));
    }
    Ok(len)
}

/// Compute the OS mapping layout for a window of `len` bytes at file
/// offset `offset`, given the OS offset granularity `granularity`.
///
/// # Errors
///
/// `InvalidInput` when `granularity` is not a non-zero power of two
/// (a broken OS report; never divide by it in that case), or when
/// `delta + len` overflows or exceeds `max_len`.
pub(crate) fn layout(
    offset: u64,
    len: usize,
    granularity: usize,
    max_len: usize,
) -> io::Result<Layout> {
    if !granularity.is_power_of_two() {
        return Err(invalid(format!(
            "OS reported an invalid mapping granularity of {granularity} bytes"
        )));
    }
    let gran_u64 = u64::try_from(granularity)
        .map_err(|_| invalid(format!("granularity {granularity} does not fit in u64")))?;
    // `granularity` is a non-zero power of two, so the mask is exact.
    let delta_u64 = offset & (gran_u64 - 1);
    // `delta_u64 < granularity <= usize::MAX`, so this cannot fail;
    // keep the checked form anyway so no `as` cast is needed.
    let delta = usize::try_from(delta_u64)
        .map_err(|_| invalid(format!("alignment delta {delta_u64} does not fit in usize")))?;
    let aligned_offset = offset - delta_u64;
    let map_len = len
        .checked_add(delta)
        .filter(|&n| n <= max_len)
        .ok_or_else(|| {
            invalid(format!(
                "mapping length {len} plus alignment {delta} exceeds the largest addressable \
             slice ({max_len} bytes)"
            ))
        })?;
    Ok(Layout {
        aligned_offset,
        delta,
        len,
        map_len,
    })
}

/// Compute the span to hand to `msync` / `FlushViewOfFile` for a
/// flush of `[offset, offset + flush_len)` inside a window of
/// `window_len` bytes that starts `delta` bytes into its OS mapping.
///
/// Returns `Ok(None)` when there is nothing to flush (`flush_len ==
/// 0`), otherwise `Ok(Some((start, count)))` where `start` is relative
/// to the start of the OS mapping, `start` is a multiple of `page`
/// (POSIX `msync` requires a page-aligned address), and
/// `start + count <= delta + window_len`, so the span never leaves the
/// mapping.
///
/// # Errors
///
/// `InvalidInput` when `offset > window_len`, when
/// `flush_len > window_len - offset`, when `page` is not a non-zero
/// power of two, or when any intermediate sum would overflow. All
/// checks happen before the caller does any pointer arithmetic.
pub(crate) fn flush_span(
    delta: usize,
    window_len: usize,
    offset: usize,
    flush_len: usize,
    page: usize,
) -> io::Result<Option<(usize, usize)>> {
    let remaining = window_len.checked_sub(offset).ok_or_else(|| {
        invalid(format!(
            "flush offset {offset} is past the end of the mapping ({window_len} bytes)"
        ))
    })?;
    if flush_len > remaining {
        return Err(invalid(format!(
            "flush range offset {offset} + length {flush_len} extends past the end of the \
             mapping ({window_len} bytes)"
        )));
    }
    if flush_len == 0 {
        return Ok(None);
    }
    if !page.is_power_of_two() {
        return Err(invalid(format!(
            "OS reported an invalid page size of {page} bytes"
        )));
    }
    let overflow = || invalid("flush range arithmetic overflowed".to_owned());
    let abs_start = delta.checked_add(offset).ok_or_else(overflow)?;
    let abs_end = abs_start.checked_add(flush_len).ok_or_else(overflow)?;
    let aligned_start = abs_start & !(page - 1);
    Ok(Some((aligned_start, abs_end - aligned_start)))
}

#[cfg(test)]
mod tests {
    use super::*;

    const G: usize = 65_536;
    const P: usize = 4096;

    #[test]
    fn resolve_len_inferred_and_explicit() {
        assert_eq!(resolve_len(100, 0, None, MAX_LEN).unwrap(), 100);
        assert_eq!(resolve_len(100, 40, None, MAX_LEN).unwrap(), 60);
        assert_eq!(resolve_len(100, 100, None, MAX_LEN).unwrap(), 0);
        assert_eq!(resolve_len(100, 40, Some(60), MAX_LEN).unwrap(), 60);
        assert_eq!(resolve_len(100, 40, Some(0), MAX_LEN).unwrap(), 0);
        assert_eq!(resolve_len(100, 100, Some(0), MAX_LEN).unwrap(), 0);
        assert_eq!(resolve_len(0, 0, None, MAX_LEN).unwrap(), 0);
        assert_eq!(resolve_len(0, 0, Some(0), MAX_LEN).unwrap(), 0);
    }

    #[test]
    fn resolve_len_rejects_out_of_file() {
        let e = resolve_len(100, 101, None, MAX_LEN).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::InvalidInput);
        assert!(resolve_len(100, 101, Some(0), MAX_LEN).is_err());
        assert!(resolve_len(100, 40, Some(61), MAX_LEN).is_err());
        assert!(resolve_len(100, 0, Some(101), MAX_LEN).is_err());
        assert!(resolve_len(0, 0, Some(1), MAX_LEN).is_err());
        assert!(resolve_len(100, u64::MAX, None, MAX_LEN).is_err());
        assert!(resolve_len(100, u64::MAX, Some(usize::MAX), MAX_LEN).is_err());
        assert!(resolve_len(u64::MAX, u64::MAX, Some(1), MAX_LEN).is_err());
        assert!(resolve_len(u64::MAX, 1, Some(usize::MAX), MAX_LEN).is_err());
    }

    #[test]
    fn resolve_len_simulated_32_bit_limits() {
        // Simulate a 32-bit target: isize::MAX == 2^31 - 1.
        let max32 = (1usize << 31) - 1;
        let big_file = 5u64 << 30; // 5 GiB
        assert!(resolve_len(big_file, 0, None, max32).is_err());
        assert!(resolve_len(big_file, 0, Some(max32 + 1), max32).is_err());
        assert_eq!(resolve_len(big_file, 0, Some(max32), max32).unwrap(), max32);
        // A window deep inside a large file is fine if it is small.
        assert_eq!(
            resolve_len(big_file, (4u64 << 30) + 7, Some(4096), max32).unwrap(),
            4096
        );
        assert_eq!(
            resolve_len(big_file, big_file - 10, None, max32).unwrap(),
            10
        );
    }

    #[test]
    fn layout_alignment() {
        let l = layout(0, 10, G, MAX_LEN).unwrap();
        assert_eq!(
            l,
            Layout {
                aligned_offset: 0,
                delta: 0,
                len: 10,
                map_len: 10
            }
        );
        let l = layout(1, 10, G, MAX_LEN).unwrap();
        assert_eq!((l.aligned_offset, l.delta, l.map_len), (0, 1, 11));
        let l = layout(G as u64 - 1, 1, G, MAX_LEN).unwrap();
        assert_eq!((l.aligned_offset, l.delta, l.map_len), (0, G - 1, G));
        let l = layout(G as u64, 5, G, MAX_LEN).unwrap();
        assert_eq!((l.aligned_offset, l.delta, l.map_len), (G as u64, 0, 5));
        let l = layout(G as u64 + 1, 5, G, MAX_LEN).unwrap();
        assert_eq!((l.aligned_offset, l.delta, l.map_len), (G as u64, 1, 6));
        // Offsets above 4 GiB keep their high bits (Windows splits
        // the offset into two DWORDs).
        let off = (7u64 << 32) + 3 * G as u64 + 17;
        let l = layout(off, 1, G, MAX_LEN).unwrap();
        assert_eq!(l.aligned_offset, (7u64 << 32) + 3 * G as u64);
        assert_eq!(l.delta, 17);
        let l = layout(u64::MAX, 0, G, MAX_LEN).unwrap();
        assert_eq!(l.delta, G - 1);
        assert_eq!(l.aligned_offset % G as u64, 0);
    }

    #[test]
    fn layout_rejects_overflow_and_bad_granularity() {
        assert!(layout(1, usize::MAX, G, usize::MAX).is_err());
        assert!(layout(1, MAX_LEN, G, MAX_LEN).is_err());
        assert!(layout(0, MAX_LEN, G, MAX_LEN).is_ok());
        let max32 = (1usize << 31) - 1;
        assert!(layout(5, max32 - 4, P, max32).is_err());
        assert!(layout(5, max32 - 5, P, max32).is_ok());
        assert!(layout(0, 1, 0, MAX_LEN).is_err());
        assert!(layout(0, 1, 3, MAX_LEN).is_err());
        assert!(layout(0, 1, 1, MAX_LEN).is_ok());
    }

    #[test]
    fn flush_span_bounds() {
        // Window of 100 bytes starting 10 bytes into its mapping.
        assert_eq!(flush_span(10, 100, 0, 100, P).unwrap(), Some((0, 110)));
        assert_eq!(flush_span(10, 100, 50, 50, P).unwrap(), Some((0, 110)));
        assert_eq!(flush_span(10, 100, 100, 0, P).unwrap(), None);
        assert_eq!(flush_span(10, 100, 0, 0, P).unwrap(), None);
        assert!(flush_span(10, 100, 101, 0, P).is_err());
        assert!(flush_span(10, 100, 100, 1, P).is_err());
        assert!(flush_span(10, 100, 1, 100, P).is_err());
        assert!(flush_span(10, 100, usize::MAX, 1, P).is_err());
        assert!(flush_span(10, 100, 1, usize::MAX, P).is_err());
        assert!(flush_span(0, 0, 0, 0, P).unwrap().is_none());
        assert!(flush_span(0, 0, 0, 1, P).is_err());
        assert!(flush_span(0, 0, 1, 0, P).is_err());
    }

    #[test]
    fn flush_span_page_alignment() {
        // Window starting exactly at a page, multi-page.
        let w = 3 * P + 5;
        assert_eq!(flush_span(0, w, P + 1, 1, P).unwrap(), Some((P, 2)));
        assert_eq!(flush_span(0, w, P, P, P).unwrap(), Some((P, P)));
        assert_eq!(flush_span(0, w, 3 * P, 5, P).unwrap(), Some((3 * P, 5)));
        assert_eq!(flush_span(0, w, P - 1, 2, P).unwrap(), Some((0, P + 1)));
        // Delta pushes the absolute start onto the next page.
        assert_eq!(flush_span(P - 1, 10, 1, 1, P).unwrap(), Some((P, 1)));
        assert_eq!(flush_span(P - 1, 10, 0, 1, P).unwrap(), Some((0, P)));
        assert!(flush_span(0, w, 0, 1, 0).is_err());
        assert!(flush_span(0, w, 0, 1, 12).is_err());
    }

    #[test]
    fn flush_span_exhaustive_small() {
        // Every (delta, window, offset, len) in a small domain obeys
        // the documented postconditions or errors.
        // Smaller domain under Miri, which interprets every iteration.
        let (page, max) = if cfg!(miri) { (4, 10) } else { (8, 24) };
        for delta in 0..page {
            for window in 0..max - 4 {
                for offset in 0..max {
                    for len in 0..max {
                        let r = flush_span(delta, window, offset, len, page);
                        let in_bounds = offset <= window && len <= window - offset;
                        match r {
                            Ok(None) => assert!(in_bounds && len == 0),
                            Ok(Some((start, count))) => {
                                assert!(in_bounds && len > 0);
                                assert_eq!(start % page, 0);
                                assert!(start <= delta + offset);
                                assert_eq!(start + count, delta + offset + len);
                                assert!(start + count <= delta + window);
                            }
                            Err(e) => {
                                assert!(!in_bounds);
                                assert_eq!(e.kind(), io::ErrorKind::InvalidInput);
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn layout_exhaustive_small() {
        let (gran, max) = if cfg!(miri) {
            (8usize, 20u64)
        } else {
            (16, 80)
        };
        for offset in 0..max {
            for len in 0..max as usize / 2 {
                let l = layout(offset, len, gran, MAX_LEN).unwrap();
                assert_eq!(l.aligned_offset % gran as u64, 0);
                assert!(l.delta < gran);
                assert_eq!(l.aligned_offset + l.delta as u64, offset);
                assert_eq!(l.map_len, l.delta + len);
            }
        }
    }
}
