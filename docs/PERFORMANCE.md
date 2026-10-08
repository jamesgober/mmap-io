# mmap-io Performance

Measured numbers for the public API surface. Run `cargo bench --all-features --bench mmap_bench` to reproduce on your own machine.

Headline numbers (re-measured for 1.1):

- **Iterator zero-copy redesign (audit H1)**: 40-2000x faster than the `chunks_owned()` path depending on chunk size.
- **Unified `as_slice` on RW (audit H4)**: 30-45x faster than the `read_into` (memcpy) path for sequential reads.
- **`touch_pages` tight loop (audit H2)**: 1 GiB in about 2.6 ms.
- **`flush()` is a real, synchronous flush**: roughly 0.4-1.3 ms on the Windows reference machine and 0.9-2 ms on the Linux one for small and medium dirty ranges. Earlier versions of this file reported a "36 ns microflush"; that number measured a flush that never reached the OS (see below).

## Reference machines

Two machines, same hardware. Re-run the benches on your target hardware for production sizing; flush numbers in particular depend on the storage device and filesystem far more than on this crate.

- **Windows**: Windows 11 Pro 26200, NTFS on SSD.
- **Linux**: WSL2 Ubuntu on the same box, ext4 on a virtual disk (VHDX) backed by the same SSD. WSL2's virtual disk adds latency to synchronous flushes; bare-metal Linux on an NVMe SSD is typically faster.
- **Toolchain**: stable Rust (1.75 MSRV verified for the library).
- **Bench harness**: criterion 0.5 with 30 samples per measurement, 300 ms warm-up, 3 s measurement window.

Unless a table says otherwise, numbers are Windows point estimates.

## The big wins

### Iterator zero-copy redesign (audit H1)

The `chunks()` and `pages()` iterators yield `MappedSlice<'a>` (zero-copy borrow into the mapping) instead of `Result<Vec<u8>>` (heap allocation + memcpy per chunk). The migration aid `chunks_owned()` preserves the old shape for callers that need owned buffers.

Measured against a 16 MiB RO file:

| Chunk size | Zero-copy (`chunks`) | Owned (`chunks_owned`) | Speedup |
|------------|---------------------|------------------------|---------|
| 4 KiB      | **7.8 µs**          | 311-458 µs             | **40-60x** |
| 64 KiB     | **0.49 µs**         | 226-982 µs             | **460-2000x** |
| page (4 KiB) | **8.6 µs**        | (see 4 KiB row)        | n/a     |

The owned column varies a lot between runs because it is dominated by the allocator; the zero-copy column is stable. Since 1.1, `MappedSlice` caches its slice pointer at construction instead of re-indexing on every deref, which made the zero-copy rows about 3x faster than in 1.0 (25.8 µs / 2.0 µs / 23.9 µs). On RW mappings each yielded item also takes its own (recursive) read lock, so it stays valid if kept after the iterator is dropped.

### Unified `as_slice` on RW mappings (audit H4)

`as_slice` works on every mode and returns a `MappedSlice<'_>` that derefs to `&[u8]`. For sequential scans this eliminates one full memcpy of the data:

| File size | `as_slice` | `read_into` (memcpy) | Speedup |
|-----------|-----------|----------------------|---------|
| 1 MiB     | **0.34 µs** | 13.7 µs            | **40x** |
| 16 MiB    | **18.5 µs** | 739 µs             | **40x** |
| 256 MiB   | **0.45 ms** | 14.7 ms            | **33x** |

The `as_slice` column touches one byte per 4 KiB page, so it measures page-table and TLB behavior; it varies by up to 30% between runs (also between runs of the same build).

Random-access reads (16 MiB RO file):

| Request size | `as_slice` | `read_into` | Speedup |
|--------------|-----------|-------------|---------|
| 64 B         | **11 ns** | 29 ns       | **2.5x** |
| 256 B        | **11 ns** | 29 ns       | **2.8x** |
| 4 KiB        | **17 ns** | 69 ns       | **4x**  |
| 64 KiB       | **16 ns** | 1.1 µs      | **70x** |

Through 1.0 `read_into` won below about 1 KiB because building a `MappedSlice` cost 50-100 ns. With the cached slice pointer and the inlined accessors, `as_slice` is now ahead at every size.

### `touch_pages` tight loop (audit H2)

`touch_pages` acquires the lock once and walks the mapping with `ptr::read_volatile` wrapped in `std::hint::black_box`:

| File size | Time   |
|-----------|--------|
| 1 MiB     | 0.35 µs |
| 8 MiB     | 9-11 µs |
| 32 MiB    | 50 µs  |
| 1 GiB (RW) | **2.6 ms** |

