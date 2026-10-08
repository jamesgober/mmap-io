//! `MemoryMappedFileBuilder::create_new` (1.1.0): exclusive create.

use std::io::ErrorKind;
use std::sync::{Arc, Barrier};
use std::thread;

use mmap_io::flush::FlushPolicy;
use mmap_io::{MemoryMappedFile, MmapIoError, MmapMode};

fn already_exists(r: &Result<MemoryMappedFile, MmapIoError>) -> bool {
    matches!(r, Err(MmapIoError::Io(e)) if e.kind() == ErrorKind::AlreadyExists)
}

#[test]
fn creates_a_fresh_read_write_mapping() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("new.bin");
    let m = MemoryMappedFile::builder(&path)
        .size(8192)
        .create_new()
        .expect("create_new");
    assert_eq!(m.len(), 8192);
    assert_eq!(m.mode(), MmapMode::ReadWrite);
    m.update_region(0, b"fresh").expect("write");
    m.flush().expect("flush");
    drop(m);
    let on_disk = std::fs::read(&path).expect("read");
    assert_eq!(on_disk.len(), 8192);
    assert_eq!(&on_disk[..5], b"fresh");
}

#[test]
fn existing_file_is_refused_and_untouched() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("exists.bin");
    std::fs::write(&path, b"precious data").expect("write");
    let r = MemoryMappedFile::builder(&path).size(4096).create_new();
    assert!(already_exists(&r), "{r:?}");
    assert_eq!(std::fs::read(&path).expect("read"), b"precious data");
    // Even an empty existing file is refused (create() would size it).
    let empty = dir.path().join("empty.bin");
    std::fs::write(&empty, b"").expect("write");
    assert!(already_exists(
        &MemoryMappedFile::builder(&empty).size(10).create_new()
    ));
    assert_eq!(std::fs::metadata(&empty).expect("meta").len(), 0);
}

#[test]
fn invalid_size_or_mode_leaves_no_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("never.bin");
    assert!(matches!(
        MemoryMappedFile::builder(&path).create_new(),
        Err(MmapIoError::ResizeFailed(_))
    ));
    assert!(matches!(
        MemoryMappedFile::builder(&path).size(0).create_new(),
        Err(MmapIoError::ResizeFailed(_))
    ));
    assert!(matches!(
        MemoryMappedFile::builder(&path).size(u64::MAX).create_new(),
        Err(MmapIoError::ResizeFailed(_))
    ));
    for mode in [MmapMode::ReadOnly, MmapMode::CopyOnWrite] {
        assert!(matches!(
            MemoryMappedFile::builder(&path)
                .size(64)
                .mode(mode)
                .create_new(),
            Err(MmapIoError::InvalidMode(_))
        ));
    }
    assert!(!path.exists(), "no file may be left behind");
}

#[test]
fn missing_parent_directory_is_an_io_error() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("no_such_dir").join("x.bin");
    assert!(matches!(
        MemoryMappedFile::builder(&path).size(64).create_new(),
        Err(MmapIoError::Io(e)) if e.kind() == ErrorKind::NotFound
    ));
}

#[test]
fn builder_options_apply() {
    let dir = tempfile::tempdir().expect("tempdir");
    let m = MemoryMappedFile::builder(dir.path().join("opts.bin"))
        .size(4096)
        .flush_policy(FlushPolicy::Always)
        .touch_hint(mmap_io::TouchHint::Eager)
        .create_new()
        .expect("create_new");
    assert_eq!(m.flush_policy(), FlushPolicy::Always);
    m.update_region(0, b"x").expect("write");
    assert_eq!(m.pending_bytes(), 0, "Always policy flushed");
}

#[test]
fn exactly_one_racer_wins() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = Arc::new(dir.path().join("race.bin"));
    let n = 8;
    let barrier = Arc::new(Barrier::new(n));
    let handles: Vec<_> = (0..n)
        .map(|i| {
            let (path, barrier) = (Arc::clone(&path), Arc::clone(&barrier));
            thread::spawn(move || {
                barrier.wait();
                let r = MemoryMappedFile::builder(path.as_path())
                    .size(4096)
                    .create_new();
                match r {
                    Ok(m) => {
                        m.update_region(0, &[i as u8 + 1]).expect("write");
                        m.flush().expect("flush");
                        Some(i as u8 + 1)
                    }
                    Err(MmapIoError::Io(e)) if e.kind() == ErrorKind::AlreadyExists => None,
                    // Windows can report a sharing violation while the
                    // winner still holds the file open.
                    Err(MmapIoError::Io(e)) if e.kind() == ErrorKind::PermissionDenied => None,
                    Err(e) => panic!("unexpected error: {e}"),
                }
            })
        })
        .collect();
    let winners: Vec<u8> = handles
        .into_iter()
        .filter_map(|h| h.join().expect("join"))
        .collect();
    assert_eq!(winners.len(), 1, "winners: {winners:?}");
    let on_disk = std::fs::read(path.as_path()).expect("read");
    assert_eq!(on_disk.len(), 4096, "never truncated by a loser");
    assert_eq!(on_disk[0], winners[0]);
}
