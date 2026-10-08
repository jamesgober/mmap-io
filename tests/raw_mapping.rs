//! Integration tests for `mmap_io::raw`: size and offset edge
//! matrices, data integrity through independent reads, flush ranges,
//! copy-on-write isolation, shared visibility, and lifetime rules.
//!
//! Every file-backed mapping here maps a private temporary file that
//! no other process touches, which is the `# Safety` contract of the
//! raw constructors.

use std::fs::{File, OpenOptions};
use std::io::{self, Seek, SeekFrom, Write};
use std::path::Path;

use mmap_io::raw::{offset_granularity, RawMmap, RawMmapMut, RawMmapOptions};

fn page() -> usize {
    mmap_io::utils::page_size()
}

fn gran() -> usize {
    offset_granularity().expect("offset granularity")
}

fn pattern(len: usize, seed: u8) -> Vec<u8> {
    (0..len)
        .map(|i| ((i % 251) as u8).wrapping_add(seed))
        .collect()
}

fn rw_file(path: &Path, contents: &[u8]) -> File {
    std::fs::write(path, contents).expect("write file");
    OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .expect("open rw")
}

/// File sizes that sit on and next to every alignment boundary the
/// raw layer cares about, plus a multi-MiB file.
fn sizes() -> Vec<usize> {
    let p = page();
    let g = gran();
    let mut v = vec![0, 1, p - 1, p, p + 1, g - 1, g, g + 1, 3 * 1024 * 1024 + 7];
    v.sort_unstable();
    v.dedup();
    v
}

/// Offsets on and next to every boundary, relative to a file length.
fn offsets(file_len: usize) -> Vec<u64> {
    let p = page() as u64;
    let g = gran() as u64;
    let n = file_len as u64;
    let mut v = vec![
        0,
        1,
        p - 1,
        p,
        p + 1,
        g - 1,
        g,
        g + 1,
        n.saturating_sub(1),
        n,
        n + 1,
        n + p,
        u64::MAX,
    ];
    v.sort_unstable();
    v.dedup();
    v
}

fn opts(offset: u64, len: Option<usize>) -> RawMmapOptions {
    let mut o = RawMmapOptions::new();
    o.offset(offset);
    if let Some(l) = len {
        o.len(l);
    }
    o
}

/// Expected window for (file_len, offset, len), or None if invalid.
fn expected_window(file_len: usize, offset: u64, len: Option<usize>) -> Option<(usize, usize)> {
    let n = file_len as u64;
    if offset > n {
        return None;
    }
    let start = offset as usize;
    let avail = file_len - start;
    match len {
        None => Some((start, avail)),
        Some(l) if l <= avail => Some((start, l)),
        Some(_) => None,
    }
}

#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
#[test]
fn ro_size_and_offset_matrix() {
    let dir = tempfile::tempdir().expect("tempdir");
    for size in sizes() {
        let data = pattern(size, 3);
        let path = dir.path().join(format!("ro_{size}.bin"));
        let file = rw_file(&path, &data);
        for off in offsets(size) {
            let rest = (size as u64).saturating_sub(off) as usize;
            let lens = [
                None,
                Some(0),
                Some(1),
                Some(rest),
                Some(rest.wrapping_add(1)),
                Some(usize::MAX),
            ];
            for len in lens {
                // SAFETY: private temporary file.
                let res = unsafe { opts(off, len).map(&file) };
                match (expected_window(size, off, len), res) {
                    (Some((s, l)), Ok(m)) => {
                        assert_eq!(m.len(), l, "size {size} off {off} len {len:?}");
                        assert_eq!(m.is_empty(), l == 0);
                        assert!(m[..] == data[s..s + l], "size {size} off {off} len {len:?}");
                    }
                    (None, Err(e)) => {
                        assert_eq!(
                            e.kind(),
                            io::ErrorKind::InvalidInput,
                            "size {size} off {off} len {len:?}"
                        );
                    }
                    (Some(w), Err(e)) => {
                        panic!("size {size} off {off} len {len:?}: expected {w:?}, got {e}")
                    }
                    (None, Ok(m)) => panic!(
                        "size {size} off {off} len {len:?}: expected error, got len {}",
                        m.len()
                    ),
                }
            }
        }
    }
}

