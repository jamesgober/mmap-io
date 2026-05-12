//! Regression test for the C3 audit finding: atomic-view references
//! must not outlive their backing memory across `resize()`.
//!
//! Before the C3 fix, `atomic_u64(...)` etc. returned a `&AtomicU64`
//! whose memory could be unmapped by a concurrent `resize()` call,
//! causing use-after-free UB. The fix returns an `AtomicView<'_, T>`
//! wrapper that holds the read lock for its lifetime; `resize()` (which
//! requires the write lock) now blocks until every live view drops.
//!
//! These tests verify the OBSERVABLE behavior:
//!   1. `resize()` actually blocks while a view is alive
//!   2. `resize()` completes promptly once the view drops
//!   3. Concurrent fetch_add over the atomic continues to work without UB
//!   4. A view obtained AFTER a resize sees the new mapping correctly
//!
//! UB itself cannot be directly observed in a normal test (it would
//! require running under MIRI or AddressSanitizer); these tests verify
//! the lock-based protection that makes UB impossible.

#![cfg(feature = "atomic")]

use mmap_io::create_mmap;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

fn tmp_path(name: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!("mmap_io_c3_test_{}_{}", name, std::process::id()));
    p
}

#[test]
fn resize_blocks_while_atomic_view_is_alive() {
    // The core C3 safety property: a live AtomicView must keep the
    // mapping pinned. resize() takes the write lock; the view holds
    // the read lock; so resize() cannot run until the view drops.
    let path = tmp_path("resize_blocks");
    let _ = fs::remove_file(&path);

    let mmap = Arc::new(create_mmap(&path, 64).expect("create"));

    // Hold an atomic view for ~300 ms.
    let view_lifetime_ms = 300;

    // Track whether resize completed.
    let resize_done = Arc::new(AtomicBool::new(false));
    let resize_done_thread = Arc::clone(&resize_done);
    let mmap_for_resize = Arc::clone(&mmap);

    // Take the view on the main thread.
    let view = mmap.atomic_u64(0).expect("atomic view");
    view.store(0xDEAD_BEEF, Ordering::SeqCst);
    let view_taken_at = Instant::now();

    // Spawn a thread that tries to resize. It should block until
    // we drop the view.
    let resize_thread = thread::spawn(move || {
        mmap_for_resize.resize(128).expect("resize");
        resize_done_thread.store(true, Ordering::Release);
        Instant::now()
    });

    // Sleep for most of the view's intended lifetime. During this
    // window, resize should NOT have completed (because we still
    // hold the read lock through `view`).
    thread::sleep(Duration::from_millis(view_lifetime_ms - 50));
    assert!(
        !resize_done.load(Ordering::Acquire),
        "C3 regression: resize() completed while an AtomicView was alive; \
         the view must hold the read lock and block writers."
    );

    // Confirm the view is still usable: a read of our stored value.
    assert_eq!(view.load(Ordering::SeqCst), 0xDEAD_BEEF);

    // Drop the view. resize should now proceed.
    drop(view);
    let view_dropped_at = Instant::now();

    // Wait for resize to finish (up to a generous timeout).
    let resize_completed_at = resize_thread.join().expect("resize thread join");

    // resize completed AFTER the view dropped.
    assert!(
        resize_completed_at > view_dropped_at,
        "resize must have completed after the view dropped"
    );
    // And shortly after (within a few hundred ms is fine).
    let resize_latency = resize_completed_at.duration_since(view_dropped_at);
    assert!(
        resize_latency < Duration::from_millis(500),
        "resize should complete promptly once view drops; took {resize_latency:?}"
    );
    // Total elapsed should be at least the view lifetime.
    let total_elapsed = resize_completed_at.duration_since(view_taken_at);
    assert!(
        total_elapsed >= Duration::from_millis(view_lifetime_ms - 50),
        "total elapsed ({total_elapsed:?}) should be at least the view lifetime"
    );

    // After resize, the mapping is 128 bytes. A new view at offset
    // 120 (which would have been out-of-bounds at 64 bytes) should
    // succeed.
    let post_resize = mmap.atomic_u64(120).expect("post-resize view");
    post_resize.store(7, Ordering::SeqCst);
    assert_eq!(post_resize.load(Ordering::SeqCst), 7);
    drop(post_resize);

    drop(mmap);
    let _ = fs::remove_file(&path);
}

