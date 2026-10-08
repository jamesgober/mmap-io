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
