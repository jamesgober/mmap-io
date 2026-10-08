//! Constructors and builder paths: `create_rw`, `open_ro`, `open_rw`,
//! `open_cow`, `from_file`, `open_or_create`, the builder's `create`,
//! `open` and `open_or_create`, `unmap`, and the OS-handle traits.

use std::fs;
use std::io::{ErrorKind, Write};

use mmap_io::flush::FlushPolicy;
use mmap_io::{MemoryMappedFile, MmapIoError, MmapMode, TouchHint};

use crate::common::{
    boundary_sizes, pattern, permissions_are_enforced, read_file, set_readonly, tmp_path,
};

/// Largest size the crate accepts on this target.
const MAX_SIZE: u64 = if cfg!(target_pointer_width = "64") {
    128 << 40
} else {
    2 << 30
};

fn assert_io_kind<T: std::fmt::Debug>(r: Result<T, MmapIoError>, kind: ErrorKind, what: &str) {
    match r {
        Err(MmapIoError::Io(e)) => assert_eq!(e.kind(), kind, "{what}: {e}"),
        other => panic!("{what}: expected Io({kind:?}), got {other:?}"),
    }
}

fn assert_io<T: std::fmt::Debug>(r: Result<T, MmapIoError>, what: &str) {
    match r {
        Err(MmapIoError::Io(_)) => {}
        other => panic!("{what}: expected Io, got {other:?}"),
    }
}

fn assert_resize_failed<T: std::fmt::Debug>(r: Result<T, MmapIoError>, what: &str) {
    match r {
        Err(MmapIoError::ResizeFailed(msg)) => assert!(!msg.is_empty(), "{what}"),
        other => panic!("{what}: expected ResizeFailed, got {other:?}"),
    }
}

fn write_file(path: &std::path::Path, bytes: &[u8]) {
    let mut f = fs::File::create(path).expect("create");
    f.write_all(bytes).expect("write");
}

#[test]
fn create_rw_every_boundary_size_round_trips() {
    for size in boundary_sizes().into_iter().chain([1 << 20, (1 << 20) + 3]) {
        let path = tmp_path("sizes.bin");
        let mmap = MemoryMappedFile::create_rw(&path, size).expect("create_rw");
        assert_eq!(mmap.len(), size);
        assert_eq!(mmap.current_len().unwrap(), size);
        assert!(!mmap.is_empty());
        assert_eq!(mmap.mode(), MmapMode::ReadWrite);
        assert_eq!(mmap.path(), &*path);
        assert_eq!(fs::metadata(&path).unwrap().len(), size);

        // A fresh file reads as zeros everywhere.
        let mut all = vec![0xFFu8; size as usize];
        mmap.read_into(0, &mut all).unwrap();
        assert!(all.iter().all(|&b| b == 0), "size {size}: not zeroed");

        let data = pattern(size as usize, 7);
        mmap.update_region(0, &data).unwrap();
        mmap.flush().unwrap();
        drop(mmap);
        assert_eq!(read_file(&path), data, "size {size}");
        let ro = MemoryMappedFile::open_ro(&path).unwrap();
        assert_eq!(ro.as_slice(0, size).unwrap(), &data[..]);
    }
}

#[test]
fn invalid_sizes_are_rejected_before_the_file_is_touched() {
    for size in [0, MAX_SIZE + 1, u64::MAX] {
        // Missing file: nothing is created.
        let path = tmp_path("never.bin");
        assert_resize_failed(MemoryMappedFile::create_rw(&path, size), "create_rw");
        assert!(!path.exists(), "create_rw({size}) created the file");
        assert_resize_failed(
            MemoryMappedFile::builder(&path).size(size).create(),
            "builder create",
        );
        assert!(!path.exists(), "builder create({size}) created the file");
        assert_resize_failed(
            MemoryMappedFile::open_or_create(&path, size),
            "open_or_create",
        );
        assert!(!path.exists(), "open_or_create({size}) created the file");

        // Existing file: not truncated.
        write_file(&path, b"keep me");
        assert_resize_failed(
            MemoryMappedFile::create_rw(&path, size),
            "create_rw existing",
        );
        assert_resize_failed(
            MemoryMappedFile::builder(&path).size(size).create(),
            "builder create existing",
        );
        assert_eq!(read_file(&path), b"keep me", "size {size}");
    }
}

