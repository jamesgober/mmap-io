//! Concurrency stress through the safe API only.
//!
//! One `ReadWrite` mapping is shared by reader threads (`as_slice`,
//! `read_into`, `chunks`, `MmapReader`, segments), a writer
//! (`update_region` and `as_slice_mut`), a resizer (grow / shrink loop),
//! a flusher (`flush`, `flush_range`) and the `EveryMillis` background
//! flusher, for a bounded time.
//!
//! The file is an array of 16-byte records `[seq: u64][check: u64]`
//! where `check` is a keyed hash of `(index, seq)`; an all-zero record
//! means "never written / truncated". Invariants checked by readers:
//! - every record they see is either all zero or carries a valid
//!   check value (no torn record, no foreign bytes);
//! - records in the stable prefix (never truncated) never go back to
//!   an older sequence number from one read to the next;
//! - every length they observe is a whole number of records within the
//!   resizer's range, and a request built from a stale `len()` fails
//!   with `OutOfBounds` against a smaller total, never with garbage.
//!
//! The default run is short. `MMAP_IO_SOAK=1 cargo test --test
//! concurrency_stress -- --ignored` runs the long versions.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::{Duration, Instant};

use mmap_io::flush::FlushPolicy;
use mmap_io::segment::Segment;
use mmap_io::{AnonymousMmap, MemoryMappedFile, MmapIoError, MmapMode};

mod common;

const RECORD: u64 = 16;
/// Records 0..PREFIX are never truncated.
const PREFIX: u64 = 256;
const MIN_LEN: u64 = PREFIX * RECORD;
/// The resizer moves the length between MIN_LEN and MAX_LEN.
const MAX_LEN: u64 = MIN_LEN + 64 * 1024;

fn check_of(index: u64, seq: u64) -> u64 {
    let x = seq ^ index.rotate_left(32) ^ 0x9E37_79B9_7F4A_7C15;
    // splitmix64 finalizer; never 0 for a written record in practice,
    // and (0, 0) is reserved for "empty".
    let mut z = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    (z ^ (z >> 31)) | 1
}

fn encode(index: u64, seq: u64) -> [u8; RECORD as usize] {
    let mut r = [0u8; RECORD as usize];
    r[..8].copy_from_slice(&seq.to_le_bytes());
    r[8..].copy_from_slice(&check_of(index, seq).to_le_bytes());
    r
}

/// Validate one record; returns its sequence number (0 for empty).
fn decode(index: u64, bytes: &[u8]) -> u64 {
    let seq = u64::from_le_bytes(bytes[..8].try_into().unwrap());
    let check = u64::from_le_bytes(bytes[8..16].try_into().unwrap());
    if seq == 0 && check == 0 {
        return 0;
    }
    assert_eq!(
        check,
        check_of(index, seq),
        "torn or foreign record {index}: seq={seq:#x} check={check:#x}"
    );
    seq
}

/// Validate every record in `bytes`, which starts at record `first`.
/// Updates `last_seen` for prefix records and checks monotonicity.
fn verify_records(first: u64, bytes: &[u8], last_seen: &mut [u64]) {
    assert_eq!(bytes.len() as u64 % RECORD, 0, "partial record");
    for (k, rec) in bytes.chunks_exact(RECORD as usize).enumerate() {
        let index = first + k as u64;
        let seq = decode(index, rec);
        if index < PREFIX {
            let last = &mut last_seen[index as usize];
            assert!(
                seq >= *last,
                "record {index} went back from seq {last} to {seq}"
            );
            *last = seq;
        }
    }
}

fn valid_len(len: u64) {
    assert!(
        (MIN_LEN..=MAX_LEN).contains(&len) && len % RECORD == 0,
        "inconsistent length {len}"
    );
}

