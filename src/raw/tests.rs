//! Unit tests for the raw mapping layer. Tests that make mapping
//! syscalls are ignored under Miri; the pure arithmetic is covered by
//! `range::tests`, which runs under Miri.

use std::io::Write;

use super::*;

fn file_with(bytes: &[u8]) -> File {
    let mut f = tempfile::tempfile().expect("tempfile");
    f.write_all(bytes).expect("write");
    f
}

fn pattern(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i % 251) as u8).collect()
}

#[test]
fn types_are_send_sync_and_unpin() {
    fn check<T: Send + Sync + Unpin>() {}
    check::<RawMmap>();
    check::<RawMmapMut>();
    check::<RawMmapOptions>();
}

#[test]
fn options_builder_defaults() {
    let o = RawMmapOptions::new();
    assert_eq!(o.offset, 0);
    assert_eq!(o.len, None);
    let mut o = RawMmapOptions::default();
    o.offset(7).len(9);
    assert_eq!(o.offset, 7);
    assert_eq!(o.len, Some(9));
    let c = o.clone();
    assert_eq!(c.offset, 7);
}

#[test]
#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
fn empty_file_maps_without_os_mapping() {
    let f = file_with(b"");
    // SAFETY: private temporary file.
    let ro = unsafe { RawMmap::map(&f) }.expect("map empty");
    assert!(ro.is_empty());
    assert_eq!(&ro[..], b"");
    assert!(!ro.as_ptr().is_null());
    let gran = offset_granularity().expect("granularity");
    assert_eq!(ro.as_ptr() as usize % gran, 0);
    // SAFETY: private temporary file.
    let rw = unsafe { RawMmapMut::map_mut(&f) }.expect("map_mut empty");
    rw.flush().expect("flush empty");
    rw.flush_async().expect("flush_async empty");
    rw.flush_range(0, 0).expect("flush_range(0,0) empty");
    assert!(rw.flush_range(0, 1).is_err());
    assert!(rw.flush_range(1, 0).is_err());
    // SAFETY: private temporary file.
    let cow = unsafe { RawMmapOptions::new().map_copy(&f) }.expect("map_copy empty");
    assert!(cow.is_empty());
}

#[test]
#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
fn unaligned_offsets_expose_exact_window() {
    let data = pattern(3 * 65_536 + 123);
    let f = file_with(&data);
    for &off in &[0u64, 1, 4095, 4096, 4097, 65_535, 65_536, 65_537, 196_607] {
        // SAFETY: private temporary file.
        let m = unsafe { RawMmapOptions::new().offset(off).map(&f) }.expect("map");
        assert_eq!(&m[..], &data[off as usize..], "offset {off}");
    }
}

#[test]
#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
fn rejects_out_of_file_windows() {
    let f = file_with(&pattern(100));
    let cases: &[(u64, Option<usize>)] = &[
        (101, None),
        (101, Some(0)),
        (0, Some(101)),
        (50, Some(51)),
        (100, Some(1)),
        (u64::MAX, None),
        (u64::MAX, Some(1)),
        (1, Some(usize::MAX)),
        (u64::MAX - 1, Some(usize::MAX)),
    ];
    for &(off, len) in cases {
        let mut o = RawMmapOptions::new();
        o.offset(off);
        if let Some(l) = len {
            o.len(l);
        }
        // SAFETY: private temporary file.
        let err = unsafe { o.map(&f) }.expect_err("must fail");
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput, "({off}, {len:?})");
        // SAFETY: private temporary file.
        assert!(unsafe { o.map_copy(&f) }.is_err());
    }
}

#[test]
#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
fn offset_at_end_is_empty() {
    let f = file_with(&pattern(4097));
    // SAFETY: private temporary file.
    let m = unsafe { RawMmapOptions::new().offset(4097).map(&f) }.expect("map");
    assert!(m.is_empty());
    // SAFETY: private temporary file.
    let m = unsafe { RawMmapOptions::new().offset(17).len(0).map(&f) }.expect("map");
    assert!(m.is_empty());
}

