//! `AnonymousMmap`: sizes, the range table, slices, raw pointers, and
//! the two places where it departs from the crate-wide contract
//! (pinned as ignored bug tests).

use std::sync::mpsc;
use std::sync::Arc;
use std::time::Duration;

use mmap_io::{AnonymousMmap, MmapIoError};

use crate::common::{page, pattern};

const MAX_SIZE: u64 = if cfg!(target_pointer_width = "64") {
    128 << 40
} else {
    2 << 30
};

fn expect_oob<T: std::fmt::Debug>(r: Result<T, MmapIoError>, ctx: &str, o: u64, l: u64, t: u64) {
    match r {
        Err(MmapIoError::OutOfBounds { offset, len, total }) => {
            assert_eq!((offset, len, total), (o, l, t), "{ctx}")
        }
        other => panic!("{ctx}: expected OutOfBounds, got {other:?}"),
    }
}

#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
#[test]
fn sizes_and_zero_initialization() {
    let p = page();
    for size in [1, 2, p - 1, p, p + 1, 3 * p, 1 << 20, 64 << 20] {
        let m = AnonymousMmap::new(size).unwrap();
        assert_eq!(m.len(), size);
        assert!(!m.is_empty());
        let mut first = [0xFFu8; 1];
        let mut last = [0xFFu8; 1];
        m.read_into(0, &mut first).unwrap();
        m.read_into(size - 1, &mut last).unwrap();
        assert_eq!((first[0], last[0]), (0, 0), "size {size}");
        if size <= 1 << 20 {
            assert!(m.as_slice(0, size).unwrap().iter().all(|&b| b == 0));
        }
        assert_eq!(format!("{m:?}"), format!("AnonymousMmap {{ len: {size} }}"));
    }
}

#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
#[test]
fn invalid_sizes_are_rejected() {
    for size in [0, MAX_SIZE + 1, u64::MAX] {
        assert!(
            matches!(AnonymousMmap::new(size), Err(MmapIoError::ResizeFailed(_))),
            "size {size}"
        );
    }
}

#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
#[test]
fn non_empty_ranges_follow_the_range_rule() {
    let p = page();
    for size in [1, p - 1, p, p + 1] {
        let m = AnonymousMmap::new(size).unwrap();
        let model = pattern(size as usize, 1);
        m.update_region(0, &model).unwrap();
        let offsets = [0, 1, size - 1, size, size + 1, u64::MAX - 1, u64::MAX];
        let lens = [1, 2, size - 1, size, size + 1, u64::MAX];
        for offset in offsets {
            for len in lens {
                if len == 0 {
                    continue;
                }
                let ctx = format!("size={size} offset={offset} len={len}");
                let ok = offset.checked_add(len).is_some_and(|e| e <= size);
                match m.as_slice(offset, len) {
                    Ok(s) => {
                        assert!(ok, "{ctx}");
                        assert_eq!(&*s, &model[offset as usize..(offset + len) as usize]);
                    }
                    Err(e) => {
                        assert!(!ok, "{ctx}: {e}");
                        expect_oob(Err::<(), _>(e), &ctx, offset, len, size);
                    }
                }
                match m.as_mut_slice(offset, len) {
                    Ok(s) => {
                        assert!(ok, "{ctx}");
                        assert_eq!(s.len() as u64, len);
                    }
                    Err(e) => expect_oob(Err::<(), _>(e), &ctx, offset, len, size),
                }
                if len <= size + 1 {
                    let mut buf = vec![0u8; len as usize];
                    let r = m.read_into(offset, &mut buf);
                    if ok {
                        r.unwrap();
                        assert_eq!(&buf[..], &model[offset as usize..(offset + len) as usize]);
                        m.update_region(offset, &buf).unwrap();
                    } else {
                        expect_oob(r, &ctx, offset, len, size);
                        expect_oob(m.update_region(offset, &buf), &ctx, offset, len, size);
                    }
                }
            }
        }
        assert_eq!(m.as_slice(0, size).unwrap(), &model[..]);
    }
}

#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
#[test]
fn zero_length_requests_within_bounds_are_empty() {
    let m = AnonymousMmap::new(100).unwrap();
    for offset in [0, 1, 99, 100] {
        assert!(m.as_slice(offset, 0).unwrap().is_empty());
        assert!(m.as_mut_slice(offset, 0).unwrap().is_empty());
        m.read_into(offset, &mut []).unwrap();
        m.update_region(offset, &[]).unwrap();
    }
}