#[test]
fn builder_create_without_size_is_rejected() {
    let path = tmp_path("nosize.bin");
    assert_resize_failed(MemoryMappedFile::builder(&path).create(), "no size");
    assert_resize_failed(
        MemoryMappedFile::builder(&path)
            .mode(MmapMode::ReadWrite)
            .create(),
        "no size, explicit RW",
    );
    assert!(!path.exists());
}

#[test]
fn create_rw_truncates_an_existing_file() {
    let path = tmp_path("trunc.bin");
    write_file(&path, &pattern(10_000, 1));
    let mmap = MemoryMappedFile::create_rw(&path, 100).unwrap();
    assert_eq!(mmap.len(), 100);
    assert_eq!(mmap.as_slice(0, 100).unwrap(), &[0u8; 100][..]);
    drop(mmap);
    assert_eq!(fs::metadata(&path).unwrap().len(), 100);

    // And extends a shorter one with zeros.
    write_file(&path, b"abc");
    let mmap = MemoryMappedFile::builder(&path)
        .size(5000)
        .create()
        .unwrap();
    assert_eq!(mmap.len(), 5000);
    assert_eq!(mmap.as_slice(0, 3).unwrap(), &[0u8; 3][..]);
}

#[test]
fn opening_a_missing_file_is_not_found_everywhere() {
    let path = tmp_path("missing.bin");
    assert_io_kind(
        MemoryMappedFile::open_ro(&path),
        ErrorKind::NotFound,
        "open_ro",
    );
    assert_io_kind(
        MemoryMappedFile::open_rw(&path),
        ErrorKind::NotFound,
        "open_rw",
    );
    assert_io_kind(
        MemoryMappedFile::builder(&path).open(),
        ErrorKind::NotFound,
        "builder open (RO)",
    );
    assert_io_kind(
        MemoryMappedFile::builder(&path)
            .mode(MmapMode::ReadWrite)
            .open(),
        ErrorKind::NotFound,
        "builder open (RW)",
    );
    assert_io_kind(
        MemoryMappedFile::builder(&path)
            .mode(MmapMode::ReadOnly)
            .create(),
        ErrorKind::NotFound,
        "builder create (RO)",
    );
    assert_io_kind(
        MemoryMappedFile::builder(&path)
            .mode(MmapMode::ReadOnly)
            .open_or_create(),
        ErrorKind::NotFound,
        "builder open_or_create (RO)",
    );
    assert_io_kind(
        mmap_io::load_mmap(&path, MmapMode::ReadOnly),
        ErrorKind::NotFound,
        "load_mmap RO",
    );
    assert_io_kind(
        mmap_io::load_mmap(&path, MmapMode::ReadWrite),
        ErrorKind::NotFound,
        "load_mmap RW",
    );
    #[cfg(feature = "cow")]
    {
        assert_io_kind(
            MemoryMappedFile::open_cow(&path),
            ErrorKind::NotFound,
            "open_cow",
        );
        assert_io_kind(
            MemoryMappedFile::builder(&path)
                .mode(MmapMode::CopyOnWrite)
                .open(),
            ErrorKind::NotFound,
            "builder open (COW)",
        );
    }
    // The parent directory does not exist either.
    let deep = path.sibling("no/such/dir/file.bin");
    assert_io_kind(
        MemoryMappedFile::create_rw(&deep, 10),
        ErrorKind::NotFound,
        "create_rw in missing dir",
    );
    assert_io_kind(
        MemoryMappedFile::open_or_create(&deep, 10),
        ErrorKind::NotFound,
        "open_or_create in missing dir",
    );
    assert!(!path.exists());
}

