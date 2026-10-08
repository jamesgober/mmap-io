//! Atomic views on file-backed `ReadWrite` mappings: alignment and
//! bounds tables with exact error fields, persistence, resize
//! interaction, and multi-threaded counters.
//!
//! No test here holds an atomic view and a plain read view of the same
//! mapping at the same time.

use std::sync::atomic::Ordering;
use std::sync::Arc;

use mmap_io::{MemoryMappedFile, MmapIoError};

use crate::common::{tmp_path, TmpPath};

fn rw(size: u64) -> (TmpPath, MemoryMappedFile) {
    let path = tmp_path("atomic.bin");
    let m = MemoryMappedFile::create_rw(&path, size).unwrap();
    (path, m)
}

#[derive(Debug, PartialEq)]
enum Expect {
    Ok,
    Misaligned(u64),
    Oob(u64),
}

fn classify<T>(r: Result<T, MmapIoError>, offset: u64, total: u64) -> Expect {
    match r {
        Ok(_) => Expect::Ok,
        Err(MmapIoError::Misaligned {
            required,
            offset: o,
        }) => {
            assert_eq!(o, offset, "Misaligned offset field");
            Expect::Misaligned(required)
        }
        Err(MmapIoError::OutOfBounds {
            offset: o,
            len,
            total: t,
        }) => {
            assert_eq!((o, t), (offset, total), "OutOfBounds fields");
            Expect::Oob(len)
        }
        Err(e) => panic!("unexpected error {e}"),
    }
}

#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
#[test]
fn single_views_alignment_and_bounds_table() {
    // 13 bytes: room for one u64 at 0, u32s at 0, 4 and 8.
    for total in [13u64, 16, 4096, 4097] {
        let (_p, m) = rw(total);
        let offsets = [
            0,
            1,
            2,
            3,
            4,
            7,
            8,
            total - 4,
            total.saturating_sub(8),
            total,
            total + 4,
            total + 8,
            u64::from(u32::MAX) + 1,
            i64::MAX as u64 + 1,
            u64::MAX - 7,
            u64::MAX - 3,
            u64::MAX,
        ];
        for offset in offsets {
            let want64 = if offset % 8 != 0 {
                Expect::Misaligned(8)
            } else if offset.checked_add(8).is_some_and(|e| e <= total) {
                Expect::Ok
            } else {
                Expect::Oob(8)
            };
            assert_eq!(
                classify(m.atomic_u64(offset), offset, total),
                want64,
                "u64 total={total} offset={offset}"
            );
            let want32 = if offset % 4 != 0 {
                Expect::Misaligned(4)
            } else if offset.checked_add(4).is_some_and(|e| e <= total) {
                Expect::Ok
            } else {
                Expect::Oob(4)
            };
            assert_eq!(
                classify(m.atomic_u32(offset), offset, total),
                want32,
                "u32 total={total} offset={offset}"
            );
        }
    }
}

#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
#[test]
fn slice_views_count_table() {
    let total = 4096u64;
    let (_p, m) = rw(total);
    // (offset, count, u64 view ok?, u32 view ok?). Out-of-bounds errors
    // must report the saturated byte length `size * count`.
    let cases: &[(u64, usize, bool, bool)] = &[
        (0, 0, true, true),
        (0, 512, true, true),
        (0, 513, false, true),
        (0, 1024, false, true),
        (0, 1025, false, false),
        (4088, 1, true, true),
        (4088, 2, false, true),
        // count 0 still requires an aligned offset <= len().
        (4096, 0, true, true),
        (4104, 0, false, false),
        (u64::MAX - 7, 0, false, false),
        // Byte counts that overflow saturate instead of wrapping.
        (0, usize::MAX, false, false),
        (8, usize::MAX / 8 + 1, false, false),
        (8, usize::MAX / 4 + 1, false, false),
    ];
    for &(offset, count, ok64, ok32) in cases {
        let want = |ok: bool, size: u64| {
            if ok {
                Expect::Ok
            } else {
                Expect::Oob(size.saturating_mul(count as u64))
            }
        };
        assert_eq!(
            classify(m.atomic_u64_slice(offset, count), offset, total),
            want(ok64, 8),
            "u64 slice offset={offset} count={count}"
        );
        assert_eq!(
            classify(m.atomic_u32_slice(offset, count), offset, total),
            want(ok32, 4),
            "u32 slice offset={offset} count={count}"
        );
    }
    // Misaligned slices, whatever the count.
    for (offset, count) in [(3u64, 0usize), (4092, 1), (1, usize::MAX)] {
        assert_eq!(
            classify(m.atomic_u64_slice(offset, count), offset, total),
            Expect::Misaligned(8)
        );
    }
    assert_eq!(
        classify(m.atomic_u32_slice(2, 0), 2, total),
        Expect::Misaligned(4)
    );
    let s = m.atomic_u64_slice(4096, 0).unwrap();
    assert!(s.is_empty());
}