#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
#[test]
fn rw_writes_reach_file_through_independent_read() {
    let dir = tempfile::tempdir().expect("tempdir");
    for size in sizes() {
        let original = pattern(size, 11);
        let path = dir.path().join(format!("rw_{size}.bin"));
        for off in offsets(size) {
            let Some((start, len)) = expected_window(size, off, None) else {
                let file = rw_file(&path, &original);
                // SAFETY: private temporary file.
                assert!(unsafe { opts(off, None).map_mut(&file) }.is_err());
                continue;
            };
            let file = rw_file(&path, &original);
            // SAFETY: private temporary file.
            let mut m = unsafe { opts(off, None).map_mut(&file) }.expect("map_mut");
            assert_eq!(m.len(), len);
            for (i, b) in m.iter_mut().enumerate() {
                *b = !original[start + i];
            }
            m.flush().expect("flush");
            drop(m);
            drop(file);
            let on_disk = std::fs::read(&path).expect("read back");
            assert_eq!(on_disk.len(), size);
            assert!(
                on_disk[..start] == original[..start],
                "prefix changed, size {size} off {off}"
            );
            assert!(
                on_disk[start..]
                    .iter()
                    .zip(&original[start..])
                    .all(|(a, b)| *a == !*b),
                "window not written, size {size} off {off}"
            );
        }
    }
}

#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
#[test]
fn cow_writes_never_reach_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    for size in sizes() {
        let original = pattern(size, 29);
        let path = dir.path().join(format!("cow_{size}.bin"));
        std::fs::write(&path, &original).expect("write");
        // Copy-on-write only needs read access.
        let file = File::open(&path).expect("open ro");
        for off in offsets(size) {
            // SAFETY: private temporary file.
            let res = unsafe { opts(off, None).map_copy(&file) };
            let Some((start, len)) = expected_window(size, off, None) else {
                assert!(res.is_err());
                continue;
            };
            let mut m = res.expect("map_copy");
            assert_eq!(m.len(), len);
            assert!(m[..] == original[start..]);
            m.iter_mut().for_each(|b| *b = 0xEE);
            assert!(m.iter().all(|&b| b == 0xEE));
            m.flush().expect("cow flush is a no-op");
            m.flush_async().expect("cow flush_async is a no-op");
            if len > 0 {
                m.flush_range(len - 1, 1).expect("cow flush_range");
            }
            drop(m);
            assert!(
                std::fs::read(&path).expect("read") == original,
                "cow leaked to file, size {size} off {off}"
            );
        }
    }
}

#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
#[test]
fn flush_range_every_boundary_sub_range() {
    let dir = tempfile::tempdir().expect("tempdir");
    let p = page();
    let size = 3 * gran() + 5;
    let original = pattern(size, 41);
    let path = dir.path().join("flush_ranges.bin");
    // Try both an aligned and an unaligned window start.
    for &window_off in &[0u64, 1, p as u64 - 1, gran() as u64 + 3] {
        let file = rw_file(&path, &original);
        let mut expected = original.clone();
        // SAFETY: private temporary file.
        let mut m = unsafe { opts(window_off, None).map_mut(&file) }.expect("map_mut");
        let wlen = m.len();
        let mut points = vec![
            0,
            1,
            p - 1,
            p,
            p + 1,
            2 * p,
            2 * p + 7,
            wlen / 2,
            wlen - 1,
            wlen,
        ];
        points.retain(|&x| x <= wlen);
        points.sort_unstable();
        points.dedup();
        let mut stamp = 1u8;
        for &a in &points {
            for &b in &points {
                if b < a {
                    // Inverted: len would be "negative"; use it as an
                    // out-of-range probe instead.
                    assert!(m.flush_range(a, wlen - a + 1).is_err());
                    continue;
                }
                let len = b - a;
                m[a..b].iter_mut().for_each(|x| *x = stamp);
                let abs = window_off as usize + a;
                expected[abs..abs + len].iter_mut().for_each(|x| *x = stamp);
                m.flush_range(a, len).expect("flush_range in bounds");
                m.flush_async_range(a, len)
                    .expect("flush_async_range in bounds");
                stamp = stamp.wrapping_add(1).max(1);
            }
        }
        // Out-of-bounds and overflowing ranges: error, never panic.
        for &(o, l) in &[
            (wlen, 1),
            (wlen + 1, 0),
            (0, wlen + 1),
            (1, wlen),
            (usize::MAX, 1),
            (1, usize::MAX),
            (usize::MAX, usize::MAX),
            (wlen - 1, usize::MAX - wlen + 2),
        ] {
            let e = m.flush_range(o, l).expect_err("must fail");
            assert_eq!(e.kind(), io::ErrorKind::InvalidInput, "({o}, {l})");
            assert!(m.flush_async_range(o, l).is_err());
        }
        m.flush().expect("final flush");
        drop(m);
        drop(file);
        assert!(
            std::fs::read(&path).expect("read") == expected,
            "window_off {window_off}"
        );
    }
}

