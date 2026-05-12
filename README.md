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
    Zero-copy reads. Efficient writes. Safe concurrent access.<br>
    Built for databases, game runtimes, caches, and real-time applications.
</p>

---

## Capabilities

- **Zero-copy reads** and efficient writes.
- **Read-only**, **read-write**, and **copy-on-write** modes.
- **Segment-based access** (offset + length).
- **Thread-safe** via interior mutability (parking_lot `RwLock`).
- **Cross-platform** via `memmap2`.
- Optional **async** helpers with Tokio.
- **Configurable flush policies** with smart microflush optimization.
- **Page prewarming** for predictable benchmark timing.
- **Huge pages support** (best-effort, Linux/Windows).
- **MSRV: 1.75**.

## Quick start

```toml
[dependencies]
mmap-io = "0.9"
```

```rust
use mmap_io::MemoryMappedFile;

fn main() -> Result<(), mmap_io::MmapIoError> {
    // Open an existing file in read-only mode
    let mmap = MemoryMappedFile::open_ro("data.bin")?;

    // Zero-copy read of the first 16 bytes
    let slice = mmap.as_slice(0, 16)?;
    println!("First bytes: {slice:?}");

    Ok(())
}
```

Or write a fresh file:

```rust
use mmap_io::{create_mmap, update_region, flush};

fn main() -> Result<(), mmap_io::MmapIoError> {
    let mmap = create_mmap("data.bin", 1024 * 1024)?;
    update_region(&mmap, 100, b"Hello, mmap!")?;
    flush(&mmap)?;
    Ok(())
}
```

## Optional features

| Feature     | Description                                                                                         |
|-------------|-----------------------------------------------------------------------------------------------------|
| `async`     | Tokio-based async helpers for asynchronous file and memory operations.                              |
| `advise`    | Memory hinting via `madvise`/`posix_madvise` (Unix) or `PrefetchVirtualMemory` (Windows).            |
| `iterator`  | Iterator-based access to memory chunks or pages with zero-copy reads.                                |
| `hugepages` | Huge Pages via MAP_HUGETLB (Linux) or FILE_ATTRIBUTE_LARGE_PAGES (Windows); falls back to regular pages. |
| `cow`       | Copy-on-Write mapping mode using private per-process memory views.                                   |
| `locking`   | Page-level memory locking via `mlock`/`munlock` (Unix) or `VirtualLock` (Windows).                   |
| `atomic`    | Atomic views into memory as aligned `u32` / `u64` with strict alignment checks.                      |
| `watch`     | File change notifications via polling fallback (native inotify/kqueue/FSEvents/RDCW planned).         |

> ⚠️ Features are opt-in. Enable only those relevant to your use case to reduce compile time and dependency footprint.

### Default features

By default, the following features are enabled:

- `advise` — memory access hinting for performance.
- `iterator` — iterator-based chunk/page access.

## Installation patterns

Default features:

```toml
[dependencies]
mmap-io = "0.9"
```

Enable async helpers:

```toml
[dependencies]
mmap-io = { version = "0.9", features = ["async"] }
```

Multiple features:

```toml
[dependencies]
mmap-io = { version = "0.9", features = ["cow", "locking"] }
```

Minimal — disable defaults, opt into only what you need:

```toml
[dependencies]
mmap-io = { version = "0.9", default-features = false, features = ["locking"] }
```

## Flush Policy

mmap-io supports configurable flush behavior for ReadWrite mappings via `FlushPolicy`, letting you trade off durability and throughput.

Policy variants:

