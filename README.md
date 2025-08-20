<div align="center">
    <picture>
        <source media="(prefers-color-scheme: dark)" srcset="https://raw.githubusercontent.com/asotex/.github/refs/heads/main/media/asotex-icon-white.png">
        <img width="81px" alt="Asotex brand logo, featuring the Asotex A-Icon, followed by the word Asotex." src="https://raw.githubusercontent.com/asotex/.github/refs/heads/main/media/asotex-icon-dark.png">
    </picture>
    <h1>
        <strong>mmap-io</strong>
        <sup>
            <br>
            <sub>MEMORY-MAPPED IO</sub>
            <br>
        </sup>
    </h1>
        <a href="https://crates.io/crates/mmap-io" alt="mmap-io on Crates.io"><img alt="Crates.io" src="https://img.shields.io/crates/v/mmap-io"></a>
        <span>&nbsp;</span>
        <a href="https://crates.io/crates/mmap-io" alt="Download mmap-io"><img alt="Crates.io Downloads" src="https://img.shields.io/crates/d/mmap-io?color=%230099ff"></a>
        <span>&nbsp;</span>
        <a href="https://docs.rs/mmap-io" title="mmap-io Documentation"><img alt="docs.rs" src="https://img.shields.io/docsrs/mmap-io"></a>
        <span>&nbsp;</span>
        <a href="https://github.com/asotex/mmap-io/actions"><img alt="GitHub CI" src="https://github.com/asotex/mmap-io/actions/workflows/ci.yml/badge.svg"></a>
</div>
<br>
<p>
    High-performance, async-ready memory-mapped file I/O library for Rust. Provides fast, zero-copy reads and efficient writes with safe, concurrent access. Designed for databases, game engines, caches, and real-time applications.
</p>
<br>

## Capabilities
- **Zero-copy reads** and **efficient writes**.
- **Read-only** and **read-write** modes.
- **Segment-based access** (*offset* + *length*)
- **Thread-safe** via interior mutability (parking_lot `RwLock`)
- **Cross-platform** via `memmap2`
- Optional **async** helpers with `Tokio`.
- **MSRV: 1.76**


<br>


## Optional Features

The following optional Cargo features enable extended functionality for `mmap-io`. Enable only what you need to minimize binary size and dependencies.

| Feature     | Description                                                                                         |
|-------------|-----------------------------------------------------------------------------------------------------|
| `async`     | Enables **Tokio-based async helpers** for asynchronous file and memory operations.                  |
| `advise`    | Enables memory hinting using **`madvise`/`posix_madvise` (Unix)** or **Prefetch (Windows)**.        |
| `iterator`  | Provides **iterator-based access** to memory chunks or pages with zero-copy read access.            |
| `hugepages` | Enables support for Huge Pages via MAP_HUGETLB (Linux) or FILE_ATTRIBUTE_LARGE_PAGES (Windows), reducing TLB misses and improving performance for large memory regions. Requires system configuration and elevated privileges. |
| `cow`       | Enables **Copy-on-Write (COW)** mapping mode using private memory views (per-process isolation).    |
| `locking`   | Enables page-level memory locking via **`mlock`/`munlock` (Unix)** or **`VirtualLock` (Windows)**.  |
| `atomic`    | Exposes **atomic views** into memory as aligned `u32` / `u64`, with strict safety guarantees.      |
| `watch`     | Enables **file change notifications** via `inotify`, `kqueue`, `FSEvents`, or `ReadDirectoryChangesW`. Falls back to polling where unavailable. |

> ⚠️ Features are opt-in. Enable only those relevant to your use case to reduce compile time and dependency bloat.


### Default Features

By default, the following features are enabled:

- `advise` – Memory access hinting for performance
- `iterator` – Iterator-based chunk/page access


<br>

## Installation

> Add to your Cargo.toml:
```toml
[dependencies]
mmap-io = { version = "0.9.4" }
```
<br>

> Enable **async** helpers (`Tokio`) when needed:
```toml
[dependencies]
mmap-io = { version = "0.9.4", features = ["async"] }
```

