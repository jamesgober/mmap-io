//! Model-based property test: random sequences of operations on a
//! `ReadWrite` mapping, checked step by step against a `Vec<u8>` model.
//!
//! After every step the whole mapping must equal the model and `len()`
//! must equal the model length; invalid requests must fail with the
//! exact `OutOfBounds` / `ResizeFailed` error and change nothing. After
//! the sequence the mapping is flushed and dropped, and the file must
//! equal the model byte for byte, read both through `std::fs` and
//! through a fresh read-only mapping.
//!
//! Failing cases shrink to a minimal operation sequence. The default
//! case count is moderate; set `PROPTEST_CASES` for a deep run, e.g.
//! `PROPTEST_CASES=5000 cargo test --release --test proptest_model`.

use std::sync::Arc;

use mmap_io::flush::FlushPolicy;
use mmap_io::segment::{Segment, SegmentMut};
use mmap_io::{MemoryMappedFile, MmapIoError, MmapMode};
use proptest::prelude::*;

mod common;

/// Upper bound for sizes, so a case spans several pages on every
/// platform but stays cheap to compare in full after each step.
const MAX_SIZE: u64 = 3 * 4096 + 123;

#[derive(Debug, Clone)]
enum Op {
    Update {
        offset: u64,
        data: Vec<u8>,
    },
    ReadInto {
        offset: u64,
        len: u64,
    },
    AsSlice {
        offset: u64,
        len: u64,
    },
    SliceMutFill {
        offset: u64,
        len: u64,
        byte: u8,
    },
    Resize {
        size: u64,
    },
    Flush,
    FlushRange {
        offset: u64,
        len: u64,
    },
    SegmentWrite {
        offset: u64,
        len: u64,
        data: Vec<u8>,
    },
    SegmentRead {
        offset: u64,
        len: u64,
    },
    ChunkSum {
        chunk: usize,
    },
    ChunksMutFill {
        chunk: usize,
        seed: u8,
    },
    ReaderReadAll,
    Clone,
}

/// Offsets and lengths: mostly in the interesting range around the
/// current size, sometimes hostile.
fn coord() -> impl Strategy<Value = u64> {
    prop_oneof![
        8 => 0..=MAX_SIZE + 16,
        1 => Just(0u64),
        1 => prop_oneof![
            Just(u64::MAX),
            Just(u64::MAX - 1),
            Just(u64::from(u32::MAX) + 1),
            Just(i64::MAX as u64 + 1),
        ],
    ]
}

fn op() -> impl Strategy<Value = Op> {
    let data = proptest::collection::vec(any::<u8>(), 0..300);
    prop_oneof![
        6 => (coord(), data.clone()).prop_map(|(offset, data)| Op::Update { offset, data }),
        3 => (coord(), 0..=MAX_SIZE + 8).prop_map(|(offset, len)| Op::ReadInto { offset, len }),
        3 => (coord(), coord()).prop_map(|(offset, len)| Op::AsSlice { offset, len }),
        3 => (coord(), coord(), any::<u8>())
            .prop_map(|(offset, len, byte)| Op::SliceMutFill { offset, len, byte }),
        3 => prop_oneof![
            8 => 1..=MAX_SIZE,
            1 => Just(0u64),
            1 => Just(u64::MAX),
        ].prop_map(|size| Op::Resize { size }),
        1 => Just(Op::Flush),
        2 => (coord(), coord()).prop_map(|(offset, len)| Op::FlushRange { offset, len }),
        2 => (coord(), coord(), data)
            .prop_map(|(offset, len, data)| Op::SegmentWrite { offset, len, data }),
        2 => (coord(), coord()).prop_map(|(offset, len)| Op::SegmentRead { offset, len }),
        1 => prop_oneof![Just(0usize), 1usize..=5000, Just(usize::MAX)]
            .prop_map(|chunk| Op::ChunkSum { chunk }),
        1 => (prop_oneof![Just(0usize), 1usize..=5000], any::<u8>())
            .prop_map(|(chunk, seed)| Op::ChunksMutFill { chunk, seed }),
        1 => Just(Op::ReaderReadAll),
        1 => Just(Op::Clone),
    ]
}

fn policy() -> impl Strategy<Value = FlushPolicy> {
    prop_oneof![
        Just(FlushPolicy::Manual),
        Just(FlushPolicy::Always),
        (1usize..2000).prop_map(FlushPolicy::EveryBytes),
        (1usize..8).prop_map(FlushPolicy::EveryWrites),
        Just(FlushPolicy::EveryMillis(1)),
    ]
}

/// `[offset, offset + len)` inside a buffer of `size` bytes, without
/// overflow. Zero-length requests are valid anywhere.
fn in_bounds(offset: u64, len: u64, size: usize) -> bool {
    len == 0 || offset.checked_add(len).is_some_and(|e| e <= size as u64)
}

