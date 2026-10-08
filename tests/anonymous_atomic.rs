//! Atomic views on `AnonymousMmap` (1.1.0).

#![cfg(feature = "atomic")]

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::thread;

use mmap_io::{AnonymousMmap, MmapIoError};

#[test]
fn views_start_at_zero_and_store() {
    let m = AnonymousMmap::new(4096).expect("anon");
    let a = m.atomic_u64(0).expect("u64");
    assert_eq!(a.load(Ordering::SeqCst), 0);
    a.store(0xDEAD_BEEF, Ordering::SeqCst);
    let b = m.atomic_u32(8).expect("u32");
    b.store(7, Ordering::SeqCst);
    let s = m.atomic_u64_slice(16, 4).expect("u64 slice");
    s[3].store(9, Ordering::SeqCst);
    let t = m.atomic_u32_slice(48, 4).expect("u32 slice");
    t[0].store(1, Ordering::SeqCst);
    drop((a, b, s, t));
    let mut buf = [0u8; 8];
    m.read_into(0, &mut buf).expect("read");
    assert_eq!(u64::from_ne_bytes(buf), 0xDEAD_BEEF);
    m.read_into(40, &mut buf).expect("read");
    assert_eq!(u64::from_ne_bytes(buf), 9);
}

#[test]
fn alignment_bounds_and_overflow() {
    let m = AnonymousMmap::new(64).expect("anon");
    assert!(matches!(
        m.atomic_u64(4),
        Err(MmapIoError::Misaligned {
            required: 8,
            offset: 4
        })
    ));
    assert!(matches!(
        m.atomic_u32(2),
        Err(MmapIoError::Misaligned {
            required: 4,
            offset: 2
        })
    ));
    assert!(matches!(
        m.atomic_u64(64),
        Err(MmapIoError::OutOfBounds { .. })
    ));
    assert!(matches!(
        m.atomic_u32(64),
        Err(MmapIoError::OutOfBounds { .. })
    ));
    assert!(matches!(
        m.atomic_u64_slice(56, 2),
        Err(MmapIoError::OutOfBounds { .. })
    ));
    assert!(matches!(
        m.atomic_u64_slice(0, usize::MAX),
        Err(MmapIoError::OutOfBounds { .. })
    ));
    assert!(matches!(
        m.atomic_u64(u64::MAX - 7),
        Err(MmapIoError::OutOfBounds { .. })
    ));
    // Zero-length slice views: aligned and within the mapping.
    assert_eq!(m.atomic_u64_slice(64, 0).expect("empty at end").len(), 0);
    assert!(m.atomic_u64_slice(72, 0).is_err());
    m.atomic_u32(60).expect("last u32");
    m.atomic_u64(56).expect("last u64");
}

#[test]
fn exclusion_with_plain_views_and_writers() {
    let m = AnonymousMmap::new(4096).expect("anon");
    let a = m.atomic_u64(0).expect("atomic");
    assert!(matches!(
        m.as_slice(0, 16),
        Err(MmapIoError::InvalidMode(_))
    ));
    assert!(matches!(
        m.try_as_slice(4, 4),
        Err(MmapIoError::InvalidMode(_))
    ));
    assert_eq!(m.as_slice(8, 8).expect("disjoint").len(), 8);
    assert!(matches!(m.atomic_u32(0), Err(MmapIoError::InvalidMode(_))));
    // The view holds the read lock: writers report would-block.
    assert!(!m.try_update_region(100, b"x").expect("would block"));
    a.store(5, Ordering::SeqCst);
    let mut buf = [0u8; 8];
    m.read_into(0, &mut buf).expect("copy reads atomically");
    assert_eq!(u64::from_ne_bytes(buf), 5);
    drop(a);
    let s = m.as_slice(0, 16).expect("view after drop");
    assert!(matches!(m.atomic_u64(8), Err(MmapIoError::InvalidMode(_))));
    drop(s);
    assert!(m.try_update_region(0, &[0; 8]).expect("free"));
}

#[test]
fn concurrent_counters() {
    let m = Arc::new(AnonymousMmap::new(4096).expect("anon"));
    let handles: Vec<_> = (0..8)
        .map(|_| {
            let m = Arc::clone(&m);
            thread::spawn(move || {
                let c = m.atomic_u64(64).expect("atomic");
                for _ in 0..10_000 {
                    c.fetch_add(1, Ordering::Relaxed);
                }
            })
        })
        .collect();
    for h in handles {
        h.join().expect("join");
    }
    assert_eq!(
        m.atomic_u64(64).expect("atomic").load(Ordering::SeqCst),
        80_000
    );
}

#[test]
fn views_are_send_and_sync() {
    fn check<T: Send + Sync>(_: &T) {}
    let m = AnonymousMmap::new(64).expect("anon");
    let v = m.atomic_u64(0).expect("atomic");
    check(&v);
    let s = m.atomic_u32_slice(8, 2).expect("slice");
    check(&s);
}