> Or, enable other features like: `cow`, `locking`, or `advise`
```toml
[dependencies]
mmap-io = { version = "0.9.4, features = ["cow", "locking"] }
```
See full list of [Features](#optional-features) (shown above).

<br>

If you're building for minimal environments or want total control over feature flags, you can disable default features by using `default-features = false` (see below).
```toml
[dependencies]
mmap-io = { version = "0.9.4", default-features = false, features = ["locking"] }
```

<br>

## Example Usage

```rust
use mmap_io::{MmapMode, MemoryMappedFile};

fn main() -> std::io::Result<()> {
    // Open an existing file in read-only mode
    let mmap = MemoryMappedFile::open("data.bin", MmapMode::ReadOnly)?;

    // Read memory-mapped contents
    let slice = mmap.as_slice();
    println!("First byte: {}", slice[0]);

    Ok(())
}
```
<br>

## Flush Policy
**mmap-io** supports configurable flush behavior for ReadWrite mappings via a `FlushPolicy`, allowing you to trade off durability and throughput.

#### Policy variants:
- **FlushPolicy::Never** / **FlushPolicy::Manual**: No automatic flushes. Call `mmap.flush()` when you want durability.
- **FlushPolicy::Always**: Flush after every write; slowest but most durable.
- **FlushPolicy::EveryBytes(*n*)**: Accumulate bytes written across `update_region()` calls; flush when at least n bytes have been written.
- **FlushPolicy::EveryWrites(*n*)**: Flush after every n writes (calls to `update_region()`).
- **FlushPolicy::EveryMillis(*ms*)**: Automatically flushes pending writes at the specified interval using a background thread.


#### Using the builder to set a policy:
```rust
use mmap_io::{MemoryMappedFile, MmapMode};
use mmap_io::flush::FlushPolicy;

let mmap = MemoryMappedFile::builder("file.bin")
    .mode(MmapMode::ReadWrite)
    .size(1_000_000)
    .flush_policy(FlushPolicy::EveryBytes(256 * 1024)) // flush every 256KB written
    .create()?;
```

#### Manual flush example:
```rust
use mmap_io::{create_mmap, update_region, flush};

let mmap = create_mmap("data.bin", 1024 * 1024)?;
update_region(&mmap, 0, b"batch1")?;
// ... more batched writes ...
flush(&mmap)?; // ensure durability now
```

<br>

## Benchmark variants:
- **update_only**: No flush between writes (Manual policy).
- **update_plus_flush**: Explicit flush after each write.
- **update_threshold**: Builder sets threshold to flush periodically to measure batching behavior.


<br>

> [!NOTE]
> On some platforms, visibility of writes without explicit flush may still occur due to OS behavior, but durability timing is best-effort without flush.

<br>


#### Create a file, write to it, and read back:
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

<br>

## Memory Advise (feature = "advise")

#### Optimize memory access patterns:
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

<br>

## Iterator-Based Access (feature = "iterator")

Process files in chunks efficiently:

```rust
#[cfg(feature = "iterator")]
use mmap_io::create_mmap;

fn main() -> Result<(), mmap_io::MmapIoError> {
    let mmap = create_mmap("large_file.bin", 10 * 1024 * 1024)?;
    
    // Process file in 1MB chunks
    for (i, chunk) in mmap.chunks(1024 * 1024).enumerate() {
        let data = chunk?;
        println!("Processing chunk {} with {} bytes", i, data.len());
    }
    
    // Process file page by page (optimal for OS)
    for page in mmap.pages() {
        let page_data = page?;
        // Process page...
    }
    
    Ok(())
}
```

<br>




## Page Pre-warming (feature = "locking")

Eliminate page fault latency by pre-warming pages into memory.

```rust
#[cfg(feature = "locking")]
use mmap_io::{MemoryMappedFile, MmapMode, TouchHint};

fn main() -> Result<(), mmap_io::MmapIoError> {
    // Eagerly pre-warm all pages on creation for benchmarks
    let mmap = MemoryMappedFile::builder("benchmark.bin")
        .size(1024 * 1024)
        .touch_hint(TouchHint::Eager)
        .create()?;

    // Manually pre-warm a specific range before a critical operation
    mmap.touch_pages_range(0, 512 * 1024)?;

    Ok(())
}

```
> Note: The locking feature is currently required for this functionality as it provides the necessary ow-level memory control.

<br>



## Atomic Operations (feature = "atomic")

Lock-free concurrent access:

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
    println!("Counter was: {}", old);
    
    Ok(())
}
```

<br>

## Memory Locking (feature = "locking")

Prevent pages from being swapped:

```rust
#[cfg(feature = "locking")]
use mmap_io::create_mmap;

