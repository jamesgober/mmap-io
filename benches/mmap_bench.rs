//! Criterion benchmarks for `mmap-io`.
//!
//! 0.9.7 adds workload-pattern benches covering:
//! - Sequential reads at multiple file sizes (1 MiB, 16 MiB, 256 MiB).
//! - Random reads with a hand-rolled xorshift PRNG.
//! - Sequential writes under `Manual`, `EveryBytes(64 KiB)`, and
//!   `EveryMillis(10)` policies.
//! - Iterator throughput at 4 KiB / 64 KiB / page-sized chunks (now
//!   zero-copy after H1).
//! - `touch_pages` on 1 GiB (now fast after H2).
//! - Atomic-view contention across threads.
//!
//! All filesystem temp files use `mmap_io_bench_<name>_<pid>` so
//! parallel `cargo bench` runs from different processes don't collide.

use criterion::{criterion_group, criterion_main, BatchSize, BenchmarkId, Criterion, Throughput};
use mmap_io::flush::FlushPolicy;
use mmap_io::{MemoryMappedFile, MmapMode};
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

/// Hand-rolled xorshift64* PRNG. Used to generate reproducible random
/// offsets in benches without taking on a `rand` dev-dep. Quality is
/// fine for read-offset selection; do not use for cryptography.
struct XorShift64(u64);

impl XorShift64 {
    fn new(seed: u64) -> Self {
        Self(if seed == 0 {
            0x9E37_79B9_7F4A_7C15
        } else {
            seed
        })
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
}

fn tmp_path(name: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!("mmap_io_bench_{}_{}", name, std::process::id()));
    p
}

// ---------------------------------------------------------------------
// Existing benches (preserved from prior milestones)
// ---------------------------------------------------------------------

fn bench_create_rw(b: &mut Criterion) {
    let mut group = b.benchmark_group("create_rw");
    for &size in &[4_usize * 1024, 64 * 1024, 1024 * 1024] {
        group.throughput(Throughput::Bytes(size as u64));
        group.bench_with_input(BenchmarkId::from_parameter(size), &size, |ben, &sz| {
            ben.iter_batched(
                || {
                    let path = tmp_path(&format!("create_rw_{sz}"));
                    let _ = fs::remove_file(&path);
                    (path, sz)
                },
                |(path, sz)| {
                    let _m = MemoryMappedFile::create_rw(&path, sz as u64).expect("create_rw");
                    let _ = fs::remove_file(&path);
                },
                BatchSize::SmallInput,
            )
        });
    }
    group.finish();
}

fn bench_update_region_flush(b: &mut Criterion) {
    let mut group = b.benchmark_group("update_region_flush");
    for &size in &[4_usize * 1024, 64 * 1024, 1024 * 1024] {
        group.throughput(Throughput::Bytes(size as u64));

        group.bench_with_input(BenchmarkId::new("update_only", size), &size, |ben, &sz| {
            let path = tmp_path(&format!("update_only_{sz}"));
            let _ = fs::remove_file(&path);
            let mmap = MemoryMappedFile::create_rw(&path, sz as u64).expect("create_rw");
            let payload = vec![0xAB_u8; sz];
            ben.iter(|| {
                mmap.update_region(0, &payload).expect("update");
                criterion::black_box(&payload);
            });
            let _ = fs::remove_file(&path);
        });

        group.bench_with_input(
            BenchmarkId::new("update_plus_flush", size),
            &size,
            |ben, &sz| {
                let path = tmp_path(&format!("update_flush_{sz}"));
                let _ = fs::remove_file(&path);
                let mmap = MemoryMappedFile::create_rw(&path, sz as u64).expect("create_rw");
                let payload = vec![0xAC_u8; sz];
                ben.iter(|| {
                    mmap.update_region(0, &payload).expect("update");
                    mmap.flush().expect("flush");
                });
                let _ = fs::remove_file(&path);
            },
        );

        group.bench_with_input(
            BenchmarkId::new("update_threshold", size),
            &size,
            |ben, &sz| {
                let path = tmp_path(&format!("update_threshold_{sz}"));
                let _ = fs::remove_file(&path);
                let mmap = MemoryMappedFile::builder(&path)
                    .mode(MmapMode::ReadWrite)
                    .size(sz as u64)
                    .flush_policy(FlushPolicy::EveryBytes(sz))
                    .create()
                    .expect("builder create_rw with threshold");
                let payload = vec![0xAD_u8; sz];
                ben.iter(|| {
                    mmap.update_region(0, &payload).expect("update");
                    criterion::black_box(&payload);
                });
                let _ = fs::remove_file(&path);
            },
        );
    }
    group.finish();
}

