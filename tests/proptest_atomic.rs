//! Property tests for the atomic view alignment + bounds contract.
//!
//! `atomic_u32`, `atomic_u64`, `atomic_u32_slice`, `atomic_u64_slice`
//! all carry a hard requirement: the offset must be naturally aligned
//! for the element type, and the requested range must lie within the
//! file. Violations MUST surface as `MmapIoError::Misaligned` or
//! `MmapIoError::OutOfBounds`, never as undefined behavior.
//!
//! These tests generate random offsets (including deliberately
//! misaligned ones) and verify the right error variant fires for each
//! invalid input, and that aligned in-bounds requests succeed.

#![cfg(feature = "atomic")]

use mmap_io::{errors::MmapIoError, MemoryMappedFile};
use proptest::prelude::*;
use std::path::PathBuf;
use std::sync::atomic::Ordering;

const FILE_SIZE: u64 = 4096; // single page, plenty of room for atomic slots

fn tmp_path(tag: &str, seed: u64) -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!(
        "mmap_io_proptest_atomic_{}_{}_{}",
        tag,
        std::process::id(),
        seed
    ));
    p
}

fn fresh_rw(tag: &str, seed: u64) -> (MemoryMappedFile, PathBuf) {
    let path = tmp_path(tag, seed);
    let _ = std::fs::remove_file(&path);
    let mmap = MemoryMappedFile::create_rw(&path, FILE_SIZE).expect("create_rw");
    (mmap, path)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(1024))]

    /// `atomic_u64`: aligned + in-bounds offsets succeed. Misaligned
    /// offsets MUST yield `Misaligned`. OOB offsets MUST yield
    /// `OutOfBounds`. No silent UB.
    #[test]
    fn atomic_u64_alignment_and_bounds(offset in 0u64..(FILE_SIZE * 2)) {
        let (mmap, path) = fresh_rw("u64_align", offset);

        let aligned = offset % 8 == 0;
        let in_bounds = offset.saturating_add(8) <= FILE_SIZE;

        // Scope the view inside a block so it (and the borrow of
        // `mmap`) is dropped before `drop(mmap)` below.
        {
            let result = mmap.atomic_u64(offset);
            match result {
                Ok(view) => {
                    prop_assert!(aligned, "got Ok for misaligned offset {}", offset);
                    prop_assert!(in_bounds, "got Ok for OOB offset {}", offset);
                    // Round-trip: a store/load should reflect.
                    view.store(0xDEADBEEFCAFEBABE, Ordering::SeqCst);
                    prop_assert_eq!(view.load(Ordering::SeqCst), 0xDEADBEEFCAFEBABE);
                }
                Err(MmapIoError::Misaligned { required, offset: e_off }) => {
                    prop_assert!(!aligned, "got Misaligned for aligned offset {}", offset);
                    prop_assert_eq!(required, 8);
                    prop_assert_eq!(e_off, offset);
                }
                Err(MmapIoError::OutOfBounds { offset: e_off, len, total }) => {
                    prop_assert!(!in_bounds, "got OutOfBounds for in-bounds offset {}", offset);
                    prop_assert!(aligned, "got OutOfBounds before Misaligned for misaligned offset {}", offset);
                    prop_assert_eq!(e_off, offset);
                    prop_assert_eq!(len, 8);
                    prop_assert_eq!(total, FILE_SIZE);
                }
                Err(other) => prop_assert!(false, "unexpected error variant: {:?}", other),
            }
        }

        drop(mmap);
        let _ = std::fs::remove_file(&path);
    }

    /// `atomic_u32`: same property as `atomic_u64` with 4-byte
    /// alignment.
    #[test]
    fn atomic_u32_alignment_and_bounds(offset in 0u64..(FILE_SIZE * 2)) {
        let (mmap, path) = fresh_rw("u32_align", offset);

        let aligned = offset % 4 == 0;
        let in_bounds = offset.saturating_add(4) <= FILE_SIZE;

        {
            let result = mmap.atomic_u32(offset);
            match result {
                Ok(view) => {
                    prop_assert!(aligned, "got Ok for misaligned offset {}", offset);
                    prop_assert!(in_bounds, "got Ok for OOB offset {}", offset);
                    view.store(0xDEADBEEF, Ordering::SeqCst);
                    prop_assert_eq!(view.load(Ordering::SeqCst), 0xDEADBEEF);
                }
                Err(MmapIoError::Misaligned { required, offset: e_off }) => {
                    prop_assert!(!aligned);
                    prop_assert_eq!(required, 4);
                    prop_assert_eq!(e_off, offset);
                }
                Err(MmapIoError::OutOfBounds { offset: e_off, len, total }) => {
                    prop_assert!(!in_bounds);
                    prop_assert!(aligned);
                    prop_assert_eq!(e_off, offset);
                    prop_assert_eq!(len, 4);
                    prop_assert_eq!(total, FILE_SIZE);
                }
                Err(other) => prop_assert!(false, "unexpected error variant: {:?}", other),
            }
        }

        drop(mmap);
        let _ = std::fs::remove_file(&path);
    }

    /// Slice views: same alignment requirement on the starting
    /// offset; bounds checked against the total range
    /// `offset + count * size_of::<T>()`.
    #[test]
    fn atomic_u64_slice_alignment_and_bounds(
        offset in 0u64..(FILE_SIZE * 2),
        count in 0usize..(FILE_SIZE / 8 + 16) as usize,
    ) {
        let (mmap, path) = fresh_rw("u64_slice", offset.wrapping_add(count as u64));

        let aligned = offset % 8 == 0;
        let total_bytes = (count as u64).saturating_mul(8);
        let in_bounds = offset.saturating_add(total_bytes) <= FILE_SIZE;

        {
            let result = mmap.atomic_u64_slice(offset, count);
            match result {
                Ok(view) => {
                    prop_assert!(aligned);
                    prop_assert!(in_bounds);
                    prop_assert_eq!(view.len(), count);
                    // Touch every element to confirm the slice is real
                    // (no spurious zero-len decoy).
                    for (i, atom) in view.iter().enumerate() {
                        atom.store(i as u64, Ordering::SeqCst);
                    }
                    for (i, atom) in view.iter().enumerate() {
                        prop_assert_eq!(atom.load(Ordering::SeqCst), i as u64);
                    }
                }
                Err(MmapIoError::Misaligned { required, offset: e_off }) => {
                    prop_assert!(!aligned);
                    prop_assert_eq!(required, 8);
                    prop_assert_eq!(e_off, offset);
                }
                Err(MmapIoError::OutOfBounds { offset: e_off, len: e_len, total }) => {
                    prop_assert!(!in_bounds);
                    prop_assert!(aligned);
                    prop_assert_eq!(e_off, offset);
                    prop_assert_eq!(e_len, total_bytes);
                    prop_assert_eq!(total, FILE_SIZE);
                }
                Err(other) => prop_assert!(false, "unexpected error variant: {:?}", other),
            }
        }

        drop(mmap);
        let _ = std::fs::remove_file(&path);
    }

    /// Slice view for u32 - same contract with 4-byte alignment.
    #[test]
    fn atomic_u32_slice_alignment_and_bounds(
        offset in 0u64..(FILE_SIZE * 2),
        count in 0usize..(FILE_SIZE / 4 + 16) as usize,
    ) {
        let (mmap, path) = fresh_rw("u32_slice", offset.wrapping_add(count as u64));

        let aligned = offset % 4 == 0;
        let total_bytes = (count as u64).saturating_mul(4);
        let in_bounds = offset.saturating_add(total_bytes) <= FILE_SIZE;

        {
            let result = mmap.atomic_u32_slice(offset, count);
            match result {
                Ok(view) => {
                    prop_assert!(aligned);
                    prop_assert!(in_bounds);
                    prop_assert_eq!(view.len(), count);
                    for (i, atom) in view.iter().enumerate() {
                        atom.store(i as u32, Ordering::SeqCst);
                    }
                    for (i, atom) in view.iter().enumerate() {
                        prop_assert_eq!(atom.load(Ordering::SeqCst), i as u32);
                    }
                }
                Err(MmapIoError::Misaligned { required, offset: e_off }) => {
                    prop_assert!(!aligned);
                    prop_assert_eq!(required, 4);
                    prop_assert_eq!(e_off, offset);
                }
                Err(MmapIoError::OutOfBounds { offset: e_off, len: e_len, total }) => {
                    prop_assert!(!in_bounds);
                    prop_assert!(aligned);
                    prop_assert_eq!(e_off, offset);
                    prop_assert_eq!(e_len, total_bytes);
                    prop_assert_eq!(total, FILE_SIZE);
                }
                Err(other) => prop_assert!(false, "unexpected error variant: {:?}", other),
            }
        }

        drop(mmap);
        let _ = std::fs::remove_file(&path);
    }
}