#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
#[test]
fn misalignment_is_reported_before_bounds() {
    let (_p, m) = rw(16);
    assert!(matches!(
        m.atomic_u64(1001),
        Err(MmapIoError::Misaligned {
            required: 8,
            offset: 1001
        })
    ));
    assert!(matches!(
        m.atomic_u32_slice(1002, 1),
        Err(MmapIoError::Misaligned {
            required: 4,
            offset: 1002
        })
    ));
}

#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
#[test]
fn read_only_mappings_refuse_views_before_checking_alignment() {
    let path = tmp_path("ro.bin");
    drop(MemoryMappedFile::create_rw(&path, 64).unwrap());
    let ro = MemoryMappedFile::open_ro(&path).unwrap();
    for offset in [0u64, 1, 64, u64::MAX] {
        assert!(matches!(
            ro.atomic_u64(offset),
            Err(MmapIoError::InvalidMode(_))
        ));
        assert!(matches!(
            ro.atomic_u32(offset),
            Err(MmapIoError::InvalidMode(_))
        ));
        assert!(matches!(
            ro.atomic_u64_slice(offset, 0),
            Err(MmapIoError::InvalidMode(_))
        ));
        assert!(matches!(
            ro.atomic_u32_slice(offset, 1),
            Err(MmapIoError::InvalidMode(_))
        ));
    }
}

#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
#[test]
fn stored_values_are_native_endian_bytes_and_persist() {
    let (path, m) = rw(64);
    {
        let a = m.atomic_u64(8).unwrap();
        a.store(0x0102_0304_0506_0708, Ordering::SeqCst);
        let b = m.atomic_u32(20).unwrap();
        b.store(0xA1B2_C3D4, Ordering::SeqCst);
        let s = m.atomic_u64_slice(32, 4).unwrap();
        for (i, x) in s.iter().enumerate() {
            x.store(u64::MAX - i as u64, Ordering::Relaxed);
        }
    }
    let mut buf = [0u8; 8];
    m.read_into(8, &mut buf).unwrap();
    assert_eq!(buf, 0x0102_0304_0506_0708u64.to_ne_bytes());
    let mut b4 = [0u8; 4];
    m.read_into(20, &mut b4).unwrap();
    assert_eq!(b4, 0xA1B2_C3D4u32.to_ne_bytes());
    m.flush().unwrap();
    drop(m);

    let bytes = std::fs::read(&path).unwrap();
    for i in 0..4usize {
        let at = 32 + i * 8;
        assert_eq!(
            u64::from_ne_bytes(bytes[at..at + 8].try_into().unwrap()),
            u64::MAX - i as u64
        );
    }
    // And a fresh mapping reads them back through views.
    let m = MemoryMappedFile::open_rw(&path).unwrap();
    assert_eq!(
        m.atomic_u64(8).unwrap().load(Ordering::SeqCst),
        0x0102_0304_0506_0708
    );
    assert_eq!(
        m.atomic_u32(20).unwrap().load(Ordering::SeqCst),
        0xA1B2_C3D4
    );
}

#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
#[test]
fn views_follow_resizes() {
    let (_p, m) = rw(16);
    m.atomic_u64(8).unwrap().store(42, Ordering::SeqCst);
    m.resize(4096 * 2).unwrap();
    assert_eq!(m.atomic_u64(8).unwrap().load(Ordering::SeqCst), 42);
    m.atomic_u64(8184).unwrap().store(7, Ordering::SeqCst);
    m.resize(12).unwrap();
    assert!(matches!(
        m.atomic_u64(8),
        Err(MmapIoError::OutOfBounds {
            offset: 8,
            len: 8,
            total: 12
        })
    ));
    // The low-address half of the u64 is still there as a u32.
    let half = u32::from_ne_bytes(42u64.to_ne_bytes()[..4].try_into().unwrap());
    assert_eq!(m.atomic_u32(8).unwrap().load(Ordering::SeqCst), half);
}

#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
#[test]
fn concurrent_fetch_add_on_shared_counters_is_exact() {
    let (_p, m) = rw(4096);
    let m = Arc::new(m);
    let threads = 8;
    let per_thread = 5_000u64;
    std::thread::scope(|s| {
        for t in 0..threads {
            let m = Arc::clone(&m);
            s.spawn(move || {
                // One long-lived view and many short-lived ones.
                let counters = m.atomic_u64_slice(0, 4).unwrap();
                for i in 0..per_thread {
                    counters[(i % 4) as usize].fetch_add(1, Ordering::Relaxed);
                }
                drop(counters);
                for _ in 0..per_thread {
                    m.atomic_u32(64 + 4 * (t % 4))
                        .unwrap()
                        .fetch_add(1, Ordering::Relaxed);
                }
            });
        }
    });
    let counters = m.atomic_u64_slice(0, 4).unwrap();
    let sum: u64 = counters.iter().map(|c| c.load(Ordering::SeqCst)).sum();
    assert_eq!(sum, threads * per_thread);
    drop(counters);
    let words = m.atomic_u32_slice(64, 4).unwrap();
    let sum: u64 = words
        .iter()
        .map(|c| u64::from(c.load(Ordering::SeqCst)))
        .sum();
    assert_eq!(sum, threads * per_thread);
}