#[test]
#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
fn anon_is_zeroed_and_writable() {
    let mut a = RawMmapMut::map_anon(100_000).expect("anon");
    assert_eq!(a.len(), 100_000);
    assert!(a.iter().all(|&b| b == 0));
    a[99_999] = 3;
    assert_eq!(a[99_999], 3);
    a.flush().expect("anon flush is a no-op");
    a.flush_range(10, 10).expect("anon flush_range");
    assert!(a.flush_range(99_999, 2).is_err());
    let z = RawMmapMut::map_anon(0).expect("anon 0");
    assert!(z.is_empty());
    let o = RawMmapOptions::new()
        .offset(12345)
        .map_anon()
        .expect("offset ignored");
    assert!(o.is_empty());
    assert!(RawMmapMut::map_anon(usize::MAX).is_err());
}

#[test]
#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
fn debug_does_not_dump_contents() {
    let f = file_with(b"secret-bytes");
    // SAFETY: private temporary file.
    let m = unsafe { RawMmap::map(&f) }.expect("map");
    let s = format!("{m:?}");
    assert!(s.starts_with("RawMmap {"), "{s}");
    assert!(s.contains("len: 12"), "{s}");
    assert!(!s.contains("secret"), "{s}");
    let a = RawMmapMut::map_anon(4).expect("anon");
    let s = format!("{a:?}");
    assert!(s.starts_with("RawMmapMut {"), "{s}");
}

#[test]
#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
fn as_ref_as_mut_and_pointers_agree() {
    let mut a = RawMmapMut::map_anon(16).expect("anon");
    let p = a.as_mut_ptr();
    assert_eq!(p.cast_const(), a.as_ptr());
    assert_eq!(a.as_ref().as_ptr(), a.as_ptr());
    a.as_mut()[3] = 9;
    assert_eq!(a[3], 9);
    let f = file_with(b"abc");
    // SAFETY: private temporary file.
    let m = unsafe { RawMmap::map(&f) }.expect("map");
    assert_eq!(m.as_ref(), b"abc");
    assert_eq!(m.as_ptr(), m[..].as_ptr());
}

#[test]
#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
fn write_only_file_cannot_be_mapped_read_write() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("wo.bin");
    std::fs::write(&path, b"0123").expect("write");
    let ro = File::open(&path).expect("open ro");
    // SAFETY: private temporary file.
    assert!(unsafe { RawMmapMut::map_mut(&ro) }.is_err());
    // A read-only handle is enough for copy-on-write.
    // SAFETY: private temporary file.
    let mut cow = unsafe { RawMmapOptions::new().map_copy(&ro) }.expect("cow on ro handle");
    cow[0] = b'X';
    assert_eq!(&cow[..], b"X123");
    drop(cow);
    assert_eq!(std::fs::read(&path).expect("read"), b"0123");
}

#[test]
fn options_flags_builder() {
    let mut o = RawMmapOptions::new();
    assert_eq!(o.flags, MapFlags::default());
    o.populate().huge();
    assert!(o.flags.populate && o.flags.huge);
    let c = o.clone();
    assert_eq!(c.flags, o.flags);
}

#[test]
#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
fn populate_maps_file_and_anon() {
    let data = pattern(3 * 4096 + 7);
    let f = file_with(&data);
    // SAFETY: private temporary file.
    let m = unsafe { RawMmapOptions::new().offset(5).populate().map(&f) }.expect("map");
    assert_eq!(&m[..], &data[5..]);
    let a = RawMmapOptions::new()
        .len(64 * 1024)
        .populate()
        .map_anon()
        .expect("anon");
    assert!(a.iter().all(|&b| b == 0));
    let z = RawMmapOptions::new().populate().map_anon().expect("empty");
    assert!(z.is_empty());
}