fn bench_read_into_rw(b: &mut Criterion) {
    let mut group = b.benchmark_group("read_into_rw");
    for &size in &[4_usize * 1024, 64 * 1024, 1024 * 1024] {
        group.throughput(Throughput::Bytes(size as u64));
        group.bench_with_input(BenchmarkId::from_parameter(size), &size, |ben, &sz| {
            let path = tmp_path(&format!("read_into_rw_{sz}"));
            let _ = fs::remove_file(&path);
            let mmap = MemoryMappedFile::create_rw(&path, sz as u64).expect("create_rw");
            mmap.update_region(0, &vec![1u8; sz]).expect("seed");
            mmap.flush().expect("flush");

            let mut buf = vec![0u8; sz];
            ben.iter(|| {
                mmap.read_into(0, &mut buf).expect("read_into");
                criterion::black_box(&buf);
            });
            let _ = fs::remove_file(&path);
        });
    }
    group.finish();
}

fn bench_as_slice_ro(b: &mut Criterion) {
    let mut group = b.benchmark_group("as_slice_ro");
    for &size in &[4_usize * 1024, 64 * 1024, 1024 * 1024] {
        group.throughput(Throughput::Bytes(size as u64));
        group.bench_with_input(BenchmarkId::from_parameter(size), &size, |ben, &sz| {
            let path = tmp_path(&format!("as_slice_ro_{sz}"));
            let _ = fs::remove_file(&path);
            {
                let mmap = MemoryMappedFile::create_rw(&path, sz as u64).expect("create_rw");
                mmap.update_region(0, &vec![2u8; sz]).expect("seed");
                mmap.flush().expect("flush");
            }
            let ro = MemoryMappedFile::open_ro(&path).expect("open_ro");
            ben.iter(|| {
                let s = ro.as_slice(0, sz as u64).expect("as_slice");
                criterion::black_box(&*s);
            });
            let _ = fs::remove_file(&path);
        });
    }
    group.finish();
}

fn bench_resize(b: &mut Criterion) {
    let mut group = b.benchmark_group("resize");
    group.bench_function("grow_1MB_to_8MB", |ben| {
        let path = tmp_path("resize_grow");
        let _ = fs::remove_file(&path);
        let mmap = MemoryMappedFile::create_rw(&path, 1024 * 1024).expect("create_rw");
        ben.iter(|| {
            mmap.resize(8 * 1024 * 1024).expect("resize grow");
            mmap.resize(1024 * 1024).expect("resize shrink");
        });
        let _ = fs::remove_file(&path);
    });
    group.finish();
}

// ---------------------------------------------------------------------
// 0.9.7 workload-pattern benches
// ---------------------------------------------------------------------

/// Sequential RO read at 1 MiB / 16 MiB / 256 MiB. Compares the cost
/// of `as_slice` (zero-copy view) versus `read_into` (memcpy).
fn bench_sequential_read(b: &mut Criterion) {
    let mut group = b.benchmark_group("sequential_read");
    group.measurement_time(Duration::from_secs(5));
    for &size in &[1024 * 1024usize, 16 * 1024 * 1024, 256 * 1024 * 1024] {
        group.throughput(Throughput::Bytes(size as u64));

        // Seed once per size.
        let path = tmp_path(&format!("seq_read_{size}"));
        let _ = fs::remove_file(&path);
        {
            let rw = MemoryMappedFile::create_rw(&path, size as u64).expect("create_rw");
            rw.update_region(0, &vec![0xA5u8; size]).expect("seed");
            rw.flush().expect("flush");
        }

        let ro = MemoryMappedFile::open_ro(&path).expect("open_ro");

        group.bench_with_input(BenchmarkId::new("as_slice", size), &size, |ben, &sz| {
            ben.iter(|| {
                let s = ro.as_slice(0, sz as u64).expect("as_slice");
                // Touch one byte per 4 KiB to force the read into
                // observable work without dominating the bench
                // with a full scan.
                let mut sum: u64 = 0;
                let mut off = 0usize;
                while off < sz {
                    sum = sum.wrapping_add(s[off] as u64);
                    off += 4096;
                }
                criterion::black_box(sum);
            });
        });

        group.bench_with_input(BenchmarkId::new("read_into", size), &size, |ben, &sz| {
            let mut buf = vec![0u8; sz];
            ben.iter(|| {
                ro.read_into(0, &mut buf).expect("read_into");
                criterion::black_box(&buf[0]);
            });
        });

        drop(ro);
        let _ = fs::remove_file(&path);
    }
    group.finish();
}

