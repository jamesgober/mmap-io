//! Fuzz target: `update_region` bounds and behaviour.
//!
//! Same shape as `read_into` but exercises the write path.

#![no_main]

use libfuzzer_sys::fuzz_target;
use mmap_io::MemoryMappedFile;

#[derive(arbitrary::Arbitrary, Debug)]
struct Input {
    file_size: u32,
    offset: u64,
    data: Vec<u8>,
}

fuzz_target!(|input: Input| {
    let size = (input.file_size as u64) % (64 * 1024) + 1;

    let path = std::env::temp_dir().join(format!(
        "mmap_io_fuzz_update_region_{}",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);

    let Ok(mmap) = MemoryMappedFile::create_rw(&path, size) else {
        let _ = std::fs::remove_file(&path);
        return;
    };

    // Bound payload size so the fuzzer doesn't allocate gigabytes.
    let payload = if input.data.len() > 8 * 1024 {
        &input.data[..8 * 1024]
    } else {
        &input.data[..]
    };

    let _ = mmap.update_region(input.offset, payload);
    // Also exercise the flush_range path with the same (possibly
    // OOB) coordinates to hit C1 accounting and the microflush
    // page-alignment optimisation.
    let _ = mmap.flush_range(input.offset, payload.len() as u64);

    drop(mmap);
    let _ = std::fs::remove_file(&path);
});
