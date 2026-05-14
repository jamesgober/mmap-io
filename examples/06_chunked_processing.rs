//! Example 06: bulk-scan a mapped file via the zero-copy chunk
//! iterator.
//!
//! `mmap.chunks(N)` yields `MappedSlice<'a>` items borrowed directly
//! from the mapped region. No allocation per chunk, no memcpy.
//! Perfect for big sequential scans (parsing a binary file, computing
//! a hash, counting newlines, etc.) where you want predictable I/O
//! and stable memory residence.
//!
//! Run with:
//!   cargo run --example 06_chunked_processing

#[cfg(not(feature = "iterator"))]
fn main() {
    eprintln!("This example requires the `iterator` feature (default-on).");
    eprintln!("Re-run with: cargo run --example 06_chunked_processing");
    std::process::exit(1);
}

#[cfg(feature = "iterator")]
fn main() -> Result<(), mmap_io::MmapIoError> {
    use mmap_io::MemoryMappedFile;
    use std::path::PathBuf;

    let path = PathBuf::from("example_06_scan.bin");
    let _ = std::fs::remove_file(&path);

    // Seed a 1 MiB file with a deterministic byte pattern so the
    // scan result is reproducible.
    let total_bytes: usize = 1024 * 1024;
    let mmap = MemoryMappedFile::create_rw(&path, total_bytes as u64)?;
    for offset in (0..total_bytes).step_by(4096) {
        let len = std::cmp::min(4096, total_bytes - offset);
        let value = ((offset / 4096) as u8).wrapping_mul(7);
        mmap.update_region(offset as u64, &vec![value; len])?;
    }
    mmap.flush()?;
    drop(mmap);

    // Re-open read-only and scan via the zero-copy iterator.
    let ro = MemoryMappedFile::open_ro(&path)?;
    let chunk_size = 64 * 1024;

    let mut total_bytes_seen: u64 = 0;
    let mut byte_sum: u64 = 0;
    let mut chunks_seen: u32 = 0;

    // Each `chunk` is a `MappedSlice<'_>` borrowed from `ro`. No
    // allocation here.
    for chunk in ro.chunks(chunk_size) {
        chunks_seen += 1;
        total_bytes_seen += chunk.len() as u64;
        for &b in chunk.iter() {
            byte_sum = byte_sum.wrapping_add(u64::from(b));
        }
    }

    println!(
        "Scanned {} chunks (~{} KiB each)",
        chunks_seen,
        chunk_size / 1024
    );
    println!("Total bytes:    {total_bytes_seen}");
    println!("Byte sum:       0x{byte_sum:016X}");

    drop(ro);
    let _ = std::fs::remove_file(&path);
    Ok(())
}
