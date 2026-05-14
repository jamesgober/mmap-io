//! Example 04: lock-free shared counter via an atomic view over a
//! memory-mapped file.
//!
//! This is the canonical pattern for cross-thread (and cross-process,
//! when both processes mmap the same file) statistics: a few aligned
//! `AtomicU64` slots in a small file, every writer does
//! `fetch_add(1, ...)` against them, no locks involved.
//!
//! Run with:
//!   cargo run --example 04_atomic_counter --features atomic

#[cfg(not(feature = "atomic"))]
fn main() {
    eprintln!("This example requires the `atomic` feature.");
    eprintln!("Re-run with: cargo run --example 04_atomic_counter --features atomic");
    std::process::exit(1);
}

#[cfg(feature = "atomic")]
fn main() -> Result<(), mmap_io::MmapIoError> {
    use mmap_io::MemoryMappedFile;
    use std::path::PathBuf;
    use std::sync::atomic::Ordering;
    use std::sync::Arc;
    use std::thread;

    let path = PathBuf::from("example_04_counter.bin");
    let _ = std::fs::remove_file(&path);

    // 64 bytes is enough for 8 independent u64 counters. Cache-line
    // alignment is achieved naturally because the mapping base is
    // page-aligned.
    let mmap = Arc::new(MemoryMappedFile::create_rw(&path, 64)?);

    // Initialise the counter at offset 0 to zero.
    {
        let counter = mmap.atomic_u64(0)?;
        counter.store(0, Ordering::SeqCst);
    }

    // Spawn 8 threads, each does 10_000 fetch_add ops. Final value
    // must be exactly 80_000 if the atomic view is truly atomic
    // across threads.
    let mut handles = Vec::with_capacity(8);
    for _ in 0..8 {
        let mmap = Arc::clone(&mmap);
        handles.push(thread::spawn(
            move || -> Result<(), mmap_io::MmapIoError> {
                let view = mmap.atomic_u64(0)?;
                for _ in 0..10_000 {
                    view.fetch_add(1, Ordering::Relaxed);
                }
                Ok(())
            },
        ));
    }

    for h in handles {
        h.join().expect("thread panic")?;
    }

    let final_value = mmap.atomic_u64(0)?.load(Ordering::SeqCst);
    println!("Final counter value: {final_value} (expected 80000)");
    assert_eq!(final_value, 80_000);

    // Drop the Arc before removing the file so the mapping releases
    // first (matters on Windows where files cannot be deleted while
    // a mapping is alive).
    drop(mmap);
    let _ = std::fs::remove_file(&path);
    Ok(())
}