fn assert_oob<T>(
    r: Result<T, MmapIoError>,
    offset: u64,
    len: u64,
    total: usize,
) -> Result<(), TestCaseError> {
    match r {
        Err(MmapIoError::OutOfBounds {
            offset: o,
            len: l,
            total: t,
        }) => {
            prop_assert_eq!((o, l, t), (offset, len, total as u64));
            Ok(())
        }
        Err(e) => Err(TestCaseError::fail(format!(
            "expected OutOfBounds, got {e}"
        ))),
        Ok(_) => Err(TestCaseError::fail(format!(
            "({offset}, {len}) accepted on a {total}-byte mapping"
        ))),
    }
}

/// Apply one operation to the mapping and the model.
fn apply(
    handles: &mut Vec<Arc<MemoryMappedFile>>,
    model: &mut Vec<u8>,
    op: &Op,
    step: usize,
) -> Result<(), TestCaseError> {
    let m = Arc::clone(&handles[step % handles.len()]);
    let size = model.len();
    match op {
        Op::Update { offset, data } => {
            let r = m.update_region(*offset, data);
            if in_bounds(*offset, data.len() as u64, size) {
                prop_assert!(r.is_ok(), "update_region: {:?}", r);
                if !data.is_empty() {
                    let o = *offset as usize;
                    model[o..o + data.len()].copy_from_slice(data);
                }
            } else {
                assert_oob(r, *offset, data.len() as u64, size)?;
            }
        }
        Op::ReadInto { offset, len } => {
            let mut buf = vec![0x5Au8; *len as usize];
            let r = m.read_into(*offset, &mut buf);
            if in_bounds(*offset, *len, size) {
                prop_assert!(r.is_ok());
                if *len > 0 {
                    let o = *offset as usize;
                    prop_assert_eq!(&buf[..], &model[o..o + *len as usize]);
                }
            } else {
                assert_oob(r, *offset, *len, size)?;
            }
        }
        Op::AsSlice { offset, len } => match m.as_slice(*offset, *len) {
            Ok(s) => {
                prop_assert!(in_bounds(*offset, *len, size));
                if *len > 0 {
                    let o = *offset as usize;
                    prop_assert_eq!(&*s, &model[o..o + *len as usize]);
                } else {
                    prop_assert!(s.is_empty());
                }
            }
            Err(e) => {
                prop_assert!(!in_bounds(*offset, *len, size));
                assert_oob(Err::<(), _>(e), *offset, *len, size)?;
            }
        },
        Op::SliceMutFill { offset, len, byte } => match m.as_slice_mut(*offset, *len) {
            Ok(mut s) => {
                prop_assert!(in_bounds(*offset, *len, size));
                prop_assert_eq!(s.len() as u64, *len);
                s.as_mut().fill(*byte);
                if *len > 0 {
                    let o = *offset as usize;
                    model[o..o + *len as usize].fill(*byte);
                }
            }
            Err(e) => {
                prop_assert!(!in_bounds(*offset, *len, size));
                assert_oob(Err::<(), _>(e), *offset, *len, size)?;
            }
        },
        Op::Resize { size: new } => {
            let r = m.resize(*new);
            if *new == 0 || *new > MAX_SIZE * 1024 {
                prop_assert!(
                    matches!(r, Err(MmapIoError::ResizeFailed(_))),
                    "resize({}): {:?}",
                    new,
                    r
                );
            } else {
                prop_assert!(r.is_ok(), "resize({}): {:?}", new, r);
                model.resize(*new as usize, 0);
            }
        }
        Op::Flush => {
            prop_assert!(m.flush().is_ok());
            prop_assert_eq!(m.pending_bytes(), 0);
        }
        Op::FlushRange { offset, len } => {
            let r = m.flush_range(*offset, *len);
            if in_bounds(*offset, *len, size) {
                prop_assert!(r.is_ok(), "flush_range: {:?}", r);
            } else {
                assert_oob(r, *offset, *len, size)?;
            }
        }
        Op::SegmentWrite { offset, len, data } => {
            let parent = Arc::clone(&m);
            match SegmentMut::new(parent, *offset, *len) {
                Ok(seg) => {
                    prop_assert!(in_bounds(*offset, *len, size));
                    let r = seg.write(data);
                    if data.len() as u64 > *len {
                        let is_oob = matches!(
                            r,
                            Err(MmapIoError::OutOfBounds { offset: 0, len: l, total: t })
                                if l == data.len() as u64 && t == *len
                        );
                        prop_assert!(is_oob, "segment overflow: {:?}", r);
                    } else {
                        prop_assert!(r.is_ok(), "segment write: {:?}", r);
                        if !data.is_empty() {
                            let o = *offset as usize;
                            model[o..o + data.len()].copy_from_slice(data);
                        }
                    }
                }
                Err(e) => {
                    prop_assert!(!in_bounds(*offset, *len, size));
                    assert_oob(Err::<(), _>(e), *offset, *len, size)?;
                }
            }
        }
        Op::SegmentRead { offset, len } => match Segment::new(Arc::clone(&m), *offset, *len) {
            Ok(seg) => {
                prop_assert!(in_bounds(*offset, *len, size));
                let s = seg.as_slice();
                prop_assert!(s.is_ok());
                let s = s.unwrap();
                if *len > 0 {
                    let o = *offset as usize;
                    prop_assert_eq!(&*s, &model[o..o + *len as usize]);
                }
            }
            Err(e) => {
                prop_assert!(!in_bounds(*offset, *len, size));
                assert_oob(Err::<(), _>(e), *offset, *len, size)?;
            }
        },
        Op::ChunkSum { chunk } => {
            #[cfg(feature = "iterator")]
            {
                let mut sum = 0u64;
                let mut count = 0usize;
                for c in m.chunks(*chunk) {
                    sum += c.iter().map(|&b| u64::from(b)).sum::<u64>();
                    count += 1;
                }
                let expect_count = if *chunk == 0 {
                    0
                } else {
                    size.div_ceil(*chunk)
                };
                prop_assert_eq!(count, expect_count);
                let expect_sum: u64 = if *chunk == 0 {
                    0
                } else {
                    model.iter().map(|&b| u64::from(b)).sum()
                };
                prop_assert_eq!(sum, expect_sum);
            }
            #[cfg(not(feature = "iterator"))]
            let _ = chunk;
        }
        Op::ChunksMutFill { chunk, seed } => {
            #[cfg(feature = "iterator")]
            {
                let r = m.chunks_mut(*chunk).for_each_mut(|off, c| {
                    c.fill(seed.wrapping_add(off as u8));
                    Ok(())
                });
                prop_assert!(r.is_ok());
                if *chunk > 0 {
                    for (i, c) in model.chunks_mut(*chunk).enumerate() {
                        c.fill(seed.wrapping_add((i * *chunk) as u8));
                    }
                }
            }
            #[cfg(not(feature = "iterator"))]
            let _ = (chunk, seed);
        }
        Op::ReaderReadAll => {
            let mut out = Vec::new();
            let r = std::io::Read::read_to_end(&mut m.reader(), &mut out);
            prop_assert!(r.is_ok());
            prop_assert_eq!(&out, &*model);
        }
        Op::Clone => {
            if handles.len() < 4 {
                handles.push(Arc::new((*m).clone()));
            }
        }
    }
    // The invariant: mapping == model, after every step, through every
    // handle.
    for h in handles.iter() {
        prop_assert_eq!(h.len(), model.len() as u64);
        prop_assert_eq!(&*h.as_slice(0, model.len() as u64).unwrap(), &model[..]);
    }
    Ok(())
}

