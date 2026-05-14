//! Example 10: shared-memory IPC pattern using a file-backed mmap
//! and an atomic counter as the shared synchronization primitive.
//!
//! Two threads (representing two processes) each open the same file
//! and increment a shared counter. In a real deployment these would
//! be separate processes both calling `MemoryMappedFile::open_rw`
//! against the same path; the mapping is shared via the OS-level
//! file-backed shared mapping (MAP_SHARED on POSIX, the default
//! CreateFileMapping behavior on Windows).
//!
//! Run with:
//!   cargo run --example 10_ipc_shared_state --features atomic

#[cfg(not(feature = "atomic"))]
fn main() {
    eprintln!("This example requires the `atomic` feature.");
    eprintln!("Re-run with: cargo run --example 10_ipc_shared_state --features atomic");
    std::process::exit(1);
}

#[cfg(feature = "atomic")]
fn main() -> Result<(), mmap_io::MmapIoError> {
    use mmap_io::MemoryMappedFile;
    use std::path::PathBuf;
    use std::sync::atomic::Ordering;
    use std::thread;
    use std::time::Duration;

    let path = PathBuf::from("example_10_shared.bin");
    let _ = std::fs::remove_file(&path);

    // 64 bytes: 8 slots for u64 counters, plus headroom.
    // Offset 0  = "writer ready" flag (becomes 1 once writer initialised)
    // Offset 8  = total writes counter
    // Offset 16 = last write timestamp (nanoseconds since some epoch)
    let writer = MemoryMappedFile::create_rw(&path, 64)?;
    {
        let ready = writer.atomic_u64(0)?;
        ready.store(0, Ordering::SeqCst);
    }

    // The "writer process" thread: opens its own handle, marks
    // ready, then increments a counter periodically.
    let path_for_writer = path.clone();
    let writer_handle = thread::spawn(move || -> Result<(), mmap_io::MmapIoError> {
        // In a real IPC scenario this would be a separate process
        // calling `open_rw` against the same path.
        let mmap = MemoryMappedFile::open_rw(&path_for_writer)?;
        let ready = mmap.atomic_u64(0)?;
        let counter = mmap.atomic_u64(8)?;

        ready.store(1, Ordering::SeqCst);
        println!("[writer] ready, beginning increments");

        for i in 0..20 {
            counter.fetch_add(1, Ordering::Relaxed);
            // Don't hammer the cache line; modest pacing.
            thread::sleep(Duration::from_millis(20));
            if i == 9 {
                println!("[writer] halfway through ({} increments so far)", i + 1);
            }
        }
        println!("[writer] done");
        Ok(())
    });

    // The "reader process" thread: waits for the writer to signal
    // ready, then polls the counter and reports observed values.
    let path_for_reader = path.clone();
    let reader_handle = thread::spawn(move || -> Result<(), mmap_io::MmapIoError> {
        let mmap = MemoryMappedFile::open_rw(&path_for_reader)?;
        let ready = mmap.atomic_u64(0)?;
        let counter = mmap.atomic_u64(8)?;

        // Spin until the writer signals ready.
        while ready.load(Ordering::SeqCst) == 0 {
            thread::sleep(Duration::from_millis(5));
        }
        println!("[reader] writer ready, polling counter");

        let mut last_seen: u64 = 0;
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while std::time::Instant::now() < deadline {
            let v = counter.load(Ordering::Relaxed);
            if v != last_seen {
                println!("[reader] counter advanced to {}", v);
                last_seen = v;
                if v >= 20 {
                    break;
                }
            }
            thread::sleep(Duration::from_millis(10));
        }

        println!(
            "[reader] done at counter={}",
            counter.load(Ordering::SeqCst)
        );
        Ok(())
    });

    writer_handle.join().expect("writer panic")?;
    reader_handle.join().expect("reader panic")?;

    let final_count = writer.atomic_u64(8)?.load(Ordering::SeqCst);
    println!("Final shared counter value: {final_count} (expected 20)");
    assert_eq!(final_count, 20);

    drop(writer);
    let _ = std::fs::remove_file(&path);
    Ok(())
}
