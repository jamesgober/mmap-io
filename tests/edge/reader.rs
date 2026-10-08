//! `MmapReader` (`Read` + `Seek`) checked against `std::io::Cursor`,
//! which the reader documents as its contract.

use std::io::{Cursor, ErrorKind, Read, Seek, SeekFrom};

use mmap_io::MemoryMappedFile;
use proptest::prelude::*;

use crate::common::{pattern, tmp_path, TmpPath};

fn mapping(bytes: &[u8]) -> (TmpPath, MemoryMappedFile) {
    let path = tmp_path("reader.bin");
    std::fs::write(&path, bytes).unwrap();
    let m = MemoryMappedFile::open_ro(&path).unwrap();
    (path, m)
}

#[derive(Debug, Clone)]
enum Op {
    Read(usize),
    Seek(SeekFrom),
    SetPosition(u64),
    ReadExact(usize),
}

/// Apply `op` to both the reader and a `Cursor` over the same bytes,
/// and require identical results and positions.
fn step(r: &mut mmap_io::mmap::MmapReader<'_>, c: &mut Cursor<&[u8]>, op: &Op, ctx: &str) {
    match op {
        Op::Read(n) => {
            let mut a = vec![0xCCu8; *n];
            let mut b = vec![0xCCu8; *n];
            let ra = r.read(&mut a).map_err(|e| e.kind());
            let rb = c.read(&mut b).map_err(|e| e.kind());
            assert_eq!(ra, rb, "{ctx}: read({n})");
            assert_eq!(a, b, "{ctx}: read({n}) bytes");
        }
        Op::ReadExact(n) => {
            let before = r.position();
            let mut a = vec![0u8; *n];
            let mut b = vec![0u8; *n];
            let ra = r.read_exact(&mut a).map_err(|e| e.kind());
            let rb = c.read_exact(&mut b).map_err(|e| e.kind());
            assert_eq!(ra, rb, "{ctx}: read_exact({n})");
            if ra.is_ok() {
                assert_eq!(a, b, "{ctx}: read_exact bytes");
            } else {
                // The position after a failed read_exact is unspecified
                // by `Read` (Cursor jumps to the end, the default
                // implementation stops where the data ran out). It must
                // not move backwards when it was inside the data.
                // Here the reader consumes what is left and stops.
                let end = c.get_ref().len() as u64;
                assert_eq!(r.position(), before.max(end), "{ctx}: failed read_exact");
                c.set_position(r.position());
            }
        }
        Op::Seek(sf) => {
            let ra = r.seek(*sf).map_err(|e| e.kind());
            let rb = c.seek(*sf).map_err(|e| e.kind());
            assert_eq!(ra, rb, "{ctx}: seek({sf:?})");
        }
        Op::SetPosition(p) => {
            r.set_position(*p);
            c.set_position(*p);
        }
    }
    assert_eq!(r.position(), c.position(), "{ctx}: position after {op:?}");
}

#[test]
fn reader_matches_cursor_on_a_table_of_edge_operations() {
    for len in [1usize, 2, 4095, 4096, 4097, 70_000] {
        let data = pattern(len, 3);
        let (_p, m) = mapping(&data);
        let n = len as i64;
        let ops = vec![
            Op::Read(0),
            Op::Read(1),
            Op::Read(len + 1),
            Op::Read(1),
            Op::Seek(SeekFrom::Start(0)),
            Op::Read(len),
            Op::Seek(SeekFrom::End(0)),
            Op::Read(10),
            Op::Seek(SeekFrom::End(-1)),
            Op::Read(10),
            Op::Seek(SeekFrom::End(-n)),
            Op::Seek(SeekFrom::End(-n - 1)),
            Op::Seek(SeekFrom::End(1)),
            Op::Read(1),
            Op::Seek(SeekFrom::End(i64::MIN)),
            Op::Seek(SeekFrom::End(i64::MAX)),
            Op::Seek(SeekFrom::Current(0)),
            Op::Seek(SeekFrom::Start(u64::MAX)),
            Op::Read(1),
            Op::Seek(SeekFrom::Current(1)),
            Op::Seek(SeekFrom::Current(-1)),
            Op::Seek(SeekFrom::Current(i64::MIN)),
            Op::Seek(SeekFrom::Start(1)),
            Op::Seek(SeekFrom::Current(-2)),
            Op::Seek(SeekFrom::Current(-1)),
            Op::ReadExact(len),
            Op::ReadExact(1),
            Op::SetPosition(u64::from(u32::MAX) + 7),
            Op::Read(3),
            Op::SetPosition(len as u64 - 1),
            Op::ReadExact(2),
            Op::SetPosition(0),
            Op::ReadExact(0),
            Op::Seek(SeekFrom::Current(i64::MAX)),
            Op::Seek(SeekFrom::Current(i64::MAX)),
            Op::Seek(SeekFrom::Current(i64::MAX)),
        ];
        let mut r = m.reader();
        let mut c = Cursor::new(&data[..]);
        for (i, op) in ops.iter().enumerate() {
            step(&mut r, &mut c, op, &format!("len={len} op#{i}"));
        }
    }
}