fn cases() -> u32 {
    std::env::var("PROPTEST_CASES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(256)
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: cases(),
        max_shrink_iters: 4096,
        ..ProptestConfig::default()
    })]

    #[test]
    fn mapping_matches_a_vec_model(
        initial in 1..=MAX_SIZE,
        policy in policy(),
        ops in proptest::collection::vec(op(), 1..60),
    ) {
        let path = common::tmp_path("model.bin");
        let m = MemoryMappedFile::builder(&path)
            .mode(MmapMode::ReadWrite)
            .size(initial)
            .flush_policy(policy)
            .create()
            .unwrap();
        let mut handles = vec![Arc::new(m)];
        let mut model = vec![0u8; initial as usize];
        for (step, op) in ops.iter().enumerate() {
            apply(&mut handles, &mut model, op, step)
                .map_err(|e| TestCaseError::fail(format!("step {step} {op:?}: {e}")))?;
        }

        // Durability within this process: flush, drop every handle,
        // and the file must hold exactly the model.
        handles[0].flush().unwrap();
        drop(handles);
        let on_disk = std::fs::read(&*path).unwrap();
        prop_assert_eq!(on_disk.len(), model.len());
        prop_assert!(on_disk == model, "file differs from the model after reopen");
        let ro = MemoryMappedFile::open_ro(&path).unwrap();
        prop_assert_eq!(&*ro.as_slice(0, ro.len()).unwrap(), &model[..]);
        drop(ro);
        let rw = MemoryMappedFile::open_rw(&path).unwrap();
        prop_assert_eq!(&*rw.as_slice(0, rw.len()).unwrap(), &model[..]);
    }
}