/// The crate-level docs ("Range validation") and docs/API.md say a
/// zero-length request is accepted at any offset, including past the
/// end, and API.md says `AnonymousMmap` reads, writes and slices work
/// identically to `MemoryMappedFile`. `AnonymousMmap` returns
/// `OutOfBounds` instead.
#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
#[test]
fn zero_length_requests_past_the_end_are_accepted() {
    let m = AnonymousMmap::new(100).unwrap();
    for offset in [101, u64::MAX] {
        assert!(
            m.as_slice(offset, 0).unwrap().is_empty(),
            "as_slice({offset}, 0)"
        );
        assert!(m.as_mut_slice(offset, 0).unwrap().is_empty());
        m.read_into(offset, &mut []).unwrap();
        m.update_region(offset, &[]).unwrap();
    }
}

#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
#[test]
fn slices_write_through_and_report_their_length() {
    let m = AnonymousMmap::new(4096).unwrap();
    {
        let mut s = m.as_mut_slice(10, 5).unwrap();
        assert_eq!(s.len(), 5);
        assert!(!s.is_empty());
        s.as_mut().copy_from_slice(b"hello");
        s[0] = b'H';
        assert_eq!(&s[..], b"Hello");
    }
    let r = m.as_slice(10, 5).unwrap();
    assert_eq!(r, b"Hello");
    assert_eq!(r.len(), 5);
    // Several read slices can coexist.
    let r2 = m.as_slice(0, 4096).unwrap();
    assert_eq!(&r2[10..15], b"Hello");
}

#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
#[test]
fn raw_pointers_alias_the_mapping() {
    let m = AnonymousMmap::new(page() + 1).unwrap();
    // SAFETY: no slice of `m` is alive while the pointers are used, the
    // writes stay inside `len()`, and `AnonymousMmap` is never remapped.
    unsafe {
        let w = m.as_mut_ptr();
        w.write(0xAB);
        w.add(page() as usize).write(0xCD);
        let r = m.as_ptr();
        assert_eq!(r, w.cast_const());
        assert_eq!(r.read(), 0xAB);
    }
    let mut b = [0u8; 1];
    m.read_into(page(), &mut b).unwrap();
    assert_eq!(b[0], 0xCD);
}

#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
#[test]
fn concurrent_disjoint_writers_and_readers() {
    let m = Arc::new(AnonymousMmap::new(64 * 1024).unwrap());
    std::thread::scope(|s| {
        for t in 0..8u64 {
            let m = Arc::clone(&m);
            s.spawn(move || {
                let data = pattern(8192, t as u8);
                for _ in 0..50 {
                    m.update_region(t * 8192, &data).unwrap();
                    let mut back = vec![0u8; 8192];
                    m.read_into(t * 8192, &mut back).unwrap();
                    assert_eq!(back, data);
                }
            });
        }
    });
}

/// `MemoryMappedFile` takes its read guards with `read_recursive`, so a
/// thread that holds a slice can take another one while a writer is
/// queued. `AnonymousMmap::as_slice` uses the fair `read()`, so the
/// second slice waits behind the queued writer, which waits for the
/// first slice: a deadlock. API.md documents the recursive behavior for
/// read views in general.
#[cfg_attr(
    miri,
    ignore = "parking_lot_core futex call: Miri rejects &AtomicI32 for the *mut u32 syscall argument (dependency false positive)"
)]
#[test]
fn nested_read_slices_do_not_deadlock_behind_a_queued_writer() {
    let m = Arc::new(AnonymousMmap::new(4096).unwrap());
    let (done_tx, done_rx) = mpsc::channel();
    let (held_tx, held_rx) = mpsc::channel();
    let reader = {
        let m = Arc::clone(&m);
        std::thread::spawn(move || {
            let first = m.as_slice(0, 10).unwrap();
            held_tx.send(()).unwrap();
            // Give the writer time to queue on the lock. There is no
            // API to observe a parked writer, so this one wait is
            // timed; if the writer is not queued yet the test passes
            // trivially rather than failing spuriously.
            std::thread::sleep(Duration::from_millis(200));
            let second = m.as_slice(0, 10).unwrap();
            assert_eq!(first.len(), second.len());
            drop((first, second));
            done_tx.send(()).unwrap();
        })
    };
    held_rx.recv().unwrap();
    let writer = {
        let m = Arc::clone(&m);
        std::thread::spawn(move || m.update_region(0, b"w").unwrap())
    };
    let finished = done_rx.recv_timeout(Duration::from_secs(10)).is_ok();
    assert!(finished, "reader deadlocked taking a second slice");
    reader.join().unwrap();
    writer.join().unwrap();
}
