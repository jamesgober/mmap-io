use criterion::{criterion_group, criterion_main, BatchSize, BenchmarkId, Criterion, Throughput};
use mmap_io::MemoryMappedFile;
use std::fs;
use std::path::PathBuf;

// Simple helper to build a unique temp path per bench
fn tmp_path(name: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!("mmap_io_bench_{}_{}", name, std::process::id()));
    p
}

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
    use mmap_io::flush::FlushPolicy;

    let mut group = b.benchmark_group("update_region_flush");
    for &size in &[4_usize * 1024, 64 * 1024, 1024 * 1024] {
        group.throughput(Throughput::Bytes(size as u64));
        // Variant A: updates without flush (manual/batched control)
        group.bench_with_input(BenchmarkId::new("update_only", size), &size, |ben, &sz| {
            let path = tmp_path(&format!("update_only_{sz}"));
            let _ = fs::remove_file(&path);
            let mmap = MemoryMappedFile::create_rw(&path, sz as u64).expect("create_rw");

            let payload = vec![0xAB_u8; sz];
            ben.iter(|| {
                mmap.update_region(0, &payload).expect("update");
                // No flush here: caller controls durability policy
                criterion::black_box(&payload);
            });

            let _ = fs::remove_file(&path);
        });

        // Variant B: updates with flush to measure sync overhead
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

        // Variant C: threshold-based automatic flushing via builder
        group.bench_with_input(
            BenchmarkId::new("update_threshold", size),
            &size,
            |ben, &sz| {
                let path = tmp_path(&format!("update_threshold_{sz}"));
                let _ = fs::remove_file(&path);

                // Use builder to set a byte-threshold flush policy equal to the write size
                let mmap = mmap_io::MemoryMappedFile::builder(&path)
                    .mode(mmap_io::MmapMode::ReadWrite)
                    .size(sz as u64)
                    .flush_policy(FlushPolicy::EveryBytes(sz)) // flush roughly once per write
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
            // Seed some data
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
            // Create file and seed data
            {
                let mmap = MemoryMappedFile::create_rw(&path, sz as u64).expect("create_rw");
                mmap.update_region(0, &vec![2u8; sz]).expect("seed");
                mmap.flush().expect("flush");
            }
            // Open RO
            let ro = MemoryMappedFile::open_ro(&path).expect("open_ro");

            ben.iter(|| {
                let s = ro.as_slice(0, sz as u64).expect("as_slice");
                criterion::black_box(s);
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

fn bench_page_fault_costs(b: &mut Criterion) {
    let mut group = b.benchmark_group("page_fault_costs");

    // Test different block sizes to investigate allocator/page fault costs
    for &block_size in &[4_usize * 1024, 8 * 1024, 16 * 1024, 32 * 1024, 64 * 1024] {
        group.throughput(Throughput::Bytes(block_size as u64));

        // Test with cold pages (no prewarm)
        group.bench_with_input(
            BenchmarkId::new("cold_pages", block_size),
            &block_size,
            |ben, &sz| {
                let total_size = 16 * 1024 * 1024; // 16MB total

                ben.iter_batched(
                    || {
                        // Create fresh mapping for each iteration to ensure cold pages
                        let path =
                            tmp_path(&format!("cold_pages_iter_{}_{}", sz, std::process::id()));
                        let _ = fs::remove_file(&path);
                        let mmap =
                            MemoryMappedFile::create_rw(&path, total_size).expect("create_rw");
                        (path, mmap)
                    },
                    |(path, mmap)| {
                        // Access memory in block-sized chunks to trigger page faults
                        let data = vec![0xAB_u8; sz];
                        for offset in (0..total_size).step_by(sz) {
                            let write_size = std::cmp::min(sz as u64, total_size - offset);
                            mmap.update_region(offset, &data[..write_size as usize])
                                .expect("update");
                        }
                        let _ = fs::remove_file(&path);
                    },
                    BatchSize::SmallInput,
                );
            },
        );

        // Test with warm pages (prewarmed)
        group.bench_with_input(
            BenchmarkId::new("warm_pages", block_size),
            &block_size,
            |ben, &sz| {
                let total_size = 16 * 1024 * 1024; // 16MB total

                ben.iter_batched(
                    || {
                        let path =
                            tmp_path(&format!("warm_pages_iter_{}_{}", sz, std::process::id()));
                        let _ = fs::remove_file(&path);
                        let mmap =
                            MemoryMappedFile::create_rw(&path, total_size).expect("create_rw");
                        // Prewarm all pages
                        mmap.touch_pages().expect("touch_pages");
                        (path, mmap)
                    },
                    |(path, mmap)| {
                        // Access memory in block-sized chunks with prewarmed pages
                        let data = vec![0xAC_u8; sz];
                        for offset in (0..total_size).step_by(sz) {
                            let write_size = std::cmp::min(sz as u64, total_size - offset);
                            mmap.update_region(offset, &data[..write_size as usize])
                                .expect("update");
                        }
                        let _ = fs::remove_file(&path);
                    },
                    BatchSize::SmallInput,
                );
            },
        );
    }
    group.finish();
}

fn bench_microflush_overhead(b: &mut Criterion) {
    let mut group = b.benchmark_group("microflush_overhead");

    // Test different flush sizes to measure microflush optimization
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
                    // Write then flush small range to test microflush optimization
                    mmap.update_region(0, &data).expect("update");
                    mmap.flush_range(0, sz as u64).expect("flush_range");
                });

                let _ = fs::remove_file(&path);
            },
        );
    }
    group.finish();
}

#[cfg(feature = "iterator")]
fn bench_iterator_chunks(b: &mut Criterion) {
    let mut group = b.benchmark_group("iterator_chunks");
    let sz = 4 * 1024 * 1024;
    group.throughput(Throughput::Bytes(sz as u64));
    group.bench_function("iterate_4MB_by_64KB", |ben| {
        let path = tmp_path("iter_chunks");
        let _ = fs::remove_file(&path);
        let mmap = MemoryMappedFile::create_rw(&path, sz as u64).expect("create_rw");
        mmap.update_region(0, &vec![3u8; sz]).expect("seed");
        ben.iter(|| {
            // Use public API to iterate over chunks
            let mut total = 0usize;
            for chunk_res in mmap.chunks(64 * 1024) {
                let chunk: Vec<u8> = chunk_res.expect("chunk");
                total += chunk.len();
                criterion::black_box(&chunk);
            }
            criterion::black_box(total);
        });
        let _ = fs::remove_file(&path);
    });
    group.finish();
}
#[cfg(not(feature = "iterator"))]
fn bench_iterator_chunks(_: &mut Criterion) {}

#[cfg(feature = "advise")]
fn bench_advise(b: &mut Criterion) {
    use mmap_io::advise::MmapAdvice;
    let mut group = b.benchmark_group("advise");
    group.bench_function("sequential_willneed", |ben| {
        let path = tmp_path("advise");
        let _ = fs::remove_file(&path);
        let mmap = MemoryMappedFile::create_rw(&path, 4 * 1024 * 1024).expect("create_rw");
        ben.iter(|| {
            // Measure advise over full region
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
        .warm_up_time(std::time::Duration::from_millis(300))
        .measurement_time(std::time::Duration::from_secs(3))
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
        bench_touch_pages,
        bench_page_fault_costs,
        bench_microflush_overhead,
        bench_iterator_chunks,
        bench_advise,
        bench_cow_open
}

criterion_main!(mmap_benches);
