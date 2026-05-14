//! Example 02: create a new file, write to it via mmap, flush, and
//! verify the bytes round-trip through a fresh read-only mapping.
//!
//! Demonstrates the basic write workflow:
//!   1. `create_rw` to allocate a sized file
//!   2. `update_region` for in-place writes
//!   3. `flush` for durability
//!   4. `open_ro` + `as_slice` to read back via a separate mapping
//!
//! Run with:
//!   cargo run --example 02_create_and_write

use mmap_io::MemoryMappedFile;
use std::path::PathBuf;

fn main() -> Result<(), mmap_io::MmapIoError> {
    let path = PathBuf::from("example_02_output.bin");
    let _ = std::fs::remove_file(&path);

    // Create a 1 KiB file mapped read-write.
    let mmap = MemoryMappedFile::create_rw(&path, 1024)?;
    println!("Created {} ({} bytes)", path.display(), mmap.len());

    // Write a greeting at offset 0 and a marker further in.
    mmap.update_region(0, b"hello, mmap-io")?;
    mmap.update_region(64, b"...and again at offset 64")?;

    // Force durability before dropping the mapping.
    mmap.flush()?;
    drop(mmap);

    // Round-trip verification: open read-only and inspect.
    let ro = MemoryMappedFile::open_ro(&path)?;
    {
        let head = ro.as_slice(0, 14)?;
        let mid = ro.as_slice(64, 25)?;
        println!("Read back head: {:?}", String::from_utf8_lossy(&head));
        println!("Read back mid:  {:?}", String::from_utf8_lossy(&mid));
    } // slices dropped here, releasing the borrow on `ro`

    drop(ro);
    let _ = std::fs::remove_file(&path);
    Ok(())
}