#[test]
fn empty_file_maps_read_only_but_not_read_write() {
    let path = tmp_path("empty.bin");
    write_file(&path, b"");

    let ro = MemoryMappedFile::open_ro(&path).expect("RO map of empty file");
    assert_eq!(ro.len(), 0);
    assert!(ro.is_empty());
    assert_eq!(ro.as_slice(0, 0).unwrap().len(), 0);
    assert_eq!(ro.as_slice(7, 0).unwrap().len(), 0);
    assert!(matches!(
        ro.as_slice(0, 1),
        Err(MmapIoError::OutOfBounds {
            offset: 0,
            len: 1,
            total: 0
        })
    ));
    ro.read_into(0, &mut []).unwrap();
    ro.touch_pages().unwrap();
    ro.flush().unwrap();
    ro.flush_range(0, 0).unwrap();
    ro.prefetch_range(0, 0).unwrap();
    let mut out = Vec::new();
    std::io::Read::read_to_end(&mut ro.reader(), &mut out).unwrap();
    assert!(out.is_empty());
    #[cfg(feature = "iterator")]
    {
        assert_eq!(ro.chunks(1).count(), 0);
        assert_eq!(ro.pages().len(), 0);
    }
    #[cfg(feature = "locking")]
    {
        ro.lock_all().unwrap();
        ro.unlock_all().unwrap();
    }
    drop(ro);

    let from =
        MemoryMappedFile::from_file(fs::File::open(&path).unwrap(), MmapMode::ReadOnly, &*path)
            .expect("from_file RO empty");
    assert!(from.is_empty());
    drop(from);

    assert_resize_failed(MemoryMappedFile::open_rw(&path), "open_rw empty");
    assert_resize_failed(
        MemoryMappedFile::builder(&path)
            .mode(MmapMode::ReadWrite)
            .open(),
        "builder open RW empty",
    );
    assert_resize_failed(
        mmap_io::load_mmap(&path, MmapMode::ReadWrite),
        "load_mmap RW empty",
    );
    let rw_file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
        .unwrap();
    assert_resize_failed(
        MemoryMappedFile::from_file(rw_file, MmapMode::ReadWrite, &*path),
        "from_file RW empty",
    );
    #[cfg(feature = "cow")]
    {
        assert_resize_failed(MemoryMappedFile::open_cow(&path), "open_cow empty");
        assert_resize_failed(
            MemoryMappedFile::builder(&path)
                .mode(MmapMode::CopyOnWrite)
                .open(),
            "builder open COW empty",
        );
        assert_resize_failed(
            MemoryMappedFile::from_file(
                fs::File::open(&path).unwrap(),
                MmapMode::CopyOnWrite,
                &*path,
            ),
            "from_file COW empty",
        );
    }
    // None of the failures changed the file.
    assert_eq!(fs::metadata(&path).unwrap().len(), 0);
}

#[test]
fn directories_are_rejected_with_io_errors() {
    let path = tmp_path("unused");
    let dir = path.dir();
    assert_io(MemoryMappedFile::open_ro(dir), "open_ro(dir)");
    assert_io(MemoryMappedFile::open_rw(dir), "open_rw(dir)");
    assert_io(MemoryMappedFile::create_rw(dir, 4096), "create_rw(dir)");
    assert_io(
        MemoryMappedFile::open_or_create(dir, 4096),
        "open_or_create(dir)",
    );
    assert_io(MemoryMappedFile::builder(dir).open(), "builder open(dir)");
    assert_io(
        MemoryMappedFile::builder(dir).size(4096).create(),
        "builder create(dir)",
    );
    #[cfg(feature = "cow")]
    assert_io(MemoryMappedFile::open_cow(dir), "open_cow(dir)");
    assert!(dir.is_dir(), "the directory must survive");
}

#[test]
fn read_only_permission_allows_only_read_only_mappings() {
    let path = tmp_path("readonly.bin");
    write_file(&path, &pattern(4096, 3));
    set_readonly(&path, true);
    if !permissions_are_enforced(&path) {
        // Running with privileges that bypass permission bits (root).
        set_readonly(&path, false);
        return;
    }

    assert_io_kind(
        MemoryMappedFile::open_rw(&path),
        ErrorKind::PermissionDenied,
        "open_rw",
    );
    assert_io_kind(
        MemoryMappedFile::create_rw(&path, 4096),
        ErrorKind::PermissionDenied,
        "create_rw",
    );
    assert_io_kind(
        MemoryMappedFile::open_or_create(&path, 4096),
        ErrorKind::PermissionDenied,
        "open_or_create",
    );
    assert_io_kind(
        MemoryMappedFile::builder(&path)
            .mode(MmapMode::ReadWrite)
            .open(),
        ErrorKind::PermissionDenied,
        "builder open RW",
    );
    assert_io_kind(
        mmap_io::write_mmap(&path, 0, b"x"),
        ErrorKind::PermissionDenied,
        "write_mmap",
    );

    let ro = MemoryMappedFile::open_ro(&path).expect("open_ro on a read-only file");
    assert_eq!(ro.as_slice(0, 4096).unwrap(), &pattern(4096, 3)[..]);
    drop(ro);
    #[cfg(feature = "cow")]
    {
        let cow = MemoryMappedFile::open_cow(&path).expect("open_cow on a read-only file");
        assert_eq!(cow.len(), 4096);
    }

    // A read-only handle cannot be mapped read-write.
    assert_io(
        MemoryMappedFile::from_file(fs::File::open(&path).unwrap(), MmapMode::ReadWrite, &*path),
        "from_file RW on a read-only handle",
    );

    assert_eq!(read_file(&path), pattern(4096, 3), "file changed");
    set_readonly(&path, false);
}

