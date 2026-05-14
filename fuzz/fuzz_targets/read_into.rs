//! Fuzz target: `read_into` bounds and behaviour.
//!
//! Generates a fresh mapping of random size, picks a random
//! offset / dst-buffer-length, and exercises `read_into`. The
//! contract under test: `read_into` MUST either succeed or return
//! `MmapIoError::OutOfBounds`; it MUST NEVER panic or trigger UB.
//!
//! Run with (from the crate root):
//!   cd fuzz
//!   cargo +nightly fuzz run read_into -- -runs=1000000
//!
//! Requires nightly Rust for libFuzzer support, and a Linux or WSL
//! environment. The `libfuzzer-sys` crate cannot link the LLVM
//! libFuzzer runtime on stock Windows.

#![no_main]

use libfuzzer_sys::fuzz_target;
use mmap_io::MemoryMappedFile;

#[derive(arbitrary::Arbitrary, Debug)]
struct Input {
    file_size: u32,  // bounded below to keep the file small
    offset: u64,
    buf_len: u16,
}

fuzz_target!(|input: Input| {
    // Constrain the file size to something the fuzzer can iterate
    // through quickly. 64 KiB is large enough to span multiple pages
    // and tickle alignment-sensitive paths.
    let size = (input.file_size as u64) % (64 * 1024) + 1;

    let path = std::env::temp_dir().join(format!(
        "mmap_io_fuzz_read_into_{}",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);

    let Ok(mmap) = MemoryMappedFile::create_rw(&path, size) else {
        let _ = std::fs::remove_file(&path);
        return;
    };

    let mut buf = vec![0u8; input.buf_len as usize];
    // We do NOT care whether the call succeeds or fails. We care
    // that it does not panic / segfault / UB. libFuzzer treats any
    // unhandled panic or sanitizer trip as a crash.
    let _ = mmap.read_into(input.offset, &mut buf);

    drop(mmap);
    let _ = std::fs::remove_file(&path);
});
