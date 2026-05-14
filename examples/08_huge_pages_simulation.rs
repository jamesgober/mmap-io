//! Example 08: best-effort huge-page mapping.
//!
//! `.huge_pages(true)` on the builder asks the kernel to back the
//! mapping with huge pages (2 MiB on most Linux configs). This
//! reduces TLB misses for large mappings and improves throughput on
//! random-access workloads.
//!
//! It is intentionally best-effort: if `MAP_HUGETLB` fails (lacking
//! privilege, no huge pages reserved, etc.) the kernel falls back
//! to standard 4 KiB pages and the mapping still functions
//! correctly. On non-Linux platforms the flag is a no-op.
//!
//! Run with:
//!   cargo run --example 08_huge_pages_simulation --features hugepages

#[cfg(not(feature = "hugepages"))]
fn main() {
    eprintln!("This example requires the `hugepages` feature.");
    eprintln!("Re-run with: cargo run --example 08_huge_pages_simulation --features hugepages");
    std::process::exit(1);
}

#[cfg(feature = "hugepages")]
fn main() -> Result<(), mmap_io::MmapIoError> {
    use mmap_io::{MemoryMappedFile, MmapMode, TouchHint};
    use std::path::PathBuf;
    use std::time::Instant;

    let path = PathBuf::from("example_08_hugepages.bin");
    let _ = std::fs::remove_file(&path);

    // 4 MiB is enough to span at least one huge page on every
    // supported configuration where huge pages would be applied.
    let size: u64 = 4 * 1024 * 1024;

    let started = Instant::now();
    let mmap = MemoryMappedFile::builder(&path)
        .mode(MmapMode::ReadWrite)
        .size(size)
        .huge_pages(true) // best-effort; falls back to 4 KiB pages silently
        .touch_hint(TouchHint::Eager) // prewarm so the first access doesn't pay page-fault cost
        .create()?;
    let setup = started.elapsed();
    println!("Mapping created in {:?} ({} bytes)", setup, mmap.len());

    // Hot write loop. With huge pages active, the TLB miss rate is
    // dramatically lower than 4 KiB pages for sequential scans over
    // multi-megabyte regions.
    let started = Instant::now();
    let payload = vec![0xC7u8; 4096];
    let mut offset = 0u64;
    while offset < size {
        mmap.update_region(offset, &payload)?;
        offset += payload.len() as u64;
    }
    mmap.flush()?;
    let write_time = started.elapsed();
    println!("Wrote {} bytes in {:?}", size, write_time);
    println!(
        "Throughput: ~{:.1} MiB/s",
        (size as f64) / (1024.0 * 1024.0) / write_time.as_secs_f64()
    );

    println!(
        "\nNote: huge-page backing is best-effort. mmap-io does not\
        \ncurrently expose a 'did I actually get huge pages?' query;\
        \nuse /proc/<pid>/smaps to verify on Linux. The mapping is\
        \nfunctionally identical whether huge pages were granted or not."
    );

    drop(mmap);
    let _ = std::fs::remove_file(&path);
    Ok(())
}