fn main() -> Result<(), mmap_io::MmapIoError> {
    let mmap = create_mmap("critical.bin", 4096)?;
    
    // Lock pages in memory (requires privileges)
    mmap.lock(0, 4096)?;
    
    // Critical operations that need guaranteed memory residence...
    
    // Unlock when done
    mmap.unlock(0, 4096)?;
    
    Ok(())
}
```

<br>

## File Watching (feature = "watch")

Monitor file changes:

```rust
#[cfg(feature = "watch")]
use mmap_io::{create_mmap, ChangeEvent};

fn main() -> Result<(), mmap_io::MmapIoError> {
    let mmap = create_mmap("watched.bin", 1024)?;
    
    // Set up file watcher
    let handle = mmap.watch(|event: ChangeEvent| {
        println!("File changed: {:?}", event.kind);
    })?;
    
    // File is being watched...
    // Handle is dropped when out of scope, stopping the watch
    
    Ok(())
}
```

<br>

## Copy-on-Write Mode (feature = "cow")

Private memory views:

```rust
#[cfg(feature = "cow")]
use mmap_io::{MemoryMappedFile, MmapMode};

fn main() -> Result<(), mmap_io::MmapIoError> {
    // Open file in copy-on-write mode
    let cow_mmap = MemoryMappedFile::open_cow("shared.bin")?;
    
    // Reads see the original file content
    let data = cow_mmap.as_slice(0, 100)?;
    
    // Writes would only affect this process (when implemented)
    // Other processes see original file unchanged
    
    Ok(())
}
```

<br>

## Async Operations (feature = "async")

Tokio-based async helpers:

```rust
#[cfg(feature = "async")]
#[tokio::main]
async fn main() -> Result<(), mmap_io::MmapIoError> {
    use mmap_io::manager::r#async::{create_mmap_async, copy_mmap_async};

    // Create file asynchronously
    let mmap = create_mmap_async("async.bin", 4096).await?;
    mmap.update_region(0, b"async data")?;
    mmap.flush()?;
    
    // Copy file asynchronously
    copy_mmap_async("async.bin", "copy.bin").await?;
    
    Ok(())
}
```

<br>

## Async-Only Flushing

When using **async** write helpers, **mmap-io** enforces durability by flushing after each **async** write. This avoids visibility inconsistencies across platforms when awaiting **async** tasks.

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

Contract: After await-ing update_region_async or flush_async, reopening a fresh RO mapping observes the persisted data.

<br>

## Platform Parity

Flush visibility is guaranteed across OSes: after calling `flush()` or `flush_range()`, a newly opened read-only mapping will observe the persisted bytes on all supported platforms.

Examples:
- **Full-file flush**: both written regions are visible after `flush()`.
- **Range flush**: only the flushed range is guaranteed visible; a later `flush()` persists remaining regions.

See parity tests in the repository that validate this contract on all platforms.

<br>

## Huge Pages (feature = "hugepages")

Best-effort huge page support to reduce TLB misses and improve performance for large memory regions:

**Linux**: Uses a multi-tier approach for optimal huge page allocation::

1. **Tier 1**: Attempts an optimized mapping with immediate `MADV_HUGEPAGE` to encourage kernel huge page allocation.
2. **Tier 2**: Falls back to a standard mapping with a `MADV_HUGEPAGE` hint for Transparent Huge Pages (THP).
3. **Tier 3**: Silently falls back to regular pages if huge pages are unavailable.

**Windows**: Attempts to use `FILE_ATTRIBUTE_LARGE_PAGES` when creating files. Requires the "Lock Pages in Memory" privilege and system configuration. Falls back to normal pages if unavailable.

**Other platforms**: No-op (uses normal pages).

⚠️ **Important**: `.huge_pages(true)` does **NOT guarantee** huge pages will be used. The actual allocation depends on:
- System configuration (huge pages must be enabled)
- Available memory and fragmentation  
- Kernel heuristics and available huge page pool
- Process privileges (for explicit huge page allocation)

The mapping will function correctly regardless of whether huge pages are actually used.

Usage via builder:
```rust
#[cfg(feature = "hugepages")]
use mmap_io::{MemoryMappedFile, MmapMode};

