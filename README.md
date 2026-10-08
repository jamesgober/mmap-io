<h1 align="center">
    <img width="99" alt="Rust logo" src="https://raw.githubusercontent.com/jamesgober/rust-collection/72baabd71f00e14aa9184efcb16fa3deddda3a0a/assets/rust-logo.svg">
    <br>
    <strong>mmap-io</strong>
    <br>
    <sup><sub>MEMORY-MAPPED FILE I/O FOR RUST</sub></sup>
</h1>

<p align="center">
    <a href="https://crates.io/crates/mmap-io"><img alt="crates.io" src="https://img.shields.io/crates/v/mmap-io.svg"></a>
    <a href="https://crates.io/crates/mmap-io"><img alt="downloads" src="https://img.shields.io/crates/d/mmap-io.svg"></a>
    <a href="https://docs.rs/mmap-io"><img alt="docs.rs" src="https://docs.rs/mmap-io/badge.svg"></a>
    <img alt="MSRV" src="https://img.shields.io/badge/MSRV-1.75%2B-blue.svg?style=flat-square" title="Rust Version">
    <a href="https://github.com/jamesgober/mmap-io/actions/workflows/ci.yml"><img alt="CI" src="https://github.com/jamesgober/mmap-io/actions/workflows/ci.yml/badge.svg"></a>
</p>

<p align="center">
    Zero-copy reads. Lock-free atomic views. Safe concurrent access.<br>
    Built for databases, log structures, caches, game runtimes, and shared-memory IPC.
</p>

---

## What you get

- **Zero-copy reads on every mode.** `as_slice` returns a `MappedSlice<'_>` borrowed directly from the mapping. No allocation. No memcpy. Works on read-only, read-write, and copy-on-write mappings uniformly.
- **Zero-allocation iteration.** `mmap.chunks(N)` and `mmap.pages()` walk the file in fixed strides without ever heap-allocating. A 1 GiB scan at 4 KiB chunks skips 262,144 allocations and half the memory bandwidth of the naive approach.
- **Aligned atomic views.** On read-write mappings, `atomic_u32` / `atomic_u64` return a wrapper that derefs to `&AtomicU64`. Multi-thread `fetch_add` over a memory-mapped counter is one cache-line ping; no cross-process locking required.
- **Configurable durability.** `flush()` is synchronous (`msync(MS_SYNC)` on Unix, `FlushViewOfFile` + `FlushFileBuffers` on Windows). `FlushPolicy::EveryBytes(N)`, `EveryWrites(N)`, `EveryMillis(N)`, `Always`, or `Manual` decide when the crate flushes for you; the millis policy runs a background flusher bound to the mapping's lifetime.
- **Thread-safe.** Interior mutability via `parking_lot::RwLock`. Multiple concurrent readers, one writer at a time. Every live read view (slice, iterator item, atomic view) blocks writes and `resize()` until released, so memory under your reference cannot move.
- **Anonymous mappings.** Process-local memory without a backing file via `AnonymousMmap::new(size)` for shared scratch buffers between threads, large temporary allocations, or as the kernel substrate for IPC patterns.
- **Cross-platform.** Linux, macOS, Windows. Per-platform hooks where they exist (`MADV_HUGEPAGE` for the huge-page hint, `posix_fadvise` for OS-level prefetch on Linux).
- **Opt-in surface.** Default features are `advise` + `iterator`. Everything else (`async`, `atomic`, `cow`, `locking`, `watch`, `hugepages`) is off by default to keep compile time tight.
- **MSRV: 1.75.** Pinned and verified in CI.

## Quick start

```toml
[dependencies]
mmap-io = "1.0"
```

```rust
use mmap_io::MemoryMappedFile;

fn main() -> Result<(), mmap_io::MmapIoError> {
    // Open an existing file in read-only mode.
    let mmap = MemoryMappedFile::open_ro("data.bin")?;

    // Zero-copy read of the first 16 bytes. `slice` derefs to &[u8].
    let slice = mmap.as_slice(0, 16)?;
    println!("First bytes: {:?}", &*slice);

    Ok(())
}
```