#[test]
#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
fn huge_anon_maps_or_reports_os_error() {
    // Without reserved huge pages Linux refuses MAP_HUGETLB; elsewhere
    // the flag is ignored. Either way nothing panics, and a successful
    // mapping is usable and reports the requested length.
    match RawMmapOptions::new().len(4096).huge().map_anon() {
        Ok(mut m) => {
            assert_eq!(m.len(), 4096);
            m[4095] = 1;
            assert_eq!(m[4095], 1);
        }
        Err(e) => {
            // Only Linux / Android honour the flag and can refuse it.
            let honours_huge = cfg!(any(target_os = "linux", target_os = "android"));
            assert!(honours_huge, "unexpected error: {e}");
        }
    }
    // Rounding up to the huge page size must not overflow.
    assert!(RawMmapOptions::new()
        .len(usize::MAX - 10)
        .huge()
        .map_anon()
        .is_err());
}

#[test]
#[cfg(feature = "advise")]
#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
fn advise_validates_ranges_and_refuses_private_dontneed() {
    use crate::advise::MmapAdvice;
    let all = [
        MmapAdvice::Normal,
        MmapAdvice::Random,
        MmapAdvice::Sequential,
        MmapAdvice::WillNeed,
    ];
    let data = pattern(3 * 4096 + 11);
    let f = file_with(&data);
    // SAFETY: private temporary file.
    let ro = unsafe { RawMmapOptions::new().offset(3).map(&f) }.expect("map");
    for &a in &all {
        ro.advise(a).expect("advise whole");
        ro.advise_range(a, 1, 10).expect("unaligned start");
        ro.advise_range(a, ro.len() - 1, 1).expect("last byte");
        ro.advise_range(a, ro.len(), 0).expect("empty at end");
    }
    // Shared read-only file mapping: DontNeed keeps the bytes.
    ro.advise(MmapAdvice::DontNeed).expect("dontneed shared");
    assert_eq!(&ro[..], &data[3..]);
    for &(off, len) in &[
        (ro.len() + 1, 0),
        (ro.len(), 1),
        (0, ro.len() + 1),
        (usize::MAX, 1),
        (1, usize::MAX),
    ] {
        let e = ro
            .advise_range(MmapAdvice::Normal, off, len)
            .expect_err("out of range");
        assert_eq!(e.kind(), io::ErrorKind::InvalidInput, "({off}, {len})");
    }
    // Private mappings refuse DontNeed, even for an empty range.
    let mut anon = RawMmapMut::map_anon(8192).expect("anon");
    anon[0] = 9;
    let e = anon
        .advise(MmapAdvice::DontNeed)
        .expect_err("anon dontneed");
    assert_eq!(e.kind(), io::ErrorKind::InvalidInput);
    assert!(anon.advise_range(MmapAdvice::DontNeed, 0, 0).is_err());
    assert_eq!(anon[0], 9, "private data must survive");
    anon.advise(MmapAdvice::WillNeed).expect("anon willneed");
    // SAFETY: private temporary file.
    let mut cow = unsafe { RawMmapOptions::new().map_copy(&f) }.expect("cow");
    cow[1] = 0xEE;
    assert!(cow.advise(MmapAdvice::DontNeed).is_err());
    assert_eq!(cow[1], 0xEE);
    // Made read-only, a private mapping is still private.
    let cow_ro = cow.make_read_only().expect("cow ro");
    assert!(cow_ro.advise(MmapAdvice::DontNeed).is_err());
    // Empty mappings accept the empty range.
    let empty = RawMmapMut::map_anon(0).expect("empty");
    empty.advise(MmapAdvice::Normal).expect("empty advise");
}

#[test]
#[cfg(feature = "locking")]
#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
fn lock_unlock_round_trip() {
    let a = RawMmapMut::map_anon(4096).expect("anon");
    // Locking may need privileges; unlocking must always succeed.
    if a.lock().is_ok() {
        a.unlock().expect("unlock after lock");
    }
    a.unlock().expect("unlock without lock");
    let empty = RawMmapMut::map_anon(0).expect("empty");
    empty.lock().expect("empty lock");
    empty.unlock().expect("empty unlock");
    let f = file_with(&pattern(100));
    // SAFETY: private temporary file.
    let ro = unsafe { RawMmap::map(&f) }.expect("map");
    if ro.lock().is_ok() {
        ro.unlock().expect("unlock ro");
    }
}

