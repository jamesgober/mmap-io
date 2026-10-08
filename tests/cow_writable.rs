//! Copy-on-write mappings in 1.1.0.
//!
//! By default (`open_cow`, the builder, `load_mmap`, `from_file`) a
//! copy-on-write mapping is read-only through the safe API, exactly as
//! in 1.0. Opting in (`open_cow_writable`, the builder's
//! `cow_writable(true)`) makes every write path work on private pages:
//! nothing reaches the file, flushes are no-ops, and the locking rules
//! match `ReadWrite`.

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
    let cow = MemoryMappedFile::open_cow_writable(&path).expect("open_cow_writable");
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
    let cow = MemoryMappedFile::open_cow_writable(&path).expect("open_cow_writable");

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
    let cow = MemoryMappedFile::open_cow_writable(&path).expect("open_cow_writable");
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
    let cow = MemoryMappedFile::open_cow_writable(&path).expect("open_cow_writable");
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
    let cow = Arc::new(MemoryMappedFile::open_cow_writable(&path).expect("open_cow_writable"));
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
    let cow = Arc::new(MemoryMappedFile::open_cow_writable(&path).expect("open_cow_writable"));
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
    let a = MemoryMappedFile::open_cow_writable(&path).expect("a");
    let b = MemoryMappedFile::open_cow_writable(&path).expect("b");
    a.update_region(0, b"AAAA").expect("write a");
    assert_eq!(b.as_slice(0, 4).expect("b"), &data[..4]);
    // Clones share the same private pages.
    let a2 = a.clone();
    assert_eq!(&*a2.as_slice(0, 4).expect("a2"), b"AAAA");
}

#[test]
fn builder_cow_writable_opts_in_and_from_file_stays_read_only() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (path, data) = fixture(dir.path(), "h.bin", 4096);
    let m = MemoryMappedFile::builder(&path)
        .mode(MmapMode::CopyOnWrite)
        .cow_writable(true)
        .open()
        .expect("builder open");
    assert!(m.is_cow_writable());
    m.update_region(0, b"B").expect("write");
    assert_eq!(m.as_slice(0, 1).expect("slice")[0], b'B');

    // `create` and `open_or_create` open the existing file for COW and
    // honor the flag too.
    for writable in [false, true] {
        let c = MemoryMappedFile::builder(&path)
            .mode(MmapMode::CopyOnWrite)
            .cow_writable(writable)
            .create()
            .expect("builder create");
        let o = MemoryMappedFile::builder(&path)
            .mode(MmapMode::CopyOnWrite)
            .cow_writable(writable)
            .open_or_create()
            .expect("builder open_or_create");
        for x in [&c, &o] {
            assert_eq!(x.mode(), MmapMode::CopyOnWrite);
            assert_eq!(x.is_cow_writable(), writable);
            assert_eq!(x.update_region(2, b"C").is_ok(), writable);
        }
    }
    // Explicitly off is the default.
    let off = MemoryMappedFile::builder(&path)
        .mode(MmapMode::CopyOnWrite)
        .cow_writable(false)
        .open()
        .expect("builder open, flag off");
    assert!(!off.is_cow_writable());
    assert_eq!(off.as_slice_bytes(0, 4).expect("shim"), &data[..4]);

    // `from_file` has no opt-in: its COW mapping is the read-only kind.
    let file = fs::File::open(&path).expect("open ro handle");
    let m2 = MemoryMappedFile::from_file(file, MmapMode::CopyOnWrite, &path).expect("from_file");
    assert!(!m2.is_cow_writable());
    assert!(matches!(
        m2.update_region(1, b"F"),
        Err(MmapIoError::InvalidMode(_))
    ));
    drop((m, m2, off));
    assert_eq!(fs::read(&path).expect("read"), data);
}

#[test]
fn cow_writable_is_ignored_for_other_modes() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (path, data) = fixture(dir.path(), "modes.bin", 4096);
    let ro = MemoryMappedFile::builder(&path)
        .mode(MmapMode::ReadOnly)
        .cow_writable(true)
        .open()
        .expect("ro");
    assert_eq!(ro.mode(), MmapMode::ReadOnly);
    assert!(!ro.is_cow_writable());
    assert!(matches!(
        ro.update_region(0, b"x"),
        Err(MmapIoError::InvalidMode(_))
    ));
    drop(ro);

    let rw = MemoryMappedFile::builder(&path)
        .mode(MmapMode::ReadWrite)
        .cow_writable(true)
        .open()
        .expect("rw");
    assert_eq!(rw.mode(), MmapMode::ReadWrite);
    assert!(!rw.is_cow_writable());
    rw.update_region(0, b"shared").expect("rw write");
    rw.flush().expect("flush");
    drop(rw);
    let now = fs::read(&path).expect("read");
    assert_eq!(&now[..6], b"shared");
    assert_eq!(&now[6..], &data[6..]);

    let created = MemoryMappedFile::builder(dir.path().join("new.bin"))
        .size(64)
        .cow_writable(true)
        .create_new()
        .expect("create_new");
    assert!(!created.is_cow_writable());
}