#[test]
fn concurrent_atomic_fetch_add_with_resize_attempts() {
    // Stress test: multiple threads do concurrent fetch_add via
    // short-lived views; meanwhile a resize attempt waits its turn.
    // Without C3 fix this could trigger UB; with the fix the test
    // simply works because views and resize serialize cleanly via
    // the RwLock.
    let path = tmp_path("concurrent_fetch_resize");
    let _ = fs::remove_file(&path);

    let mmap = Arc::new(create_mmap(&path, 8).expect("create"));
    {
        let v = mmap.atomic_u64(0).expect("init");
        v.store(0, Ordering::SeqCst);
    }

    let stop = Arc::new(AtomicBool::new(false));

    // Spawn 4 hammer threads.
    let hammer_handles: Vec<_> = (0..4)
        .map(|_| {
            let mmap = Arc::clone(&mmap);
            let stop = Arc::clone(&stop);
            thread::spawn(move || {
                let mut count = 0u64;
                while !stop.load(Ordering::Acquire) {
                    // Each iteration takes a fresh short-lived view.
                    // The view's lifetime is just the fetch_add call.
                    let v = mmap.atomic_u64(0).expect("view");
                    v.fetch_add(1, Ordering::SeqCst);
                    count += 1;
                    if count >= 1000 {
                        break;
                    }
                }
                count
            })
        })
        .collect();

    // Spawn a thread that tries to resize after a brief delay.
    let resize_mmap = Arc::clone(&mmap);
    let resize_handle = thread::spawn(move || {
        thread::sleep(Duration::from_millis(20));
        resize_mmap.resize(64).expect("resize");
    });

    let mut total = 0u64;
    for h in hammer_handles {
        total += h.join().expect("hammer join");
    }
    stop.store(true, Ordering::Release);
    resize_handle.join().expect("resize join");

    // After all hammers finish and the resize completes, the
    // accumulated count should match: no torn writes, no panics,
    // no UB.
    let final_view = mmap.atomic_u64(0).expect("final view");
    assert_eq!(final_view.load(Ordering::SeqCst), total);
    drop(final_view);

    drop(mmap);
    let _ = fs::remove_file(&path);
}

#[test]
fn slice_view_also_blocks_resize() {
    // Same property for AtomicSliceView.
    let path = tmp_path("slice_blocks_resize");
    let _ = fs::remove_file(&path);

    let mmap = Arc::new(create_mmap(&path, 64).expect("create"));

    let slice_view = mmap.atomic_u64_slice(0, 4).expect("slice view");
    for (i, atomic) in slice_view.iter().enumerate() {
        atomic.store(i as u64, Ordering::SeqCst);
    }

    let resize_done = Arc::new(AtomicBool::new(false));
    let resize_done_t = Arc::clone(&resize_done);
    let mmap_for_resize = Arc::clone(&mmap);
    let resize_handle = thread::spawn(move || {
        mmap_for_resize.resize(256).expect("resize");
        resize_done_t.store(true, Ordering::Release);
    });

    thread::sleep(Duration::from_millis(150));
    assert!(
        !resize_done.load(Ordering::Acquire),
        "C3 regression: resize() completed while an AtomicSliceView was alive"
    );

    // Slice view is still readable.
    for (i, atomic) in slice_view.iter().enumerate() {
        assert_eq!(atomic.load(Ordering::SeqCst), i as u64);
    }

    drop(slice_view);
    resize_handle.join().expect("resize join");
    assert!(resize_done.load(Ordering::Acquire));

    drop(mmap);
    let _ = fs::remove_file(&path);
}