#[test]
fn failed_seeks_leave_the_position_unchanged() {
    let (_p, m) = mapping(b"0123456789");
    let mut r = m.reader();
    r.seek(SeekFrom::Start(4)).unwrap();
    for bad in [
        SeekFrom::Current(-5),
        SeekFrom::End(-11),
        SeekFrom::Current(i64::MIN),
    ] {
        let e = r.seek(bad).unwrap_err();
        assert_eq!(e.kind(), ErrorKind::InvalidInput, "{bad:?}");
        assert_eq!(r.position(), 4, "{bad:?}");
    }
    r.set_position(u64::MAX);
    assert_eq!(
        r.seek(SeekFrom::Current(1)).unwrap_err().kind(),
        ErrorKind::InvalidInput
    );
    assert_eq!(r.position(), u64::MAX);
    assert_eq!(r.read(&mut [0u8; 4]).unwrap(), 0);
}

#[test]
fn read_to_end_copy_and_bufreader_see_the_whole_file() {
    let data = pattern(3 * 4096 + 17, 9);
    let (_p, m) = mapping(&data);
    let mut out = Vec::new();
    m.reader().read_to_end(&mut out).unwrap();
    assert_eq!(out, data);

    let mut sink = Vec::new();
    let n = std::io::copy(&mut m.reader(), &mut sink).unwrap();
    assert_eq!(n, data.len() as u64);
    assert_eq!(sink, data);

    let mut buffered = std::io::BufReader::with_capacity(7, m.reader());
    let mut out = Vec::new();
    buffered.read_to_end(&mut out).unwrap();
    assert_eq!(out, data);

    // Two readers on one mapping are independent.
    let mut a = m.reader();
    let mut b = m.reader();
    a.seek(SeekFrom::Start(100)).unwrap();
    let mut x = [0u8; 4];
    b.read_exact(&mut x).unwrap();
    assert_eq!(&x, &data[..4]);
    assert_eq!(a.position(), 100);
}

#[test]
fn reader_works_on_read_write_mappings_and_follows_resizes() {
    let path = tmp_path("rw_reader.bin");
    let m = MemoryMappedFile::create_rw(&path, 100).unwrap();
    m.update_region(0, &pattern(100, 1)).unwrap();
    let mut r = m.reader();
    let mut buf = [0u8; 60];
    r.read_exact(&mut buf).unwrap();
    assert_eq!(&buf[..], &pattern(100, 1)[..60]);

    // Shrink below the cursor: the next read is EOF, not an error.
    let mut r = m.reader();
    r.set_position(60);
    m.resize(50).unwrap();
    assert_eq!(r.read(&mut buf).unwrap(), 0);
    assert_eq!(r.position(), 60);
    // Grow again: the bytes past the old end are zeros.
    m.resize(200).unwrap();
    let n = r.read(&mut buf).unwrap();
    assert_eq!(n, 60);
    assert!(buf.iter().all(|&b| b == 0));
    assert_eq!(r.seek(SeekFrom::End(0)).unwrap(), 200);
}

#[cfg(feature = "cow")]
#[test]
fn reader_works_on_copy_on_write_mappings() {
    let data = pattern(5000, 4);
    let (path, _ro) = mapping(&data);
    let cow = MemoryMappedFile::open_cow(&path).unwrap();
    let mut out = Vec::new();
    cow.reader().read_to_end(&mut out).unwrap();
    assert_eq!(out, data);
}

fn op_strategy(len: usize) -> impl Strategy<Value = Op> {
    let l = len as i64;
    prop_oneof![
        (0..=len + 8).prop_map(Op::Read),
        (0..=len + 8).prop_map(Op::ReadExact),
        (0..=len as u64 + 8).prop_map(|p| Op::Seek(SeekFrom::Start(p))),
        (-l - 8..=8i64).prop_map(|d| Op::Seek(SeekFrom::End(d))),
        (-l - 8..=l + 8).prop_map(|d| Op::Seek(SeekFrom::Current(d))),
        prop_oneof![Just(i64::MIN), Just(i64::MAX)].prop_map(|d| Op::Seek(SeekFrom::Current(d))),
        prop_oneof![Just(0u64), Just(u64::MAX), 0..=len as u64 + 8].prop_map(Op::SetPosition),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 128, ..ProptestConfig::default() })]

    /// Random operation sequences behave exactly like `Cursor`.
    #[test]
    fn reader_is_equivalent_to_cursor(
        (len, ops) in (1usize..=5000).prop_flat_map(|len| {
            (Just(len), proptest::collection::vec(op_strategy(len), 1..40))
        })
    ) {
        let data = pattern(len, 5);
        let (_p, m) = mapping(&data);
        let mut r = m.reader();
        let mut c = Cursor::new(&data[..]);
        for (i, op) in ops.iter().enumerate() {
            step(&mut r, &mut c, op, &format!("len={len} op#{i}"));
        }
    }
}
