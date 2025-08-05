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
mmap-io = { version = "0.1.0" }
```

Enable async helpers (Tokio) when needed:

```toml
[dependencies]
mmap-io = { version = "0.1.0", features = ["async"] }
```

## Usage

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

Mutable slice access:

```rust
use mmap_io::create_mmap;

fn main() -> Result<(), mmap_io::MmapIoError> {
    let mmap = create_mmap("data_mut.bin", 4096)?;
    {
        let mut g = mmap.as_slice_mut(0, 5)?;
        g.as_mut().copy_from_slice(b"ABCDE");
    }
    mmap.flush()?;
    Ok(())
}
```

Async helpers (feature = "async"):

```rust
#[cfg(feature = "async")]
#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<(), mmap_io::MmapIoError> {
    use mmap_io::manager::r#async::{create_mmap_async, copy_mmap_async, delete_mmap_async};

    let src = "async_src.bin";
    let dst = "async_dst.bin";

    let mmap = create_mmap_async(src, 4096).await?;
    mmap.update_region(0, b"async data")?;
    mmap.flush()?;
    drop(mmap);

    copy_mmap_async(src, dst).await?;

    let ro = mmap_io::load_mmap(dst, mmap_io::MmapMode::ReadOnly)?;
    assert_eq!(ro.as_slice(0, 10)?, b"async data");

    delete_mmap_async(src).await?;
    delete_mmap_async(dst).await?;
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
You may obtain a copy of the License at:

    http://www.apache.org/licenses/LICENSE-2.0

Unless required by applicable law or agreed to in writing, software distributed under the License is distributed on an "AS IS" BASIS,
WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied. See the License for the specific language governing permissions and limitations under the License.

Copyright (c) 2025 James Gober
