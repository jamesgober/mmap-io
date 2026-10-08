//! `chunks`, `pages`, the owned variants, and `chunks_mut`.

use mmap_io::{MemoryMappedFile, MmapIoError};

use crate::common::{page, pattern, tmp_path, TmpPath};

fn rw(size: u64, seed: u8) -> (TmpPath, MemoryMappedFile, Vec<u8>) {
    let path = tmp_path("iter.bin");
    let m = MemoryMappedFile::create_rw(&path, size).unwrap();
    let data = pattern(size as usize, seed);
    m.update_region(0, &data).unwrap();
    (path, m, data)
}

fn ro_of(path: &TmpPath) -> MemoryMappedFile {
    MemoryMappedFile::open_ro(path).unwrap()
}

/// Check one chunk size against the model: count, per-chunk lengths,
/// exact `size_hint` at every step, and concatenated content.
fn check_chunks(m: &MemoryMappedFile, data: &[u8], cs: usize, ctx: &str) {
    let len = data.len();
    let expected = if cs == 0 { 0 } else { len.div_ceil(cs) };
    let mut it = m.chunks(cs);
    assert_eq!(it.len(), expected, "{ctx}: ExactSizeIterator::len");
    let mut joined = Vec::with_capacity(len);
    let mut seen = 0;
    while let Some(c) = it.next() {
        seen += 1;
        let remaining = expected - seen;
        assert_eq!(it.size_hint(), (remaining, Some(remaining)), "{ctx}");
        let want = if seen < expected {
            cs
        } else {
            len - cs * (expected - 1)
        };
        assert_eq!(c.len(), want, "{ctx}: chunk {seen}");
        joined.extend_from_slice(&c);
    }
    assert_eq!(seen, expected, "{ctx}: count");
    assert!(it.next().is_none(), "{ctx}: fused after the end");
    assert_eq!(it.size_hint(), (0, Some(0)));
    let want: &[u8] = if cs == 0 { &[] } else { data };
    assert_eq!(joined, want, "{ctx}: content");

    // Owned variant yields the same bytes.
    let owned: Vec<u8> = m
        .chunks_owned(cs)
        .map(|c| c.expect("owned chunk"))
        .collect::<Vec<_>>()
        .concat();
    assert_eq!(owned, want, "{ctx}: chunks_owned");
    assert_eq!(m.chunks_owned(cs).len(), expected);
}

#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
#[test]
fn chunk_sizes_around_every_boundary() {
    let p = page() as usize;
    for size in [
        1u64,
        2,
        p as u64 - 1,
        p as u64,
        p as u64 + 1,
        3 * p as u64 + 5,
    ] {
        let (path, m, data) = rw(size, 1);
        let len = size as usize;
        let sizes = [
            0,
            1,
            2,
            3,
            p - 1,
            p,
            p + 1,
            len.saturating_sub(1),
            len,
            len + 1,
            usize::MAX,
        ];
        for cs in sizes {
            check_chunks(&m, &data, cs, &format!("RW size={size} cs={cs}"));
        }
        m.flush().unwrap();
        let ro = ro_of(&path);
        for cs in sizes {
            check_chunks(&ro, &data, cs, &format!("RO size={size} cs={cs}"));
        }
        #[cfg(feature = "cow")]
        {
            let cow = MemoryMappedFile::open_cow(&path).unwrap();
            for cs in sizes {
                check_chunks(&cow, &data, cs, &format!("COW size={size} cs={cs}"));
            }
        }
    }
}

#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
#[test]
fn pages_equal_page_sized_chunks() {
    let p = page() as usize;
    for size in [1u64, p as u64, p as u64 + 1, 5 * p as u64 - 1] {
        let (path, m, data) = rw(size, 2);
        let pages: Vec<Vec<u8>> = m.pages().map(|s| s.to_vec()).collect();
        let chunks: Vec<Vec<u8>> = m.chunks(p).map(|s| s.to_vec()).collect();
        assert_eq!(pages, chunks);
        assert_eq!(m.pages().len(), (size as usize).div_ceil(p));
        let owned: Vec<Vec<u8>> = m.pages_owned().map(|r| r.unwrap()).collect();
        assert_eq!(owned, chunks);
        assert_eq!(m.pages_owned().size_hint(), m.pages().size_hint());
        assert_eq!(pages.concat(), data);
        m.flush().unwrap();
        let ro = ro_of(&path);
        assert_eq!(ro.pages().map(|s| s.to_vec()).collect::<Vec<_>>(), chunks);
    }
}

#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
#[test]
fn empty_read_only_mapping_yields_nothing() {
    let path = tmp_path("empty.bin");
    std::fs::write(&path, b"").unwrap();
    let ro = ro_of(&path);
    for cs in [0, 1, 4096, usize::MAX] {
        assert_eq!(ro.chunks(cs).count(), 0);
        assert_eq!(ro.chunks(cs).size_hint(), (0, Some(0)));
        assert_eq!(ro.chunks_owned(cs).count(), 0);
    }
    assert_eq!(ro.pages().count(), 0);
    assert_eq!(ro.pages_owned().count(), 0);
}

#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
#[test]
fn items_outlive_their_iterator_and_coexist() {
    let (_p, m, data) = rw(10_000, 3);
    let items: Vec<_> = m.chunks(1000).collect();
    assert_eq!(items.len(), 10);
    // Several iterators and their items alive at once on one thread.
    let again: Vec<_> = m.chunks(3333).collect();
    let pages: Vec<_> = m.pages().collect();
    let mut joined = Vec::new();
    for c in &items {
        joined.extend_from_slice(c);
    }
    assert_eq!(joined, data);
    assert_eq!(again.len(), 4);
    assert!(!pages.is_empty());
    // A plain slice can be taken while the items are alive.
    assert_eq!(m.as_slice(0, 10).unwrap(), &data[..10]);
    drop((items, again, pages));
    // Every guard is gone: writers and resize run again.
    m.resize(20_000).unwrap();
    assert_eq!(m.chunks(1000).count(), 20);
}

