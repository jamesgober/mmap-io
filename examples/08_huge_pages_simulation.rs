//! Example 08: best-effort huge-page mapping.
//!
//! `.huge_pages(true)` on the builder issues `madvise(MADV_HUGEPAGE)`
//! on Linux, asking the kernel to back the mapping with transparent
//! huge pages (2 MiB on most configs). When honored, this reduces TLB
//! misses for large mappings.
//!
//! It is only a hint. For a file on a typical disk filesystem the
//! kernel keeps base pages; tmpfs/shmem mounted with `huge=` can use
//! huge pages. `MAP_HUGETLB` is not used. On non-Linux platforms the
//! flag has no effect. `is_hugepage_backed()` reports the outcome.
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

    let path: PathBuf = std::env::temp_dir().join("example_08_hugepages.bin");
    let _ = std::fs::remove_file(&path);

    // 4 MiB is enough to span at least one huge page on every
    // supported configuration where huge pages would be applied.
    let size: u64 = 4 * 1024 * 1024;

    let started = Instant::now();
    let mmap = MemoryMappedFile::builder(&path)
        .mode(MmapMode::ReadWrite)
        .size(size)
        .huge_pages(true) // a hint; the kernel may keep base pages
        .touch_hint(TouchHint::Eager) // prewarm so the first access doesn't pay page-fault cost
        .create()?;
    let setup = started.elapsed();
    println!("Mapping created in {:?} ({} bytes)", setup, mmap.len());

    // Hot write loop. If the kernel did use huge pages, the TLB miss
    // rate is lower than with 4 KiB pages for scans over
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

    // Linux reports Some(true/false); other platforms report None.
    println!("Huge-page backed: {:?}", mmap.is_hugepage_backed());
    println!(
        "\nNote: huge pages are a hint. The mapping is functionally\
        \nidentical whether the kernel granted them or not."
    );

    drop(mmap);
    let _ = std::fs::remove_file(&path);
    Ok(())
}
