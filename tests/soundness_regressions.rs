//! Regression tests for lock-lifetime and resize-race soundness bugs.
//!
//! Each test pins one observable property:
//!
//! - Items yielded by `chunks()` / `pages()` keep the mapping pinned
//!   after the iterator itself is dropped (a yielded chunk used to
//!   borrow memory that `resize()` could unmap).
//! - Atomic views refuse read-only and copy-on-write mappings, whose
//!   pages are not writable.
//! - A shrinking `resize()` does not truncate the file while a view
//!   into the doomed tail is still alive.
//! - Accessors validate against the length protected by the lock they
//!   hold, so a concurrent `resize()` yields `OutOfBounds`, never a
//!   panic or an out-of-range access.
//! - Taking a second read view on the same thread while a writer is
//!   queued does not deadlock.

#![cfg(feature = "iterator")]

use mmap_io::MemoryMappedFile;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

fn tmp_path(name: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!("mmap_io_soundness_{}_{}", name, std::process::id()));
    p
}

/// Spawn `resize(new_size)` on another thread. The returned flag flips
/// to `true` once `resize` has returned.
fn resize_in_background(
    mmap: &Arc<MemoryMappedFile>,
    new_size: u64,
) -> (thread::JoinHandle<()>, Arc<AtomicBool>) {
    let done = Arc::new(AtomicBool::new(false));
    let done_t = Arc::clone(&done);
    let m = Arc::clone(mmap);
    let h = thread::spawn(move || {
        m.resize(new_size).expect("resize");
        done_t.store(true, Ordering::SeqCst);
    });
    (h, done)
}

#[cfg(feature = "iterator")]
#[test]
fn yielded_chunk_pins_mapping_after_iterator_drops() {
    let path = tmp_path("chunk_pins");
    let _ = fs::remove_file(&path);
    let mmap = Arc::new(MemoryMappedFile::create_rw(&path, 64 * 1024).expect("create"));
    mmap.update_region(60 * 1024, &[0x5A; 4096]).expect("seed");

    // `last()` consumes and drops the iterator; only the chunk lives on.
    let chunk = mmap.chunks(4096).last().expect("at least one chunk");

    // Growing remaps the file, so the chunk's old address would dangle.
    let (h, done) = resize_in_background(&mmap, 1024 * 1024);
    thread::sleep(Duration::from_millis(200));
    assert!(
        !done.load(Ordering::SeqCst),
        "resize() completed while a yielded chunk was still alive"
    );
    assert!(chunk.iter().all(|&b| b == 0x5A));
    drop(chunk);

    h.join().expect("resize thread");
    assert!(done.load(Ordering::SeqCst));
    assert_eq!(mmap.len(), 1024 * 1024);

    drop(mmap);
    let _ = fs::remove_file(&path);
}

#[cfg(feature = "iterator")]
#[test]
fn yielded_page_pins_mapping_after_iterator_drops() {
    let path = tmp_path("page_pins");
    let _ = fs::remove_file(&path);
    let ps = mmap_io::utils::page_size() as u64;
    let mmap = Arc::new(MemoryMappedFile::create_rw(&path, ps * 4).expect("create"));

    let pages: Vec<_> = mmap.pages().collect();
    assert_eq!(pages.len(), 4);

    let (h, done) = resize_in_background(&mmap, ps * 64);
    thread::sleep(Duration::from_millis(200));
    assert!(
        !done.load(Ordering::SeqCst),
        "resize() completed while yielded pages were still alive"
    );
    drop(pages);
    h.join().expect("resize thread");
    assert_eq!(mmap.len(), ps * 64);

    drop(mmap);
    let _ = fs::remove_file(&path);
}
