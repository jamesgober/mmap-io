//! Everything else on `MemoryMappedFile`: slice wrapper traits, raw
//! pointers, touch/advise/lock behavior beyond range checks, `bytes`
//! conversions, huge-page introspection, and watch error paths.

use mmap_io::{MappedSlice, MemoryMappedFile, MmapIoError, MmapMode, TouchHint};

use crate::common::{page, pattern, tmp_path, TmpPath};

fn rw(size: u64) -> (TmpPath, MemoryMappedFile, Vec<u8>) {
    let path = tmp_path("misc.bin");
    let m = MemoryMappedFile::create_rw(&path, size).unwrap();
    let data = pattern(size as usize, 11);
    m.update_region(0, &data).unwrap();
    (path, m, data)
}

// The `s == &want` comparisons exercise the `PartialEq<&[u8; N]>` and
// `PartialEq<&[u8]>` impls on purpose.
#[allow(clippy::op_ref)]
#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
#[test]
fn mapped_slice_trait_impls_agree_with_the_bytes() {
    let (path, m, data) = rw(64);
    m.flush().unwrap();
    let ro = MemoryMappedFile::open_ro(&path).unwrap();
    for map in [&m, &ro] {
        let s: MappedSlice<'_> = map.as_slice(4, 4).unwrap();
        let want: [u8; 4] = data[4..8].try_into().unwrap();
        assert!(s == want);
        assert!(s == &want);
        assert!(s == want[..]);
        assert!(s == &want[..]);
        assert_eq!(s, map.as_slice(4, 4).unwrap());
        assert_ne!(s, map.as_slice(5, 4).unwrap());
        assert_eq!(s.as_ref(), &want[..]);
        assert_eq!(s.as_slice(), &want[..]);
        assert_eq!(s.len(), 4);
        assert!(!s.is_empty());
        assert_eq!(format!("{s:?}"), format!("{:?}", &want[..]));
        assert_eq!(s[3], want[3]);
        assert_eq!(s.iter().copied().collect::<Vec<_>>(), want);
        let empty = map.as_slice(64, 0).unwrap();
        assert!(empty.is_empty());
        assert!(empty == [0u8; 0]);
        assert_eq!(format!("{empty:?}"), "[]");
    }
}

#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
#[test]
fn mapped_slice_is_shareable_across_threads() {
    let (_p, m, data) = rw(4096);
    let s = m.as_slice(0, 4096).unwrap();
    let sum = std::thread::scope(|sc| {
        let a = sc.spawn(|| s.iter().map(|&b| u64::from(b)).sum::<u64>());
        let b = sc.spawn(|| s.len());
        assert_eq!(b.join().unwrap(), 4096);
        a.join().unwrap()
    });
    assert_eq!(sum, data.iter().map(|&b| u64::from(b)).sum::<u64>());
    // Moved into another thread and dropped there.
    let moved = m.as_slice(10, 10).unwrap();
    std::thread::scope(|sc| {
        sc.spawn(move || assert_eq!(moved.len(), 10));
    });
    drop(s);
    m.update_region(0, b"free again").unwrap();
}

#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
#[test]
fn mapped_slice_mut_derefs_both_ways() {
    let (_p, m, _) = rw(100);
    let mut s = m.as_slice_mut(10, 10).unwrap();
    assert_eq!(s.len(), 10);
    assert!(!s.is_empty());
    s.as_mut().fill(1);
    s[0] = 2;
    (*s)[9] = 3;
    assert_eq!(&s[..], &[2, 1, 1, 1, 1, 1, 1, 1, 1, 3]);
    drop(s);
    assert_eq!(m.as_slice(10, 10).unwrap(), &[2, 1, 1, 1, 1, 1, 1, 1, 1, 3]);
    let mut z = m.as_slice_mut(1000, 0).unwrap();
    assert!(z.is_empty());
    assert!(z.as_mut().is_empty());
}