#[test]
fn read_only_handle_cannot_be_mapped_read_write_even_if_file_is_writable() {
    let path = tmp_path("ro_handle.bin");
    write_file(&path, b"0123456789");
    assert_io(
        MemoryMappedFile::from_file(fs::File::open(&path).unwrap(), MmapMode::ReadWrite, &*path),
        "from_file RW with a read-only handle",
    );
    // A read-write handle can be mapped read-only.
    let rw_handle = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
        .unwrap();
    let m = MemoryMappedFile::from_file(rw_handle, MmapMode::ReadOnly, &*path).unwrap();
    assert_eq!(m.mode(), MmapMode::ReadOnly);
    assert_eq!(m.as_slice(0, 10).unwrap(), b"0123456789");
    assert!(matches!(
        m.update_region(0, b"x"),
        Err(MmapIoError::InvalidMode(_))
    ));
}

#[test]
fn from_file_path_is_informational_only() {
    let path = tmp_path("real.bin");
    write_file(&path, b"payload");
    let fake = std::path::Path::new("this/path/does/not/exist.bin");
    let m = MemoryMappedFile::from_file(fs::File::open(&path).unwrap(), MmapMode::ReadOnly, fake)
        .unwrap();
    assert_eq!(m.path(), fake);
    assert_eq!(m.as_slice(0, 7).unwrap(), b"payload");
    assert!(format!("{m:?}").contains("does"), "{m:?}");

    let rw = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
        .unwrap();
    let m = MemoryMappedFile::from_file(rw, MmapMode::ReadWrite, "").unwrap();
    assert_eq!(m.path(), std::path::Path::new(""));
    m.update_region(0, b"P").unwrap();
    m.flush().unwrap();
    assert_eq!(m.flush_policy(), FlushPolicy::Never);
    drop(m);
    assert_eq!(read_file(&path), b"Payload");
}

#[cfg(not(feature = "cow"))]
#[test]
fn copy_on_write_requires_the_cow_feature() {
    let path = tmp_path("nocow.bin");
    write_file(&path, b"abc");
    assert!(matches!(
        MemoryMappedFile::from_file(
            fs::File::open(&path).unwrap(),
            MmapMode::CopyOnWrite,
            &*path
        ),
        Err(MmapIoError::InvalidMode(_))
    ));
    assert!(matches!(
        MemoryMappedFile::builder(&path)
            .mode(MmapMode::CopyOnWrite)
            .open(),
        Err(MmapIoError::InvalidMode(_))
    ));
    assert!(matches!(
        MemoryMappedFile::builder(&path)
            .mode(MmapMode::CopyOnWrite)
            .create(),
        Err(MmapIoError::InvalidMode(_))
    ));
    assert!(matches!(
        mmap_io::load_mmap(&path, MmapMode::CopyOnWrite),
        Err(MmapIoError::InvalidMode(_))
    ));
}