Open-or-create with one call:

```rust
use mmap_io::MemoryMappedFile;

fn main() -> Result<(), mmap_io::MmapIoError> {
    // Opens "data.bin" if it exists; creates it at 1 MiB otherwise.
    let mmap = MemoryMappedFile::open_or_create("data.bin", 1024 * 1024)?;

    mmap.update_region(100, b"Hello, mmap!")?;
    mmap.flush()?;
    Ok(())
}
```

## Optional features

| Feature     | Description                                                                                         |
|-------------|-----------------------------------------------------------------------------------------------------|
| `async`     | Runtime-agnostic async helpers via the `blocking` crate. Works on tokio, smol, async-std, or any executor. |
| `bytes`     | `bytes::Bytes` conversion for plugging into the hyper/tower/tonic/axum/reqwest ecosystem. |
| `advise`    | Memory hinting via `madvise`/`posix_madvise` (Unix) or `PrefetchVirtualMemory` (Windows).            |
| `iterator`  | Iterator-based access to memory chunks or pages with zero-copy reads.                                |
| `hugepages` | Transparent huge page hint (`madvise(MADV_HUGEPAGE)`) on Linux RW mappings; no effect on other platforms. |
| `cow`       | Copy-on-Write mapping mode: writable private per-process views whose changes never reach the file.  |
| `locking`   | Page-level memory locking via `mlock`/`munlock` (Unix) or `VirtualLock` (Windows).                   |
| `atomic`    | Atomic views into memory as aligned `u32` / `u64` with strict alignment checks.                      |
| `watch`     | Native file-change notifications: `inotify` (Linux), FSEvents (macOS), `ReadDirectoryChangesW` (Windows). |

> Features are opt-in. Enable only those relevant to your use case to reduce compile time and dependency footprint.

### Default features

By default, the following features are enabled:

- `advise`: memory access hinting for performance.
- `iterator`: iterator-based chunk/page access.

## Installation patterns

Default features:

```toml
[dependencies]
mmap-io = "1.0"
```

Enable async helpers:

```toml
[dependencies]
mmap-io = { version = "1", features = ["async"] }
```

Multiple features:

```toml
[dependencies]
mmap-io = { version = "1", features = ["cow", "locking"] }
```

Minimal: disable defaults, opt into only what you need:

```toml
[dependencies]
mmap-io = { version = "1", default-features = false, features = ["locking"] }
```

## Flush Policy