#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
#[test]
fn raw_pointers_match_the_mapping() {
    let (path, m, data) = rw(page() + 3);
    // SAFETY: the pointers are used only within `len()`, no slice is
    // alive at the same time, and no resize happens while they are used.
    unsafe {
        let p = m.as_ptr();
        assert_eq!(p.read(), data[0]);
        assert_eq!(p.add(page() as usize + 2).read(), data[page() as usize + 2]);
        let w = m.as_mut_ptr().unwrap();
        assert_eq!(w.cast_const(), p);
        w.add(1).write(0x5A);
    }
    assert_eq!(m.as_slice(1, 1).unwrap(), &[0x5A]);
    m.flush().unwrap();
    let ro = MemoryMappedFile::open_ro(&path).unwrap();
    // SAFETY: read within len() of an immutable mapping.
    unsafe {
        assert_eq!(ro.as_ptr().add(1).read(), 0x5A);
        assert!(matches!(ro.as_mut_ptr(), Err(MmapIoError::InvalidMode(_))));
    }
}

#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
#[test]
fn touch_hints_and_touch_calls_leave_contents_alone() {
    let path = tmp_path("touch.bin");
    std::fs::write(&path, pattern(5 * page() as usize + 1, 2)).unwrap();
    for hint in [TouchHint::Never, TouchHint::Eager, TouchHint::Lazy] {
        let m = MemoryMappedFile::builder(&path)
            .mode(MmapMode::ReadWrite)
            .touch_hint(hint)
            .open()
            .unwrap();
        m.touch_pages().unwrap();
        m.touch_pages_range(1, 1).unwrap();
        m.touch_pages_range(page() - 1, 2).unwrap();
        m.touch_pages_range(0, m.len()).unwrap();
        assert_eq!(
            m.as_slice(0, m.len()).unwrap(),
            &pattern(5 * page() as usize + 1, 2)[..]
        );
        assert_eq!(m.pending_bytes(), 0, "touching is not writing");
    }
}

#[cfg(feature = "advise")]
#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
#[test]
fn every_advice_keeps_the_data_including_dont_need_on_dirty_pages() {
    use mmap_io::MmapAdvice;
    let (path, m, data) = rw(4 * page() + 5);
    // Dirty, unflushed pages: DONTNEED on a shared file mapping drops
    // the page-table entries but the data lives on in the page cache.
    for advice in [
        MmapAdvice::Normal,
        MmapAdvice::Random,
        MmapAdvice::Sequential,
        MmapAdvice::WillNeed,
        MmapAdvice::DontNeed,
    ] {
        m.advise(0, m.len(), advice).unwrap();
        m.advise(1, 1, advice).unwrap();
        m.advise(m.len() - 1, 1, advice).unwrap();
        m.advise(page() + 1, page(), advice).unwrap();
        assert_eq!(m.as_slice(0, m.len()).unwrap(), &data[..], "{advice:?}");
    }
    m.flush().unwrap();
    let ro = MemoryMappedFile::open_ro(&path).unwrap();
    for advice in [MmapAdvice::DontNeed, MmapAdvice::WillNeed] {
        ro.advise(0, ro.len(), advice).unwrap();
        assert_eq!(ro.as_slice(0, ro.len()).unwrap(), &data[..]);
    }
}

#[cfg(feature = "locking")]
#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
#[test]
fn unlocking_a_range_that_was_never_locked_succeeds() {
    let (_p, m, _) = rw(3 * page());
    m.unlock(0, page()).unwrap();
    m.unlock_all().unwrap();
    m.unlock(1, 1).unwrap();
}

