//! Regression tests for the 2026-11 review of 1.1.1. Each test fails on
//! 1.1.1; the SIGBUS and ENOSPC ones crash it on Linux.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::io::ErrorKind;
use std::time::Duration;

use mmap_io::flush::FlushPolicy;
use mmap_io::{create_mmap, write_mmap, MemoryMappedFile, MmapIoError, MmapMode};

mod common;
use common::tmp_path;

fn is_invalid_mode<T>(r: &Result<T, MmapIoError>) -> bool {
    matches!(r, Err(MmapIoError::InvalidMode(_)))
}

/// F-1: `write_mmap` changed the bytes behind a live `&[u8]` of another
/// mapping of the same file.
#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
#[test]
fn write_mmap_cannot_change_a_live_shared_slice() {
    let path = tmp_path("a.bin");
    fs::write(&path, b"AAAAAAAA").unwrap();
    let ro = MemoryMappedFile::open_ro(&path).unwrap();
    let bytes: &[u8] = ro.as_slice_bytes(0, 8).unwrap();
    let r = write_mmap(&path, 0, b"BBBBBBBB");
    assert!(is_invalid_mode(&r), "{r:?}");
    assert_eq!(bytes, b"AAAAAAAA");
    drop(ro);
    write_mmap(&path, 0, b"BBBBBBBB").unwrap();
    assert_eq!(fs::read(&path).unwrap(), b"BBBBBBBB");
}

/// F-1: a second read-write handle changed the bytes behind a live
/// `MappedSlice` of the first.
#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
#[test]
fn a_second_read_write_handle_is_refused() {
    let path = tmp_path("b.bin");
    let a = MemoryMappedFile::create_rw(&path, 4096).unwrap();
    let view = a.as_slice(0, 4).unwrap();
    assert!(is_invalid_mode(&MemoryMappedFile::open_rw(&path)));
    assert!(is_invalid_mode(&MemoryMappedFile::open_ro(&path)));
    assert!(is_invalid_mode(
        &MemoryMappedFile::builder(&path)
            .mode(MmapMode::ReadWrite)
            .open()
    ));
    assert!(is_invalid_mode(&MemoryMappedFile::open_or_create(
        &path, 4096
    )));
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
        .unwrap();
    assert!(is_invalid_mode(&MemoryMappedFile::from_file(
        file,
        MmapMode::ReadWrite,
        &*path
    )));
    #[cfg(feature = "cow")]
    {
        assert!(is_invalid_mode(&MemoryMappedFile::open_cow(&path)));
        assert!(is_invalid_mode(&MemoryMappedFile::open_cow_writable(&path)));
    }
    assert_eq!(&*view, &[0; 4]);
    drop(view);
    // Clones share one lock and are always fine.
    let b = a.clone();
    b.update_region(0, b"ZZZZ").unwrap();
    assert_eq!(&*a.as_slice(0, 4).unwrap(), b"ZZZZ");
    drop((a, b));
    // Once every handle is gone the file can be mapped again.
    let again = MemoryMappedFile::open_rw(&path).unwrap();
    assert_eq!(&*again.as_slice(0, 4).unwrap(), b"ZZZZ");
}

/// F-1: the registry keys on the file, not on the path string.
#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
#[test]
fn another_path_to_the_same_file_is_refused() {
    let path = tmp_path("c.bin");
    let _a = MemoryMappedFile::create_rw(&path, 4096).unwrap();
    let dotted = path.dir().join(".").join("c.bin");
    assert!(is_invalid_mode(&MemoryMappedFile::open_ro(&dotted)));
    let link = path.sibling("link.bin");
    if fs::hard_link(&path, &link).is_ok() {
        assert!(is_invalid_mode(&MemoryMappedFile::open_ro(&link)));
        assert!(is_invalid_mode(&MemoryMappedFile::create_rw(&link, 64)));
    }
    // A different file is not affected.
    let other = path.sibling("other.bin");
    let _b = MemoryMappedFile::create_rw(&other, 4096).unwrap();
}

/// F-1: readers share, a writer excludes.
#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
#[test]
fn read_only_mappings_share_a_file() {
    let path = tmp_path("d.bin");
    fs::write(&path, b"shared").unwrap();
    let r1 = MemoryMappedFile::open_ro(&path).unwrap();
    let r2 = MemoryMappedFile::open_ro(&path).unwrap();
    #[cfg(feature = "cow")]
    let c = MemoryMappedFile::open_cow_writable(&path).unwrap();
    assert_eq!(&*r1.as_slice(0, 6).unwrap(), &*r2.as_slice(0, 6).unwrap());
    assert!(is_invalid_mode(&MemoryMappedFile::open_rw(&path)));
    assert!(is_invalid_mode(&create_mmap(&path, 64)));
    assert_eq!(fs::read(&path).unwrap(), b"shared", "not truncated");
    drop((r1, r2));
    #[cfg(feature = "cow")]
    {
        assert!(is_invalid_mode(&MemoryMappedFile::open_rw(&path)));
        drop(c);
    }
    MemoryMappedFile::open_rw(&path).unwrap();
}