#[test]
fn is_cow_writable_reports_the_opt_in_only() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (path, _) = fixture(dir.path(), "acc.bin", 4096);
    assert!(!MemoryMappedFile::open_ro(&path)
        .expect("ro")
        .is_cow_writable());
    assert!(!MemoryMappedFile::open_rw(&path)
        .expect("rw")
        .is_cow_writable());
    assert!(!MemoryMappedFile::open_cow(&path)
        .expect("cow")
        .is_cow_writable());
    assert!(!mmap_io::load_mmap(&path, MmapMode::CopyOnWrite)
        .expect("load_mmap")
        .is_cow_writable());
    let w = MemoryMappedFile::open_cow_writable(&path).expect("writable");
    assert!(w.is_cow_writable());
    assert_eq!(w.mode(), MmapMode::CopyOnWrite);
    // Clones share the mapping, and the flag with it.
    assert!(w.clone().is_cow_writable());
    // The convenience constructor rejects what open_cow rejects.
    let empty = dir.path().join("empty.bin");
    fs::write(&empty, b"").expect("write empty");
    assert!(matches!(
        MemoryMappedFile::open_cow_writable(&empty),
        Err(MmapIoError::ResizeFailed(_))
    ));
    assert!(matches!(
        MemoryMappedFile::open_cow_writable(dir.path().join("missing.bin")),
        Err(MmapIoError::Io(_))
    ));
}

/// Every way to get a default copy-on-write mapping behaves as in 1.0:
/// reads work (including the unguarded `as_slice_bytes` shim), every
/// write method returns `InvalidMode`, flushes are `Ok` no-ops, and the
/// file never changes.
#[test]
fn default_cow_is_read_only_like_1_0() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (path, data) = fixture(dir.path(), "ro_cow.bin", 3 * 4096 + 5);
    let maps = [
        MemoryMappedFile::open_cow(&path).expect("open_cow"),
        MemoryMappedFile::builder(&path)
            .mode(MmapMode::CopyOnWrite)
            .open()
            .expect("builder open"),
        MemoryMappedFile::builder(&path)
            .mode(MmapMode::CopyOnWrite)
            .create()
            .expect("builder create"),
        MemoryMappedFile::builder(&path)
            .mode(MmapMode::CopyOnWrite)
            .open_or_create()
            .expect("builder open_or_create"),
        mmap_io::load_mmap(&path, MmapMode::CopyOnWrite).expect("load_mmap"),
        MemoryMappedFile::from_file(
            fs::File::open(&path).expect("file"),
            MmapMode::CopyOnWrite,
            &path,
        )
        .expect("from_file"),
    ];
    let invalid = |r: Result<(), MmapIoError>| matches!(r, Err(MmapIoError::InvalidMode(_)));
    for m in &maps {
        assert_eq!(m.mode(), MmapMode::CopyOnWrite);
        assert!(!m.is_cow_writable());
        assert_eq!(m.as_slice_bytes(0, 16).expect("shim"), &data[..16]);
        assert_eq!(m.as_slice_bytes(5, 0).expect("empty shim"), b"");
        // Two unguarded borrows at once, as 0.9.6 / 1.0 code does.
        let (a, b) = (
            m.as_slice_bytes(0, 4).expect("a"),
            m.as_slice_bytes(4, 4).expect("b"),
        );
        assert_eq!([a, b].concat(), &data[..8]);
        assert_eq!(&*m.as_slice(100, 10).expect("slice"), &data[100..110]);

        assert!(invalid(m.update_region(0, b"x")));
        assert!(invalid(m.try_update_region(0, b"x").map(|_| ())));
        assert!(invalid(m.as_slice_mut(0, 1).map(|_| ())));
        assert!(invalid(m.try_as_slice_mut(0, 1).map(|_| ())));
        assert!(invalid(mmap_io::update_region(m, 0, b"x")));
        // SAFETY: the pointer is never used; only the error is checked.
        assert!(invalid(unsafe { m.as_mut_ptr() }.map(|_| ())));
        assert!(invalid(m.resize(8192)));
        // Empty writes are accepted in every mode, as in 1.0.
        m.update_region(0, &[]).expect("empty write");

        m.flush().expect("flush no-op");
        m.flush_range(0, m.len()).expect("flush_range no-op");
        assert_eq!(m.pending_bytes(), 0);
    }
    drop(maps);
    assert_eq!(fs::read(&path).expect("read"), data);
}

