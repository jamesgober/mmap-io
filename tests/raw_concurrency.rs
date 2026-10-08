//! Concurrency and churn tests for `mmap_io::raw`: shared readers
//! across threads (`Send + Sync`), and thousands of map / drop cycles
//! with data checks.
//!
//! Every file-backed mapping maps a private temporary file that no
//! other process touches, which is the raw constructors' `# Safety`
//! contract.

use std::fs::OpenOptions;
use std::sync::Arc;
use std::thread;

use mmap_io::raw::{offset_granularity, RawMmap, RawMmapMut, RawMmapOptions};

fn pattern(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i.wrapping_mul(31) % 256) as u8).collect()
}

fn checksum(bytes: &[u8]) -> u64 {
    bytes.iter().enumerate().fold(0u64, |acc, (i, &b)| {
        acc.wrapping_mul(1_000_003)
            .wrapping_add(b as u64 ^ i as u64)
    })
}

#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
#[test]
fn many_threads_read_one_shared_mapping() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("shared_read.bin");
    let data = pattern(4 << 20);
    std::fs::write(&path, &data).expect("write");
    let file = std::fs::File::open(&path).expect("open");
    // SAFETY: private temporary file, never modified while mapped.
    let map = unsafe { RawMmap::map(&file) }.expect("map");
    drop(file);
    let expected = checksum(&data);

    // Borrowed across scoped threads (needs Sync).
    thread::scope(|s| {
        for t in 0..16 {
            let map = &map;
            s.spawn(move || {
                for _ in 0..4 {
                    assert_eq!(checksum(map), expected, "thread {t}");
                }
            });
        }
    });

    // Moved into threads behind an Arc (needs Send + Sync).
    let shared = Arc::new(map);
    let handles: Vec<_> = (0..8)
        .map(|t| {
            let m = Arc::clone(&shared);
            thread::spawn(move || {
                // Each thread reads a different stripe.
                let stripe = m.len() / 8;
                let start = t * stripe;
                m[start..start + stripe]
                    .iter()
                    .enumerate()
                    .all(|(i, &b)| b == ((start + i).wrapping_mul(31) % 256) as u8)
            })
        })
        .collect();
    for h in handles {
        assert!(h.join().expect("join"));
    }
}

#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
#[test]
fn mutable_mapping_moves_between_threads() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("move_rw.bin");
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(&path)
        .expect("create");
    file.set_len(1 << 16).expect("set_len");
    // SAFETY: private temporary file.
    let mut map = unsafe { RawMmapMut::map_mut(&file) }.expect("map");
    for round in 0..8u8 {
        map = thread::spawn(move || {
            map.iter_mut().for_each(|b| *b = round);
            map.flush().expect("flush in thread");
            map
        })
        .join()
        .expect("join");
        assert!(map.iter().all(|&b| b == round));
    }
    drop(map);
    assert!(std::fs::read(&path).expect("read").iter().all(|&b| b == 7));
}

#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
#[test]
fn parallel_writers_on_disjoint_mappings_of_one_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("disjoint.bin");
    let gran = offset_granularity().expect("granularity");
    let chunk = gran + 13; // deliberately not granularity-aligned
    let threads = 8;
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(&path)
        .expect("create");
    file.set_len((chunk * threads) as u64).expect("set_len");
    let file = Arc::new(file);
    let handles: Vec<_> = (0..threads)
        .map(|t| {
            let file = Arc::clone(&file);
            thread::spawn(move || {
                // SAFETY: private temporary file; each thread maps and
                // writes a disjoint byte range.
                let mut m = unsafe {
                    RawMmapOptions::new()
                        .offset((t * chunk) as u64)
                        .len(chunk)
                        .map_mut(&file)
                }
                .expect("map_mut");
                m.iter_mut().for_each(|b| *b = t as u8 + 1);
                m.flush().expect("flush");
            })
        })
        .collect();
    for h in handles {
        h.join().expect("join");
    }
    drop(file);
    let bytes = std::fs::read(&path).expect("read");
    for (t, part) in bytes.chunks(chunk).enumerate() {
        assert!(part.iter().all(|&b| b == t as u8 + 1), "chunk {t}");
    }
}

#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
#[test]
fn map_drop_churn_with_data_checks() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("churn.bin");
    let gran = offset_granularity().expect("granularity");
    let size = 2 * gran + 77;
    let data = pattern(size);
    std::fs::write(&path, &data).expect("write");
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
        .expect("open");
    for i in 0..3_000usize {
        let off = (i * 7919) % size;
        // SAFETY: private temporary file; the RW mapping below writes
        // back the same bytes it read, so readers see stable data.
        let ro = unsafe { RawMmapOptions::new().offset(off as u64).map(&file) }.expect("ro");
        assert_eq!(ro.first().copied(), data.get(off).copied());
        drop(ro);
        // SAFETY: as above.
        let mut rw = unsafe {
            RawMmapOptions::new()
                .offset(off as u64)
                .len(1)
                .map_mut(&file)
        }
        .expect("rw");
        rw[0] = data[off];
        if i % 500 == 0 {
            rw.flush().expect("flush");
        }
        drop(rw);
        // SAFETY: as above.
        let mut cow = unsafe { RawMmapOptions::new().map_copy(&file) }.expect("cow");
        cow[off] ^= 0xFF;
        drop(cow);
        let anon = RawMmapMut::map_anon(1 + i % 8192).expect("anon");
        assert_eq!(anon[anon.len() - 1], 0);
    }
    drop(file);
    assert!(std::fs::read(&path).expect("read") == data);
}
