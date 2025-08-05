<div align="center">
   <img width="120px" height="auto" src="https://raw.githubusercontent.com/jamesgober/jamesgober/main/media/icons/hexagon-3.svg" alt="Triple Hexagon">
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

High-performance, async-ready memory-mapped file I/O library for Rust. Provides fast, zero-copy reads and efficient writes with safe, concurrent access. Designed for databases, game engines, caches, and real-time applications.

## Features
- Zero-copy reads and efficient writes
- Read-only and read-write modes
- Segment-based access (offset + length)
- Thread-safe via interior mutability (parking_lot `RwLock`)
- Cross-platform via `memmap2`
- Optional async helpers with Tokio
- MSRV: 1.76

## Installation

Add to your Cargo.toml:

```toml
[dependencies]
mmap-io = { version = "0.7.1" }
```

Enable async helpers (Tokio) when needed:

```toml
[dependencies]
mmap-io = { version = "0.7.1", features = ["async"] }
```

<br>

## Features

The following optional Cargo features enable extended functionality for `mmap-io`. Enable only what you need to minimize binary size and dependencies.

| Feature    | Description                                                                                         |
|------------|-----------------------------------------------------------------------------------------------------|
| `async`    | Enables **Tokio-based async helpers** for asynchronous file and memory operations.                 |
| `advise`   | Enables memory hinting using **`madvise`/`posix_madvise` (Unix)** or **Prefetch (Windows)**.       |
| `iterator` | Provides **iterator-based access** to memory chunks or pages with zero-copy read access.           |
| `cow`      | Enables **Copy-on-Write (COW)** mapping mode using private memory views (per-process isolation).   |
| `locking`  | Enables page-level memory locking via **`mlock`/`munlock` (Unix)** or **`VirtualLock` (Windows)**. |
| `atomic`   | Exposes **atomic views** into memory as aligned `u32` / `u64`, with strict safety guarantees.      |
| `watch`    | Enables **file change notifications** via `inotify`, `kqueue`, `FSEvents`, or `ReadDirectoryChangesW`. Falls back to polling where unavailable. |

> ⚠️ Features are opt-in. Enable only those relevant to your use case to reduce compile time and dependency bloat.


<br>


## Usage

### Basic Operations

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

### Memory Advise (feature = "advise")

Optimize memory access patterns:

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

### Iterator-Based Access (feature = "iterator")

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

### Atomic Operations (feature = "atomic")

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

### Memory Locking (feature = "locking")

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

### File Watching (feature = "watch")

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

### Copy-on-Write Mode (feature = "cow")

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

### Async Operations (feature = "async")

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

## Safety Notes

- All operations perform bounds checks.
- Unsafe blocks are limited to mapping calls and documented with SAFETY comments.
- Interior mutability uses `parking_lot::RwLock` for high performance.
- Avoid flushing while holding a write guard to prevent deadlocks (drop the guard first).

## License

Licensed under the Apache License, Version 2.0 (the "License");
you may not use this project except in compliance with the License.
You may obtain a copy of the License at: http://www.apache.org/licenses/LICENSE-2.0

Unless required by applicable law or agreed to in writing, software distributed under the License is distributed on an "AS IS" BASIS,
WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied. See the License for the specific language governing permissions and limitations under the License.

Copyright (c) 2025 Asotex Inc.
