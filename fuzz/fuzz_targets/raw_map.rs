//! Fuzz target: `mmap_io::raw` mapping windows and operation
//! sequences.
//!
//! Each input picks a file length, a window `(offset, len)` and a
//! mapping kind, then replays a random sequence of reads, writes and
//! flushes. The contract:
//!   - Constructing a window is `Ok` exactly when it lies inside the
//!     file, and the bytes then match the file; otherwise it returns
//!     `InvalidInput`. No input may panic.
//!   - `flush_range` / `flush_async_range` are `Ok` exactly when the
//!     range lies inside the window.
//!   - Shared writable windows reach the file; copy-on-write windows
//!     never do.
//!
//! The temporary file is private to this process, which is the raw
//! constructors' `# Safety` contract.

#![no_main]

use std::fs::OpenOptions;
use std::io::{ErrorKind, Read, Seek, SeekFrom, Write};

use libfuzzer_sys::fuzz_target;
use mmap_io::raw::{RawMmapMut, RawMmapOptions};

#[derive(arbitrary::Arbitrary, Debug)]
enum Kind {
    ReadOnly,
    Shared,
    CopyOnWrite,
    Anonymous,
}

#[derive(arbitrary::Arbitrary, Debug)]
enum Op {
    Read { at: usize },
    Write { at: usize, byte: u8 },
    Flush,
    FlushAsync,
    FlushRange { offset: usize, len: usize },
    FlushAsyncRange { offset: usize, len: usize },
}

#[derive(arbitrary::Arbitrary, Debug)]
struct Input {
    /// File length, folded to at most 192 KiB + a few bytes so every
    /// alignment boundary (4 KiB pages, 64 KiB Windows granules) is
    /// reachable without slow inputs.
    file_len: u32,
    offset: u64,
    len: Option<usize>,
    /// When set, fold offset/len into the file so most windows are
    /// valid; otherwise use the raw (often hostile) values.
    fold: bool,
    kind: Kind,
    ops: Vec<Op>,
}

fn byte_at(i: usize) -> u8 {
    (i % 251) as u8
}

fuzz_target!(|input: Input| {
    let file_len = (input.file_len % (3 * 65_536 + 8)) as usize;
    let (offset, len) = if input.fold {
        let off = input.offset % (file_len as u64 + 2);
        (off, input.len.map(|l| l % (file_len + 2)))
    } else {
        (input.offset, input.len)
    };

    let path = std::env::temp_dir().join(format!("mmap_io_fuzz_raw_{}", std::process::id()));
    let mut file = match OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(&path)
    {
        Ok(f) => f,
        Err(_) => return,
    };
    let original: Vec<u8> = (0..file_len).map(byte_at).collect();
    if file.write_all(&original).is_err() {
        return;
    }

    let mut opts = RawMmapOptions::new();
    opts.offset(offset);
    if let Some(l) = len {
        opts.len(l);
    }
    let window = if offset > file_len as u64 {
        None
    } else {
        let start = offset as usize;
        let avail = file_len - start;
        match len {
            None => Some((start, avail)),
            Some(l) if l <= avail => Some((start, l)),
            Some(_) => None,
        }
    };

    // SAFETY: the temporary file is private to this process and is not
    // modified through any other path while the mapping is alive.
    let mapped = unsafe {
        match input.kind {
            Kind::ReadOnly => opts.map(&file).map(|m| (None, Some(m))),
            Kind::Shared => opts.map_mut(&file).map(|m| (Some(m), None)),
            Kind::CopyOnWrite => opts.map_copy(&file).map(|m| (Some(m), None)),
            Kind::Anonymous => {
                RawMmapMut::map_anon(len.unwrap_or(0) % (1 << 20)).map(|m| (Some(m), None))
            }
        }
    };

    let (mut rw, ro) = match (mapped, &input.kind) {
        (Ok(pair), Kind::Anonymous) => pair,
        (Ok(pair), _) => {
            let (start, wlen) = window.expect("Ok for an out-of-file window");
            let bytes: &[u8] = match (&pair.0, &pair.1) {
                (Some(m), _) => m,
                (_, Some(m)) => m,
                _ => unreachable!(),
            };
            assert_eq!(bytes.len(), wlen);
            assert!(bytes == &original[start..start + wlen]);
            pair
        }
        (Err(_), Kind::Anonymous) => {
            // Lengths are folded below 1 MiB, so only an OS refusal
            // (out of memory) lands here; nothing to check.
            let _ = std::fs::remove_file(&path);
            return;
        }
        (Err(e), _) => {
            assert!(window.is_none(), "valid window {window:?} failed: {e}");
            assert_eq!(e.kind(), ErrorKind::InvalidInput);
            let _ = std::fs::remove_file(&path);
            return;
        }
    };

    let mut expected = original.clone();
    let base = window.map_or(0, |(s, _)| s);
    for op in input.ops.iter().take(64) {
        let wlen = rw
            .as_ref()
            .map_or_else(|| ro.as_ref().map_or(0, |m| m.len()), |m| m.len());
        match *op {
            Op::Read { at } => {
                if wlen > 0 {
                    let i = at % wlen;
                    let b = rw
                        .as_ref()
                        .map_or_else(|| ro.as_ref().map_or(0, |m| m[i]), |m| m[i]);
                    if !matches!(input.kind, Kind::Anonymous | Kind::CopyOnWrite) {
                        assert_eq!(b, expected[base + i]);
                    }
                }
            }
            Op::Write { at, byte } => {
                if let (Some(m), true) = (rw.as_mut(), wlen > 0) {
                    let i = at % wlen;
                    m[i] = byte;
                    assert_eq!(m[i], byte);
                    if matches!(input.kind, Kind::Shared) {
                        expected[base + i] = byte;
                    }
                }
            }
            Op::Flush => {
                if let Some(m) = rw.as_ref() {
                    m.flush().expect("flush");
                }
            }
            Op::FlushAsync => {
                if let Some(m) = rw.as_ref() {
                    m.flush_async().expect("flush_async");
                }
            }
            Op::FlushRange { offset, len } | Op::FlushAsyncRange { offset, len } => {
                if let Some(m) = rw.as_ref() {
                    let ok = offset <= wlen && len <= wlen - offset;
                    let r = if matches!(op, Op::FlushRange { .. }) {
                        m.flush_range(offset, len)
                    } else {
                        m.flush_async_range(offset, len)
                    };
                    assert_eq!(r.is_ok(), ok, "flush range ({offset}, {len}) of {wlen}");
                }
            }
        }
    }

    if let Some(m) = rw.as_ref() {
        m.flush().expect("final flush");
    }
    drop(rw);
    drop(ro);

    let mut on_disk = Vec::new();
    if file.seek(SeekFrom::Start(0)).is_ok() && file.read_to_end(&mut on_disk).is_ok() {
        // Shared writes landed; COW / anonymous / read-only left the
        // file untouched.
        assert!(on_disk == expected);
    }
    drop(file);
    let _ = std::fs::remove_file(&path);
});