/// Random-offset reads on a 16 MiB RO file. Tests cache-miss / TLB
/// behavior versus the sequential pattern.
fn bench_random_read(b: &mut Criterion) {
    let mut group = b.benchmark_group("random_read");
    let file_size: usize = 16 * 1024 * 1024;
    group.throughput(Throughput::Bytes(file_size as u64));

    let path = tmp_path("rand_read_16mb");
    let _ = fs::remove_file(&path);
    {
        let rw = MemoryMappedFile::create_rw(&path, file_size as u64).expect("create_rw");
        rw.update_region(0, &vec![0xB6u8; file_size]).expect("seed");
        rw.flush().expect("flush");
    }
    let ro = MemoryMappedFile::open_ro(&path).expect("open_ro");

    for &req_size in &[64usize, 256, 4096, 65536] {
        group.bench_with_input(
            BenchmarkId::new("read_into", req_size),
            &req_size,
            |ben, &rsz| {
                let mut buf = vec![0u8; rsz];
                let mut rng = XorShift64::new(0xC0FFEE);
                ben.iter(|| {
                    let max_off = (file_size - rsz) as u64;
                    let off = rng.next_u64() % (max_off + 1);
                    ro.read_into(off, &mut buf).expect("read_into");
                    criterion::black_box(&buf[0]);
                });
            },
        );

        group.bench_with_input(
            BenchmarkId::new("as_slice", req_size),
            &req_size,
            |ben, &rsz| {
                let mut rng = XorShift64::new(0xC0FFEE);
                ben.iter(|| {
                    let max_off = (file_size - rsz) as u64;
                    let off = rng.next_u64() % (max_off + 1);
                    let s = ro.as_slice(off, rsz as u64).expect("as_slice");
                    criterion::black_box(s[0]);
                });
            },
        );
    }

    drop(ro);
    let _ = fs::remove_file(&path);
    group.finish();
}

/// Sequential writes under three flush policies on a 4 MiB RW file.
/// Tests durability/throughput tradeoff after C2 made
/// `EveryMillis` actually work.
fn bench_sequential_write_policies(b: &mut Criterion) {
    let mut group = b.benchmark_group("sequential_write");
    group.measurement_time(Duration::from_secs(3));
    let file_size: usize = 4 * 1024 * 1024;
    let chunk: usize = 64 * 1024;
    let chunks_per_file = file_size / chunk;
    group.throughput(Throughput::Bytes(file_size as u64));

    let policies: &[(&str, FlushPolicy)] = &[
        ("manual", FlushPolicy::Manual),
        ("every_bytes_64k", FlushPolicy::EveryBytes(64 * 1024)),
        ("every_millis_10", FlushPolicy::EveryMillis(10)),
    ];

    for (label, policy) in policies {
        let path = tmp_path(&format!("seq_write_{label}"));
        let _ = fs::remove_file(&path);
        let mmap = MemoryMappedFile::builder(&path)
            .mode(MmapMode::ReadWrite)
            .size(file_size as u64)
            .flush_policy(*policy)
            .create()
            .expect("builder create");
        let payload = vec![0xC7u8; chunk];
        group.bench_function(*label, |ben| {
            ben.iter(|| {
                for i in 0..chunks_per_file {
                    let off = (i * chunk) as u64;
                    mmap.update_region(off, &payload).expect("update");
                }
                criterion::black_box(&payload);
            });
        });
        drop(mmap);
        let _ = fs::remove_file(&path);
    }
    group.finish();
}

