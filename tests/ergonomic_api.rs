//! Integration tests for the 0.9.8 ergonomic API surface:
//! `open_or_create`, builder `open_or_create`, `from_file`, `unmap`,
//! `flush_policy`, `pending_bytes`, `as_ptr`, `as_mut_ptr`, and
//! `prefetch_range`.

use mmap_io::flush::FlushPolicy;
use mmap_io::{MemoryMappedFile, MmapIoError, MmapMode};
use std::fs::{self, OpenOptions};
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;

fn tmp_path(name: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!("mmap_io_ergonomic_{}_{}", name, std::process::id()));
    p
}

#[test]
fn open_or_create_creates_when_missing() {
    let path = tmp_path("ooc_create");
    let _ = fs::remove_file(&path);

    assert!(!path.exists(), "precondition: file must not exist");

    let mmap = MemoryMappedFile::open_or_create(&path, 4096).expect("create path");
    assert_eq!(mmap.len(), 4096);
    assert_eq!(mmap.mode(), MmapMode::ReadWrite);

    drop(mmap);
    let _ = fs::remove_file(&path);
}

#[test]
fn open_or_create_opens_when_present() {
    let path = tmp_path("ooc_open");
    let _ = fs::remove_file(&path);

    // Pre-create at a specific size.
    {
        let mmap = MemoryMappedFile::create_rw(&path, 8192).expect("seed");
        mmap.update_region(0, b"existing-data").expect("write");
        mmap.flush().expect("flush");
    }

    // open_or_create must open the existing file (NOT truncate to the
    // requested size) and keep the existing 8192-byte length.
    let mmap = MemoryMappedFile::open_or_create(&path, 4096).expect("open path");
    assert_eq!(
        mmap.len(),
        8192,
        "open_or_create on existing file must preserve length"
    );

    let mut buf = [0u8; 13];
    mmap.read_into(0, &mut buf).expect("read_into");
    assert_eq!(&buf, b"existing-data");

    drop(mmap);
    let _ = fs::remove_file(&path);
}

#[test]
fn builder_open_or_create_create_path() {
    let path = tmp_path("builder_ooc_create");
    let _ = fs::remove_file(&path);

    let mmap = MemoryMappedFile::builder(&path)
        .mode(MmapMode::ReadWrite)
        .size(2048)
        .flush_policy(FlushPolicy::EveryBytes(1024))
        .open_or_create()
        .expect("builder open_or_create create");
    assert_eq!(mmap.len(), 2048);
    assert_eq!(mmap.flush_policy(), FlushPolicy::EveryBytes(1024));

    drop(mmap);
    let _ = fs::remove_file(&path);
}

#[test]
fn builder_open_or_create_open_path() {
    let path = tmp_path("builder_ooc_open");
    let _ = fs::remove_file(&path);

    {
        let _seed = MemoryMappedFile::create_rw(&path, 1024).expect("seed");
    }

    let mmap = MemoryMappedFile::builder(&path)
        .mode(MmapMode::ReadWrite)
        .size(99999) // ignored on the open path
        .open_or_create()
        .expect("builder open_or_create open");
    assert_eq!(mmap.len(), 1024);

    drop(mmap);
    let _ = fs::remove_file(&path);
}

#[test]
fn from_file_read_write() {
    let path = tmp_path("from_file_rw");
    let _ = fs::remove_file(&path);

    // Seed the file via raw OpenOptions to prove from_file does not
    // create the file itself.
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(true)
        .open(&path)
        .expect("open file");
    file.set_len(1024).expect("set_len");

    let mmap = MemoryMappedFile::from_file(file, MmapMode::ReadWrite, &path).expect("from_file RW");
    assert_eq!(mmap.mode(), MmapMode::ReadWrite);
    assert_eq!(mmap.len(), 1024);
    mmap.update_region(0, b"from_file works")
        .expect("update_region");

    drop(mmap);
    let _ = fs::remove_file(&path);
}

