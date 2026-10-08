//! Writable copy-on-write mappings (1.1.0): every write path works on
//! private pages, nothing reaches the file, flushes are no-ops, and the
//! locking rules match `ReadWrite`.

#![cfg(feature = "cow")]

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;

use mmap_io::{MemoryMappedFile, MmapIoError, MmapMode};

fn fixture(dir: &Path, name: &str, len: usize) -> (PathBuf, Vec<u8>) {
    let path = dir.join(name);
    let data: Vec<u8> = (0..len).map(|i| (i % 251) as u8).collect();
    fs::write(&path, &data).expect("write fixture");
    (path, data)
}

#[test]
fn update_region_is_visible_and_never_reaches_the_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (path, data) = fixture(dir.path(), "a.bin", 10_000);
    let cow = MemoryMappedFile::open_cow(&path).expect("open_cow");
    assert_eq!(cow.mode(), MmapMode::CopyOnWrite);

    cow.update_region(0, b"HEAD").expect("write at 0");
    cow.update_region(9_996, b"TAIL").expect("write at end");
    cow.update_region(5_000, &[]).expect("empty write");
    cow.update_region(20_000, &[])
        .expect("empty write past end");

    let mut buf = [0u8; 4];
    cow.read_into(0, &mut buf).expect("read");
    assert_eq!(&buf, b"HEAD");
    assert_eq!(&*cow.as_slice(9_996, 4).expect("slice"), b"TAIL");
    assert_eq!(cow.as_slice(4, 4).expect("slice"), &data[4..8]);

    cow.flush().expect("flush is a no-op");
    cow.flush_range(0, 4).expect("flush_range is a no-op");
    assert_eq!(cow.pending_bytes(), 0);
    drop(cow);
    assert_eq!(fs::read(&path).expect("read file"), data);
}

#[test]
fn write_errors_match_read_write_rules() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (path, data) = fixture(dir.path(), "b.bin", 4096);
    let cow = MemoryMappedFile::open_cow(&path).expect("open_cow");

    assert!(matches!(
        cow.update_region(4095, b"xy"),
        Err(MmapIoError::OutOfBounds { .. })
    ));
    assert!(matches!(
        cow.update_region(u64::MAX, b"x"),
        Err(MmapIoError::OutOfBounds { .. })
    ));
    assert!(matches!(
        cow.as_slice_mut(4096, 1),
        Err(MmapIoError::OutOfBounds { .. })
    ));
    assert!(matches!(
        cow.flush_range(4000, 200),
        Err(MmapIoError::OutOfBounds { .. })
    ));
    // Resize stays unsupported on private mappings.
    assert!(matches!(cow.resize(8192), Err(MmapIoError::InvalidMode(_))));
    assert_eq!(cow.len(), 4096);
    // The 0.9.6 shim cannot hand out an unguarded `&[u8]` to writable
    // memory any more.
    assert!(matches!(
        cow.as_slice_bytes(0, 4),
        Err(MmapIoError::InvalidMode(_))
    ));
    drop(cow);
    assert_eq!(fs::read(&path).expect("read"), data);
}

#[test]
fn as_slice_mut_and_raw_pointer_write_privately() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (path, data) = fixture(dir.path(), "c.bin", 8192);
    let cow = MemoryMappedFile::open_cow(&path).expect("open_cow");
    {
        let mut s = cow.as_slice_mut(100, 3).expect("slice_mut");
        s.as_mut().copy_from_slice(b"abc");
    }
    let empty = cow.as_slice_mut(9999, 0).expect("empty slice_mut");
    assert!(empty.is_empty());
    drop(empty);
    // SAFETY: the mapping is 8192 bytes long, no other reference to
    // byte 200 is alive, and the pointer is not used after this block.
    unsafe {
        let p = cow.as_mut_ptr().expect("as_mut_ptr on cow");
        p.add(200).write(0xAB);
    }
    assert_eq!(&*cow.as_slice(100, 3).expect("slice"), b"abc");
    assert_eq!(cow.as_slice(200, 1).expect("slice")[0], 0xAB);
    assert_eq!(cow.pending_bytes(), 0);
    drop(cow);
    assert_eq!(fs::read(&path).expect("read"), data);
}

#[cfg(feature = "iterator")]
#[test]
fn chunks_mut_writes_privately() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (path, data) = fixture(dir.path(), "d.bin", 10_000);
    let cow = MemoryMappedFile::open_cow(&path).expect("open_cow");
    cow.chunks_mut(4096)
        .for_each_mut(|off, chunk| {
            chunk.fill((off / 4096) as u8 + 1);
            Ok(())
        })
        .expect("chunks_mut on cow");
    let firsts: Vec<u8> = cow.chunks(4096).map(|c| c[0]).collect();
    assert_eq!(firsts, vec![1, 2, 3]);
    assert_eq!(cow.pending_bytes(), 0);
    drop(cow);
    assert_eq!(fs::read(&path).expect("read"), data);
}

#[cfg(feature = "atomic")]
#[test]
fn atomic_views_on_cow_are_shared_between_threads_and_private() {
    use std::sync::atomic::Ordering;
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("e.bin");
    fs::write(&path, vec![0u8; 64]).expect("write");
    let cow = Arc::new(MemoryMappedFile::open_cow(&path).expect("open_cow"));
    let handles: Vec<_> = (0..4)
        .map(|_| {
            let cow = Arc::clone(&cow);
            thread::spawn(move || {
                let counter = cow.atomic_u64(8).expect("atomic");
                for _ in 0..1000 {
                    counter.fetch_add(1, Ordering::SeqCst);
                }
            })
        })
        .collect();
    for h in handles {
        h.join().expect("join");
    }
    assert_eq!(
        cow.atomic_u64(8).expect("atomic").load(Ordering::SeqCst),
        4000
    );
    assert!(matches!(
        cow.atomic_u64(1),
        Err(MmapIoError::Misaligned { .. })
    ));
    assert!(matches!(
        cow.atomic_u32(64),
        Err(MmapIoError::OutOfBounds { .. })
    ));
    assert_eq!(cow.pending_bytes(), 0);
    drop(cow);
    assert_eq!(fs::read(&path).expect("read"), vec![0u8; 64]);
}