#[cfg(feature = "locking")]
#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
#[test]
fn lock_and_unlock_round_trip_when_permitted() {
    let (_p, m, data) = rw(2 * page());
    match m.lock_all() {
        Ok(()) => {
            // Locked pages stay readable and writable.
            m.update_region(0, b"locked").unwrap();
            assert_eq!(m.as_slice(0, 6).unwrap(), b"locked");
            m.unlock_all().unwrap();
            // Locking twice and unlocking once more are fine.
            if m.lock(0, 1).is_ok() {
                m.lock(0, 1).unwrap();
                m.unlock(0, 1).unwrap();
                m.unlock(0, 1).unwrap();
            }
        }
        Err(MmapIoError::LockFailed(msg)) => assert!(!msg.is_empty()),
        Err(e) => panic!("lock_all: {e}"),
    }
    assert_eq!(m.as_slice(6, 10).unwrap(), &data[6..16]);
}

#[cfg(feature = "bytes")]
#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
#[test]
fn bytes_conversions_copy_the_slice() {
    let (_p, m, data) = rw(100);
    let b = m.read_bytes(10, 20).unwrap();
    assert_eq!(&b[..], &data[10..30]);
    assert!(m.read_bytes(1000, 0).unwrap().is_empty());
    assert!(matches!(
        m.read_bytes(90, 11),
        Err(MmapIoError::OutOfBounds {
            offset: 90,
            len: 11,
            total: 100
        })
    ));
    let s = m.as_slice(0, 5).unwrap();
    let by_ref = bytes::Bytes::from(&s);
    let by_val = bytes::Bytes::from(s);
    assert_eq!(by_ref, by_val);
    assert_eq!(&by_val[..], &data[..5]);
    // The copies are independent of the mapping.
    m.update_region(0, &[0; 5]).unwrap();
    assert_eq!(&by_val[..], &data[..5]);
    drop(m);
    assert_eq!(by_ref.len(), 5);
}

#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
#[test]
fn hugepage_status_is_known_only_on_linux() {
    let (path, m, _) = rw(4096);
    let ro = MemoryMappedFile::open_ro(&path).unwrap();
    for map in [&m, &ro] {
        let s = map.is_hugepage_backed();
        if cfg!(target_os = "linux") {
            assert!(s.is_some(), "Linux should find the mapping in smaps");
        } else {
            assert_eq!(s, None);
        }
    }
    #[cfg(feature = "cow")]
    {
        let cow = MemoryMappedFile::open_cow(&path).unwrap();
        assert_eq!(
            cow.is_hugepage_backed().is_some(),
            cfg!(target_os = "linux")
        );
    }
    // Still answers after a remap.
    m.resize(3 * 4096).unwrap();
    assert_eq!(m.is_hugepage_backed().is_some(), cfg!(target_os = "linux"));
}

#[cfg(feature = "hugepages")]
#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
#[test]
fn hugepage_requests_never_fail_the_mapping() {
    let path = tmp_path("huge.bin");
    for enable in [false, true] {
        let m = MemoryMappedFile::builder(&path)
            .mode(MmapMode::ReadWrite)
            .size(4 << 20)
            .huge_pages(enable)
            .create()
            .unwrap();
        m.update_region((4 << 20) - 1, b"x").unwrap();
        m.resize(2 << 20).unwrap();
        m.resize(6 << 20).unwrap();
        assert_eq!(m.len(), 6 << 20);
        drop(m);
        let m = MemoryMappedFile::builder(&path)
            .mode(MmapMode::ReadWrite)
            .huge_pages(enable)
            .open_or_create()
            .unwrap();
        assert_eq!(m.len(), 6 << 20);
    }
}

#[cfg(feature = "watch")]
#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
#[test]
fn watching_a_file_that_no_longer_exists_fails_cleanly() {
    let path = tmp_path("gone.bin");
    std::fs::write(&path, b"").unwrap();
    // An empty file has no OS mapping, so it can be deleted on every
    // platform while the (zero-length) mapping is alive.
    let m = MemoryMappedFile::open_ro(&path).unwrap();
    std::fs::remove_file(&path).unwrap();
    match m.watch(|_| {}) {
        Err(MmapIoError::WatchFailed(msg)) => assert!(!msg.is_empty()),
        Ok(_) => panic!("watching a deleted path succeeded"),
        Err(e) => panic!("expected WatchFailed, got {e}"),
    }
}