#[test]
fn from_file_read_only() {
    let path = tmp_path("from_file_ro");
    let _ = fs::remove_file(&path);

    {
        let mmap = MemoryMappedFile::create_rw(&path, 64).expect("seed");
        mmap.update_region(0, b"ro-seed").expect("write");
        mmap.flush().expect("flush");
    }

    let file = OpenOptions::new().read(true).open(&path).expect("open RO");
    let mmap = MemoryMappedFile::from_file(file, MmapMode::ReadOnly, &path).expect("from_file RO");
    assert_eq!(mmap.mode(), MmapMode::ReadOnly);
    let s = mmap.as_slice(0, 7).expect("as_slice");
    assert_eq!(&*s, b"ro-seed");

    drop(s);
    drop(mmap);
    let _ = fs::remove_file(&path);
}

#[test]
fn from_file_rw_zero_length_errors() {
    let path = tmp_path("from_file_zero");
    let _ = fs::remove_file(&path);
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(true)
        .open(&path)
        .expect("open");
    // file.set_len(0) by default after truncate(true).
    let err = MemoryMappedFile::from_file(file, MmapMode::ReadWrite, &path)
        .expect_err("zero-length RW must error");
    assert!(matches!(err, MmapIoError::ResizeFailed(_)));

    let _ = fs::remove_file(&path);
}

#[test]
fn unmap_returns_file_when_unique() {
    let path = tmp_path("unmap_unique");
    let _ = fs::remove_file(&path);

    let mmap = MemoryMappedFile::create_rw(&path, 32).expect("create");
    mmap.update_region(0, b"hello-world-1234").expect("write");
    mmap.flush().expect("flush");

    let mut file = mmap.unmap().expect("unmap must succeed when unique");
    file.seek(SeekFrom::Start(0)).expect("seek");
    let mut buf = [0u8; 16];
    file.read_exact(&mut buf).expect("read after unmap");
    assert_eq!(&buf, b"hello-world-1234");

    drop(file);
    let _ = fs::remove_file(&path);
}

#[test]
fn unmap_returns_self_when_shared() {
    let path = tmp_path("unmap_shared");
    let _ = fs::remove_file(&path);

    let mmap = MemoryMappedFile::create_rw(&path, 16).expect("create");
    let clone = mmap.clone();
    // While `clone` is alive, unmap must give us `Err(self)`.
    let result = mmap.unmap();
    assert!(result.is_err(), "unmap must fail when clones exist");
    drop(result); // drop the returned mmap clone
    drop(clone);

    let _ = fs::remove_file(&path);
}

#[test]
fn flush_policy_returns_configured_policy() {
    let path = tmp_path("flush_policy_accessor");
    let _ = fs::remove_file(&path);

    let default = MemoryMappedFile::create_rw(&path, 32).expect("create");
    assert_eq!(default.flush_policy(), FlushPolicy::default());
    drop(default);
    let _ = fs::remove_file(&path);

    let path2 = tmp_path("flush_policy_explicit");
    let _ = fs::remove_file(&path2);
    let mmap = MemoryMappedFile::builder(&path2)
        .mode(MmapMode::ReadWrite)
        .size(32)
        .flush_policy(FlushPolicy::EveryBytes(4096))
        .create()
        .expect("builder");
    assert_eq!(mmap.flush_policy(), FlushPolicy::EveryBytes(4096));

    drop(mmap);
    let _ = fs::remove_file(&path2);
}

#[test]
fn pending_bytes_tracks_accumulator() {
    let path = tmp_path("pending_bytes");
    let _ = fs::remove_file(&path);

    let mmap = MemoryMappedFile::builder(&path)
        .mode(MmapMode::ReadWrite)
        .size(16 * 1024)
        .flush_policy(FlushPolicy::EveryBytes(8 * 1024))
        .create()
        .expect("create");
    assert_eq!(mmap.pending_bytes(), 0);

    mmap.update_region(0, &vec![0u8; 1024]).expect("write");
    assert_eq!(
        mmap.pending_bytes(),
        1024,
        "1 KiB written, threshold 8 KiB: accumulator should reflect"
    );

    mmap.update_region(1024, &vec![0u8; 1024]).expect("write");
    assert_eq!(mmap.pending_bytes(), 2048);

    // Pushing past threshold triggers a flush which clears the
    // accumulator (or near-clear; the C1 path debits by the flushed
    // length).
    mmap.update_region(2048, &vec![0u8; 8 * 1024])
        .expect("write");
    assert!(
        mmap.pending_bytes() < 4 * 1024,
        "auto-flush should have largely cleared the accumulator, got {}",
        mmap.pending_bytes()
    );

    drop(mmap);
    let _ = fs::remove_file(&path);
}