/// Iterator throughput post-H1. Zero-copy iteration over 16 MiB at
/// 4 KiB / 64 KiB / page-sized chunks. Compared to the owned variant
/// to show the H1 win.
#[cfg(feature = "iterator")]
fn bench_iterator_throughput(b: &mut Criterion) {
    let mut group = b.benchmark_group("iterator_throughput");
    group.measurement_time(Duration::from_secs(3));
    let file_size: usize = 16 * 1024 * 1024;
    group.throughput(Throughput::Bytes(file_size as u64));

    let path = tmp_path("iter_tp");
    let _ = fs::remove_file(&path);
    {
        let rw = MemoryMappedFile::create_rw(&path, file_size as u64).expect("create_rw");
        rw.update_region(0, &vec![0x88u8; file_size]).expect("seed");
        rw.flush().expect("flush");
    }
    let ro = MemoryMappedFile::open_ro(&path).expect("open_ro");

    for &chunk in &[4096usize, 65536] {
        group.bench_with_input(
            BenchmarkId::new("zero_copy_chunks", chunk),
            &chunk,
            |ben, &csz| {
                ben.iter(|| {
                    let mut total: u64 = 0;
                    for slice in ro.chunks(csz) {
                        total = total.wrapping_add(slice[0] as u64);
                    }
                    criterion::black_box(total);
                });
            },
        );

        group.bench_with_input(
            BenchmarkId::new("owned_chunks", chunk),
            &chunk,
            |ben, &csz| {
                ben.iter(|| {
                    let mut total: u64 = 0;
                    for v in ro.chunks_owned(csz) {
                        let buf = v.expect("chunk");
                        total = total.wrapping_add(buf[0] as u64);
                    }
                    criterion::black_box(total);
                });
            },
        );
    }

    group.bench_function("zero_copy_pages", |ben| {
        ben.iter(|| {
            let mut total: u64 = 0;
            for slice in ro.pages() {
                total = total.wrapping_add(slice[0] as u64);
            }
            criterion::black_box(total);
        });
    });

    drop(ro);
    let _ = fs::remove_file(&path);
    group.finish();
}
#[cfg(not(feature = "iterator"))]
fn bench_iterator_throughput(_: &mut Criterion) {}

/// `touch_pages` on a 1 GiB file. Verifies the H2 tight-loop fix:
/// expected to be 50x faster than the previous per-page
/// `read_into(1 byte)` design.
fn bench_touch_pages_large(b: &mut Criterion) {
    let mut group = b.benchmark_group("touch_pages_large");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(8));
    let file_size: u64 = 1024 * 1024 * 1024;

    let path = tmp_path("touch_1gb");
    let _ = fs::remove_file(&path);
    let mmap = MemoryMappedFile::create_rw(&path, file_size).expect("create_rw 1 GiB");

    group.throughput(Throughput::Bytes(file_size));
    group.bench_function("touch_pages_1gb_rw", |ben| {
        ben.iter(|| {
            mmap.touch_pages().expect("touch_pages");
        });
    });

    drop(mmap);
    let _ = fs::remove_file(&path);
    group.finish();
}

/// Multi-thread atomic contention. Threads fetch_add on the same
/// `AtomicU64` view. Stresses the C3 wrapper (read guard sharing)
/// and the underlying atomic instruction throughput.
#[cfg(feature = "atomic")]
fn bench_atomic_contention(b: &mut Criterion) {
    use std::sync::atomic::Ordering;
    use std::thread;

    let mut group = b.benchmark_group("atomic_contention");
    group.sample_size(20);
    group.measurement_time(Duration::from_secs(3));

    let path = tmp_path("atomic_contention");
    let _ = fs::remove_file(&path);
    let mmap = Arc::new(MemoryMappedFile::create_rw(&path, 64).expect("create_rw"));
    {
        let v = mmap.atomic_u64(0).expect("init");
        v.store(0, Ordering::SeqCst);
    }

    for &n_threads in &[1usize, 2, 4, 8] {
        group.bench_with_input(
            BenchmarkId::from_parameter(n_threads),
            &n_threads,
            |ben, &n| {
                ben.iter(|| {
                    let handles: Vec<_> = (0..n)
                        .map(|_| {
                            let mmap = Arc::clone(&mmap);
                            thread::spawn(move || {
                                let v = mmap.atomic_u64(0).expect("view");
                                for _ in 0..10_000 {
                                    v.fetch_add(1, Ordering::Relaxed);
                                }
                            })
                        })
                        .collect();
                    for h in handles {
                        h.join().expect("join");
                    }
                });
            },
        );
    }

    drop(mmap);
    let _ = fs::remove_file(&path);
    group.finish();
}
#[cfg(not(feature = "atomic"))]
fn bench_atomic_contention(_: &mut Criterion) {}

