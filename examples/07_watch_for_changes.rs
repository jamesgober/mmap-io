//! Example 07: react to file changes via the native watch backend
//! (inotify on Linux, FSEvents on macOS, ReadDirectoryChangesW on
//! Windows).
//!
//! The callback runs on a dedicated dispatcher thread once per
//! detected change. We modify the file externally (via std::fs) to
//! simulate "another process touched it" — that's what every native
//! FS watcher is actually designed to observe. mmap-side writes
//! reach the watcher only at OS-decided writeback time, which is
//! not reliable enough to demo.
//!
//! Run with:
//!   cargo run --example 07_watch_for_changes --features watch

#[cfg(not(feature = "watch"))]
fn main() {
    eprintln!("This example requires the `watch` feature.");
    eprintln!("Re-run with: cargo run --example 07_watch_for_changes --features watch");
    std::process::exit(1);
}

#[cfg(feature = "watch")]
fn main() -> Result<(), mmap_io::MmapIoError> {
    use mmap_io::{watch::ChangeEvent, MemoryMappedFile};
    use std::io::Write;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::thread;
    use std::time::Duration;

    let path = PathBuf::from("example_07_watched.bin");
    let _ = std::fs::remove_file(&path);

    let mmap = MemoryMappedFile::create_rw(&path, 64)?;
    let counter = Arc::new(AtomicUsize::new(0));

    let handle = {
        let counter = Arc::clone(&counter);
        mmap.watch(move |event: ChangeEvent| {
            let n = counter.fetch_add(1, Ordering::SeqCst) + 1;
            println!("  event #{n:2}: {:?}", event.kind);
        })?
    };

    // Let the OS register the subscription.
    thread::sleep(Duration::from_millis(200));

    // Simulate 5 external modifications. Native backends will pick
    // these up within milliseconds on Linux/Windows, ~50ms on macOS.
    for i in 0..5u8 {
        let payload = format!("update-{i}-from-external-handle");
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .expect("reopen for external write");
        f.write_all(payload.as_bytes()).expect("write");
        f.sync_all().expect("sync");
        thread::sleep(Duration::from_millis(50));
    }

    // Give the dispatcher time to flush all events through.
    thread::sleep(Duration::from_millis(500));

    println!("Total events observed: {}", counter.load(Ordering::SeqCst));

    // Dropping the handle stops the watcher and releases the OS
    // subscription.
    drop(handle);
    drop(mmap);
    let _ = std::fs::remove_file(&path);
    Ok(())
}