#[cfg(feature = "cow")]
#[test]
fn copy_on_write_paths_map_the_file_contents() {
    let path = tmp_path("cow.bin");
    let data = pattern(3 * 4096 + 11, 9);
    write_file(&path, &data);
    let maps = [
        MemoryMappedFile::open_cow(&path).unwrap(),
        MemoryMappedFile::builder(&path)
            .mode(MmapMode::CopyOnWrite)
            .open()
            .unwrap(),
        MemoryMappedFile::builder(&path)
            .mode(MmapMode::CopyOnWrite)
            .create()
            .unwrap(),
        MemoryMappedFile::builder(&path)
            .mode(MmapMode::CopyOnWrite)
            .open_or_create()
            .unwrap(),
        mmap_io::load_mmap(&path, MmapMode::CopyOnWrite).unwrap(),
        MemoryMappedFile::from_file(
            fs::File::open(&path).unwrap(),
            MmapMode::CopyOnWrite,
            &*path,
        )
        .unwrap(),
    ];
    for m in &maps {
        assert_eq!(m.mode(), MmapMode::CopyOnWrite);
        assert_eq!(m.len(), data.len() as u64);
        assert_eq!(m.as_slice(0, m.len()).unwrap(), &data[..]);
        assert_eq!(m.as_slice_bytes(1, 5).unwrap(), &data[1..6]);
        // SAFETY: a single byte read inside len() of an immutable view.
        assert_eq!(unsafe { m.as_ptr().read() }, data[0]);
        // flush is a no-op that succeeds.
        m.flush().unwrap();
        m.flush_range(0, m.len()).unwrap();
        assert_eq!(m.pending_bytes(), 0);
    }
    drop(maps);
    assert_eq!(read_file(&path), data);
}

#[test]
fn builder_default_modes() {
    let path = tmp_path("modes.bin");
    // create() defaults to ReadWrite.
    let m = MemoryMappedFile::builder(&path).size(10).create().unwrap();
    assert_eq!(m.mode(), MmapMode::ReadWrite);
    drop(m);
    // open() defaults to ReadOnly.
    let m = MemoryMappedFile::builder(&path).open().unwrap();
    assert_eq!(m.mode(), MmapMode::ReadOnly);
    drop(m);
    // open_or_create() defaults to ReadWrite.
    let m = MemoryMappedFile::builder(&path).open_or_create().unwrap();
    assert_eq!(m.mode(), MmapMode::ReadWrite);
    drop(m);
    // RO create on an existing file opens it as-is; size is ignored.
    let m = MemoryMappedFile::builder(&path)
        .mode(MmapMode::ReadOnly)
        .size(999_999)
        .create()
        .unwrap();
    assert_eq!((m.mode(), m.len()), (MmapMode::ReadOnly, 10));
    drop(m);
    // RO open_or_create on an existing file is a plain open.
    let m = MemoryMappedFile::builder(&path)
        .mode(MmapMode::ReadOnly)
        .open_or_create()
        .unwrap();
    assert_eq!((m.mode(), m.len()), (MmapMode::ReadOnly, 10));
    drop(m);
    assert_eq!(fs::metadata(&path).unwrap().len(), 10);
}

#[test]
fn open_or_create_never_truncates_and_validates_size_only_when_needed() {
    let path = tmp_path("ooc.bin");

    // Missing file, no size: rejected, nothing created.
    assert_resize_failed(
        MemoryMappedFile::builder(&path).open_or_create(),
        "missing, no size",
    );
    assert!(!path.exists());

    // Missing file with a size: created at that size.
    let m = MemoryMappedFile::open_or_create(&path, 4097).unwrap();
    assert_eq!(m.len(), 4097);
    m.update_region(4096, b"Z").unwrap();
    m.flush().unwrap();
    drop(m);

    // Existing non-empty file: size ignored (smaller, larger, zero,
    // missing, invalid), contents preserved.
    for size in [Some(1), Some(1 << 20), Some(0), None, Some(u64::MAX)] {
        let mut b = MemoryMappedFile::builder(&path);
        if let Some(s) = size {
            b = b.size(s);
        }
        let m = b.open_or_create().unwrap();
        assert_eq!(m.len(), 4097, "size {size:?}");
        assert_eq!(m.as_slice(4096, 1).unwrap(), b"Z");
    }
    assert_eq!(fs::metadata(&path).unwrap().len(), 4097);

    // Existing empty file: needs a valid size, then is extended.
    let empty = tmp_path("ooc_empty.bin");
    write_file(&empty, b"");
    assert_resize_failed(
        MemoryMappedFile::builder(&empty).open_or_create(),
        "empty, no size",
    );
    assert_resize_failed(
        MemoryMappedFile::open_or_create(&empty, 0),
        "empty, zero size",
    );
    assert_eq!(fs::metadata(&empty).unwrap().len(), 0);
    let m = MemoryMappedFile::open_or_create(&empty, 123).unwrap();
    assert_eq!(m.len(), 123);
    drop(m);
    assert_eq!(fs::metadata(&empty).unwrap().len(), 123);
}