#[test]
fn as_ptr_roundtrip_via_raw_read() {
    let path = tmp_path("as_ptr");
    let _ = fs::remove_file(&path);

    let mmap = MemoryMappedFile::create_rw(&path, 64).expect("create");
    mmap.update_region(0, b"PTR-CHECK-12345").expect("write");
    mmap.flush().expect("flush");

    // SAFETY: as_ptr returns a pointer valid for self.len() bytes
    // while &self is held and no resize() runs. We do not call
    // resize() in this test, and we drop the pointer before the
    // mapping.
    unsafe {
        let p = mmap.as_ptr();
        let s = std::slice::from_raw_parts(p, 15);
        assert_eq!(s, b"PTR-CHECK-12345");
    }

    drop(mmap);
    let _ = fs::remove_file(&path);
}

#[test]
fn as_mut_ptr_writes_visible_via_safe_path() {
    let path = tmp_path("as_mut_ptr");
    let _ = fs::remove_file(&path);

    let mmap = MemoryMappedFile::create_rw(&path, 32).expect("create");

    // SAFETY: as_mut_ptr on a RW mapping gives an exclusive pointer
    // valid for self.len() bytes. We do not form any Rust reference
    // to the same region while the raw pointer is in use, and we
    // release it before the next safe access.
    unsafe {
        let p = mmap.as_mut_ptr().expect("as_mut_ptr RW");
        let buf = std::slice::from_raw_parts_mut(p, 8);
        buf.copy_from_slice(b"raw-mut!");
    }

    let mut readback = [0u8; 8];
    mmap.read_into(0, &mut readback).expect("read_into");
    assert_eq!(&readback, b"raw-mut!");

    drop(mmap);
    let _ = fs::remove_file(&path);
}

#[test]
fn as_mut_ptr_errors_on_ro() {
    let path = tmp_path("as_mut_ptr_ro");
    let _ = fs::remove_file(&path);
    {
        let _seed = MemoryMappedFile::create_rw(&path, 16).expect("seed");
    }
    let ro = MemoryMappedFile::open_ro(&path).expect("open_ro");
    // SAFETY: we are deliberately calling the unsafe entry to verify
    // it returns InvalidMode without dereferencing the pointer.
    let result = unsafe { ro.as_mut_ptr() };
    assert!(matches!(result, Err(MmapIoError::InvalidMode(_))));

    drop(ro);
    let _ = fs::remove_file(&path);
}

#[test]
fn prefetch_range_in_bounds_succeeds() {
    let path = tmp_path("prefetch_ok");
    let _ = fs::remove_file(&path);

    let mmap = MemoryMappedFile::create_rw(&path, 64 * 1024).expect("create");
    mmap.update_region(0, &vec![0xABu8; 64 * 1024])
        .expect("write");
    mmap.flush().expect("flush");

    // On Linux this issues posix_fadvise; on other platforms it is
    // documented as a no-op. Either way it must succeed in-bounds.
    mmap.prefetch_range(0, 64 * 1024).expect("prefetch full");
    mmap.prefetch_range(4096, 8192).expect("prefetch sub-range");

    drop(mmap);
    let _ = fs::remove_file(&path);
}

#[test]
fn prefetch_range_out_of_bounds_errors() {
    let path = tmp_path("prefetch_oob");
    let _ = fs::remove_file(&path);

    let mmap = MemoryMappedFile::create_rw(&path, 4096).expect("create");
    let err = mmap
        .prefetch_range(0, 8192)
        .expect_err("OOB prefetch must error");
    assert!(matches!(err, MmapIoError::OutOfBounds { .. }));

    drop(mmap);
    let _ = fs::remove_file(&path);
}

#[test]
fn prefetch_range_zero_len_is_noop() {
    let path = tmp_path("prefetch_zero");
    let _ = fs::remove_file(&path);

    let mmap = MemoryMappedFile::create_rw(&path, 64).expect("create");
    mmap.prefetch_range(0, 0).expect("zero-len OK");
    mmap.prefetch_range(999_999, 0)
        .expect("zero-len past end OK"); // still OK
    drop(mmap);
    let _ = fs::remove_file(&path);
}
