//! Example 03: stable bookmarks into a mapped file via `Segment`.
//!
//! A `Segment` records `(offset, len)` against the parent mapping
//! and re-validates bounds on every access. The parent can be resized
//! between segment construction and use; if the segment range no
//! longer fits, the next access returns `OutOfBounds` rather than
//! reading stale or out-of-range bytes.
//!
//! Run with:
//!   cargo run --example 03_segment_views

use mmap_io::segment::Segment;
use mmap_io::MemoryMappedFile;
use std::path::PathBuf;
use std::sync::Arc;

fn main() -> Result<(), mmap_io::MmapIoError> {
    let path = PathBuf::from("example_03_segments.bin");
    let _ = std::fs::remove_file(&path);

    let mmap = Arc::new(MemoryMappedFile::create_rw(&path, 4096)?);

    // Seed three named regions.
    mmap.update_region(0, b"HEADER..........")?;
    mmap.update_region(64, b"BODY-A-CONTENT-1")?;
    mmap.update_region(128, b"BODY-B-CONTENT-2")?;
    mmap.flush()?;

    // Build segments that bookmark each region.
    let header = Segment::new(Arc::clone(&mmap), 0, 16)?;
    let body_a = Segment::new(Arc::clone(&mmap), 64, 16)?;
    let body_b = Segment::new(Arc::clone(&mmap), 128, 16)?;

    for (name, seg) in [
        ("header", &header),
        ("body_a", &body_a),
        ("body_b", &body_b),
    ] {
        let bytes = seg.as_slice()?;
        println!(
            "{name:6} @ offset {:>4}: {:?}",
            seg.offset(),
            String::from_utf8_lossy(&bytes)
        );
    }

    // Shrink the file. Segments whose range no longer fits are
    // observable via `is_valid()` and surface `OutOfBounds` on
    // access; segments still inside bounds keep working.
    mmap.resize(80)?;
    println!("\nAfter resize to 80 bytes:");
    println!("  header valid? {}", header.is_valid());
    println!("  body_a valid? {}", body_a.is_valid());
    println!("  body_b valid? {}", body_b.is_valid());

    match body_b.as_slice() {
        Ok(_) => unreachable!("body_b should be out of bounds now"),
        Err(e) => println!("  body_b read: {} (as expected)", e),
    }

    drop(header);
    drop(body_a);
    drop(body_b);
    drop(mmap);
    let _ = std::fs::remove_file(&path);
    Ok(())
}