/// F-1 (`SIGBUS`): a second handle shrank the file, or `create_mmap`
/// truncated it, under a live slice; reading the slice then killed the
/// process on Linux. Now both are refused before the file is touched.
#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
#[test]
fn truncation_under_a_live_slice_is_refused() {
    let path = tmp_path("shared.bin");
    let a = MemoryMappedFile::create_rw(&path, 64 * 1024).unwrap();
    a.update_region(60_000, b"tailtail").unwrap();
    let view = a.as_slice(60_000, 8).unwrap();

    let b = MemoryMappedFile::open_rw(&path);
    if let Ok(b) = &b {
        // 1.1.1: the shrink succeeds on Unix and the read below faults.
        let _ = b.resize(4096);
    }
    let created = create_mmap(&path, 4096);
    assert_eq!(view[0], b't', "the live view still reads");
    assert!(is_invalid_mode(&b), "second handle: {b:?}");
    assert!(is_invalid_mode(&created), "create_mmap: {created:?}");
    assert_eq!(fs::metadata(&path).unwrap().len(), 64 * 1024);
}

/// F-1: `allow_aliasing` is the explicit opt-in; a shrink is still
/// refused while another mapping exists, so its borrows cannot fault.
#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
#[test]
fn allow_aliasing_opts_in_and_shrink_stays_refused() {
    let path = tmp_path("e.bin");
    let w = MemoryMappedFile::create_rw(&path, 8192).unwrap();
    w.update_region(0, b"head").unwrap();
    // SAFETY: nothing writes to the file while a borrow of `r` is alive.
    let r = unsafe {
        MemoryMappedFile::builder(&path)
            .mode(MmapMode::ReadOnly)
            .allow_aliasing()
    }
    .open()
    .unwrap();
    assert_eq!(&*r.as_slice(0, 4).unwrap(), b"head");
    let shrink = w.resize(4096);
    assert!(
        matches!(shrink, Err(MmapIoError::ResizeFailed(_))),
        "{shrink:?}"
    );
    assert_eq!(w.len(), 8192);
    w.resize(16384).unwrap();
    drop(r);
    w.resize(4096).unwrap();
    assert_eq!(w.len(), 4096);
}

/// F-4: the `EveryMillis` worker holds a strong reference while it
/// flushes, so `unmap` failed as if another clone existed.
#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
#[test]
fn unmap_never_fails_spuriously_with_the_every_millis_flusher() {
    let path = tmp_path("f.bin");
    for i in 0..200u32 {
        let p = path.sibling(&format!("f{i}.bin"));
        let m = MemoryMappedFile::builder(&p)
            .size(1 << 16)
            .flush_policy(FlushPolicy::EveryMillis(1))
            .create()
            .unwrap();
        m.update_region(0, &[i as u8; 4096]).unwrap();
        std::thread::sleep(Duration::from_micros(900));
        assert!(m.unmap().is_ok(), "iteration {i}: spurious unmap failure");
    }
}

/// F-4 follow-up: dropping the last handle releases the file before
/// `drop` returns, even while the worker is flushing, so the file can
/// be mapped again at once.
#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
#[test]
fn reopen_right_after_drop_with_the_every_millis_flusher() {
    let path = tmp_path("g.bin");
    for i in 0..200u32 {
        let m = MemoryMappedFile::builder(&path)
            .mode(MmapMode::ReadWrite)
            .size(1 << 16)
            .flush_policy(FlushPolicy::EveryMillis(1))
            .open_or_create()
            .unwrap();
        m.update_region(u64::from(i % 64), &[1; 1024]).unwrap();
        std::thread::sleep(Duration::from_micros(900));
        drop(m);
    }
}

/// F-6: an empty file on a writable path and a missing builder size are
/// invalid input, not a failed resize.
#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
#[test]
fn empty_files_and_missing_sizes_are_invalid_input() {
    let path = tmp_path("empty.bin");
    fs::write(&path, b"").unwrap();
    let kind = |r: Result<MemoryMappedFile, MmapIoError>| match r {
        Err(MmapIoError::Io(e)) => e.kind(),
        other => panic!("expected Io, got {other:?}"),
    };
    assert_eq!(
        kind(MemoryMappedFile::open_rw(&path)),
        ErrorKind::InvalidInput
    );
    assert_eq!(
        kind(MemoryMappedFile::builder(path.sibling("new.bin")).create()),
        ErrorKind::InvalidInput
    );
}