#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
#[test]
fn iterators_reflect_the_length_at_creation_time() {
    let (_p, m, _) = rw(4096, 4);
    m.resize(100).unwrap();
    assert_eq!(m.chunks(10).count(), 10);
    m.resize(4096 * 3).unwrap();
    assert_eq!(m.chunks(4096).count(), 3);
    assert_eq!(m.chunks(4096).last().unwrap().len(), 4096);
}

#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
#[test]
fn chunks_mut_visits_every_offset_in_order() {
    for (size, cs) in [
        (1u64, 1usize),
        (10, 3),
        (4096, 4096),
        (4097, 4096),
        (100, 1000),
        (7, usize::MAX),
    ] {
        let (_p, m, _) = rw(size, 5);
        let mut offsets = Vec::new();
        let mut total = 0usize;
        m.chunks_mut(cs)
            .for_each_mut(|off, chunk| {
                offsets.push(off);
                total += chunk.len();
                chunk.fill((off % 251) as u8);
                Ok(())
            })
            .unwrap();
        let expect: Vec<u64> = (0..size).step_by(cs).collect();
        assert_eq!(offsets, expect, "size={size} cs={cs}");
        assert_eq!(total as u64, size);
        for off in &offsets {
            assert_eq!(
                m.as_slice(*off, 1).unwrap()[0],
                (*off % 251) as u8,
                "size={size} cs={cs} off={off}"
            );
        }
    }
}

#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
#[test]
fn chunks_mut_stops_at_the_first_error() {
    let (_p, m, data) = rw(10, 6);
    let mut calls = 0;
    let r = m.chunks_mut(3).for_each_mut(|off, chunk| {
        calls += 1;
        chunk.fill(0xFF);
        if off == 3 {
            Err(MmapIoError::InvalidMode("stop here"))
        } else {
            Ok(())
        }
    });
    assert!(matches!(r, Err(MmapIoError::InvalidMode("stop here"))));
    assert_eq!(calls, 2);
    assert_eq!(m.as_slice(0, 6).unwrap(), &[0xFF; 6][..]);
    assert_eq!(
        m.as_slice(6, 4).unwrap(),
        &data[6..],
        "later chunks untouched"
    );
}

#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
#[test]
fn chunks_mut_legacy_returns_the_closure_error_inside_ok() {
    #[derive(Debug, PartialEq)]
    struct Foreign(u64);
    let (_p, m, _) = rw(100, 7);
    let r =
        m.chunks_mut(10).for_each_mut_legacy(
            |off, _| {
                if off == 50 {
                    Err(Foreign(off))
                } else {
                    Ok(())
                }
            },
        );
    assert_eq!(r.unwrap(), Err(Foreign(50)));
    let r = m
        .chunks_mut(10)
        .for_each_mut_legacy(|_, c| -> Result<(), Foreign> {
            c.fill(1);
            Ok(())
        });
    assert_eq!(r.unwrap(), Ok(()));
    assert_eq!(m.as_slice(0, 100).unwrap(), &[1; 100][..]);
    // chunk_size 0 calls nothing.
    let r = m
        .chunks_mut(0)
        .for_each_mut_legacy(|_, _| -> Result<(), Foreign> { panic!("called") });
    assert_eq!(r.unwrap(), Ok(()));
}

#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
#[test]
fn chunks_mut_is_refused_on_read_only_mappings() {
    let (path, m, data) = rw(100, 8);
    m.flush().unwrap();
    drop(m);
    let ro = ro_of(&path);
    let r = ro
        .chunks_mut(10)
        .for_each_mut(|_, _| panic!("closure ran on RO"));
    assert!(matches!(r, Err(MmapIoError::InvalidMode(_))));
    let r = ro
        .chunks_mut(10)
        .for_each_mut_legacy(|_, _| -> Result<(), ()> { panic!("closure ran on RO") });
    assert!(matches!(r, Err(MmapIoError::InvalidMode(_))));
    assert_eq!(ro.as_slice(0, 100).unwrap(), &data[..]);
}

/// `for_each_mut` documents `InvalidMode` on read-only mappings without
/// exception, but a zero chunk size returns `Ok(())` before the mode is
/// checked. Harmless (nothing is written), but the error contract is
/// not what the docs say.
#[test]
#[ignore = "BUG: chunks_mut(0).for_each_mut on a read-only mapping returns Ok instead of InvalidMode"]
fn chunks_mut_zero_chunk_size_still_checks_the_mode() {
    let (path, m, _) = rw(100, 9);
    drop(m);
    let ro = ro_of(&path);
    assert!(matches!(
        ro.chunks_mut(0).for_each_mut(|_, _| Ok(())),
        Err(MmapIoError::InvalidMode(_))
    ));
}

#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
#[test]
fn chunk_items_can_be_sent_to_other_threads() {
    let (_p, m, data) = rw(64 * 1024, 10);
    let sums: Vec<u64> = std::thread::scope(|s| {
        let handles: Vec<_> = m
            .chunks(8192)
            .map(|c| s.spawn(move || c.iter().map(|&b| u64::from(b)).sum::<u64>()))
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    let expect: u64 = data.iter().map(|&b| u64::from(b)).sum();
    assert_eq!(sums.iter().sum::<u64>(), expect);
}
