//! Atomic views and plain byte views of the same bytes can never be
//! alive at the same time (1.1.0). Disjoint ranges are unaffected, and
//! copying reads read atomic bytes with atomic loads.

#![cfg(feature = "atomic")]

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::thread;

use mmap_io::{AnonymousMmap, MemoryMappedFile, MmapIoError};

fn rw(dir: &tempfile::TempDir, len: u64) -> MemoryMappedFile {
    MemoryMappedFile::create_rw(dir.path().join("x.bin"), len).expect("create_rw")
}

fn is_invalid_mode<T>(r: Result<T, MmapIoError>) -> bool {
    matches!(r, Err(MmapIoError::InvalidMode(_)))
}

#[test]
fn slice_over_live_atomic_is_refused_disjoint_is_fine() {
    let dir = tempfile::tempdir().expect("tempdir");
    let m = rw(&dir, 4096);
    let counter = m.atomic_u64(8).expect("atomic");
    counter.store(1, Ordering::SeqCst);
    assert!(is_invalid_mode(m.as_slice(0, 16)));
    assert!(is_invalid_mode(m.as_slice(15, 1)));
    assert!(is_invalid_mode(m.as_slice(0, 4096)));
    // Touching the edges, not overlapping.
    assert_eq!(m.as_slice(0, 8).expect("before").len(), 8);
    assert_eq!(m.as_slice(16, 100).expect("after").len(), 100);
    // Zero-length requests never overlap anything.
    assert!(m.as_slice(8, 0).expect("empty").is_empty());
    drop(counter);
    assert_eq!(m.as_slice(0, 4096).expect("after drop").len(), 4096);
}

#[test]
fn atomic_over_live_slice_is_refused_disjoint_is_fine() {
    let dir = tempfile::tempdir().expect("tempdir");
    let m = rw(&dir, 4096);
    let s = m.as_slice(100, 50).expect("slice");
    assert!(is_invalid_mode(m.atomic_u64(96)));
    assert!(is_invalid_mode(m.atomic_u32(148)));
    assert!(is_invalid_mode(m.atomic_u64_slice(0, 32)));
    // Order of checks: mode, alignment, bounds, then overlap.
    assert!(matches!(
        m.atomic_u64(101),
        Err(MmapIoError::Misaligned { .. })
    ));
    assert!(matches!(
        m.atomic_u64(4096),
        Err(MmapIoError::OutOfBounds { .. })
    ));
    let a = m.atomic_u64(88).expect("ends at 96, before the slice");
    let b = m.atomic_u32(152).expect("starts after the slice");
    let empty = m
        .atomic_u64_slice(104, 0)
        .expect("empty view overlaps nothing");
    assert_eq!(empty.len(), 0);
    drop((a, b, empty));
    drop(s);
    m.atomic_u64(96).expect("slice dropped");
}

#[test]
fn mixed_size_atomic_views_are_refused_same_size_allowed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let m = rw(&dir, 64);
    let wide = m.atomic_u64_slice(0, 2).expect("u64 x2");
    let same = m.atomic_u64(8).expect("same size, overlapping");
    assert!(is_invalid_mode(m.atomic_u32(4)));
    assert!(is_invalid_mode(m.atomic_u32_slice(0, 8)));
    let narrow = m.atomic_u32(16).expect("disjoint u32");
    same.store(5, Ordering::SeqCst);
    assert_eq!(wide[1].load(Ordering::SeqCst), 5);
    drop((wide, same, narrow));
    m.atomic_u32_slice(0, 8).expect("all u64 views dropped");
}

#[test]
fn copying_reads_see_atomic_values() {
    let dir = tempfile::tempdir().expect("tempdir");
    let m = rw(&dir, 4096);
    m.update_region(0, &[0xAA; 64]).expect("fill");
    let v = m.atomic_u64(16).expect("atomic");
    v.store(u64::from_ne_bytes(*b"ATOMIC!!"), Ordering::SeqCst);
    let mut buf = [0u8; 32];
    m.read_into(4, &mut buf)
        .expect("read_into across the atomic");
    assert_eq!(&buf[..12], &[0xAA; 12]);
    assert_eq!(&buf[12..20], b"ATOMIC!!");
    assert_eq!(&buf[20..], &[0xAA; 12]);
    let mut part = [0u8; 3];
    m.read_into(18, &mut part).expect("inside the atomic");
    assert_eq!(&part, b"OMI");
    let mut cursor = m.reader();
    let mut all = Vec::new();
    std::io::Read::read_to_end(&mut cursor, &mut all).expect("reader");
    assert_eq!(&all[16..24], b"ATOMIC!!");
    m.touch_pages().expect("touch with a live atomic");
    m.touch_pages_range(0, 64).expect("touch range");
    #[cfg(feature = "bytes")]
    {
        let b = m.read_bytes(10, 20).expect("read_bytes");
        assert_eq!(&b[6..14], b"ATOMIC!!");
    }
    drop(v);
}

