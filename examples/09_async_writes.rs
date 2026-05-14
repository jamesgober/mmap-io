//! Example 09: async write + flush via Tokio's spawn_blocking.
//!
//! `update_region_async` and `flush_async` move the blocking
//! syscalls onto the blocking pool so the async scheduler is never
//! stuck on disk I/O. Each `update_region_async` call also flushes
//! after the write (async-only-flushing) to guarantee post-await
//! cross-platform visibility.
//!
//! Run with:
//!   cargo run --example 09_async_writes --features async

#[cfg(not(feature = "async"))]
fn main() {
    eprintln!("This example requires the `async` feature.");
    eprintln!("Re-run with: cargo run --example 09_async_writes --features async");
    std::process::exit(1);
}

#[cfg(feature = "async")]
#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> Result<(), mmap_io::MmapIoError> {
    use mmap_io::MemoryMappedFile;
    use std::path::PathBuf;

    let path = PathBuf::from("example_09_async.bin");
    let _ = std::fs::remove_file(&path);

    let mmap = MemoryMappedFile::create_rw(&path, 4096)?;

    // Async write that flushes after returning.
    mmap.update_region_async(0, b"async-write-block-1").await?;
    mmap.update_region_async(128, b"async-write-block-2")
        .await?;

    // Explicit async flush; redundant after update_region_async, but
    // shown here for callers who batch sync update_region calls and
    // want to defer flushing to a single async boundary.
    mmap.flush_async().await?;

    // Round-trip read.
    let mut buf = [0u8; 19];
    mmap.read_into(0, &mut buf)?;
    println!("Read back: {:?}", String::from_utf8_lossy(&buf));

    drop(mmap);
    let _ = std::fs::remove_file(&path);
    Ok(())
}
