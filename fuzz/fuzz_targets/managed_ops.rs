//! Fuzz target: stateful operation sequences on `MemoryMappedFile`,
//! checked against a `Vec<u8>` model.
//!
//! Each input picks an initial size, a flush policy and a list of
//! operations (writes, reads, slices, mutable slices, resizes, flushes,
//! segments, chunk iteration, `chunks_mut`, advice, touch, prefetch and
//! the `Read`/`Seek` cursor). The contract checked after every step:
//!   - valid requests succeed and invalid ones fail with the exact
//!     `OutOfBounds` / `ResizeFailed` error, never a panic;
//!   - the mapping's length and bytes equal the model;
//! and after the sequence: flushing, dropping and reopening the file
//! yields exactly the model.

#![no_main]

use std::io::{Read, Seek, SeekFrom};
use std::sync::Arc;

use libfuzzer_sys::fuzz_target;
use mmap_io::flush::FlushPolicy;
use mmap_io::segment::{Segment, SegmentMut};
use mmap_io::{MemoryMappedFile, MmapAdvice, MmapIoError, MmapMode};

/// Sizes stay below this so every step can compare the whole mapping.
const MAX_SIZE: u64 = 3 * 65_536 + 64;

#[derive(arbitrary::Arbitrary, Debug)]
enum Policy {
    Manual,
    Always,
    EveryBytes(u16),
    EveryWrites(u8),
    EveryMillis,
}

#[derive(arbitrary::Arbitrary, Debug, Clone, Copy)]
enum Advice {
    Normal,
    Random,
    Sequential,
    WillNeed,
    DontNeed,
}

#[derive(arbitrary::Arbitrary, Debug)]
enum Op {
    Update { offset: u64, data: Vec<u8> },
    ReadInto { offset: u64, len: u16 },
    AsSlice { offset: u64, len: u64 },
    SliceMutFill { offset: u64, len: u64, byte: u8 },
    Resize { size: u64 },
    Flush,
    FlushRange { offset: u64, len: u64 },
    SegmentWrite { offset: u64, len: u64, data: Vec<u8> },
    SegmentRead { offset: u64, len: u64 },
    ChunkSum { chunk: u32 },
    ChunksMutFill { chunk: u16, seed: u8 },
    Advise { offset: u64, len: u64, advice: Advice },
    Touch { offset: u64, len: u64 },
    Prefetch { offset: u64, len: u64 },
    ReaderSeekRead { pos: i64, from_end: bool, len: u16 },
}

#[derive(arbitrary::Arbitrary, Debug)]
struct Input {
    initial: u32,
    policy: Policy,
    /// When set, fold coordinates into the current size so most
    /// requests are valid; otherwise use the raw values.
    fold: bool,
    ops: Vec<Op>,
}

fn in_bounds(offset: u64, len: u64, size: usize) -> bool {
    len == 0 || offset.checked_add(len).is_some_and(|e| e <= size as u64)
}

fn check_oob<T>(r: Result<T, MmapIoError>, offset: u64, len: u64, size: usize, what: &str) {
    match r {
        Err(MmapIoError::OutOfBounds {
            offset: o,
            len: l,
            total: t,
        }) => assert_eq!((o, l, t), (offset, len, size as u64), "{what}: error fields"),
        Err(e) => panic!("{what}({offset}, {len}) on {size}: expected OutOfBounds, got {e}"),
        Ok(_) => panic!("{what}({offset}, {len}) accepted on a {size}-byte mapping"),
    }
}

fn check_result<T>(r: Result<T, MmapIoError>, offset: u64, len: u64, size: usize, what: &str) -> Option<T> {
    if in_bounds(offset, len, size) {
        match r {
            Ok(v) => Some(v),
            Err(e) => panic!("{what}({offset}, {len}) on {size}: {e}"),
        }
    } else {
        check_oob(r, offset, len, size, what);
        None
    }
}

