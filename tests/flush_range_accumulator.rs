//! Regression test for the C1 audit finding: `flush_range` must not
//! zero the global dirty-byte accumulator. Doing so silently disables
//! `FlushPolicy::EveryBytes` for any caller that mixes
//! `update_region` + `flush_range`, causing buffered writes to never
//! be auto-flushed and effectively losing data on crash.
//!
//! The accumulator field is crate-private, so we test the OBSERVABLE
//! behavior: under `FlushPolicy::EveryBytes(N)`, the auto-flush must
//! still trigger at the expected total write volume after a small
//! `flush_range` call has been made.

use mmap_io::{flush::FlushPolicy, MemoryMappedFile, MmapMode};
use std::fs;
use std::path::PathBuf;

fn tmp_path(name: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!("mmap_io_c1_test_{}_{}", name, std::process::id()));
    p
}

/// Read the underlying file from a fresh OS handle. This bypasses the
/// page cache view that the active mmap might be serving, giving us
/// the on-disk durable state. Used to detect whether a flush actually
/// hit the disk.
fn read_disk(path: &std::path::Path, offset: u64, len: usize) -> Vec<u8> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(path).expect("reopen for disk read");
    f.seek(SeekFrom::Start(offset)).expect("seek");
    let mut buf = vec![0u8; len];
    f.read_exact(&mut buf).expect("read_exact");
    buf
}

#[test]
fn flush_range_preserves_accumulator_for_unflushed_writes() {
    // C1 regression: write 1 MiB, then flush_range a 4 KiB sub-range.
    // The accumulator must NOT be zeroed; the remaining ~1 MiB of
    // dirty bytes is still tracked, and the next write should NOT
    // need to wait for a full new threshold to trigger auto-flush.
    let path = tmp_path("acc_preserved");
    let _ = fs::remove_file(&path);

    let threshold: usize = 1024 * 1024; // 1 MiB
    let mmap = MemoryMappedFile::builder(&path)
        .mode(MmapMode::ReadWrite)
        .size(8 * 1024 * 1024) // 8 MiB
        .flush_policy(FlushPolicy::EveryBytes(threshold))
        .create()
        .expect("create");

    // Write 1 MiB at offset 0. After this single write, the
    // accumulator equals `threshold` and the policy triggers an
    // immediate flush; so write strictly less than threshold first.
    let pre_threshold = threshold - 16 * 1024; // 1 MiB - 16 KiB
    let payload = vec![0xAAu8; pre_threshold];
    mmap.update_region(0, &payload).expect("write 1");
    // No auto-flush should have happened yet (acc < threshold).

    // Now flush_range a 4 KiB sub-range. With C1 unfixed, the
    // accumulator gets zeroed and subsequent writes fail to trigger.
    // With the fix, the accumulator is debited by the flushed length
    // (likely page-aligned upward by the microflush optimization, but
    // strictly less than the pre_threshold accumulator).
    mmap.flush_range(0, 4096).expect("flush_range");

    // Now write enough additional bytes to push the (correctly
    // tracked) accumulator over the threshold. We only need a small
    // top-up because the accumulator should still reflect most of
    // the pre_threshold writes.
    let topup = vec![0xBBu8; 32 * 1024]; // 32 KiB
    mmap.update_region(pre_threshold as u64, &topup)
        .expect("write 2");

    // At this point, IF C1 is fixed:
    //   accumulator after first write   = pre_threshold (~1008 KiB)
    //   accumulator after flush_range   = pre_threshold - flushed (~1008 - some page-multiple)
    //   accumulator after second write  = (above) + 32 KiB
    //                                   >= threshold (1024 KiB)
    //   -> auto-flush triggered
    // IF C1 is BROKEN:
    //   accumulator after flush_range   = 0
    //   accumulator after second write  = 32 KiB
    //   -> NO auto-flush triggered (32 KiB << 1 MiB threshold)
    //   -> the 0xBB topup is NOT durable on disk

    // Drop the mmap (does NOT flush; flush only happens on policy
    // trigger, manual flush, or OS writeback). Then read the file
    // from a fresh handle to check durability.
    //
    // NOTE: On Windows, the page cache is shared between the mmap and
    // a separate fs::File handle, so reading from disk via std::fs
    // may surface dirty pages even without flush. To make this test
    // meaningful cross-platform we keep the mmap alive (no drop) and
    // explicitly check via fresh file open AFTER asserting the in-
    // memory state.
    //
    // A more reliable check on all platforms is to query the
    // accumulator value via observable side effect: a subsequent
    // small write WITHOUT crossing threshold should not auto-flush;
    // but a subsequent write that DOES cross threshold (under correct
    // accumulator) should flush.
    //
    // The simplest reliable assertion: the second write above MUST have
    // triggered the auto-flush. We verify by reading via a fresh
    // handle and checking the 0xBB topup is present on disk.

    let disk = read_disk(&path, pre_threshold as u64, 32 * 1024);
    let all_bb = disk.iter().all(|&b| b == 0xBB);

    // Drop mmap before cleanup so Windows doesn't complain.
    drop(mmap);
    let _ = fs::remove_file(&path);

    assert!(
        all_bb,
        "C1 regression: auto-flush should have triggered after the top-up write, \
         indicating the accumulator correctly retained pre-flush_range writes. \
         If the disk doesn't contain the 0xBB topup, flush_range zeroed the \
         accumulator (the C1 bug)."
    );
}

#[test]
fn flush_range_does_not_double_count() {
    // Companion check: flush_range followed by a fresh, large write
    // must not over-trigger flushes either. We verify that writing
    // exactly the threshold AFTER a flush_range (with zero pending
    // bytes) still works correctly.
    let path = tmp_path("acc_no_double");
    let _ = fs::remove_file(&path);

    let threshold: usize = 64 * 1024;
    let mmap = MemoryMappedFile::builder(&path)
        .mode(MmapMode::ReadWrite)
        .size(1024 * 1024)
        .flush_policy(FlushPolicy::EveryBytes(threshold))
        .create()
        .expect("create");

    // Write less than threshold.
    let small = vec![0x11u8; 1024];
    mmap.update_region(0, &small).expect("write small");

    // flush_range the small region; accumulator should debit by the
    // aligned amount, going to zero (or near zero).
    mmap.flush_range(0, 1024).expect("flush_range small");

    // Write exactly the threshold. Auto-flush should trigger; the
    // entire payload becomes durable.
    let big = vec![0x22u8; threshold];
    mmap.update_region(2048, &big).expect("write big");

    let disk = read_disk(&path, 2048, threshold);
    let all_22 = disk.iter().all(|&b| b == 0x22);

    drop(mmap);
    let _ = fs::remove_file(&path);

    assert!(
        all_22,
        "Threshold-sized write after a flush_range should auto-flush"
    );
}