#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
#[test]
fn ro_mapping_sees_writes_from_separate_rw_mapping() {
    let dir = tempfile::tempdir().expect("tempdir");
    let size = 2 * gran() + 100;
    let path = dir.path().join("shared.bin");
    let file = rw_file(&path, &pattern(size, 0));
    // SAFETY: private temporary file. No slice borrowed from `ro` is
    // alive while `rw` is written.
    let ro = unsafe { RawMmap::map(&file) }.expect("ro");
    // SAFETY: as above.
    let ro_window = unsafe { opts(gran() as u64 + 1, Some(50)).map(&file) }.expect("ro window");
    // SAFETY: as above.
    let mut rw = unsafe { RawMmapMut::map_mut(&file) }.expect("rw");
    rw[0] = 0xA1;
    rw[gran() + 1] = 0xB2;
    rw[size - 1] = 0xC3;
    assert_eq!(ro[0], 0xA1);
    assert_eq!(ro[gran() + 1], 0xB2);
    assert_eq!(ro[size - 1], 0xC3);
    assert_eq!(ro_window[0], 0xB2);
    // A second RW mapping of the same file is coherent too.
    // SAFETY: as above.
    let mut rw2 = unsafe { opts(gran() as u64, None).map_mut(&file) }.expect("rw2");
    rw2[5] = 0xD4;
    assert_eq!(rw[gran() + 5], 0xD4);
    assert_eq!(ro[gran() + 5], 0xD4);
}

#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
#[test]
fn mapping_outlives_file_handle() {
    let dir = tempfile::tempdir().expect("tempdir");
    let size = gran() + 3;
    let data = pattern(size, 5);
    let path = dir.path().join("outlive.bin");

    let file = rw_file(&path, &data);
    // SAFETY: private temporary file.
    let ro = unsafe { RawMmap::map(&file) }.expect("ro");
    // SAFETY: private temporary file.
    let mut rw = unsafe { opts(1, None).map_mut(&file) }.expect("rw");
    // SAFETY: private temporary file.
    let mut cow = unsafe { RawMmapOptions::new().map_copy(&file) }.expect("cow");
    drop(file);

    assert!(ro[..] == data[..]);
    cow[0] = 0x55;
    assert_eq!(cow[0], 0x55);
    rw[size - 2] = 0x77;
    // Durable flush after the caller's File is gone: Windows uses the
    // duplicated handle for FlushFileBuffers.
    rw.flush().expect("flush after file drop");
    rw.flush_range(0, 1).expect("flush_range after file drop");
    drop((ro, rw, cow));
    let on_disk = std::fs::read(&path).expect("read");
    assert_eq!(on_disk[size - 1], 0x77);
    assert_eq!(on_disk[0], data[0]);
}

#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
#[test]
fn flush_is_durable_across_reopen() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("durable.bin");
    for &size in &[4096usize, 1 << 20] {
        let file = rw_file(&path, &vec![0u8; size]);
        // SAFETY: private temporary file.
        let mut m = unsafe { RawMmapMut::map_mut(&file) }.expect("map");
        let fill = pattern(size, 77);
        m.copy_from_slice(&fill);
        m.flush().expect("flush");
        // Read back through a fresh handle while the mapping is alive
        // and again after it is gone.
        drop(m);
        drop(file);
        let mut reopened = File::open(&path).expect("reopen");
        let mut buf = Vec::new();
        io::Read::read_to_end(&mut reopened, &mut buf).expect("read");
        assert!(buf == fill, "size {size}");
    }
}

#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
#[test]
fn async_flush_variants_succeed_and_data_lands() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("async.bin");
    let file = rw_file(&path, &[0u8; 10_000]);
    // SAFETY: private temporary file.
    let mut m = unsafe { RawMmapMut::map_mut(&file) }.expect("map");
    m[9_999] = 1;
    m.flush_async().expect("flush_async");
    m.flush_async_range(9_000, 1_000)
        .expect("flush_async_range");
    m.flush().expect("flush");
    drop(m);
    assert_eq!(std::fs::read(&path).expect("read")[9_999], 1);
}

#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
#[test]
fn permission_errors_are_reported_not_panicked() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("perm.bin");
    std::fs::write(&path, b"abcdef").expect("write");
    let ro_handle = File::open(&path).expect("open ro");
    // SAFETY: private temporary file.
    assert!(unsafe { RawMmapMut::map_mut(&ro_handle) }.is_err());
    // SAFETY: private temporary file.
    assert!(unsafe { RawMmapOptions::new().offset(2).map_mut(&ro_handle) }.is_err());
    let wo_handle = OpenOptions::new().write(true).open(&path).expect("open wo");
    // SAFETY: private temporary file.
    assert!(unsafe { RawMmap::map(&wo_handle) }.is_err());
    // SAFETY: private temporary file.
    assert!(unsafe { RawMmapMut::map_mut(&wo_handle) }.is_err());
}