let mmap = MemoryMappedFile::builder("hp.bin")
    .mode(MmapMode::ReadWrite)
    .size(2 * 1024 * 1024) // 2MB - typical huge page size
    .huge_pages(true) // best-effort optimization
    .create()?;
```

**Important Notes:**
- Huge pages are a performance optimization hint, not a guarantee
- The mapping will function correctly regardless of whether huge pages are used
- On Linux, THP provides automatic, transparent fallback to normal pages
- Performance benefits are most noticeable with large, frequently-accessed mappings

<br>

## Safety Notes

- All operations perform bounds checks.
- Unsafe blocks are limited to mapping calls and documented with SAFETY comments.
- Interior mutability uses `parking_lot::RwLock` for high performance.
- Avoid flushing while holding a write guard to prevent deadlocks (drop the guard first).

<br>

## ⚠️ Unsafe Code Disclaimer

This crate uses `unsafe` internally to manage raw memory mappings (`mmap`, `VirtualAlloc`, etc.) across platforms. All public APIs are designed to be memory-safe when used correctly. However:

- **You must not modify the file concurrently** outside of this process.
- **Mapped slices are only valid** as long as the underlying file and mapping stay valid.
- **Behavior is undefined** if you access a truncated or deleted file via a stale mapping.

We document all unsafe logic in the source and mark any footguns with caution.


<hr><br>

<ul>
    <li>
        <b><a href="./docs/API.md" title="API Reference and Code Examples">API Reference</a>:</b> A complete collection of code examples and usage details.
    </li>
    <li>
        <b><a href="./CHANGELOG.md" title="Project Changelog">Changelog</a>:</b> A detailed history of all project versions and updates.
    </li>
</ul>


<br><br>



<!--// CONTRIBUTERS // 
<div>
    <br><br>
    <h2 align="center">CONTRIBUTERS</h2>
</div>
<br><br>-->


<!--// SPONSORS // -
<div>
    <br><br>
    <h2 align="center">SPONSORS</h2>
</div>
<br><br>->

<!--// LICENSE // -->
<div>
    <br><br>
    <h2 align="center">LICENSE</h2>
    <p>
        Licensed under the <strong>Apache License</strong>, <b>Version 2.0</b> (the "License"); you may not use this project except in compliance with the License.
    </p>
    <p>See the <code>LICENSE</code> file included with this project for more information.</p>
    <br>
    <p>You may obtain a copy of the License at: <a href="http://www.apache.org/licenses/LICENSE-2.0">http://www.apache.org/licenses/LICENSE-2.0</a></p>
    <br>
    <p>Unless required by applicable law or agreed to in writing, software distributed under the License is distributed on an "AS IS" BASIS, WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied. See the License for the specific language governing permissions and limitations under the License.</p>
</div>

<!--// FOOTER // -->
<div align="center">
    <br>
    <h2></h2>
    <div><!-- FOOT: NAVIGATION -->
        <sup> 
            <a href="https://asotex.com" title="Asotex Website">ASOTEX.COM</a>
            <span>&nbsp;&middot;&nbsp;</span>
            <a href="https://asotex.com/about" title="About Asotex">ABOUT</a>
            <span>&nbsp;&middot;&nbsp;</span>
            <a href="https://asotex.com/corporate/investors/" title="Asotex Investors">INVESTORS</a>
            <span>&nbsp;&middot;&nbsp;</span>
            <a href="https://asotex.com/corporate/partners/" title="Asotex Partners">PARTNERS</a>
            <span>&nbsp;&middot;&nbsp;</span>
            <a href="https://asotex.com/legal/" title="Asotex Legal Documentation">LEGAL</a>
            <span>&nbsp;&middot;&nbsp;</span>
            <a href="https://asotex.com/contact/" title="Contact Asotex">CONTACT</a>
        </sup>
    </div>
    <sub><!-- FOOT: COPYRIGHT -->
        Copyright &copy; 2025 Asotex Inc. All Rights Reserved.
    </sub>
</div>