/// How long each phase runs.
fn duration(default_ms: u64) -> Duration {
    if std::env::var_os("MMAP_IO_SOAK").is_some() {
        let secs = std::env::var("MMAP_IO_SOAK_SECS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(120);
        Duration::from_secs(secs)
    } else {
        Duration::from_millis(default_ms)
    }
}

/// Small deterministic PRNG so every thread's choices are reproducible
/// from its seed.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

struct Stats {
    reads: AtomicU64,
    writes: AtomicU64,
    resizes: AtomicU64,
    flushes: AtomicU64,
    oob: AtomicU64,
}

fn reader(m: &MemoryMappedFile, stop: &AtomicBool, seed: u64, stats: &Stats) {
    let mut rng = Rng(seed | 1);
    let mut last_seen = vec![0u64; PREFIX as usize];
    let parent = Arc::new(m.clone());
    while !stop.load(Ordering::Relaxed) {
        match rng.below(6) {
            // Whole mapping through a stale length.
            0 => {
                let len = m.len();
                valid_len(len);
                match m.as_slice(0, len) {
                    Ok(s) => {
                        assert_eq!(s.len() as u64, len);
                        verify_records(0, &s, &mut last_seen);
                    }
                    Err(MmapIoError::OutOfBounds {
                        offset: 0,
                        len: l,
                        total,
                    }) => {
                        assert_eq!(l, len);
                        assert!(total < len, "OutOfBounds with total {total} >= {len}");
                        valid_len(total);
                        stats.oob.fetch_add(1, Ordering::Relaxed);
                    }
                    Err(e) => panic!("as_slice: {e}"),
                }
            }
            // One prefix record by copy.
            1 => {
                let i = rng.below(PREFIX);
                let mut buf = [0u8; RECORD as usize];
                m.read_into(i * RECORD, &mut buf).expect("prefix read");
                verify_records(i, &buf, &mut last_seen);
            }
            // A tail record, which may be cut off at any moment.
            2 => {
                let i = PREFIX + rng.below((MAX_LEN - MIN_LEN) / RECORD);
                let mut buf = [0u8; RECORD as usize];
                match m.read_into(i * RECORD, &mut buf) {
                    Ok(()) => {
                        decode(i, &buf);
                    }
                    Err(MmapIoError::OutOfBounds { total, .. }) => {
                        assert!(
                            total < (i + 1) * RECORD,
                            "tail OutOfBounds with total {total}"
                        );
                        stats.oob.fetch_add(1, Ordering::Relaxed);
                    }
                    Err(e) => panic!("tail read: {e}"),
                }
            }
            // Chunk iteration: one consistent snapshot.
            #[cfg(feature = "iterator")]
            3 => {
                let cs = (1 + rng.below(64)) as usize * RECORD as usize;
                let it = m.chunks(cs);
                let expected = it.len();
                let mut total = 0u64;
                let mut n = 0;
                for c in it {
                    verify_records(total / RECORD, &c, &mut last_seen);
                    total += c.len() as u64;
                    n += 1;
                }
                assert_eq!(n, expected, "ExactSizeIterator lied");
                valid_len(total);
            }
            // Read + Seek cursor over the prefix.
            4 => {
                use std::io::{Read, Seek, SeekFrom};
                let i = rng.below(PREFIX);
                let mut r = m.reader();
                r.seek(SeekFrom::Start(i * RECORD)).unwrap();
                let mut buf = [0u8; RECORD as usize];
                r.read_exact(&mut buf).expect("reader prefix read");
                verify_records(i, &buf, &mut last_seen);
            }
            // Segment over a prefix range.
            _ => {
                let i = rng.below(PREFIX - 8);
                let seg = Segment::new(Arc::clone(&parent), i * RECORD, 8 * RECORD).unwrap();
                let s = seg.as_slice().expect("prefix segment");
                verify_records(i, &s, &mut last_seen);
            }
        }
        stats.reads.fetch_add(1, Ordering::Relaxed);
        // Read guards are recursive, so a steady stream of readers can
        // starve the writer and the resizer. Leave gaps (load shaping,
        // not synchronization) so every role makes progress.
        if rng.below(4) == 0 {
            thread::sleep(Duration::from_micros(20));
        } else {
            thread::yield_now();
        }
    }
}

fn writer(m: &MemoryMappedFile, stop: &AtomicBool, seq: &AtomicU64, stats: &Stats) {
    let mut rng = Rng(0xDEAD_BEEF);
    while !stop.load(Ordering::Relaxed) {
        let s = seq.fetch_add(1, Ordering::Relaxed) + 1;
        let span = if rng.below(4) == 0 {
            MAX_LEN / RECORD
        } else {
            PREFIX
        };
        let i = rng.below(span);
        let r = if rng.below(3) == 0 {
            m.as_slice_mut(i * RECORD, RECORD)
                .map(|mut g| g.as_mut().copy_from_slice(&encode(i, s)))
        } else {
            m.update_region(i * RECORD, &encode(i, s))
        };
        match r {
            Ok(()) => {}
            Err(MmapIoError::OutOfBounds { .. }) if i >= PREFIX => {
                stats.oob.fetch_add(1, Ordering::Relaxed);
            }
            Err(e) => panic!("write of record {i}: {e}"),
        }
        stats.writes.fetch_add(1, Ordering::Relaxed);
    }
}

fn resizer(m: &MemoryMappedFile, stop: &AtomicBool, stats: &Stats) {
    let mut rng = Rng(0x1234_5678);
    while !stop.load(Ordering::Relaxed) {
        let tail = rng.below((MAX_LEN - MIN_LEN) / RECORD + 1);
        m.resize(MIN_LEN + tail * RECORD).expect("resize");
        stats.resizes.fetch_add(1, Ordering::Relaxed);
        thread::yield_now();
    }
}

fn flusher(m: &MemoryMappedFile, stop: &AtomicBool, stats: &Stats) {
    let mut rng = Rng(0xF1F1);
    while !stop.load(Ordering::Relaxed) {
        if rng.below(2) == 0 {
            m.flush().expect("flush");
        } else {
            let len = m.len();
            let off = rng.below(len);
            match m.flush_range(off, len - off) {
                Ok(()) | Err(MmapIoError::OutOfBounds { .. }) => {}
                Err(e) => panic!("flush_range: {e}"),
            }
        }
        stats.flushes.fetch_add(1, Ordering::Relaxed);
    }
}

/// Run the full mix for `run` and verify the file afterwards.
fn run_mix(run: Duration, readers: usize) {
    let path = common::tmp_path("stress.bin");
    let m = MemoryMappedFile::builder(&path)
        .mode(MmapMode::ReadWrite)
        .size(MIN_LEN)
        .flush_policy(FlushPolicy::EveryMillis(1))
        .create()
        .unwrap();
    let stop = AtomicBool::new(false);
    let seq = AtomicU64::new(0);
    let stats = Stats {
        reads: AtomicU64::new(0),
        writes: AtomicU64::new(0),
        resizes: AtomicU64::new(0),
        flushes: AtomicU64::new(0),
        oob: AtomicU64::new(0),
    };
    let start = Barrier::new(readers + 4);
    thread::scope(|s| {
        for r in 0..readers {
            let (m, stop, stats, start) = (m.clone(), &stop, &stats, &start);
            s.spawn(move || {
                start.wait();
                reader(&m, stop, 0xA11CE + r as u64 * 7919, stats);
            });
        }
        {
            let (m, stop, seq, stats, start) = (m.clone(), &stop, &seq, &stats, &start);
            s.spawn(move || {
                start.wait();
                writer(&m, stop, seq, stats);
            });
        }
        {
            let (m, stop, stats, start) = (m.clone(), &stop, &stats, &start);
            s.spawn(move || {
                start.wait();
                resizer(&m, stop, stats);
            });
        }
        {
            let (m, stop, stats, start) = (m.clone(), &stop, &stats, &start);
            s.spawn(move || {
                start.wait();
                flusher(&m, stop, stats);
            });
        }
        start.wait();
        thread::sleep(run);
        stop.store(true, Ordering::Relaxed);
    });

    let reads = stats.reads.load(Ordering::Relaxed);
    let writes = stats.writes.load(Ordering::Relaxed);
    eprintln!(
        "stress: {reads} reads, {writes} writes, {} resizes, {} flushes, {} expected OutOfBounds",
        stats.resizes.load(Ordering::Relaxed),
        stats.flushes.load(Ordering::Relaxed),
        stats.oob.load(Ordering::Relaxed),
    );
    assert!(reads > 0 && writes > 0, "a role made no progress");
    assert!(stats.resizes.load(Ordering::Relaxed) > 0);
    assert!(stats.flushes.load(Ordering::Relaxed) > 0);

    // Final state: every prefix record valid; the newest sequence
    // number is no larger than the writer's counter.
    m.flush().unwrap();
    let len = m.len();
    valid_len(len);
    drop(m);
    let bytes = std::fs::read(&path).unwrap();
    assert_eq!(bytes.len() as u64, len);
    let max_seq = seq.load(Ordering::Relaxed);
    for (i, rec) in bytes.chunks_exact(RECORD as usize).enumerate() {
        let s = decode(i as u64, rec);
        assert!(s <= max_seq, "record {i} has seq {s} > {max_seq}");
    }
}

#[test]
fn mixed_readers_writer_resizer_and_flushers() {
    run_mix(duration(1500), 4);
}

#[test]
#[ignore = "long-running: soak version of the mixed stress test (MMAP_IO_SOAK=1, MMAP_IO_SOAK_SECS)"]
fn soak_mixed_readers_writer_resizer_and_flushers() {
    run_mix(duration(60_000).max(Duration::from_secs(60)), 8);
}

/// Many short-lived clones and drops of the handle while the
/// `EveryMillis` thread keeps flushing: the last drop must stop the
/// flusher without a hang or a use of freed state.
#[test]
fn clone_and_drop_churn_with_background_flusher() {
    let path = common::tmp_path("churn.bin");
    let deadline = Instant::now() + duration(500);
    let mut rounds = 0;
    while Instant::now() < deadline || rounds < 20 {
        let m = MemoryMappedFile::builder(&path)
            .mode(MmapMode::ReadWrite)
            .size(4096)
            .flush_policy(FlushPolicy::EveryMillis(1))
            .open_or_create()
            .unwrap();
        thread::scope(|s| {
            for t in 0..4u64 {
                let m = m.clone();
                s.spawn(move || {
                    for k in 0..50u64 {
                        m.update_region((t * 64 + k) % 4000, &[k as u8]).unwrap();
                        let c = m.clone();
                        drop(c);
                    }
                });
            }
        });
        drop(m);
        rounds += 1;
    }
}

/// `AnonymousMmap` under concurrent writers of whole records and
/// readers that validate them. (No nested read slices: see the ignored
/// deadlock test in tests/edge/anonymous.rs.)
#[test]
fn anonymous_mapping_records_are_never_torn() {
    let m = Arc::new(AnonymousMmap::new(PREFIX * RECORD).unwrap());
    let stop = AtomicBool::new(false);
    let seq = AtomicU64::new(0);
    thread::scope(|s| {
        for w in 0..2u64 {
            let (m, stop, seq) = (Arc::clone(&m), &stop, &seq);
            s.spawn(move || {
                let mut rng = Rng(w + 99);
                while !stop.load(Ordering::Relaxed) {
                    let i = rng.below(PREFIX);
                    let n = seq.fetch_add(1, Ordering::Relaxed) + 1;
                    m.update_region(i * RECORD, &encode(i, n)).unwrap();
                }
            });
        }
        for r in 0..4u64 {
            let (m, stop) = (Arc::clone(&m), &stop);
            s.spawn(move || {
                let mut rng = Rng(r + 7);
                while !stop.load(Ordering::Relaxed) {
                    if rng.below(2) == 0 {
                        let s = m.as_slice(0, PREFIX * RECORD).unwrap();
                        for (i, rec) in s.chunks_exact(RECORD as usize).enumerate() {
                            decode(i as u64, rec);
                        }
                    } else {
                        let i = rng.below(PREFIX);
                        let mut buf = [0u8; RECORD as usize];
                        m.read_into(i * RECORD, &mut buf).unwrap();
                        decode(i, &buf);
                    }
                }
            });
        }
        thread::sleep(duration(500));
        stop.store(true, Ordering::Relaxed);
    });
}

/// Atomic counters hammered from many threads while another thread
/// resizes the mapping. Only atomic views touch the counter region and
/// no plain read view is taken on this mapping while they run.
#[cfg(feature = "atomic")]
#[test]
fn atomic_counters_survive_concurrent_resizes() {
    let path = common::tmp_path("atomic_stress.bin");
    let m = MemoryMappedFile::create_rw(&path, 4096).unwrap();
    let stop = AtomicBool::new(false);
    let per_thread = 20_000u64;
    let threads = 6u64;
    thread::scope(|s| {
        for _ in 0..threads {
            let m = m.clone();
            s.spawn(move || {
                for i in 0..per_thread {
                    let v = m.atomic_u64((i % 8) * 8).unwrap();
                    v.fetch_add(1, Ordering::Relaxed);
                }
            });
        }
        let (m2, stop) = (m.clone(), &stop);
        s.spawn(move || {
            let mut grow = true;
            while !stop.load(Ordering::Relaxed) {
                m2.resize(if grow { 64 * 1024 } else { 4096 }).unwrap();
                grow = !grow;
            }
        });
        // Wait for the counter threads by polling the total; the resizer
        // stops afterwards.
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            let total: u64 = (0..8)
                .map(|k| m.atomic_u64(k * 8).unwrap().load(Ordering::Relaxed))
                .sum();
            if total == threads * per_thread || Instant::now() > deadline {
                break;
            }
            thread::yield_now();
        }
        stop.store(true, Ordering::Relaxed);
    });
    let total: u64 = (0..8)
        .map(|k| m.atomic_u64(k * 8).unwrap().load(Ordering::SeqCst))
        .sum();
    assert_eq!(total, threads * per_thread);
}