#[test]
fn default_cow_segment_writes_are_refused() {
    use mmap_io::segment::{Segment, SegmentMut};
    let dir = tempfile::tempdir().expect("tempdir");
    let (path, data) = fixture(dir.path(), "seg.bin", 4096);
    let m = Arc::new(MemoryMappedFile::open_cow(&path).expect("open_cow"));
    let seg = SegmentMut::new(Arc::clone(&m), 0, 16).expect("segment");
    assert!(matches!(seg.write(b"x"), Err(MmapIoError::InvalidMode(_))));
    assert!(matches!(
        seg.as_slice_mut(),
        Err(MmapIoError::InvalidMode(_))
    ));
    let read = Segment::new(Arc::clone(&m), 0, 16).expect("read segment");
    assert_eq!(&*read.as_slice().expect("slice"), &data[..16]);

    let w = Arc::new(MemoryMappedFile::open_cow_writable(&path).expect("writable"));
    let seg = SegmentMut::new(Arc::clone(&w), 0, 16).expect("segment");
    seg.write(b"segment!").expect("private segment write");
    assert_eq!(&*w.as_slice(0, 8).expect("slice"), b"segment!");
    drop((seg, w));
    assert_eq!(fs::read(&path).expect("read"), data);
}

#[cfg(feature = "iterator")]
#[test]
fn default_cow_chunks_mut_is_refused() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (path, data) = fixture(dir.path(), "it.bin", 8192);
    let m = MemoryMappedFile::open_cow(&path).expect("open_cow");
    for size in [0, 1, 4096] {
        assert!(matches!(
            m.chunks_mut(size).for_each_mut(|_, c| {
                c.fill(0);
                Ok(())
            }),
            Err(MmapIoError::InvalidMode(_))
        ));
    }
    assert_eq!(m.chunks(4096).count(), 2);
    drop(m);
    assert_eq!(fs::read(&path).expect("read"), data);
}

#[cfg(feature = "atomic")]
#[test]
fn atomics_need_the_writable_opt_in() {
    use std::sync::atomic::Ordering;
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("atom.bin");
    fs::write(&path, vec![0u8; 64]).expect("write");
    let ro = MemoryMappedFile::open_cow(&path).expect("open_cow");
    assert!(matches!(ro.atomic_u64(0), Err(MmapIoError::InvalidMode(_))));
    assert!(matches!(ro.atomic_u32(0), Err(MmapIoError::InvalidMode(_))));
    assert!(matches!(
        ro.atomic_u64_slice(0, 2),
        Err(MmapIoError::InvalidMode(_))
    ));
    assert!(matches!(
        ro.atomic_u32_slice(0, 2),
        Err(MmapIoError::InvalidMode(_))
    ));

    let w = MemoryMappedFile::open_cow_writable(&path).expect("writable");
    w.atomic_u32_slice(0, 2).expect("slice view")[1].store(9, Ordering::SeqCst);
    let mut buf = [0u8; 8];
    w.read_into(0, &mut buf).expect("read");
    assert_eq!(u32::from_ne_bytes([buf[4], buf[5], buf[6], buf[7]]), 9);
    // The read-only mapping of the same file never sees private stores.
    assert_eq!(ro.as_slice_bytes(0, 8).expect("shim"), &[0u8; 8]);
    drop((ro, w));
    assert_eq!(fs::read(&path).expect("read"), vec![0u8; 64]);
}

#[cfg(feature = "advise")]
#[test]
fn dontneed_on_default_cow_does_not_wait_for_views() {
    use mmap_io::MmapAdvice;
    let dir = tempfile::tempdir().expect("tempdir");
    let (path, data) = fixture(dir.path(), "dn_ro.bin", 3 * 4096);
    let m = MemoryMappedFile::open_cow(&path).expect("open_cow");
    // No private copies exist, so DontNeed cannot change the bytes and
    // runs while views are alive on this very thread (a writable COW
    // mapping would deadlock here).
    let view = m.as_slice(0, 4096).expect("view");
    let shim = m.as_slice_bytes(4096, 16).expect("shim");
    m.advise(0, 3 * 4096, MmapAdvice::DontNeed)
        .expect("dontneed with live views");
    assert_eq!(&*view, &data[..4096]);
    assert_eq!(shim, &data[4096..4096 + 16]);
    drop(view);
    assert_eq!(&*m.as_slice(0, 3 * 4096).expect("slice"), &data[..]);
}

#[test]
fn read_only_file_permission_is_enough() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (path, data) = fixture(dir.path(), "ro.bin", 4096);
    let mut perms = fs::metadata(&path).expect("meta").permissions();
    perms.set_readonly(true);
    fs::set_permissions(&path, perms.clone()).expect("set readonly");
    let cow = MemoryMappedFile::open_cow_writable(&path).expect("open on read-only file");
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
    let cow = Arc::new(MemoryMappedFile::open_cow_writable(&path).expect("open_cow_writable"));
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
