# mmap-io Performance

Measured numbers for the public API surface. Run `cargo bench --all-features --bench mmap_bench` to reproduce on your own machine.

The headline wins from the 0.9.5 → 0.9.10 audit work are now backed by real numbers, not just theory:

- **Iterator zero-copy redesign (audit H1)**: 13-475x faster than the old `chunks_owned()` path depending on chunk size.
- **Unified `as_slice` on RW (audit H4)**: 15-49x faster than the `read_into` (memcpy) path for sequential reads.
- **`touch_pages` tight loop (audit H2)**: 1 GiB in 2 ms (≈500 GiB/s effective rate, dominated by RAM bandwidth and cache behaviour).
- **Microflush optimisation**: sub-100 ns end-to-end for small range flushes.

## Reference machine

These numbers are from a Windows 11 development box. Linux numbers are typically 1.2x-2x faster on the syscall-heavy paths (advise, lock, flush) thanks to faster system call entry; the user-space-only paths (`as_slice`, iterator walking, atomic operations) are within noise of the same. Re-run the benches on your target hardware for production sizing.

- **OS**: Windows 11 Pro 26200
- **Toolchain**: stable Rust (1.75 MSRV verified)
- **Filesystem**: NTFS on SSD
- **Bench harness**: criterion 0.5 with 30 samples per measurement, 300 ms warm-up, 3 s measurement window

## The big wins

### Iterator zero-copy redesign (audit H1)

The `chunks()` and `pages()` iterators changed in 0.9.7 from yielding `Result<Vec<u8>>` (heap allocation + memcpy per chunk) to yielding `MappedSlice<'a>` (zero-copy borrow into the mapping). The migration aid `chunks_owned()` preserves the old shape for callers that need owned buffers.

Measured against a 16 MiB RO file:

| Chunk size | Zero-copy (`chunks`) | Owned (`chunks_owned`) | Speedup |
|------------|---------------------|------------------------|---------|
| 4 KiB      | **25.8 µs**         | 341.2 µs               | **13.2x** |
| 64 KiB     | **2.0 µs**          | 962.3 µs               | **475x** |
| page (4 KiB) | **23.9 µs**       | (see 4 KiB row)        | —       |

The 64 KiB result is more extreme because the owned-chunk path's allocator overhead grows with the size of each `Vec<u8>` allocation. Zero-copy stays in the low microseconds because it's pure pointer arithmetic; the allocator overhead disappears.

### Unified `as_slice` on RW mappings (audit H4)

Through 0.9.6 RW mappings forced you to use `read_into` (memcpy) for reads. Since 0.9.7 `as_slice` works on every mode and returns a `MappedSlice<'_>` that derefs to `&[u8]`. For sequential scans this eliminates one full memcpy of the data:

| File size | `as_slice` | `read_into` (memcpy) | Speedup |
|-----------|-----------|----------------------|---------|
| 1 MiB     | **615 ns** | 19.6 µs              | **32x** |
| 16 MiB    | **17.2 µs** | 849 µs              | **49x** |
| 256 MiB   | **1.0 ms** | 14.7 ms              | **15x** |

The speedup compresses at larger sizes because the underlying memory walk dominates the wall-clock; the memcpy is no longer the bottleneck. But for small / medium reads (the common case) the win is huge.

Random-access reads see a smaller but still real win:

| Request size | `as_slice` | `read_into` | Speedup |
|--------------|-----------|-------------|---------|
| 64 B         | 55 ns     | 53 ns       | ~1x     |
| 256 B        | 63 ns     | 38 ns       | ~0.6x   |
| 4 KiB        | **56 ns** | 129 ns      | **2.3x** |
| 64 KiB       | **38 ns** | 1.29 µs     | **34x** |

At very small request sizes (64-256 B) `read_into` ties or wins because the memcpy is too small to matter and `as_slice` carries the cost of constructing a `MappedSlice`. The crossover is at ~1 KiB; above that, `as_slice` pulls ahead and stays ahead.

### `touch_pages` tight loop (audit H2)

Pre-0.9.7 `touch_pages` called `read_into(offset, &mut buf[..1])` for each page, which acquired the lock, validated bounds, and memcpy'd a byte 262,144 times for a 1 GiB file. The new implementation acquires the lock ONCE and walks the mapping with `ptr::read_volatile` wrapped in `std::hint::black_box`:

| File size | Time   | Effective rate |
|-----------|--------|----------------|
| 1 MiB     | 354 ns | 2,800 GiB/s    |
| 8 MiB     | 9.5 µs | 832 GiB/s      |
| 32 MiB    | 48 µs  | 670 GiB/s      |
| 1 GiB     | **2.08 ms** | **481 GiB/s** |