fuzz_target!(|input: Input| {
    let initial = u64::from(input.initial) % MAX_SIZE + 1;
    let policy = match input.policy {
        Policy::Manual => FlushPolicy::Manual,
        Policy::Always => FlushPolicy::Always,
        Policy::EveryBytes(n) => FlushPolicy::EveryBytes(usize::from(n)),
        Policy::EveryWrites(n) => FlushPolicy::EveryWrites(usize::from(n)),
        Policy::EveryMillis => FlushPolicy::EveryMillis(1),
    };
    let path = std::env::temp_dir().join(format!("mmap_io_fuzz_managed_{}", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let m = Arc::new(
        MemoryMappedFile::builder(&path)
            .mode(MmapMode::ReadWrite)
            .size(initial)
            .flush_policy(policy)
            .create()
            .expect("create"),
    );
    let mut model = vec![0u8; initial as usize];

    for op in input.ops.iter().take(64) {
        let size = model.len();
        let fold = |v: u64| {
            if input.fold {
                v % (size as u64 + 2)
            } else {
                v
            }
        };
        match op {
            Op::Update { offset, data } => {
                let offset = fold(*offset);
                let data = &data[..data.len().min(4096)];
                if check_result(m.update_region(offset, data), offset, data.len() as u64, size, "update_region").is_some()
                    && !data.is_empty()
                {
                    let o = offset as usize;
                    model[o..o + data.len()].copy_from_slice(data);
                }
            }
            Op::ReadInto { offset, len } => {
                let offset = fold(*offset);
                let mut buf = vec![0u8; usize::from(*len)];
                let r = m.read_into(offset, &mut buf);
                if check_result(r, offset, u64::from(*len), size, "read_into").is_some() && *len > 0 {
                    let o = offset as usize;
                    assert_eq!(&buf[..], &model[o..o + buf.len()]);
                }
            }
            Op::AsSlice { offset, len } => {
                let (offset, len) = (fold(*offset), fold(*len));
                if let Some(s) = check_result(m.as_slice(offset, len), offset, len, size, "as_slice") {
                    assert_eq!(s.len() as u64, len);
                    if len > 0 {
                        assert_eq!(&*s, &model[offset as usize..(offset + len) as usize]);
                    }
                }
            }
            Op::SliceMutFill { offset, len, byte } => {
                let (offset, len) = (fold(*offset), fold(*len));
                let r = m.as_slice_mut(offset, len).map(|mut s| s.as_mut().fill(*byte));
                if check_result(r, offset, len, size, "as_slice_mut").is_some() && len > 0 {
                    model[offset as usize..(offset + len) as usize].fill(*byte);
                }
            }
            Op::Resize { size: new } => {
                let new = if input.fold { new % (MAX_SIZE + 1) } else { *new };
                match m.resize(new) {
                    Ok(()) => {
                        assert!(new > 0 && new <= 128u64 << 40, "resize({new}) accepted");
                        if new > 4 * MAX_SIZE {
                            // Accepted but huge: shrink back to keep the
                            // input cheap, mirroring the model.
                            m.resize(MAX_SIZE).expect("shrink back");
                            model.resize(MAX_SIZE as usize, 0);
                        } else {
                            model.resize(new as usize, 0);
                        }
                    }
                    Err(MmapIoError::ResizeFailed(_)) => assert!(new == 0 || new > (128u64 << 40)),
                    // Out of address space or disk: allowed, but must
                    // leave the mapping unchanged.
                    Err(MmapIoError::Io(_)) => assert!(new > 4 * MAX_SIZE),
                    Err(e) => panic!("resize({new}): {e}"),
                }
            }
            Op::Flush => {
                m.flush().expect("flush");
                assert_eq!(m.pending_bytes(), 0);
            }
            Op::FlushRange { offset, len } => {
                let (offset, len) = (fold(*offset), fold(*len));
                check_result(m.flush_range(offset, len), offset, len, size, "flush_range");
            }
            Op::SegmentWrite { offset, len, data } => {
                let (offset, len) = (fold(*offset), fold(*len));
                let data = &data[..data.len().min(4096)];
                if let Some(seg) = check_result(SegmentMut::new(Arc::clone(&m), offset, len), offset, len, size, "SegmentMut::new") {
                    let r = seg.write(data);
                    if data.len() as u64 > len {
                        check_oob(r, 0, data.len() as u64, len as usize, "SegmentMut::write");
                    } else {
                        r.expect("segment write");
                        if !data.is_empty() {
                            let o = offset as usize;
                            model[o..o + data.len()].copy_from_slice(data);
                        }
                    }
                }
            }
            Op::SegmentRead { offset, len } => {
                let (offset, len) = (fold(*offset), fold(*len));
                if let Some(seg) = check_result(Segment::new(Arc::clone(&m), offset, len), offset, len, size, "Segment::new") {
                    let s = seg.as_slice().expect("segment read");
                    if len > 0 {
                        assert_eq!(&*s, &model[offset as usize..(offset + len) as usize]);
                    }
                }
            }
            Op::ChunkSum { chunk } => {
                let chunk = *chunk as usize;
                let mut sum = 0u64;
                let mut count = 0usize;
                for c in m.chunks(chunk) {
                    sum += c.iter().map(|&b| u64::from(b)).sum::<u64>();
                    count += 1;
                }
                if chunk == 0 {
                    assert_eq!((count, sum), (0, 0));
                } else {
                    assert_eq!(count, size.div_ceil(chunk));
                    assert_eq!(sum, model.iter().map(|&b| u64::from(b)).sum::<u64>());
                }
            }
            Op::ChunksMutFill { chunk, seed } => {
                let chunk = usize::from(*chunk);
                m.chunks_mut(chunk)
                    .for_each_mut(|off, c| {
                        c.fill(seed.wrapping_add(off as u8));
                        Ok(())
                    })
                    .expect("chunks_mut");
                if chunk > 0 {
                    for (i, c) in model.chunks_mut(chunk).enumerate() {
                        c.fill(seed.wrapping_add((i * chunk) as u8));
                    }
                }
            }
            Op::Advise { offset, len, advice } => {
                let (offset, len) = (fold(*offset), fold(*len));
                let advice = match advice {
                    Advice::Normal => MmapAdvice::Normal,
                    Advice::Random => MmapAdvice::Random,
                    Advice::Sequential => MmapAdvice::Sequential,
                    Advice::WillNeed => MmapAdvice::WillNeed,
                    Advice::DontNeed => MmapAdvice::DontNeed,
                };
                check_result(m.advise(offset, len, advice), offset, len, size, "advise");
            }
            Op::Touch { offset, len } => {
                let (offset, len) = (fold(*offset), fold(*len));
                check_result(m.touch_pages_range(offset, len), offset, len, size, "touch_pages_range");
            }
            Op::Prefetch { offset, len } => {
                let (offset, len) = (fold(*offset), fold(*len));
                check_result(m.prefetch_range(offset, len), offset, len, size, "prefetch_range");
            }
            Op::ReaderSeekRead { pos, from_end, len } => {
                let mut r = m.reader();
                let mut c = std::io::Cursor::new(&model[..]);
                let target = if *from_end { SeekFrom::End(*pos) } else { SeekFrom::Current(*pos) };
                assert_eq!(
                    r.seek(target).map_err(|e| e.kind()),
                    c.seek(target).map_err(|e| e.kind())
                );
                let mut a = vec![0u8; usize::from(*len)];
                let mut b = vec![0u8; usize::from(*len)];
                assert_eq!(r.read(&mut a).expect("read"), c.read(&mut b).expect("read"));
                assert_eq!(a, b);
                assert_eq!(r.position(), c.position());
            }
        }
        assert_eq!(m.len(), model.len() as u64);
        assert_eq!(&*m.as_slice(0, model.len() as u64).expect("whole"), &model[..]);
    }

    m.flush().expect("final flush");
    drop(m);
    let on_disk = std::fs::read(&path).expect("read back");
    assert!(on_disk == model, "file differs from the model");
    let _ = std::fs::remove_file(&path);
});
