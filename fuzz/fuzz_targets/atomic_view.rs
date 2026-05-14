//! Fuzz target: atomic-view alignment and bounds.
//!
//! Generates random offsets (deliberately including misaligned ones)
//! and exercises all four atomic accessors. The contract:
//!   - Misaligned offsets MUST return `Misaligned`, not UB
//!   - Out-of-bounds offsets MUST return `OutOfBounds`
//!   - Aligned in-bounds offsets MUST return an `AtomicView` /
//!     `AtomicSliceView` whose load/store roundtrips work

#![no_main]

use libfuzzer_sys::fuzz_target;
use mmap_io::MemoryMappedFile;
use std::sync::atomic::Ordering;

#[derive(arbitrary::Arbitrary, Debug)]
struct Input {
    offset: u64,
    slot_kind: u8,
    slice_count: u8,
}

fuzz_target!(|input: Input| {
    let path = std::env::temp_dir().join(format!(
        "mmap_io_fuzz_atomic_{}",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);

    // Fixed 4 KiB file is plenty for atomic-slot fuzzing.
    let Ok(mmap) = MemoryMappedFile::create_rw(&path, 4096) else {
        let _ = std::fs::remove_file(&path);
        return;
    };

    match input.slot_kind % 4 {
        0 => {
            // atomic_u32: 4-byte alignment expected
            if let Ok(view) = mmap.atomic_u32(input.offset) {
                view.store(0xDEADBEEF, Ordering::SeqCst);
                let _ = view.load(Ordering::SeqCst);
            }
        }
        1 => {
            // atomic_u64: 8-byte alignment expected
            if let Ok(view) = mmap.atomic_u64(input.offset) {
                view.store(0xDEADBEEFCAFEBABE, Ordering::SeqCst);
                let _ = view.load(Ordering::SeqCst);
            }
        }
        2 => {
            // atomic_u32_slice
            let count = (input.slice_count as usize) % 32;
            if let Ok(view) = mmap.atomic_u32_slice(input.offset, count) {
                for (i, slot) in view.iter().enumerate() {
                    slot.store(i as u32, Ordering::Relaxed);
                }
            }
        }
        _ => {
            // atomic_u64_slice
            let count = (input.slice_count as usize) % 16;
            if let Ok(view) = mmap.atomic_u64_slice(input.offset, count) {
                for (i, slot) in view.iter().enumerate() {
                    slot.store(i as u64, Ordering::Relaxed);
                }
            }
        }
    }

    drop(mmap);
    let _ = std::fs::remove_file(&path);
});