/// F-2: by default the disk blocks of a new file are reserved; with
/// `sparse(true)` they are not.
#[cfg(target_os = "linux")]
#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
#[test]
fn new_files_are_preallocated_unless_sparse() {
    use std::os::unix::fs::MetadataExt;
    let size: u64 = 8 << 20;
    let path = tmp_path("h.bin");
    let dense = MemoryMappedFile::create_rw(&path, size).unwrap();
    let allocated = fs::metadata(&path).unwrap().blocks() * 512;
    let sparse_path = path.sibling("sparse.bin");
    let sparse = MemoryMappedFile::builder(&sparse_path)
        .size(size)
        .sparse(true)
        .create()
        .unwrap();
    let sparse_allocated = fs::metadata(&sparse_path).unwrap().blocks() * 512;
    assert!(
        sparse_allocated < size,
        "sparse file has {sparse_allocated} bytes allocated"
    );
    // A file system without fallocate leaves both sparse; the common
    // ones (ext4, xfs, btrfs, tmpfs) reserve the space.
    if allocated < size {
        eprintln!("file system did not reserve space ({allocated} of {size} bytes); skipped");
        return;
    }
    dense.resize(2 * size).unwrap();
    assert!(fs::metadata(&path).unwrap().blocks() * 512 >= 2 * size);
    sparse.resize(2 * size).unwrap();
    assert!(fs::metadata(&sparse_path).unwrap().blocks() * 512 < size);
}

/// F-2 (`ENOSPC`): writing into a sparse mapping on a full file system
/// killed the process with `SIGBUS`. Now the space is reserved up front
/// and a full file system fails `create_rw` / `resize` with an error.
///
/// Needs a small file system: set `MMAP_IO_SMALL_FS` to a directory on
/// one with at most a few MiB free, for example a 1 MiB tmpfs mounted
/// inside `unshare --user --map-root-user --mount` with
/// `mount -t tmpfs -o size=1m tmpfs DIR`. Skipped otherwise.
#[cfg(target_os = "linux")]
#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
#[test]
fn full_file_system_is_an_error_not_sigbus() {
    let Some(dir) = std::env::var_os("MMAP_IO_SMALL_FS") else {
        eprintln!("MMAP_IO_SMALL_FS not set; skipped");
        return;
    };
    let dir = std::path::PathBuf::from(dir);
    let is_enospc =
        |e: &MmapIoError| matches!(e, MmapIoError::Io(io) if io.raw_os_error() == Some(28));

    // 1.1.1: create_rw succeeds (sparse) and a write a few hundred pages
    // in raises SIGBUS.
    let path = dir.join("big.bin");
    match MemoryMappedFile::create_rw(&path, 64 << 20) {
        Err(e) => assert!(is_enospc(&e), "{e:?}"),
        Ok(m) => {
            let mut off = 0;
            while off < m.len() {
                m.update_region(off, b"x").unwrap();
                off += 4096;
            }
            panic!("64 MiB fit on the small file system; use a smaller one");
        }
    }
    assert!(!path.exists(), "the failed create left a file behind");

    // Growing a small mapping past the free space fails the resize and
    // leaves the mapping as it was.
    let small = dir.join("small.bin");
    let m = MemoryMappedFile::create_rw(&small, 4096).unwrap();
    m.update_region(0, b"keep").unwrap();
    let grown = m.resize(64 << 20);
    assert!(grown.as_ref().is_err_and(is_enospc), "{grown:?}");
    assert_eq!(m.len(), 4096);
    assert_eq!(fs::metadata(&small).unwrap().len(), 4096);
    assert_eq!(&*m.as_slice(0, 4).unwrap(), b"keep");
    drop(m);
    fs::remove_file(&small).unwrap();
}

/// F-8: on Linux `flush()` no longer holds the mapping lock while the
/// data is written back, so it completes while a writer holds the lock.
#[cfg(target_os = "linux")]
#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
#[test]
fn flush_does_not_wait_for_a_writer() {
    let path = tmp_path("i.bin");
    let m = MemoryMappedFile::create_rw(&path, 1 << 16).unwrap();
    m.update_region(0, b"data").unwrap();
    let mut guard = m.as_slice_mut(100, 4).unwrap();
    guard.as_mut().copy_from_slice(b"held");
    let (tx, rx) = std::sync::mpsc::channel();
    let other = m.clone();
    let flusher = std::thread::spawn(move || {
        let r = other.flush();
        let _ = tx.send(r.is_ok());
    });
    let done = rx.recv_timeout(Duration::from_secs(10));
    drop(guard);
    flusher.join().unwrap();
    assert_eq!(done, Ok(true), "flush waited for the write guard");
}

/// Found while landing the registry: a claim released after the file was
/// closed could outlive the inode, and a new file that got the same inode
/// number was refused. Two threads create, map, unlink and drop files as
/// fast as they can; no mapping of a fresh file may ever be refused.
#[cfg(unix)]
#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
#[test]
fn reused_inode_numbers_are_never_refused() {
    let dir = tempfile::tempdir().unwrap();
    let workers: Vec<_> = (0..4)
        .map(|t| {
            let dir = dir.path().to_path_buf();
            std::thread::spawn(move || {
                for i in 0..500 {
                    let path = dir.join(format!("w{t}-{i}.bin"));
                    let m = match MemoryMappedFile::create_rw(&path, 4096) {
                        Ok(m) => m,
                        Err(e) => panic!("thread {t} round {i}: {e}"),
                    };
                    m.update_region(0, b"x").unwrap();
                    fs::remove_file(&path).unwrap();
                    drop(m);
                }
            })
        })
        .collect();
    for w in workers {
        w.join().unwrap();
    }
}