#[test]
fn builder_options_apply_on_every_read_write_path() {
    let policies = [
        FlushPolicy::Never,
        FlushPolicy::Manual,
        FlushPolicy::Always,
        FlushPolicy::EveryBytes(10),
        FlushPolicy::EveryWrites(3),
        FlushPolicy::EveryMillis(0),
        FlushPolicy::EveryMillis(60_000),
    ];
    let hints = [TouchHint::Never, TouchHint::Eager, TouchHint::Lazy];
    for policy in policies {
        for hint in hints {
            let path = tmp_path("opts.bin");
            let b = || {
                MemoryMappedFile::builder(&path)
                    .mode(MmapMode::ReadWrite)
                    .size(8192)
                    .flush_policy(policy)
                    .touch_hint(hint)
            };
            let created = b().create().unwrap();
            assert_eq!(created.flush_policy(), policy);
            created.update_region(0, b"abc").unwrap();
            drop(created);
            let opened = b().open().unwrap();
            assert_eq!(opened.flush_policy(), policy);
            assert_eq!(opened.as_slice(0, 3).unwrap(), b"abc");
            drop(opened);
            let ooc = b().open_or_create().unwrap();
            assert_eq!(ooc.flush_policy(), policy);
            drop(ooc);
            // Read-only mappings never flush, whatever was requested.
            let ro = MemoryMappedFile::builder(&path)
                .mode(MmapMode::ReadOnly)
                .flush_policy(policy)
                .touch_hint(hint)
                .open()
                .unwrap();
            assert_eq!(ro.flush_policy(), FlushPolicy::Never);
        }
    }
}

#[test]
fn convenience_constructors_use_the_manual_policy() {
    let path = tmp_path("conv.bin");
    let m = MemoryMappedFile::create_rw(&path, 64).unwrap();
    assert_eq!(m.flush_policy(), FlushPolicy::Never);
    drop(m);
    assert_eq!(
        MemoryMappedFile::open_rw(&path).unwrap().flush_policy(),
        FlushPolicy::Never
    );
    assert_eq!(
        MemoryMappedFile::open_ro(&path).unwrap().flush_policy(),
        FlushPolicy::Never
    );
    assert_eq!(
        MemoryMappedFile::open_or_create(&path, 1)
            .unwrap()
            .flush_policy(),
        FlushPolicy::Never
    );
}

#[test]
fn unicode_and_space_paths_work() {
    for name in [
        "with spaces.bin",
        "unicod\u{e9} \u{65e5}\u{672c}\u{8a9e} \u{3b1}\u{3b2}.bin",
        " leading-space.bin",
        "dots...bin",
        "UPPER.lower.BIN",
    ] {
        let path = tmp_path(name);
        let m = MemoryMappedFile::create_rw(&path, 32).unwrap();
        m.update_region(0, name.as_bytes().get(..16).unwrap_or(name.as_bytes()))
            .unwrap();
        m.flush().unwrap();
        assert_eq!(m.path(), &*path);
        drop(m);
        let ro = MemoryMappedFile::open_ro(&path).unwrap();
        assert_eq!(ro.path(), &*path);
        let want = name.as_bytes().get(..16).unwrap_or(name.as_bytes());
        assert_eq!(ro.as_slice(0, want.len() as u64).unwrap(), want);
        assert!(path.exists(), "{name}");
    }
}

#[test]
fn debug_output_names_path_mode_and_length() {
    let path = tmp_path("dbg.bin");
    let m = MemoryMappedFile::create_rw(&path, 4242).unwrap();
    let s = format!("{m:?}");
    assert!(s.starts_with("MemoryMappedFile"), "{s}");
    assert!(s.contains("ReadWrite"), "{s}");
    assert!(s.contains("4242"), "{s}");
    assert!(s.contains("dbg.bin"), "{s}");
    #[cfg(feature = "hugepages")]
    assert!(s.contains("huge_pages"), "{s}");
    m.resize(17).unwrap();
    assert!(format!("{m:?}").contains("17"));
    drop(m);
    let ro = MemoryMappedFile::open_ro(&path).unwrap();
    assert!(format!("{ro:?}").contains("ReadOnly"));
}

