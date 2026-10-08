//! Example demonstrating several mmap-io features: the huge-page
//! hint, eager page touching, time-based flushing, range flushes, and
//! a cold-vs-warm performance comparison.

use mmap_io::{flush::FlushPolicy, MemoryMappedFile, MmapMode, TouchHint};
use std::io::ErrorKind;

fn cleanup_path<P: AsRef<std::path::Path>>(p: P) {
    let p = p.as_ref();
    match std::fs::remove_file(p) {
        Ok(_) => {}
        Err(e) if e.kind() == ErrorKind::NotFound => {}
        Err(_) => {
            // Either it's a directory or some other error.
            // Try removing as a directory; if that fails for any reason
            // other than NotFound, panic.
            if let Err(e) = std::fs::remove_dir_all(p) {
                if e.kind() != ErrorKind::NotFound {
                    panic!("cleanup: {e}");
                }
            }
        }
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("Demonstrating major mmap-io features...\n");

    // 1. Huge-page hint (Linux MADV_HUGEPAGE; no-op elsewhere).
    println!("1. Creating mapping with the huge-page hint...");
    #[cfg(feature = "hugepages")]
    let mmap = MemoryMappedFile::builder("demo_huge.bin")
        .mode(MmapMode::ReadWrite)
        .size(4 * 1024 * 1024) // 4MB
        .huge_pages(true) // a hint; the kernel may keep regular pages
        .create()?;

    #[cfg(not(feature = "hugepages"))]
    let mmap = MemoryMappedFile::builder("demo_huge.bin")
        .mode(MmapMode::ReadWrite)
        .size(4 * 1024 * 1024)
        .create()?;

    println!(
        "  Mapping created (huge-page backed: {:?})",
        mmap.is_hugepage_backed()
    );

    // 2. TouchHint::Eager for benchmarking consistency.
    println!("\n2. Creating mapping with eager page touching...");
    let benchmark_mmap = MemoryMappedFile::builder("benchmark.bin")
        .mode(MmapMode::ReadWrite)
        .size(2 * 1024 * 1024) // 2MB
        .touch_hint(TouchHint::Eager) // Pre-touch all pages during creation
        .create()?;

    println!("  All pages pre-touched for consistent benchmark timing");

    // 3. Time-based flushing.
    println!("\n3. Creating mapping with automatic time-based flushing...");
    let auto_flush_mmap = MemoryMappedFile::builder("auto_flush.bin")
        .mode(MmapMode::ReadWrite)
        .size(1024 * 1024) // 1MB
        .flush_policy(FlushPolicy::EveryMillis(500)) // Auto-flush every 500ms
        .create()?;

    println!("  Time-based flushing enabled (500ms intervals)");

    // 4. Combination: huge pages + eager touch + automatic flushing.
    println!("\n4. Creating mapping with all major optimizations enabled...");
    let ultimate_builder = MemoryMappedFile::builder("ultimate.bin")
        .mode(MmapMode::ReadWrite)
        .size(8 * 1024 * 1024) // 8MB
        .touch_hint(TouchHint::Eager) // Pre-touch for benchmarks
        .flush_policy(FlushPolicy::EveryBytes(1024 * 1024)); // Flush every 1MB
    #[cfg(feature = "hugepages")]
    let ultimate_builder = ultimate_builder.huge_pages(true);

    let ultimate_mmap = ultimate_builder.create()?;
    println!("  Combined mapping created with all optimizations");

    // 5. Touch pages functionality.
    println!("\n5. Demonstrating explicit page touching...");

    // Touch all pages explicitly
    ultimate_mmap.touch_pages()?;
    println!("  All pages touched explicitly");

    // Touch specific range
    ultimate_mmap.touch_pages_range(0, 1024 * 1024)?; // Touch first 1MB
    println!("  First 1MB pages touched");

    // 6. Range flush.
    println!("\n6. Flushing a small range...");

    let small_data = vec![0x42; 512]; // 512 bytes (sub-page)
    ultimate_mmap.update_region(0, &small_data)?;

    // Synchronous flush of the page holding these bytes.
    ultimate_mmap.flush_range(0, 512)?;
    println!("  Range flush completed (the OS flushes whole pages)");

    // 7. Performance comparison: cold vs warm access.
    println!("\n7. Performance comparison demonstration...");

    // Create mapping without eager touch
    let cold_mmap = MemoryMappedFile::builder("cold.bin")
        .mode(MmapMode::ReadWrite)
        .size(2 * 1024 * 1024)
        .touch_hint(TouchHint::Never)
        .create()?;

    // Time cold access (with page faults)
    let start = std::time::Instant::now();
    let data = vec![0xAB; 4096];
    for i in 0..512 {
        cold_mmap.update_region(i * 4096, &data)?;
    }
    let cold_time = start.elapsed();

    // Time warm access (pages already touched)
    let start = std::time::Instant::now();
    for i in 0..512 {
        ultimate_mmap.update_region(i * 4096, &data)?;
    }
    let warm_time = start.elapsed();

    println!("  Performance results:");
    println!("    Cold access (with page faults): {cold_time:?}");
    println!("    Warm access (pre-touched):      {warm_time:?}");
    let speedup = cold_time.as_nanos() as f64 / warm_time.as_nanos() as f64;
    println!("    Speedup: {speedup:.2}x faster with page prewarming");

    // 8. Huge pages behavior.
    println!("\n8. Huge pages behavior:");
    println!("    Linux: madvise(MADV_HUGEPAGE) hint on RW mappings");
    println!("    Other platforms: no effect");
    println!("    Note: .huge_pages(true) does NOT guarantee huge pages");

    // 9. Clean up.
    println!("\n9. Cleaning up...");
    drop(mmap);
    drop(benchmark_mmap);
    drop(auto_flush_mmap);
    drop(ultimate_mmap);
    drop(cold_mmap);

    // Remove test files
    cleanup_path("demo_huge.bin");
    cleanup_path("benchmark.bin");
    cleanup_path("auto_flush.bin");
    cleanup_path("ultimate.bin");
    cleanup_path("cold.bin");

    println!("  Cleanup completed");

    println!("\nAll major features demonstrated successfully.\n");
    println!("Summary:");
    println!("  - The huge-page hint never fails the mapping");
    println!("  - Eager page touching removes first-access page faults");
    println!("  - Time-based flushing runs on a background thread");
    println!("  - flush() and flush_range() are synchronous");

    Ok(())
}