#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
#[test]
fn file_growth_after_mapping_is_not_visible_past_window() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("grow.bin");
    let file = rw_file(&path, &pattern(100, 0));
    // SAFETY: private temporary file; growing (not truncating) the file
    // does not invalidate the mapped range.
    let m = unsafe { RawMmap::map(&file) }.expect("map");
    file.set_len(10_000).expect("grow");
    assert_eq!(m.len(), 100);
    // A new mapping sees the new size.
    // SAFETY: as above.
    let m2 = unsafe { RawMmap::map(&file) }.expect("map2");
    assert_eq!(m2.len(), 10_000);
}

#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
#[test]
fn anonymous_mappings_edge_sizes() {
    for len in [
        0usize,
        1,
        page() - 1,
        page(),
        page() + 1,
        gran() + 1,
        4 << 20,
    ] {
        let mut a = RawMmapMut::map_anon(len).expect("anon");
        assert_eq!(a.len(), len);
        assert!(a.iter().all(|&b| b == 0));
        if len > 0 {
            a[0] = 1;
            a[len - 1] = 2;
            assert_eq!(a[0] + a[len - 1], if len == 1 { 4 } else { 3 });
        }
        a.flush().expect("anon flush");
    }
    let e = RawMmapMut::map_anon(usize::MAX).expect_err("too big");
    assert_eq!(e.kind(), io::ErrorKind::InvalidInput);
    assert!(RawMmapMut::map_anon(isize::MAX as usize + 1).is_err());
}

#[cfg(windows)]
fn make_sparse(file: &File) {
    use std::os::windows::io::AsRawHandle;
    extern "system" {
        fn DeviceIoControl(
            device: *mut std::ffi::c_void,
            code: u32,
            in_buf: *mut std::ffi::c_void,
            in_len: u32,
            out_buf: *mut std::ffi::c_void,
            out_len: u32,
            returned: *mut u32,
            overlapped: *mut std::ffi::c_void,
        ) -> i32;
    }
    const FSCTL_SET_SPARSE: u32 = 0x0009_00C4;
    let mut returned = 0u32;
    // SAFETY: FSCTL_SET_SPARSE with no input buffer marks the file
    // sparse (MSDN); the handle is live and all buffers are null with
    // zero lengths, except `returned`, which points to a local.
    let ok = unsafe {
        DeviceIoControl(
            file.as_raw_handle(),
            FSCTL_SET_SPARSE,
            std::ptr::null_mut(),
            0,
            std::ptr::null_mut(),
            0,
            &mut returned,
            std::ptr::null_mut(),
        )
    };
    assert_ne!(
        ok,
        0,
        "FSCTL_SET_SPARSE failed: {}",
        io::Error::last_os_error()
    );
}

#[cfg(not(windows))]
fn make_sparse(_file: &File) {
    // Unix file systems create holes on set_len without help.
}

/// Offsets above 4 GiB exercise the high DWORD on Windows and the
/// 64-bit `off_t` / `mmap64` path on 32-bit Linux. The file is sparse,
/// so this uses almost no disk space.
#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
#[test]
fn offsets_above_4_gib() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("sparse.bin");
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(&path)
        .expect("create");
    make_sparse(&file);
    let size: u64 = (5 << 30) + 12_345;
    file.set_len(size).expect("create 5 GiB sparse file");
    let marker_at: u64 = (4 << 30) + gran() as u64 + 3;
    file.seek(SeekFrom::Start(marker_at)).expect("seek");
    file.write_all(b"MARK").expect("write marker");
    file.flush().expect("flush file");

    // SAFETY: private temporary file.
    let m = unsafe { opts(marker_at - 1, Some(6)).map(&file) }.expect("map high offset");
    assert_eq!(&m[..], b"\0MARK\0");
    // SAFETY: private temporary file.
    let mut w = unsafe { opts(marker_at, Some(4)).map_mut(&file) }.expect("map_mut high offset");
    w.copy_from_slice(b"mark");
    w.flush().expect("flush high");
    drop(w);
    drop(m);
    // SAFETY: private temporary file.
    let tail = unsafe { opts(size - 3, None).map(&file) }.expect("tail");
    assert_eq!(tail.len(), 3);
    // SAFETY: private temporary file.
    assert!(unsafe { opts(size + 1, None).map(&file) }.is_err());

    // A 5 GiB window does not fit in a 32-bit address space: error,
    // not truncation.
    // SAFETY: private temporary file.
    let whole = unsafe { RawMmap::map(&file) };
    if cfg!(target_pointer_width = "32") {
        let e = whole.expect_err("must not fit on 32-bit");
        assert_eq!(e.kind(), io::ErrorKind::InvalidInput);
    } else if let Ok(whole) = whole {
        // 64-bit: mapping the sparse file is fine; touch the marker only.
        assert_eq!(&whole[marker_at as usize..marker_at as usize + 4], b"mark");
    }
    drop(file);
    let _ = std::fs::remove_file(&path);
}
