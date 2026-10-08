//! `AnonymousMmap::with_huge_pages` (1.1.0): `MAP_HUGETLB` with a
//! fallback to base pages plus `MADV_HUGEPAGE`.

#![cfg(feature = "hugepages")]

use mmap_io::{AnonymousMmap, MmapIoError};

const MIB: u64 = 1024 * 1024;

#[test]
fn behaves_like_new() {
    let m = AnonymousMmap::with_huge_pages(4 * MIB).expect("with_huge_pages");
    assert_eq!(m.len(), 4 * MIB);
    m.update_region(4 * MIB - 5, b"tail!")
        .expect("write at end");
    let mut buf = [0u8; 5];
    m.read_into(4 * MIB - 5, &mut buf).expect("read");
    assert_eq!(&buf, b"tail!");
    assert!(m.as_slice(0, 16).expect("slice").iter().all(|&b| b == 0));
    assert!(matches!(
        m.update_region(4 * MIB, b"x"),
        Err(MmapIoError::OutOfBounds { .. })
    ));
    // A size that is not a huge-page multiple still reports its length.
    let odd = AnonymousMmap::with_huge_pages(4097).expect("odd size");
    assert_eq!(odd.len(), 4097);
    odd.update_region(4096, b"z").expect("last byte");
}

#[test]
fn rejects_zero_and_oversized() {
    assert!(matches!(
        AnonymousMmap::with_huge_pages(0),
        Err(MmapIoError::ResizeFailed(_))
    ));
    assert!(matches!(
        AnonymousMmap::with_huge_pages(u64::MAX),
        Err(MmapIoError::ResizeFailed(_))
    ));
}

#[cfg(not(target_os = "linux"))]
#[test]
fn hugepage_status_is_unknown_off_linux() {
    let m = AnonymousMmap::with_huge_pages(2 * MIB).expect("map");
    assert_eq!(m.is_hugepage_backed(), None);
}

/// Linux: with no huge pages reserved (the default, and the WSL2
/// default), `MAP_HUGETLB` fails and `with_huge_pages` must fall back
/// instead of failing. With pages reserved it must use them.
#[cfg(target_os = "linux")]
#[test]
fn linux_hugetlb_or_fallback() {
    let reserved: u64 = std::fs::read_to_string("/proc/sys/vm/nr_hugepages")
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0);
    let raw = mmap_io::raw::RawMmapOptions::new()
        .len(2 * MIB as usize)
        .huge()
        .map_anon();
    if reserved == 0 {
        assert!(
            raw.is_err(),
            "MAP_HUGETLB cannot succeed without reserved pages"
        );
    }
    drop(raw);

    let m = AnonymousMmap::with_huge_pages(8 * MIB).expect("never fails on Linux");
    // Touch every page so transparent huge pages (if granted) appear.
    for off in (0..8 * MIB).step_by(4096) {
        m.update_region(off, &[1]).expect("touch");
    }
    let backed = m.is_hugepage_backed();
    assert!(backed.is_some(), "smaps lookup failed");
    let thp =
        std::fs::read_to_string("/sys/kernel/mm/transparent_hugepage/enabled").unwrap_or_default();
    eprintln!(
        "nr_hugepages={reserved} thp=[{}] backed={backed:?}",
        thp.trim()
    );
    if reserved >= 4 {
        assert_eq!(backed, Some(true), "reserved huge pages were not used");
    }
    // The normal constructor never asks for huge pages explicitly.
    let plain = AnonymousMmap::new(4096).expect("new");
    assert!(plain.is_hugepage_backed().is_some());
}