These are not memory-bandwidth measurements. The OS only faults pages that aren't already resident; once they're warm, touching them is bounded by the page-table walk and the single volatile byte read per page. Cold runs (when the file isn't in cache) are bounded by the storage read rate.

## Flush cost

Since 1.1, `flush()` and `flush_range()` on a ReadWrite mapping always flush synchronously: `msync(MS_SYNC)` on Unix, `FlushViewOfFile` + `FlushFileBuffers` on Windows. Before 1.1 they returned early whenever the crate's dirty-byte counter was zero, and under the default `FlushPolicy::Never` / `Manual` that counter was never incremented, so an explicit flush after `update_region` did nothing. On Linux the non-skipped path used `msync(MS_ASYNC)`, which only schedules writeback. The 1.0 numbers below therefore measure a no-op.

`flush_range` after a write of the given size (64 KiB file):

| Range size | 1.0 (no-op) | 1.1 Windows | 1.1 Linux (WSL2) |
|------------|-------------|-------------|------------------|
| 64 B       | 36 ns       | 0.43 ms     | 2.1 ms           |
| 256 B      | 37 ns       | 0.47 ms     | 1.1 ms           |
| 1 KiB      | 39 ns       | 0.50 ms     | 1.2 ms           |
| 4 KiB      | 52 ns       | 0.78 ms     | 1.2 ms           |
| 8 KiB      | 67 ns       | 1.28 ms     | 1.1 ms           |

The time is the storage round-trip, not the byte count: the kernel flushes whole pages, and on Windows `FlushFileBuffers` flushes the whole file's buffers whatever the range. A sub-page range costs the same as a page.

`update_region` of the whole file followed by `flush()`:

| File size | 1.0 (no-op) | 1.1 Windows | 1.1 Linux (WSL2) |
|-----------|-------------|-------------|------------------|
| 4 KiB     | 43 ns       | 0.47 ms     | 0.88 ms          |
| 64 KiB    | 0.59 µs     | 0.44 ms     | 0.87 ms          |
| 1 MiB     | 16 µs       | 0.87 ms     | 1.75 ms          |

`update_region` alone costs 34 ns / 0.6 µs / 16 µs for the same sizes. Batch writes and flush once per batch; every flush is a synchronous write-back.

## Cost of the atomic / plain view check (1.1)

Since 1.1 a plain view (`MappedSlice` from `as_slice`, an iterator item) and an atomic view of the same bytes cannot coexist (see `docs/SAFETY.md`, category 9). With the `atomic` feature enabled, every plain view of a `ReadWrite` / `CopyOnWrite` mapping registers its byte range for its lifetime, and copying reads (`read_into`) hold the registry's read lock during the copy. Without the `atomic` feature the check compiles to nothing. `ReadOnly` mappings are never affected.

Per-operation cost on a 16 MiB `ReadWrite` mapping, hot cache (a tight loop of 5 million calls at 64 offsets, `--release`; the criterion group `rw_views` measures the same operations at random offsets, where cache misses dominate):

| Operation (Windows / Linux WSL2) | before the check | 1.1, `atomic` off | 1.1, `atomic` on |
|-----------|--------------------:|------------------:|-----------------:|
| `as_slice(off, 64)` + drop | 9.3 / 9.2 ns | 9.0 / 9.0 ns | 37.8 / 40.3 ns |
| `read_into` 64 B | 9.7 / 10.1 ns | 9.2 / 9.7 ns | 18.5 / 20.0 ns |
| `chunks(4096)` per item | 9.6 / 9.4 ns | 9.4 / 9.8 ns | 34.3 / 37.5 ns |

Each cell is Windows / Linux. "Before" is the 1.1 branch just before the check was added (same raw layer, same locking); with `atomic` on it measured 9.2 / 12.5 ns for `as_slice`. A 16 MiB scan with 4 KiB chunks therefore goes from about 40 µs to about 155 µs on a RW mapping when `atomic` is enabled (criterion `rw_views/chunks_4096`, Windows); RO mappings stay at 8-10 µs. The added time is one uncontended shard lock to register and one to deregister per view (the shards keep concurrent readers on different cache lines), plus a read lock of the registry's atomic set per copying read. REPS.md section 2 puts safety ahead of speed here: without the check, an atomic store racing a plain read of the same bytes is undefined behavior that safe code could reach.

## Starting write-back without waiting

`schedule_flush_range` (1.1) starts write-back and returns; it is not durable. `flush_range` waits for durability. Each iteration rewrites the range first (`update_region`), so there is always something dirty; the `write only` column is that write alone. Criterion medians, 1.1.

| Range | `flush_range` Windows | `schedule_flush_range` Windows | write only Windows | `flush_range` Linux (WSL2) | `schedule_flush_range` Linux (WSL2) | write only Linux |
|-------|----------------------:|-------------------------------:|-------------------:|---------------------------:|------------------------------------:|-----------------:|
| 4 KiB | 6.0 ms | 297 µs | 66 ns | 4.68 ms | 282 ns | 80 ns |
| 1 MiB | 4.8 ms | 2.06 ms | 178 µs | 6.77 ms | 34.3 µs | 22.7 µs |

- **Linux**: `sync_file_range(SYNC_FILE_RANGE_WRITE)` queues the pages and returns: about 0.2 µs for one page, 12 µs for 256 pages, versus milliseconds for the durable flush. Unlike `msync(MS_ASYNC)` (a no-op on Linux), it really starts write-out: `tests/schedule_flush.rs` watches the mapping's dirty page count in `/proc/self/smaps` drop right after the call, while without it 16 MiB stayed dirty for the full 5 s observed.
- **Windows**: `FlushViewOfFile` hands the pages to the cache manager and skips `FlushFileBuffers`, so it is about 2.5-20x cheaper than the durable flush here, but it still issues the writes synchronously to the file system cache and is not free.
- These Windows flush numbers are higher than the `flush_range` table above (0.4-1.3 ms) because the machine was under other load during this run; compare columns within one table, not across tables.

## Atomic operations

`atomic_u64::fetch_add` under N-thread contention, 10,000 ops per thread:

| Threads | Total time | ns/op |
|---------|-----------|-------|
| 1       | 138 µs    | 13.8 ns |
| 2       | 196 µs    | 9.8 ns |
| 4       | 370 µs    | 9.2 ns |
| 8       | 694 µs    | 8.7 ns |

All threads contend on the same cache line, so the cache-coherence protocol serialises the writes. Spread your counters across separate cache lines if you need higher throughput. Atomic views require a ReadWrite mapping.

## Sequential writes under different flush policies

4 MiB total write, 64 KiB per `update_region` call:

| Policy                  | 1.0 Windows | 1.1 Windows | 1.0 Linux | 1.1 Linux (WSL2) | Notes |
|-------------------------|-------------|-------------|-----------|------------------|-------|
| `Manual`                | 56 µs       | 52 µs       | 53 µs     | 57 µs            | No syscalls in the write loop |
| `EveryMillis(10)`       | 71 µs       | 58 µs       | 55 µs     | 80 µs            | Background thread flushes every 10 ms; on Linux each flush now holds the read lock for a synchronous `msync`, which writers wait for |
| `EveryBytes(64 KiB)`    | 19 ms       | 19.6 ms     | 62 µs     | **58 ms**        | 64 synchronous flushes (one per write) |

`EveryBytes(64 KiB)` flushes after every 64 KiB write. On Windows it was already synchronous in 1.0 (the counter was non-zero, so memmap2's flush ran); on Linux 1.0 used `msync(MS_ASYNC)`, which is why its 62 µs was not a durable flush. If you need bounded-by-bytes durability, prefer a larger threshold (1 MiB+) so each flush covers more writes.

## File operations

| Operation | Windows | Linux (WSL2) |
|-----------|---------|--------------|
| `create_rw` 4 KiB - 1 MiB | 0.25-0.7 ms (very noisy) | 13 µs |
| `open_cow` 4 MiB | 28-63 µs | 2.7 µs |
| `resize` (1 MiB -> 8 MiB -> 1 MiB) | 77 µs | 6 µs |
| `advise` (Sequential, 4 MiB) | 11 ns (no-op on Windows) | 74 ns |
| `read_into_rw` 4 KiB | 82 ns | 24 ns |
| `read_into_rw` 64 KiB | 0.60 µs | 0.57 µs |
| `read_into_rw` 1 MiB | 15 µs | 15 µs |

`create_rw` on Windows is dominated by file-creation overhead and varies by more than 2x between runs. `resize` on Windows got slower in 1.1 (24 µs in 1.0) because shrinking now really truncates the file: the view is unmapped, the file truncated, and the prefix remapped. In 1.0 a Windows shrink only lowered the cached length, which left the file at its old size and broke a later grow.

## Notes on reading these numbers

- **Throughput numbers in the criterion HTML report are misleading for partial-touch benchmarks.** When a bench iterates over a 16 MiB file but reads only `slice[0]` from each chunk (1 byte), criterion reports throughput as if all 16 MiB were processed. The wall-clock time is the honest number.
- **Cold vs warm runs differ by an order of magnitude on some metrics.** First access to a file pays page-fault cost; warm access is in the page cache. `touch_pages` exists precisely to convert cold to warm at a controlled moment.
- **Numbers above are point estimates** (criterion's `point_estimate` field). Run-to-run variation on these machines was up to 30% for memory-bound benches and over 2x for file creation; use the criterion HTML reports for tighter analysis.

## Reproducing

```sh
# Full suite, save a baseline:
cargo bench --all-features --bench mmap_bench -- --save-baseline mine

# Compare two baselines:
cargo install critcmp
critcmp mine your-branch
```

CI runs the full bench suite on every push to `main` and PR, uploads the criterion results as a build artifact, and compares the PR head against the merge-base on the same runner. A regression of more than 10% on any benchmark (the REPS.md limit) fails the check. See `.github/workflows/bench-regression.yml`.