#[test]
fn concurrent_disjoint_writers_and_readers() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (path, data) = fixture(dir.path(), "f.bin", 8 * 1024);
    let cow = Arc::new(MemoryMappedFile::open_cow(&path).expect("open_cow"));
    let handles: Vec<_> = (0..8u8)
        .map(|t| {
            let cow = Arc::clone(&cow);
            thread::spawn(move || {
                for _ in 0..50 {
                    cow.update_region(u64::from(t) * 1024, &[t; 1024])
                        .expect("write");
                    let mut buf = [0u8; 16];
                    cow.read_into(u64::from(t) * 1024, &mut buf).expect("read");
                    assert_eq!(buf, [t; 16]);
                }
            })
        })
        .collect();
    for h in handles {
        h.join().expect("join");
    }
    for t in 0..8u8 {
        assert!(cow
            .as_slice(u64::from(t) * 1024, 1024)
            .expect("slice")
            .iter()
            .all(|&b| b == t));
    }
    drop(cow);
    assert_eq!(fs::read(&path).expect("read"), data);
}

#[test]
fn two_cow_mappings_are_independent() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (path, data) = fixture(dir.path(), "g.bin", 4096);
    let a = MemoryMappedFile::open_cow(&path).expect("a");
    let b = MemoryMappedFile::open_cow(&path).expect("b");
    a.update_region(0, b"AAAA").expect("write a");
    assert_eq!(b.as_slice(0, 4).expect("b"), &data[..4]);
    // Clones share the same private pages.
    let a2 = a.clone();
    assert_eq!(&*a2.as_slice(0, 4).expect("a2"), b"AAAA");
}

#[test]
fn builder_and_from_file_open_writable_cow() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (path, data) = fixture(dir.path(), "h.bin", 4096);
    let m = MemoryMappedFile::builder(&path)
        .mode(MmapMode::CopyOnWrite)
        .open()
        .expect("builder open");
    m.update_region(0, b"B").expect("write");
    assert_eq!(m.as_slice(0, 1).expect("slice")[0], b'B');

    let file = fs::File::open(&path).expect("open ro handle");
    let m2 = MemoryMappedFile::from_file(file, MmapMode::CopyOnWrite, &path).expect("from_file");
    m2.update_region(1, b"F").expect("write");
    assert_eq!(m2.as_slice(1, 1).expect("slice")[0], b'F');
    drop((m, m2));
    assert_eq!(fs::read(&path).expect("read"), data);
}

#[test]
fn read_only_file_permission_is_enough() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (path, data) = fixture(dir.path(), "ro.bin", 4096);
    let mut perms = fs::metadata(&path).expect("meta").permissions();
    perms.set_readonly(true);
    fs::set_permissions(&path, perms.clone()).expect("set readonly");
    let cow = MemoryMappedFile::open_cow(&path).expect("open_cow on read-only file");
    cow.update_region(10, b"private").expect("write");
    assert_eq!(&*cow.as_slice(10, 7).expect("slice"), b"private");
    drop(cow);
    assert_eq!(fs::read(&path).expect("read"), data);
    #[allow(clippy::permissions_set_readonly_false)] // test cleanup: let tempdir delete the file
    perms.set_readonly(false);
    fs::set_permissions(&path, perms).expect("restore");
}

#[cfg(feature = "advise")]
#[test]
fn dontneed_on_cow_waits_for_views_and_keeps_memory_sound() {
    use mmap_io::MmapAdvice;
    let dir = tempfile::tempdir().expect("tempdir");
    let (path, data) = fixture(dir.path(), "dn.bin", 3 * 4096);
    let cow = Arc::new(MemoryMappedFile::open_cow(&path).expect("open_cow"));
    cow.update_region(0, &[0xEE; 4096]).expect("private write");

    // A live view keeps DontNeed out: it would change the bytes the
    // view is borrowing.
    let view = cow.as_slice(0, 4096).expect("view");
    let worker = {
        let cow = Arc::clone(&cow);
        thread::spawn(move || cow.advise(0, 4096, MmapAdvice::DontNeed))
    };
    thread::sleep(std::time::Duration::from_millis(150));
    assert!(!worker.is_finished(), "DontNeed ran while a view was alive");
    assert!(
        view.iter().all(|&b| b == 0xEE),
        "view changed under its borrow"
    );
    drop(view);
    worker.join().expect("join").expect("advise");

    // Linux discards the private copies, so the file contents come
    // back. Other platforms treat the hint as advisory.
    let now = cow.as_slice(0, 4096).expect("slice").to_vec();
    if cfg!(target_os = "linux") {
        assert_eq!(now, &data[..4096]);
    } else {
        assert!(now == data[..4096] || now.iter().all(|&b| b == 0xEE));
    }
    // Other hints only take a read guard and leave private data alone.
    cow.update_region(4096, b"keep").expect("write");
    let held = cow.as_slice(8192, 1).expect("held view");
    cow.advise(4096, 4096, MmapAdvice::WillNeed)
        .expect("willneed with a live view");
    drop(held);
    assert_eq!(&*cow.as_slice(4096, 4).expect("slice"), b"keep");
    drop(cow);
    assert_eq!(fs::read(&path).expect("read"), data);
}