// ---------------------------------------------------------------------
// Existing benches preserved from prior milestones
// ---------------------------------------------------------------------

fn bench_touch_pages(b: &mut Criterion) {
    let mut group = b.benchmark_group("touch_pages");
    for &size in &[1024 * 1024, 8 * 1024 * 1024, 32 * 1024 * 1024] {
        group.throughput(Throughput::Bytes(size as u64));
        group.bench_with_input(BenchmarkId::from_parameter(size), &size, |ben, &sz| {
            let path = tmp_path(&format!("touch_pages_{sz}"));
            let _ = fs::remove_file(&path);
            let mmap = MemoryMappedFile::create_rw(&path, sz as u64).expect("create_rw");
            ben.iter(|| {
                mmap.touch_pages().expect("touch_pages");
            });
            let _ = fs::remove_file(&path);
        });
    }
    group.finish();
}

fn bench_microflush_overhead(b: &mut Criterion) {
    let mut group = b.benchmark_group("microflush_overhead");
    for &flush_size in &[64_usize, 256, 512, 1024, 2048, 4096, 8192] {
        group.throughput(Throughput::Bytes(flush_size as u64));
        group.bench_with_input(
            BenchmarkId::from_parameter(flush_size),
            &flush_size,
            |ben, &sz| {
                let path = tmp_path(&format!("microflush_{sz}"));
                let _ = fs::remove_file(&path);
                let mmap = MemoryMappedFile::create_rw(&path, 64 * 1024).expect("create_rw");
                let data = vec![0xCD_u8; sz];
                ben.iter(|| {
                    mmap.update_region(0, &data).expect("update");
                    mmap.flush_range(0, sz as u64).expect("flush_range");
                });
                let _ = fs::remove_file(&path);
            },
        );
    }
    group.finish();
}

#[cfg(feature = "advise")]
fn bench_advise(b: &mut Criterion) {
    use mmap_io::advise::MmapAdvice;
    let mut group = b.benchmark_group("advise");
    group.bench_function("sequential_willneed", |ben| {
        let path = tmp_path("advise");
        let _ = fs::remove_file(&path);
        let mmap = MemoryMappedFile::create_rw(&path, 4 * 1024 * 1024).expect("create_rw");
        ben.iter(|| {
            mmap.advise(0, mmap.len(), MmapAdvice::Sequential).ok();
        });
        let _ = fs::remove_file(&path);
    });
    group.finish();
}
#[cfg(not(feature = "advise"))]
fn bench_advise(_: &mut Criterion) {}

#[cfg(feature = "cow")]
fn bench_cow_open(b: &mut Criterion) {
    let mut group = b.benchmark_group("cow_open");
    group.bench_function("open_cow_4MB", |ben| {
        let path = tmp_path("cow_open");
        let _ = fs::remove_file(&path);
        {
            let rw = MemoryMappedFile::create_rw(&path, 4 * 1024 * 1024).expect("create_rw");
            rw.update_region(0, &vec![5u8; 4096]).expect("seed");
            rw.flush().expect("flush");
        }
        ben.iter(|| {
            let cow = MemoryMappedFile::open_cow(&path).expect("open_cow");
            criterion::black_box(cow);
        });
        let _ = fs::remove_file(&path);
    });
    group.finish();
}
#[cfg(not(feature = "cow"))]
fn bench_cow_open(_: &mut Criterion) {}

fn criterion_config() -> Criterion {
    Criterion::default()
        .sample_size(30)
        .warm_up_time(Duration::from_millis(300))
        .measurement_time(Duration::from_secs(3))
}

criterion_group! {
    name = mmap_benches;
    config = criterion_config();
    targets =
        bench_create_rw,
        bench_update_region_flush,
        bench_read_into_rw,
        bench_as_slice_ro,
        bench_resize,
        bench_sequential_read,
        bench_random_read,
        bench_sequential_write_policies,
        bench_iterator_throughput,
        bench_touch_pages_large,
        bench_atomic_contention,
        bench_touch_pages,
        bench_microflush_overhead,
        bench_advise,
        bench_cow_open
}

criterion_main!(mmap_benches);
