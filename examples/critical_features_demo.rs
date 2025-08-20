//! Example demonstrating all the critical new features

use mmap_io::{flush::FlushPolicy, MemoryMappedFile, MmapMode, TouchHint};
use std::io::ErrorKind;

fn cleanup_path<P: AsRef<std::path::Path>>(p: P) {
    let p = p.as_ref();
    match std::fs::remove_file(p) {
        Ok(_) => {}
        Err(e) if e.kind() == ErrorKind::NotFound => {}
        Err(e) if e.kind() == ErrorKind::IsADirectory => {
            if let Err(e) = std::fs::remove_dir_all(p) {
                if e.kind() != ErrorKind::NotFound {
                    panic!("cleanup: {e}");
                }
            }
        }
        Err(e) => panic!("cleanup: {e}"),
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("🚀 Demonstrating all critical mmap-io features...\n");

    // 1. **Real Huge Page Retention with Multi-Tier Fallback**
    println!("1. Creating mapping with huge pages (multi-tier fallback)...");
    #[cfg(feature = "hugepages")]
    let mmap = MemoryMappedFile::builder("demo_huge.bin")
        .mode(MmapMode::ReadWrite)
        .size(4 * 1024 * 1024) // 4MB for huge page optimization
        .huge_pages(true) // Tries: optimized mapping → THP → regular pages
        .create()?;

    #[cfg(not(feature = "hugepages"))]
    let mmap = MemoryMappedFile::builder("demo_huge.bin")
        .mode(MmapMode::ReadWrite)
        .size(4 * 1024 * 1024)
        .create()?;

    println!("✓ Huge pages mapping created (with automatic fallback)");

    // 2. **TouchHint::Eager for Benchmarking**
    println!("\n2. Creating mapping with eager page touching...");
    let benchmark_mmap = MemoryMappedFile::builder("benchmark.bin")
        .mode(MmapMode::ReadWrite)
        .size(2 * 1024 * 1024) // 2MB
        .touch_hint(TouchHint::Eager) // Pre-touch all pages during creation
        .create()?;

    println!("✓ All pages pre-touched for consistent benchmark timing");

    // 3. **Time-Based Flushing**
    println!("\n3. Creating mapping with automatic time-based flushing...");
    let auto_flush_mmap = MemoryMappedFile::builder("auto_flush.bin")
        .mode(MmapMode::ReadWrite)
        .size(1024 * 1024) // 1MB
        .flush_policy(FlushPolicy::EveryMillis(500)) // Auto-flush every 500ms
        .create()?;

    println!("✓ Time-based flushing enabled (500ms intervals)");

    // 4. **Combination: Huge Pages + Eager Touch + Automatic Flushing**
    println!("\n4. Creating ultimate performance mapping with all features...");
    #[allow(unused_mut)]
    let mut ultimate_builder = MemoryMappedFile::builder("ultimate.bin")
        .mode(MmapMode::ReadWrite)
        .size(8 * 1024 * 1024) // 8MB
        .touch_hint(TouchHint::Eager) // Pre-touch for benchmarks
        .flush_policy(FlushPolicy::EveryBytes(1024 * 1024)); // Flush every 1MB

    #[cfg(feature = "hugepages")]
    {
        ultimate_builder = ultimate_builder.huge_pages(true);
    }

    let ultimate_mmap = ultimate_builder.create()?;
    println!("✓ Ultimate performance mapping created with all optimizations");

    // 5. **Demonstrate Touch Pages Functionality**
    println!("\n5. Demonstrating explicit page touching...");

    // Touch all pages explicitly
    ultimate_mmap.touch_pages()?;
    println!("✓ All pages touched explicitly");

    // Touch specific range
    ultimate_mmap.touch_pages_range(0, 1024 * 1024)?; // Touch first 1MB
    println!("✓ First 1MB pages touched");

    // 6. **Demonstrate Microflush Optimization**
    println!("\n6. Testing microflush optimization...");

    // Write small data (triggers microflush optimization)
    let small_data = vec![0x42; 512]; // 512 bytes (sub-page)
    ultimate_mmap.update_region(0, &small_data)?;

    // Flush small range (automatically page-aligned)
    ultimate_mmap.flush_range(0, 512)?;
    println!("✓ Microflush completed with page alignment optimization");

    // 7. **Performance Comparison: Cold vs Warm Access**
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

    println!("📊 Performance Results:");
    println!("   Cold access (with page faults): {cold_time:?}");
    println!("   Warm access (pre-touched): {warm_time:?}");
    let speedup = cold_time.as_nanos() as f64 / warm_time.as_nanos() as f64;
    println!("   Speedup: {speedup:.2}x faster with page prewarming");

    // 8. **Demonstrate Fallback Behavior Documentation**
    println!("\n8. Huge pages fallback behavior:");
    println!("   ✓ Tier 1: Optimized mapping with MADV_HUGEPAGE + populate");
    println!("   ✓ Tier 2: Standard mapping with MADV_HUGEPAGE (THP)");
    println!("   ✓ Tier 3: Silent fallback to regular pages");
    println!("   ⚠️  Note: .huge_pages(true) does NOT guarantee huge pages");

    // 9. **Clean Up**
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

    println!("✓ Cleanup completed");

    println!("\n🎉 All critical features demonstrated successfully!");
    println!("\n📚 Key Benefits:");
    println!("   • Real huge page retention with automatic fallback");
    println!("   • Eager page touching eliminates benchmark timing variance");
    println!("   • Automatic time-based flushing reduces manual overhead");
    println!("   • Microflush optimization improves small write performance");
    println!("   • Comprehensive fallback ensures reliability across systems");

    Ok(())
}

#[cfg_attr(not(target_os = "linux"), ignore)]
#[test]
fn test_hugepages_fallback_behavior() { /* ... */
}
