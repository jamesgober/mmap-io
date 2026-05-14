//! Example 01: read a file via a read-only memory map.
//!
//! Demonstrates the simplest use case: open an existing file with
//! `MemoryMappedFile::open_ro`, take a zero-copy slice over part of
//! the mapped region, and use it as `&[u8]`.
//!
//! Run with:
//!   cargo run --example 01_read_a_file -- <path-to-file>
//! (defaults to Cargo.toml in the current directory if no path given)

use mmap_io::MemoryMappedFile;
use std::env;
use std::path::PathBuf;

fn main() -> Result<(), mmap_io::MmapIoError> {
    let path: PathBuf = env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("Cargo.toml"));

    let mmap = MemoryMappedFile::open_ro(&path)?;
    let len = mmap.len();
    println!("Mapped {} bytes from {}", len, path.display());

    // Zero-copy view of the first 64 bytes (or the whole file if smaller).
    let preview_len = len.min(64);
    if preview_len > 0 {
        // `as_slice` returns `MappedSlice<'_>`, which derefs to `&[u8]`.
        let slice = mmap.as_slice(0, preview_len)?;
        let printable = String::from_utf8_lossy(&slice);
        println!("First {} bytes: {:?}", preview_len, printable);
    } else {
        println!("(file is empty)");
    }

    Ok(())
}