#[test]
#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
fn protection_round_trips_keep_contents_and_access() {
    // Anonymous.
    let mut a = RawMmapMut::map_anon(3 * 4096).expect("anon");
    a[0] = 1;
    a[3 * 4096 - 1] = 2;
    let ro = a.make_read_only().expect("anon ro");
    assert_eq!((ro[0], ro[3 * 4096 - 1]), (1, 2));
    let mut a = ro.make_mut().expect("anon rw");
    a[1] = 3;
    assert_eq!(&a[..2], &[1, 3]);

    // Copy-on-write: writes stay private across both transitions.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("cow.bin");
    std::fs::write(&path, pattern(5000)).expect("write");
    let file = File::open(&path).expect("open ro");
    // SAFETY: private temporary file.
    let mut cow = unsafe { RawMmapOptions::new().offset(10).map_copy(&file) }.expect("cow");
    cow[0] = 0xAA;
    let cow = cow.make_read_only().expect("cow ro");
    assert_eq!(cow[0], 0xAA);
    let mut cow = cow.make_mut().expect("cow rw");
    cow[1] = 0xBB;
    assert_eq!(&cow[..2], &[0xAA, 0xBB]);
    cow.flush().expect("cow flush no-op");
    drop(cow);
    assert_eq!(std::fs::read(&path).expect("read"), pattern(5000));

    // Shared writable file: make_mut restores write-back.
    let path = dir.path().join("shared.bin");
    std::fs::write(&path, vec![0u8; 8192]).expect("write");
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
        .expect("open rw");
    // SAFETY: private temporary file.
    let mut rw = unsafe { RawMmapOptions::new().offset(4096).map_mut(&file) }.expect("rw");
    rw[0] = 7;
    let ro = rw.make_read_only().expect("rw ro");
    assert_eq!(ro[0], 7);
    let mut rw = ro.make_mut().expect("rw again");
    rw[1] = 8;
    rw.flush().expect("flush");
    drop(rw);
    let on_disk = std::fs::read(&path).expect("read");
    assert_eq!(&on_disk[4096..4098], &[7, 8]);

    // Empty mappings convert without a syscall.
    let e = RawMmapMut::map_anon(0).expect("empty");
    let e = e.make_read_only().expect("empty ro");
    let e = e.make_mut().expect("empty rw");
    assert!(e.is_empty());
}

#[test]
#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
fn make_mut_of_read_only_file_mapping() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("ro.bin");
    std::fs::write(&path, b"0123456789").expect("write");
    let rw_file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
        .expect("open rw");
    // SAFETY: private temporary file.
    let ro = unsafe { RawMmap::map(&rw_file) }.expect("map");
    let result = ro.make_mut();
    if cfg!(windows) {
        let e = result.expect_err("PAGE_READONLY views cannot become writable");
        assert_eq!(e.kind(), io::ErrorKind::Unsupported);
    } else {
        let mut rw = result.expect("mprotect on a read-write fd");
        rw[0] = b'X';
        rw.flush().expect("flush now writes back");
        drop(rw);
        assert_eq!(std::fs::read(&path).expect("read"), b"X123456789");
    }
    // A file opened read-only can never be made writable.
    let ro_file = File::open(&path).expect("open ro");
    // SAFETY: private temporary file.
    let ro = unsafe { RawMmap::map(&ro_file) }.expect("map");
    assert!(ro.make_mut().is_err());
    // An empty read-only mapping converts trivially everywhere.
    let empty = file_with(b"");
    // SAFETY: private temporary file.
    let ro = unsafe { RawMmap::map(&empty) }.expect("map empty");
    assert!(ro.make_mut().expect("empty make_mut").is_empty());
}
