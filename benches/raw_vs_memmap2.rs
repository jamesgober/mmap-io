//! Temporary comparison bench: `mmap_io::raw` against `memmap2`.
//!
//! Measures the per-operation cost of the platform layer only:
//! creating a mapping (map + drop), dropping a mapping, and flushing a
//! dirtied 4 KiB / 1 MiB mapping. Both sides map the same private
//! temporary file the same way.
//!
//! This file exists only while `memmap2` is still a dependency. Delete
//! it together with the `memmap2` entry in `Cargo.toml` when the crate
//! switches to `crate::raw`.
//!
//! Run: `cargo bench --bench raw_vs_memmap2`

use std::fs::{File, OpenOptions};
use std::path::Path;
use std::time::Duration;

use criterion::{black_box, criterion_group, criterion_main, BatchSize, Criterion};
use mmap_io::raw::{RawMmap, RawMmapMut, RawMmapOptions};

const KIB: usize = 1024;
const MIB: usize = 1024 * 1024;

fn make_file(path: &Path, len: usize) -> File {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)
        .expect("create bench file");
    file.set_len(len as u64).expect("set_len");
    file
}

// SAFETY (all four helpers): the bench files are private to this
// process and are never modified or truncated while mapped, except
// through the mapping under test, which is the documented contract of
// both `memmap2` and `mmap_io::raw` file-backed constructors.

fn m2_ro(file: &File) -> memmap2::Mmap {
    // SAFETY: see the note above the helpers.
    unsafe { memmap2::Mmap::map(file) }.expect("memmap2 map")
}

fn m2_rw(file: &File) -> memmap2::MmapMut {
    // SAFETY: see the note above the helpers.
    unsafe { memmap2::MmapMut::map_mut(file) }.expect("memmap2 map_mut")
}

fn raw_ro(file: &File) -> RawMmap {
    // SAFETY: see the note above the helpers.
    unsafe { RawMmap::map(file) }.expect("raw map")
}

fn raw_rw(file: &File) -> RawMmapMut {
    // SAFETY: see the note above the helpers.
    unsafe { RawMmapMut::map_mut(file) }.expect("raw map_mut")
}

fn m2_window(file: &File) -> memmap2::Mmap {
    // SAFETY: see the note above the helpers.
    unsafe {
        memmap2::MmapOptions::new()
            .offset(4097)
            .len(256 * KIB)
            .map(file)
    }
    .expect("memmap2 window")
}

fn raw_window(file: &File) -> RawMmap {
    // SAFETY: see the note above the helpers.
    unsafe { RawMmapOptions::new().offset(4097).len(256 * KIB).map(file) }.expect("raw window")
}

/// Touch one byte per 4 KiB so every page of the mapping is dirty.
fn dirty(bytes: &mut [u8], round: u8) {
    for i in (0..bytes.len()).step_by(4 * KIB) {
        bytes[i] = round;
    }
}

fn bench_map(c: &mut Criterion) {
    let dir = tempfile::tempdir().expect("tempdir");
    let file = make_file(&dir.path().join("map.bin"), MIB);
    let mut g = c.benchmark_group("map_1mib_then_drop");
    g.bench_function("ro/memmap2", |b| b.iter(|| black_box(m2_ro(&file))));
    g.bench_function("ro/raw", |b| b.iter(|| black_box(raw_ro(&file))));
    g.bench_function("rw/memmap2", |b| b.iter(|| black_box(m2_rw(&file))));
    g.bench_function("rw/raw", |b| b.iter(|| black_box(raw_rw(&file))));
    g.bench_function("ro_offset_len/memmap2", |b| {
        b.iter(|| black_box(m2_window(&file)));
    });
    g.bench_function("ro_offset_len/raw", |b| {
        b.iter(|| black_box(raw_window(&file)))
    });
    g.bench_function("anon/memmap2", |b| {
        b.iter(|| black_box(memmap2::MmapMut::map_anon(MIB).expect("anon")));
    });
    g.bench_function("anon/raw", |b| {
        b.iter(|| black_box(RawMmapMut::map_anon(MIB).expect("anon")));
    });
    g.finish();
}

fn bench_drop(c: &mut Criterion) {
    let dir = tempfile::tempdir().expect("tempdir");
    let file = make_file(&dir.path().join("drop.bin"), MIB);
    let mut g = c.benchmark_group("drop_1mib_rw");
    g.bench_function("memmap2", |b| {
        b.iter_batched(|| m2_rw(&file), drop, BatchSize::SmallInput);
    });
    g.bench_function("raw", |b| {
        b.iter_batched(|| raw_rw(&file), drop, BatchSize::SmallInput);
    });
    g.finish();
}

fn bench_flush(c: &mut Criterion) {
    let dir = tempfile::tempdir().expect("tempdir");
    for (name, len) in [("4kib", 4 * KIB), ("1mib", MIB)] {
        let file = make_file(&dir.path().join(format!("flush_{name}.bin")), len);
        let mut g = c.benchmark_group(format!("dirty_then_flush_{name}"));
        let mut round = 0u8;
        let mut m2 = m2_rw(&file);
        g.bench_function("memmap2", |b| {
            b.iter(|| {
                round = round.wrapping_add(1);
                dirty(&mut m2, round);
                m2.flush().expect("flush");
            });
        });
        drop(m2);
        let mut raw = raw_rw(&file);
        g.bench_function("raw", |b| {
            b.iter(|| {
                round = round.wrapping_add(1);
                dirty(&mut raw, round);
                raw.flush().expect("flush");
            });
        });
        drop(raw);
        g.finish();
    }
}

fn config() -> Criterion {
    Criterion::default()
        .sample_size(30)
        .warm_up_time(Duration::from_secs(1))
        .measurement_time(Duration::from_secs(4))
}

criterion_group! {
    name = benches;
    config = config();
    targets = bench_map, bench_drop, bench_flush
}
criterion_main!(benches);