- **`FlushPolicy::Never`** / **`FlushPolicy::Manual`** — no automatic flushes. Call `mmap.flush()` when you want durability.
- **`FlushPolicy::Always`** — flush after every write; slowest but most durable.
- **`FlushPolicy::EveryBytes(n)`** — accumulate bytes written across `update_region()` calls; flush when at least `n` bytes have been written.
- **`FlushPolicy::EveryWrites(n)`** — flush after every `n` writes.
- **`FlushPolicy::EveryMillis(ms)`** — automatically flushes pending writes at the specified interval using a background thread.

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
> On some platforms, visibility of writes without explicit flush may still occur due to OS behavior, but durability timing is best-effort without flush.

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

    // Process file in 1MB chunks
    for (i, chunk) in mmap.chunks(1024 * 1024).enumerate() {
        let data = chunk?;
        println!("Processing chunk {i} with {} bytes", data.len());
    }

    // Process file page by page (OS-optimal)
    for page in mmap.pages() {
        let _page_data = page?;
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

Monitor file changes (currently polling-based; native backends planned):

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

## Copy-on-Write Mode (`feature = "cow"`)

Private per-process memory views:

```rust
#[cfg(feature = "cow")]
use mmap_io::MemoryMappedFile;

fn main() -> Result<(), mmap_io::MmapIoError> {
    let cow_mmap = MemoryMappedFile::open_cow("shared.bin")?;

    // Reads see the original file content
    let _data = cow_mmap.as_slice(0, 100)?;

    // Writes affect this process only; underlying file remains unchanged.
    Ok(())
}
```

## Async Operations (`feature = "async"`)

Tokio-based async helpers:

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

When using async write helpers, mmap-io enforces durability by flushing after each async write. This avoids visibility inconsistencies across platforms when awaiting async tasks.

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

Contract: after awaiting `update_region_async` or `flush_async`, opening a fresh RO mapping observes the persisted data.

## Platform Parity

Flush visibility is guaranteed across operating systems: after calling `flush()` or `flush_range()`, a newly opened read-only mapping will observe the persisted bytes on all supported platforms.

- **Full-file flush**: both written regions are visible after `flush()`.
- **Range flush**: only the flushed range is guaranteed visible; a later `flush()` persists remaining regions.

See parity tests in the repository that validate this contract on each platform.

## Huge Pages (`feature = "hugepages"`)

Best-effort huge page support to reduce TLB misses and improve performance for large mappings.

**Linux** — multi-tier approach for huge page allocation:

1. **Tier 1**: Optimized mapping with immediate `MADV_HUGEPAGE` to encourage kernel huge page allocation.
2. **Tier 2**: Standard mapping with `MADV_HUGEPAGE` hint for Transparent Huge Pages (THP).
3. **Tier 3**: Silent fallback to regular pages if huge pages are unavailable.

**Windows** — attempts `FILE_ATTRIBUTE_LARGE_PAGES`. Requires the "Lock Pages in Memory" privilege and system configuration. Falls back to normal pages if unavailable.

**Other platforms** — no-op.

> ⚠️ `.huge_pages(true)` does **NOT guarantee** huge pages will be used. Actual allocation depends on system configuration, available memory, kernel heuristics, and process privileges. The mapping functions correctly regardless of whether huge pages are actually used.

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

- All operations perform bounds checks.
- Unsafe blocks are limited to mapping calls and documented with SAFETY comments.
- Interior mutability uses `parking_lot::RwLock` for high performance.
- Avoid flushing while holding a write guard to prevent deadlocks — drop the guard first.

## ⚠️ Unsafe Code Disclaimer

This crate uses `unsafe` internally to manage raw memory mappings (`mmap`, `VirtualAlloc`, etc.) across platforms. Public APIs are designed to be memory-safe when used correctly. However:

- **You must not modify the file concurrently** outside of this process.
- **Mapped slices are only valid** as long as the underlying file and mapping stay valid.
- **Behavior is undefined** if you access a truncated or deleted file via a stale mapping.

All unsafe logic is documented in the source and footguns are marked with caution.

## Minimum supported Rust version

`1.75` — pinned in `Cargo.toml` and verified by CI.

## Further reading

- **[API Reference](./docs/API.md)** — full collection of code examples and usage details.
- **[Changelog](./CHANGELOG.md)** — history of project versions and updates.

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