These are not memory-bandwidth measurements. The OS only faults pages that aren't already resident; once they're warm, touching them is bounded by the page-table walk and the single volatile byte read per page (one cache line per stride). The 1 GiB result with the OS already holding the file in the page cache is the realistic upper bound for warm `touch_pages`. Cold runs (when the file isn't in cache) will be bounded by SSD/HDD read rate.

### Microflush optimisation

`flush_range` for sub-page-sized regions expands to page-aligned boundaries to batch the underlying `msync` / `FlushViewOfFile` call:

| Flush size | Time  | Effective rate |
|------------|-------|----------------|
| 64 B       | 36 ns | 1.6 GiB/s      |
| 256 B      | 37 ns | 6.4 GiB/s      |
| 512 B      | 42 ns | 10.8 GiB/s     |
| 1 KiB      | 40 ns | 23.7 GiB/s     |
| 2 KiB      | 47 ns | 40.7 GiB/s     |
| 4 KiB      | 54 ns | 73.0 GiB/s     |
| 8 KiB      | 72 ns | 112 GiB/s      |

The wall-clock is dominated by the syscall round-trip (msync on Linux / FlushViewOfFile on Windows). The fact that the time barely moves between 64 B and 4 KiB tells you the syscall itself is the floor, not the byte count.

## Atomic operations

`atomic_u64::fetch_add` under N-thread contention, 10,000 ops per thread (60,000 / 100,000 ops total):

| Threads | Total time | ns/op | Scaling |
|---------|-----------|-------|---------|
| 1       | 100 µs    | 10 ns | baseline |
| 2       | 171 µs    | 8.5 ns | 0.85x per thread |
| 4       | 334 µs    | 8.3 ns | 0.83x per thread |
| 8       | 620 µs    | 7.75 ns | 0.77x per thread |

Scaling is sub-linear because all threads contend on the same cache line. This is exactly how a single shared atomic should behave: the cache-coherence protocol serialises the writes. Spread your counters across separate cache lines if you need higher throughput.

## Sequential writes under different flush policies

4 MiB total write, 64 KiB per `update_region` call:

| Policy                  | Total time | Notes |
|-------------------------|-----------|-------|
| `Manual`                | 64 µs     | Fastest: no syscalls in the write loop |
| `EveryMillis(10)`       | 84 µs     | Background flush thread runs every 10 ms; almost no overhead on the write path |
| `EveryBytes(64 KiB)`    | **17.3 ms** | Flushes once per `update_region` call (32 flushes for 4 MiB at 64 KiB each); 200x slower |

The `EveryBytes` policy is much slower than `Manual` because it forces a flush after every write. If you need bounded-by-bytes durability, prefer a larger threshold (1 MiB+) so the flush amortises across more writes.

## File operations

| Operation | Time |
|-----------|------|
| `create_rw` 4 KiB | 399 µs |
| `create_rw` 64 KiB | 237 µs |
| `create_rw` 1 MiB | 246 µs |
| `open_cow` 4 MiB | 28.7 µs |
| `resize` (1 MiB grow + shrink) | 29.4 µs |
| `advise` (sequential WillNeed) | 23 ns |
| `read_into_rw` 4 KiB | 39 ns |
| `read_into_rw` 64 KiB | 540 ns |
| `read_into_rw` 1 MiB | 14.2 µs |

`create_rw` is dominated by Windows file-creation overhead; the actual mmap setup is sub-microsecond. The smallest size (4 KiB) is paradoxically slower because the small file forces extra metadata allocations.

`advise` at 23 ns is the cost of one syscall round-trip. Whether to issue the advice depends on workload: it pays off when the OS prefetcher would otherwise miss your access pattern (random reads especially benefit from `WillNeed` if you can predict the offset).

## Notes on reading these numbers

- **Throughput numbers in the criterion HTML report are misleading for partial-touch benchmarks.** When a bench iterates over a 16 MiB file but reads only `slice[0]` from each chunk (1 byte), criterion reports throughput as if all 16 MiB were processed. The wall-clock time is the honest number; throughput is "criterion's framing of the unit-of-work" and depends on how the bench was written.
- **Cold vs warm runs differ by an order of magnitude on some metrics.** First access to a file pays page-fault cost; warm access is in the page cache. `touch_pages` exists precisely to convert cold to warm at a controlled moment.
- **Numbers above are point estimates** (criterion's `point_estimate` field). The confidence intervals are typically ±2-10% of the point estimate; use the criterion HTML reports for tighter analysis.

## Reproducing

```sh
# Full suite, save baseline named "0.9.10":
cargo bench --all-features --bench mmap_bench -- --save-baseline 0.9.10

# Compare two baselines:
cargo install critcmp
critcmp 0.9.10 your-branch
```

CI runs the full bench suite on every push to `main` and PR, uploads the criterion JSON as a build artifact, and (since 0.9.10) compares the PR head against the merge-base on the same runner. A regression of more than 15% on any group fails the check. See `.github/workflows/bench-regression.yml`.
