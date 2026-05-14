//! Example 05: append-only log pattern with size-tracked growth.
//!
//! Demonstrates how to use `resize` to grow a mapping on demand
//! while keeping a write cursor. This is the skeleton you'd build a
//! write-ahead log (WAL) on top of: each "record" is a length-
//! prefixed byte string written at the current tail offset; the
//! mapping grows in 64 KiB chunks when the tail approaches the
//! current end.
//!
//! Run with:
//!   cargo run --example 05_log_appender

use mmap_io::flush::FlushPolicy;
use mmap_io::{MemoryMappedFile, MmapMode};
use std::path::PathBuf;

fn main() -> Result<(), mmap_io::MmapIoError> {
    let path = PathBuf::from("example_05_log.bin");
    let _ = std::fs::remove_file(&path);

    // Start small; grow as needed.
    let initial_size: u64 = 4 * 1024;
    let grow_step: u64 = 64 * 1024;

    let mmap = MemoryMappedFile::builder(&path)
        .mode(MmapMode::ReadWrite)
        .size(initial_size)
        .flush_policy(FlushPolicy::EveryBytes(8 * 1024))
        .create()?;

    let messages = [
        b"first log entry".as_slice(),
        b"second entry with more bytes".as_slice(),
        b"third entry that is significantly longer to exercise growth across a chunk boundary"
            .as_slice(),
        b"fourth and final".as_slice(),
    ];

    let mut tail: u64 = 0;
    for (i, msg) in messages.iter().enumerate() {
        // Length-prefix: 4-byte little-endian u32.
        let len = msg.len() as u32;
        let len_bytes = len.to_le_bytes();
        let needed = (len_bytes.len() + msg.len()) as u64;

        // Grow if the next write would exceed the mapping.
        if tail + needed > mmap.len() {
            let new_size = mmap.len() + grow_step;
            println!(
                "  grow: {} -> {} to fit record #{i} ({} bytes)",
                mmap.len(),
                new_size,
                needed
            );
            mmap.resize(new_size)?;
        }

        mmap.update_region(tail, &len_bytes)?;
        mmap.update_region(tail + 4, msg)?;
        tail += needed;
        println!("  wrote #{i:2}: {:?}", String::from_utf8_lossy(msg));
    }

    mmap.flush()?;
    println!("Log tail: {tail} bytes; mapping size: {}", mmap.len());

    // Read back: open RO and walk the records.
    drop(mmap);
    let ro = MemoryMappedFile::open_ro(&path)?;
    {
        let mut cursor: u64 = 0;
        let mut idx = 0;
        while cursor < tail {
            let header = ro.as_slice(cursor, 4)?;
            let len = u32::from_le_bytes([header[0], header[1], header[2], header[3]]);
            drop(header);
            cursor += 4;
            let body = ro.as_slice(cursor, len as u64)?;
            println!("  read  #{idx:2}: {:?}", String::from_utf8_lossy(&body));
            drop(body);
            cursor += len as u64;
            idx += 1;
        }
    }

    drop(ro);
    let _ = std::fs::remove_file(&path);
    Ok(())
}