#[test]
fn unmap_returns_the_file_only_for_the_last_handle() {
    let path = tmp_path("unmap.bin");
    let m = MemoryMappedFile::create_rw(&path, 8192).unwrap();
    m.update_region(0, b"mapped").unwrap();
    m.flush().unwrap();

    let clone = m.clone();
    let m = match m.unmap() {
        Ok(_) => panic!("unmap succeeded with a clone alive"),
        Err(back) => back,
    };
    // The returned handle is the same, fully usable mapping.
    assert_eq!(m.as_slice(0, 6).unwrap(), b"mapped");
    clone.update_region(0, b"MAPPED").unwrap();
    assert_eq!(m.as_slice(0, 6).unwrap(), b"MAPPED");
    drop(clone);

    let mut file = m.unmap().expect("last handle unmaps");
    assert_eq!(file.metadata().unwrap().len(), 8192);
    // The view is gone: shrinking the file now works on every
    // platform (Windows refuses to truncate a mapped file).
    file.set_len(10).unwrap();
    std::io::Seek::seek(&mut file, std::io::SeekFrom::End(0)).unwrap();
    file.write_all(b"!").unwrap();
    drop(file);
    let bytes = read_file(&path);
    assert_eq!(bytes.len(), 11);
    assert_eq!(&bytes[..6], b"MAPPED");
    assert_eq!(bytes[10], b'!');
}

#[test]
fn unmap_works_for_every_mode_and_with_a_background_flusher() {
    let path = tmp_path("unmap_modes.bin");
    let m = MemoryMappedFile::builder(&path)
        .size(4096)
        .flush_policy(FlushPolicy::EveryMillis(1))
        .create()
        .unwrap();
    m.update_region(0, b"x").unwrap();
    let start = std::time::Instant::now();
    let f = m.unmap().expect("unmap with flusher");
    assert!(start.elapsed() < std::time::Duration::from_secs(10));
    drop(f);

    let ro = MemoryMappedFile::open_ro(&path).unwrap();
    let f = ro.unmap().expect("unmap RO");
    assert_eq!(f.metadata().unwrap().len(), 4096);
    #[cfg(feature = "cow")]
    {
        let cow = MemoryMappedFile::open_cow(&path).unwrap();
        cow.unmap().expect("unmap COW");
    }
}

#[test]
fn os_handle_refers_to_the_mapped_file() {
    let path = tmp_path("handle.bin");
    let m = MemoryMappedFile::create_rw(&path, 777).unwrap();
    #[cfg(unix)]
    let file = {
        use std::os::fd::{AsFd, AsRawFd};
        assert!(m.as_raw_fd() >= 0);
        fs::File::from(m.as_fd().try_clone_to_owned().unwrap())
    };
    #[cfg(windows)]
    let file = {
        use std::os::windows::io::{AsHandle, AsRawHandle};
        assert!(!m.as_raw_handle().is_null());
        fs::File::from(m.as_handle().try_clone_to_owned().unwrap())
    };
    assert_eq!(file.metadata().unwrap().len(), 777);
}

#[test]
fn clones_share_one_mapping() {
    let path = tmp_path("clones.bin");
    let a = MemoryMappedFile::create_rw(&path, 4096).unwrap();
    let b = a.clone();
    a.update_region(10, b"shared").unwrap();
    assert_eq!(b.as_slice(10, 6).unwrap(), b"shared");
    b.resize(8192).unwrap();
    assert_eq!(a.len(), 8192);
    assert_eq!(a.pending_bytes(), b.pending_bytes());
    a.flush().unwrap();
    assert_eq!(b.pending_bytes(), 0);
}

#[test]
fn two_independent_mappings_of_one_file_see_each_others_writes() {
    let path = tmp_path("two.bin");
    let w = MemoryMappedFile::create_rw(&path, 4096).unwrap();
    let r = MemoryMappedFile::open_ro(&path).unwrap();
    let w2 = MemoryMappedFile::open_rw(&path).unwrap();
    w.update_region(0, b"first").unwrap();
    assert_eq!(r.as_slice(0, 5).unwrap(), b"first");
    assert_eq!(w2.as_slice(0, 5).unwrap(), b"first");
    w2.update_region(100, b"second").unwrap();
    assert_eq!(w.as_slice(100, 6).unwrap(), b"second");
    // std::fs readers see the page cache too, before any flush.
    assert_eq!(&read_file(&path)[100..106], b"second");
}