`mmap.flush()` on a ReadWrite mapping always writes dirty pages back and waits for the OS to report them written: `msync(MS_SYNC)` on Unix, `FlushViewOfFile` + `FlushFileBuffers` on Windows. (On macOS, `msync` does not issue `F_FULLFSYNC`, so the drive's own cache may still hold the data.) `flush_range(offset, len)` does the same for one range. Expect milliseconds, not nanoseconds; see [docs/PERFORMANCE.md](./docs/PERFORMANCE.md).

`FlushPolicy` only decides when the crate flushes *for you*, letting you trade durability for throughput:

- **`FlushPolicy::Never`** / **`FlushPolicy::Manual`** (default): no automatic flushes. Call `mmap.flush()` when you want durability.
- **`FlushPolicy::Always`**: flush after every `update_region()`; slowest but most durable.
- **`FlushPolicy::EveryBytes(n)`**: flush after the `update_region()` call that brings the bytes written since the last flush to at least `n`.
- **`FlushPolicy::EveryWrites(n)`**: flush after every `n` `update_region()` calls.
- **`FlushPolicy::EveryMillis(ms)`**: a background thread flushes every `ms` milliseconds when anything is pending.

`mmap.pending_bytes()` counts bytes written since the last flush through every write path (`update_region`, `as_slice_mut`, `chunks_mut`, atomic views, `as_mut_ptr`); policies use it, explicit `flush()` ignores it.

Builder usage:

```rust
use mmap_io::{MemoryMappedFile, MmapMode};
use mmap_io::flush::FlushPolicy;

let mmap = MemoryMappedFile::builder("file.bin")
    .mode(MmapMode::ReadWrite)
    .size(1_000_000)
    .flush_policy(FlushPolicy::EveryBytes(256 * 1024)) // flush every 256KB written
    .create()?;
```

Manual flush:

```rust
use mmap_io::{create_mmap, update_region, flush};

let mmap = create_mmap("data.bin", 1024 * 1024)?;
update_region(&mmap, 0, b"batch1")?;
// ... more batched writes ...
flush(&mmap)?; // ensure durability now
```

> [!NOTE]
> Visibility and durability are different things. Other mappings and readers of the same file on the same machine see your writes immediately through the shared page cache, flushed or not. A flush is what makes them survive a crash or power loss; without one, the OS writes them back whenever it chooses.

## Round-trip example

Create a file, write to it, and read back:

```rust
use mmap_io::{create_mmap, update_region, flush, load_mmap, MmapMode};

fn main() -> Result<(), mmap_io::MmapIoError> {
    // Create a 1MB memory-mapped file
    let mmap = create_mmap("data.bin", 1024 * 1024)?;

    // Write data at offset 100
    update_region(&mmap, 100, b"Hello, mmap!")?;

    // Persist to disk
    flush(&mmap)?;

    // Open read-only and verify
    let ro = load_mmap("data.bin", MmapMode::ReadOnly)?;
    let slice = ro.as_slice(100, 12)?;
    assert_eq!(slice, b"Hello, mmap!");

    Ok(())
}
```

## Memory Advise (`feature = "advise"`)

Optimize OS-level memory access patterns:

```rust
#[cfg(feature = "advise")]
use mmap_io::{create_mmap, MmapAdvice};

fn main() -> Result<(), mmap_io::MmapIoError> {
    let mmap = create_mmap("data.bin", 1024 * 1024)?;

    // Advise sequential access for better prefetching
    mmap.advise(0, 1024 * 1024, MmapAdvice::Sequential)?;

    // Process file sequentially...

    // Advise that we won't need this region soon
    mmap.advise(0, 512 * 1024, MmapAdvice::DontNeed)?;

    Ok(())
}
```

## Iterator-Based Access (`feature = "iterator"`)

Process files in chunks or pages:

```rust
#[cfg(feature = "iterator")]
use mmap_io::create_mmap;

fn main() -> Result<(), mmap_io::MmapIoError> {
    let mmap = create_mmap("large_file.bin", 10 * 1024 * 1024)?;

    // Process file in 1MB chunks. Each item is a zero-copy MappedSlice
    // that derefs to &[u8].
    for (i, chunk) in mmap.chunks(1024 * 1024).enumerate() {
        println!("Processing chunk {i} with {} bytes", chunk.len());
    }

    // Process file page by page (OS-optimal)
    for page in mmap.pages() {
        let _first = page.first();
        // Process page...
    }

    Ok(())
}
```

## Page Pre-warming

Eliminate page-fault latency by pre-warming pages into memory before a critical section:

```rust
use mmap_io::{MemoryMappedFile, MmapMode, TouchHint};

fn main() -> Result<(), mmap_io::MmapIoError> {
    // Eagerly pre-warm all pages on creation for benchmarks
    let mmap = MemoryMappedFile::builder("benchmark.bin")
        .mode(MmapMode::ReadWrite)
        .size(1024 * 1024)
        .touch_hint(TouchHint::Eager)
        .create()?;

    // Manually pre-warm a specific range before a critical operation
    mmap.touch_pages_range(0, 512 * 1024)?;

    Ok(())
}
```

## Atomic Operations (`feature = "atomic"`)

Lock-free concurrent access at aligned offsets:

```rust
#[cfg(feature = "atomic")]
use mmap_io::create_mmap;
use std::sync::atomic::Ordering;

fn main() -> Result<(), mmap_io::MmapIoError> {
    let mmap = create_mmap("counters.bin", 64)?;

    // Get atomic view of u64 at offset 0
    let counter = mmap.atomic_u64(0)?;
    counter.store(0, Ordering::SeqCst);

    // Increment atomically from multiple threads
    let old = counter.fetch_add(1, Ordering::SeqCst);
    println!("Counter was: {old}");

    Ok(())
}
```

## Memory Locking (`feature = "locking"`)

Prevent pages from being swapped (requires elevated privileges):

```rust
#[cfg(feature = "locking")]
use mmap_io::create_mmap;

fn main() -> Result<(), mmap_io::MmapIoError> {
    let mmap = create_mmap("critical.bin", 4096)?;

    // Lock pages in memory
    mmap.lock(0, 4096)?;

    // Critical operations that need guaranteed memory residence...

    // Unlock when done
    mmap.unlock(0, 4096)?;

    Ok(())
}
```

## File Watching (`feature = "watch"`)

Native OS event sources (`inotify` on Linux, FSEvents on macOS, `ReadDirectoryChangesW` on Windows). Drop the returned handle to stop the watch and release the OS subscription.

```rust
#[cfg(feature = "watch")]
use mmap_io::{create_mmap, ChangeEvent};

fn main() -> Result<(), mmap_io::MmapIoError> {
    let mmap = create_mmap("watched.bin", 1024)?;

    let _handle = mmap.watch(|event: ChangeEvent| {
        println!("File changed: {:?}", event.kind);
    })?;

    // File is being watched... handle is dropped when out of scope.

    Ok(())
}
```

Note: mmap-side writes (`update_region` + `flush`) are not a reliable trigger for FS watchers; they reach the watcher only at OS-decided writeback time. Reliable detection comes from `std::fs` API writes (another process, another file handle), which is the real-world use case for `watch`.

## Copy-on-Write Mode (`feature = "cow"`)

Private, writable mapping of an existing file (since 1.1.0). Every write method works (`update_region`, `as_slice_mut`, `chunks_mut`, atomic views); written pages are copied on first write, the changes are visible through this mapping only, and they never reach the file. `flush()` is a no-op, `pending_bytes()` stays 0, and `resize()` is not supported. The file only needs read permission. Locking follows the `ReadWrite` rules: a live view blocks writers.

```rust
#[cfg(feature = "cow")]
use mmap_io::MemoryMappedFile;

fn main() -> Result<(), mmap_io::MmapIoError> {
    let cow_mmap = MemoryMappedFile::open_cow("shared.bin")?;

    // Patch the in-memory image; the file on disk is untouched.
    cow_mmap.update_region(0, b"patched")?;
    assert_eq!(&*cow_mmap.as_slice(0, 7)?, b"patched");
    cow_mmap.flush()?; // no-op for copy-on-write
    Ok(())
}
```

Before 1.1.0 this mode was read-only (every write returned `InvalidMode`).

## Async Operations (`feature = "async"`)

Runtime-agnostic async helpers (they run on the `blocking` crate's thread pool, so tokio, smol, async-std, or any executor works; this example uses tokio):

```rust
#[cfg(feature = "async")]
#[tokio::main]
async fn main() -> Result<(), mmap_io::MmapIoError> {
    use mmap_io::manager::r#async::{create_mmap_async, copy_mmap_async};

    let mmap = create_mmap_async("async.bin", 4096).await?;
    mmap.update_region(0, b"async data")?;
    mmap.flush()?;

    copy_mmap_async("async.bin", "copy.bin").await?;

    Ok(())
}
```

### Async-Only Flushing

`update_region_async` flushes after each write, so the data is durable once the future resolves. It copies `data` into a `Vec` first (one allocation), because the blocking task must own its input.

```rust
#[cfg(feature = "async")]
#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<(), mmap_io::MmapIoError> {
    use mmap_io::MemoryMappedFile;

    let mmap = MemoryMappedFile::create_rw("data.bin", 4096)?;
    // Async write that auto-flushes under the hood
    mmap.update_region_async(128, b"ASYNC-FLUSH").await?;
    // Optional explicit async flush
    mmap.flush_async().await?;
    Ok(())
}
```

Contract: after awaiting `update_region_async` or `flush_async`, the written bytes have been flushed to the file.

## Platform Parity

`flush()` and `flush_range()` behave the same on every platform: they return once the OS reports the pages written (see the Flush Policy section for the per-OS calls). A newly opened read-only mapping sees written bytes on every platform whether or not they were flushed, because mappings of the same file share the page cache; flushing is about durability, not visibility.

- **Full-file flush**: every dirty page of the mapping is written back.
- **Range flush**: the pages covering the range are written back; other dirty pages are written by a later `flush()` or by the OS.

See the parity tests in the repository that check this on each platform.

## Huge Pages (`feature = "hugepages"`)

A hint, not a guarantee. `.huge_pages(true)` on the builder affects `ReadWrite` mappings only:

**Linux**: after mapping (and after every `resize`), the crate calls `madvise(MADV_HUGEPAGE)` on the mapping to ask for Transparent Huge Pages. The kernel decides. For file-backed mappings it can only use huge pages where the filesystem's page cache supports them (for example tmpfs/shmem mounted with `huge=`); on most disk filesystems the mapping stays on regular pages. `MAP_HUGETLB` is not used (it requires a file on hugetlbfs), and pages are not pre-faulted.

**macOS / Windows**: no effect. Windows large pages are not used for file mappings.

Use `mmap.is_hugepage_backed()` (Linux) to see what the kernel actually did. The mapping behaves the same either way.

Builder usage:

```rust
#[cfg(feature = "hugepages")]
use mmap_io::{MemoryMappedFile, MmapMode};

let mmap = MemoryMappedFile::builder("hp.bin")
    .mode(MmapMode::ReadWrite)
    .size(2 * 1024 * 1024) // 2MB - typical huge page size
    .huge_pages(true) // best-effort optimization
    .create()?;
```

## Safety Notes

- All operations perform bounds checks, under the mapping lock. A zero-length request is accepted at any offset.
- Every `unsafe` block carries a SAFETY comment; [docs/SAFETY.md](./docs/SAFETY.md) explains the locking model.
- Interior mutability uses `parking_lot::RwLock`.
- A live `MappedSlice`, iterator item, or atomic view holds the read lock; a `MappedSliceMut` holds the write lock. Calling a method that needs the other kind of lock (for example `update_region` or `resize` while holding a slice, or `flush` while holding a `MappedSliceMut`) on the same thread deadlocks. Drop the guard first.

## ⚠️ Unsafe Code Disclaimer

This crate uses `unsafe` internally to manage raw memory mappings (`mmap` on Unix, `MapViewOfFile` on Windows, through `memmap2`). Public APIs are memory-safe within one process. However:

- **You must not modify or truncate the file from another process** while it is mapped here; readers can see torn data or crash with `SIGBUS`.
- **Do not mix atomic and plain access to the same bytes**: reading bytes through a `MappedSlice` while another thread stores to them through an atomic view is a data race.
- **Raw pointers** from `as_ptr` / `as_mut_ptr` are invalidated by `resize()`.

All unsafe logic is documented in the source and footguns are marked with caution.

## Minimum supported Rust version

`1.75`, pinned in `Cargo.toml` and verified by CI.

## Further reading

- **[API Reference](./docs/API.md)**: full collection of code examples and usage details.
- **[Changelog](./CHANGELOG.md)**: history of project versions and updates.

## License

Licensed under the **Apache License, Version 2.0**. See [LICENSE](LICENSE) for the full text.

You may obtain a copy of the License at: <http://www.apache.org/licenses/LICENSE-2.0>

Unless required by applicable law or agreed to in writing, software distributed under the License is distributed on an "AS IS" BASIS, WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied. See the License for specific language governing permissions and limitations.



<!-- COPYRIGHT
---------------------------------->
<div align="center">
    <br>
    <h2></h2>
    Copyright &copy; 2026 James Gober.
</div>