#[cfg(feature = "iterator")]
#[test]
fn iterator_items_over_atomics_are_snapshots_others_zero_copy() {
    let dir = tempfile::tempdir().expect("tempdir");
    let m = rw(&dir, 4 * 1024);
    m.update_region(0, &[7; 4096]).expect("fill");
    let v = m.atomic_u64(1024 + 8).expect("atomic in the second chunk");
    v.store(u64::MAX, Ordering::SeqCst);
    // SAFETY: only the address is used, to compare with item pointers.
    let base = unsafe { m.as_ptr() } as usize;
    for (i, chunk) in m.chunks(1024).enumerate() {
        assert_eq!(chunk.len(), 1024);
        if i == 1 {
            assert_ne!(chunk.as_ptr() as usize, base + 1024, "must be a copy");
            assert_eq!(&chunk[8..16], &[0xFF; 8]);
            assert!(chunk[..8].iter().all(|&b| b == 7));
        } else {
            assert_eq!(chunk.as_ptr() as usize, base + i * 1024, "zero-copy");
        }
    }
    let owned: Vec<Vec<u8>> = m
        .chunks_owned(1024)
        .collect::<Result<_, _>>()
        .expect("owned");
    assert_eq!(&owned[1][8..16], &[0xFF; 8]);
    drop(v);
}

#[cfg(feature = "iterator")]
#[test]
fn live_item_blocks_atomics_only_on_its_bytes() {
    let dir = tempfile::tempdir().expect("tempdir");
    let m = rw(&dir, 4 * 1024);
    let mut it = m.chunks(1024);
    let first = it.next().expect("first");
    assert!(is_invalid_mode(m.atomic_u64(0)));
    m.atomic_u64(2048)
        .expect("other chunk")
        .store(1, Ordering::SeqCst);
    drop(first);
    m.atomic_u64(0).expect("item dropped, iterator still alive");
    let second = it.next().expect("second");
    drop(it);
    // An item outliving its iterator still keeps atomics off its bytes.
    assert!(is_invalid_mode(m.atomic_u32(1024)));
    drop(second);
    m.atomic_u32(1024).expect("all gone");
}

#[test]
fn segment_and_cow_and_anonymous_follow_the_same_rule() {
    let dir = tempfile::tempdir().expect("tempdir");
    let m = Arc::new(rw(&dir, 4096));
    let seg = mmap_io::segment::Segment::new(Arc::clone(&m), 0, 64).expect("segment");
    let v = m.atomic_u32(32).expect("atomic");
    assert!(is_invalid_mode(seg.as_slice()));
    drop(v);
    assert_eq!(seg.as_slice().expect("segment slice").len(), 64);

    #[cfg(feature = "cow")]
    {
        let cow = MemoryMappedFile::open_cow(dir.path().join("x.bin")).expect("cow");
        let a = cow.atomic_u64(0).expect("cow atomic");
        assert!(is_invalid_mode(cow.as_slice(0, 8)));
        drop(a);
        let s = cow.as_slice(0, 8).expect("cow slice");
        assert!(is_invalid_mode(cow.atomic_u64(0)));
        drop(s);
    }

    let anon = AnonymousMmap::new(4096).expect("anon");
    let s = anon.as_slice(0, 16).expect("anon slice");
    drop(s);
    let mut buf = [0u8; 16];
    anon.read_into(0, &mut buf).expect("anon read");
}

#[test]
fn concurrent_atomic_stores_and_copying_reads_never_tear() {
    let dir = tempfile::tempdir().expect("tempdir");
    let m = Arc::new(rw(&dir, 4096));
    let writer = {
        let m = Arc::clone(&m);
        thread::spawn(move || {
            let words = m.atomic_u64_slice(0, 8).expect("atomics");
            for k in 0..20_000u64 {
                let byte = (k % 255) as u8;
                for w in words.iter() {
                    w.store(u64::from_ne_bytes([byte; 8]), Ordering::Release);
                }
            }
        })
    };
    let readers: Vec<_> = (0..3)
        .map(|_| {
            let m = Arc::clone(&m);
            thread::spawn(move || {
                let mut buf = [0u8; 80];
                for _ in 0..20_000 {
                    m.read_into(0, &mut buf).expect("read");
                    for word in buf[..64].chunks(8) {
                        assert!(word.iter().all(|&b| b == word[0]), "torn read: {word:?}");
                    }
                }
            })
        })
        .collect();
    writer.join().expect("writer");
    for r in readers {
        r.join().expect("reader");
    }
}

#[test]
fn racing_slices_and_atomics_never_overlap() {
    let dir = tempfile::tempdir().expect("tempdir");
    let m = Arc::new(rw(&dir, 4096));
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let atomics = {
        let (m, stop) = (Arc::clone(&m), Arc::clone(&stop));
        thread::spawn(move || {
            let mut made = 0u32;
            while !stop.load(Ordering::Relaxed) {
                if let Ok(v) = m.atomic_u64(64) {
                    v.fetch_add(1, Ordering::SeqCst);
                    made += 1;
                }
            }
            made
        })
    };
    let mut slices = 0u32;
    for _ in 0..20_000 {
        if let Ok(s) = m.as_slice(60, 16) {
            // While this slice lives no atomic view of 64..72 can exist,
            // so the bytes must not change under it.
            let before = s.to_vec();
            std::hint::black_box(&before);
            assert_eq!(&*s, &before[..]);
            slices += 1;
        }
    }
    stop.store(true, Ordering::Relaxed);
    let made = atomics.join().expect("join");
    assert!(made > 0 || slices > 0);
}
