//! `MmapReader` implements `BufRead` (1.1.0): zero-copy on read-only
//! mappings, a lock-free 4 KiB inline copy on writable ones.

// These tests map real files or anonymous memory; Miri cannot run
// the mmap family of syscalls.
#![cfg(not(miri))]

use std::io::{BufRead, Read, Seek, SeekFrom};

use mmap_io::MemoryMappedFile;

fn text(lines: usize) -> Vec<u8> {
    let mut out = Vec::new();
    for i in 0..lines {
        out.extend_from_slice(format!("line {i:05} {}\n", "x".repeat(i % 97)).as_bytes());
    }
    out
}

fn check_lines(mmap: &MemoryMappedFile, expected: &[u8]) {
    let got: Vec<String> = mmap
        .reader()
        .lines()
        .collect::<Result<_, _>>()
        .expect("lines");
    let want: Vec<String> = String::from_utf8(expected.to_vec())
        .expect("utf8")
        .lines()
        .map(str::to_owned)
        .collect();
    assert_eq!(got, want);
}

#[test]
fn read_only_lines_are_zero_copy() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("ro.txt");
    let data = text(2000);
    std::fs::write(&path, &data).expect("write");
    let ro = MemoryMappedFile::open_ro(&path).expect("open_ro");
    check_lines(&ro, &data);

    let mut r = ro.reader();
    r.seek(SeekFrom::Start(100)).expect("seek");
    let buf = r.fill_buf().expect("fill_buf");
    assert_eq!(buf.len(), data.len() - 100, "the whole rest of the mapping");
    // SAFETY: only the address is compared.
    let base = unsafe { ro.as_ptr() } as usize;
    assert_eq!(buf.as_ptr() as usize, base + 100, "borrowed, not copied");
    let n = buf.len();
    r.consume(n);
    assert!(r.fill_buf().expect("eof").is_empty());
}

#[test]
fn read_write_lines_across_buffer_boundaries() {
    let dir = tempfile::tempdir().expect("tempdir");
    let data = text(3000); // far more than one 4 KiB buffer
    let m = MemoryMappedFile::create_rw(dir.path().join("rw.txt"), data.len() as u64)
        .expect("create_rw");
    m.update_region(0, &data).expect("fill");
    check_lines(&m, &data);

    let mut r = m.reader();
    let first = r.fill_buf().expect("fill_buf").len();
    assert_eq!(first, 4096, "writable mappings buffer at most 4 KiB");
    let mut collected = Vec::new();
    loop {
        let n = r.read_until(b'\n', &mut collected).expect("read_until");
        if n == 0 {
            break;
        }
    }
    assert_eq!(collected, data);
}

#[test]
fn reader_holds_no_lock_on_writable_mappings() {
    let dir = tempfile::tempdir().expect("tempdir");
    let m = MemoryMappedFile::create_rw(dir.path().join("w.bin"), 8192).expect("create_rw");
    m.update_region(0, b"before\n").expect("write");
    let mut r = m.reader();
    assert_eq!(&r.fill_buf().expect("fill_buf")[..7], b"before\n");
    // A write on the same thread does not deadlock: nothing is held.
    m.update_region(0, b"AFTER!\n")
        .expect("write while reader is alive");
    // The buffer is a snapshot until consumed or discarded.
    assert_eq!(&r.fill_buf().expect("fill_buf")[..7], b"before\n");
    r.seek(SeekFrom::Start(0))
        .expect("seek discards the buffer");
    assert_eq!(&r.fill_buf().expect("fill_buf")[..7], b"AFTER!\n");
    let mut line = String::new();
    r.read_line(&mut line).expect("read_line");
    assert_eq!(line, "AFTER!\n");
    assert_eq!(r.position(), 7);
    // Drop the mapping right after the reader's last use (no drop glue).
    drop(m);
}

#[test]
fn mixing_read_seek_and_fill_buf() {
    let dir = tempfile::tempdir().expect("tempdir");
    let m = MemoryMappedFile::create_rw(dir.path().join("m.bin"), 10_000).expect("create_rw");
    let data: Vec<u8> = (0..10_000u32).map(|i| (i % 251) as u8).collect();
    m.update_region(0, &data).expect("fill");
    let mut r = m.reader();
    assert_eq!(r.fill_buf().expect("fill")[0], data[0]);
    r.consume(10);
    let mut two = [0u8; 2];
    r.read_exact(&mut two).expect("read");
    assert_eq!(two, [data[10], data[11]]);
    assert_eq!(r.fill_buf().expect("fill")[0], data[12]);
    r.consume(4084); // to the end of a 4 KiB buffer that starts at 12
    assert_eq!(r.position(), 4096);
    assert_eq!(r.fill_buf().expect("refill")[0], data[4096]);
    r.set_position(9_999);
    assert_eq!(r.fill_buf().expect("last byte"), &data[9_999..]);
    r.consume(1);
    assert!(r.fill_buf().expect("eof").is_empty());
    r.set_position(u64::MAX);
    assert!(r.fill_buf().expect("past eof").is_empty());
    r.consume(usize::MAX); // saturates, no panic
    assert_eq!(r.position(), u64::MAX);
}

#[cfg(feature = "atomic")]
#[test]
fn buffered_reads_see_atomic_values() {
    use std::sync::atomic::Ordering;
    let dir = tempfile::tempdir().expect("tempdir");
    let m = MemoryMappedFile::create_rw(dir.path().join("a.bin"), 64).expect("create_rw");
    let v = m.atomic_u64(8).expect("atomic");
    v.store(u64::from_ne_bytes(*b"ATOMIC!\n"), Ordering::SeqCst);
    let mut r = m.reader();
    let buf = r.fill_buf().expect("fill_buf over a live atomic view");
    assert_eq!(&buf[8..16], b"ATOMIC!\n");
    drop(v);
}

#[cfg(feature = "cow")]
#[test]
fn copy_on_write_lines_include_private_edits() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("c.txt");
    std::fs::write(&path, b"one\ntwo\nthree\n").expect("write");

    // Default COW is read-only, so fill_buf lends the mapping itself.
    let ro_cow = MemoryMappedFile::open_cow(&path).expect("open_cow");
    let mut r = ro_cow.reader();
    assert_eq!(r.fill_buf().expect("fill_buf").len(), 14);
    drop(ro_cow);

    let cow = MemoryMappedFile::open_cow_writable(&path).expect("open_cow_writable");
    cow.update_region(4, b"TWO").expect("private edit");
    let lines: Vec<String> = cow
        .reader()
        .lines()
        .collect::<Result<_, _>>()
        .expect("lines");
    assert_eq!(lines, ["one", "TWO", "three"]);
    assert_eq!(std::fs::read(&path).expect("read"), b"one\ntwo\nthree\n");
}

#[test]
fn empty_and_single_byte_files() {
    let dir = tempfile::tempdir().expect("tempdir");
    let empty = dir.path().join("empty.bin");
    std::fs::write(&empty, b"").expect("write");
    let ro = MemoryMappedFile::open_ro(&empty).expect("open_ro");
    assert!(ro.reader().fill_buf().expect("empty").is_empty());
    assert_eq!(ro.reader().lines().count(), 0);
    let m = MemoryMappedFile::create_rw(dir.path().join("one.bin"), 1).expect("create_rw");
    m.update_region(0, b"z").expect("write");
    let mut r = m.reader();
    assert_eq!(r.fill_buf().expect("fill"), b"z");
}
