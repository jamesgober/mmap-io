# mmap-io API Reference

Complete reference for public-facing APIs. Each item lists its signature, parameters, description, errors, and examples.

Prerequisites:
- MSRV: 1.76
- Default (sync) APIs always available
- Async helpers require `features = ["async"]`

---

## Crate Exports

- Errors
  - `MmapIoError`
  - `errors::Result<T> = std::result::Result<T, MmapIoError>`
- Core types
  - `MemoryMappedFile`
  - `MmapMode`
- Manager (high-level helpers)
  - `create_mmap`, `load_mmap`, `write_mmap`, `update_region`, `flush`, `copy_mmap`, `delete_mmap`
  - Async (feature "async"): `create_mmap_async`, `copy_mmap_async`, `delete_mmap_async`
- Segments
  - `segment::Segment`, `segment::SegmentMut`

---

## Errors

### errors::MmapIoError (enum)
Variants:
- `Io(std::io::Error)` — I/O and OS errors
- `InvalidMode(&'static str)` — Called an operation in the wrong mode
- `OutOfBounds { offset: u64, len: u64, total: u64 }` — Range outside file
- `FlushFailed(String)` — Flush operation failed
- `ResizeFailed(String)` — Resize requested with invalid size or OS error

Common Display strings:
- `"I/O error: ..."`
- `"invalid access mode: ..."`
- `"range out of bounds: offset=..., len=..., total=..."`
- `"flush failed: ..."`
- `"resize failed: ..."`

---

## Modes

### mmap::MmapMode (enum)
- `ReadOnly` — read-only mapping
- `ReadWrite` — read-write mapping

---

## Core: MemoryMappedFile

### mmap::MemoryMappedFile::create_rw(path, size) -> Result<MemoryMappedFile>
Parameters:
- `path: impl AsRef<Path>` — File path (created/truncated)
- `size: u64` — File size in bytes (must be > 0)

Description:
Creates and truncates the file to `size`, then memory-maps it in read-write mode.

Errors:
- `ResizeFailed("Size must be greater than zero")` if `size == 0`
- `Io` if file creation/mapping fails

Example:
```rust
use mmap_io::MemoryMappedFile;

let mmap = MemoryMappedFile::create_rw("data.bin", 4096)?;
assert_eq!(mmap.mode(), mmap_io::MmapMode::ReadWrite);
```

---

### mmap::MemoryMappedFile::open_ro(path) -> Result<MemoryMappedFile>
Parameters:
- `path: impl AsRef<Path>` — Existing file path

Description:
Opens an existing file read-only and maps it.

Errors:
- `Io` if open/mapping fails

Example:
```rust
let ro = mmap_io::MemoryMappedFile::open_ro("data.bin")?;
let bytes = ro.as_slice(0, 10)?;
```

---

### mmap::MemoryMappedFile::open_rw(path) -> Result<MemoryMappedFile>
Parameters:
- `path: impl AsRef<Path>` — Existing file path

Description:
Opens an existing file read-write and maps it.

Errors:
- `ResizeFailed("Cannot map zero-length file")` if file length is 0
- `Io` if open/mapping fails

Example:
```rust
let rw = mmap_io::MemoryMappedFile::open_rw("data.bin")?;
rw.update_region(0, b"hello")?;
```

---

### mmap::MemoryMappedFile::mode(&self) -> MmapMode
Description:
Returns the current mapping mode.

Example:
```rust
let m = mmap_io::MemoryMappedFile::open_ro("data.bin")?;
assert_eq!(m.mode(), mmap_io::MmapMode::ReadOnly);
```

---

### mmap::MemoryMappedFile::len(&self) -> u64
Description:
Returns current file length in bytes (cached).

Example:
```rust
let m = mmap_io::MemoryMappedFile::open_ro("data.bin")?;
println!("file size: {}", m.len());
```

---

### mmap::MemoryMappedFile::is_empty(&self) -> bool
Description:
Returns true if length == 0.

---

### mmap::MemoryMappedFile::as_slice(&self, offset, len) -> Result<&[u8]>
Parameters:
- `offset: u64` — Start position
- `len: u64` — Number of bytes

Description:
Returns zero-copy read-only slice. Only valid in `ReadOnly` mode. For RW mapping, use `read_into`.

Errors:
- `OutOfBounds` if range invalid
- `InvalidMode("use read_into for RW mappings")` for RW

Example:
```rust
let ro = mmap_io::MemoryMappedFile::open_ro("data.bin")?;
let bytes = ro.as_slice(10, 5)?;
```

---

### mmap::MemoryMappedFile::as_slice_mut(&self, offset, len) -> Result<MappedSliceMut<'_>>
Parameters:
- `offset: u64`
- `len: u64`

Description:
Returns a write-locked slice wrapper for RW mappings. Holds an exclusive write lock while in scope.

Errors:
- `InvalidMode` if not ReadWrite
- `OutOfBounds` if range invalid

Example:
```rust
let rw = mmap_io::MemoryMappedFile::open_rw("data.bin")?;
{
  let mut guard = rw.as_slice_mut(0, 4)?;
  guard.as_mut().copy_from_slice(b"ABCD");
}
rw.flush()?;
```

---

### mmap::MemoryMappedFile::update_region(&self, offset, data) -> Result<()>
Parameters:
- `offset: u64`
- `data: &[u8]`

Description:
Bounds-checked, zero-copy write into RW mapping.

Errors:
- `InvalidMode` if not ReadWrite
- `OutOfBounds` if range invalid

Example:
```rust
let rw = mmap_io::MemoryMappedFile::open_rw("data.bin")?;
rw.update_region(100, b"payload")?;
```

---

### mmap::MemoryMappedFile::flush(&self) -> Result<()>
Description:
Flushes all changes to disk. No-op for RO mapping.

Errors:
- `FlushFailed` if OS flush fails

---

### mmap::MemoryMappedFile::flush_range(&self, offset, len) -> Result<()>
Parameters:
- `offset: u64`
- `len: u64`

Description:
Flushes a specific range of bytes. No-op for zero len or RO mapping.

Errors:
- `OutOfBounds` if range invalid
- `FlushFailed` on OS error

---

### mmap::MemoryMappedFile::resize(&self, new_size) -> Result<()>
Parameters:
- `new_size: u64` — New size in bytes (must be > 0)

Description:
Resizes underlying file (RW only) and remaps. Cached length updated.

Errors:
- `InvalidMode` if not RW
- `ResizeFailed("Size must be greater than zero")` if `new_size == 0`
- `Io` on OS error

Example:
```rust
let rw = mmap_io::MemoryMappedFile::open_rw("data.bin")?;
rw.resize(8192)?;
assert_eq!(rw.len(), 8192);
```

---

### mmap::MemoryMappedFile::path(&self) -> &Path
Description:
Returns the path to the underlying file.

---

### mmap::MemoryMappedFile::current_len(&self) -> Result<u64>
Description:
Returns current length (uses cached value). Intended for consistency checks.

Errors:
- `Io` if metadata query fails in future variants (currently cached)

---

### mmap::MemoryMappedFile::read_into(&self, offset, buf: &mut [u8]) -> Result<()>
Parameters:
- `offset: u64`
- `buf: &mut [u8]` — Destination buffer (length determines read length)

Description:
Reads bytes into `buf`. Works for both RO and RW mappings.

Errors:
- `OutOfBounds` if range invalid

Example:
```rust
let ro = mmap_io::MemoryMappedFile::open_ro("data.bin")?;
let mut tmp = [0u8; 4];
ro.read_into(0, &mut tmp)?;
```

---

### mmap::MappedSliceMut<'_>
Wrapper that holds a write lock and a mutable byte slice for a range.

Methods:
- `as_mut(&mut self) -> &mut [u8]` — Returns the mutable slice view

Example:
```rust
let rw = mmap_io::MemoryMappedFile::open_rw("data.bin")?;
let mut guard = rw.as_slice_mut(0, 3)?;
guard.as_mut().copy_from_slice(b"xyz");
```

---

## Segments

### segment::Segment::new(parent, offset, len) -> Result<Segment>
Parameters:
- `parent: Arc<MemoryMappedFile>`
- `offset: u64`
- `len: u64`

Description:
Immutable view into a region of a file. Delegates to `as_slice`.

Errors:
- `OutOfBounds` if invalid

Example:
```rust
use std::sync::Arc;
use mmap_io::segment::Segment;

let parent = Arc::new(mmap_io::MemoryMappedFile::open_ro("data.bin")?);
let seg = Segment::new(parent, 100, 50)?;
let data = seg.as_slice()?;
```

Accessors:
- `len(&self) -> u64`
- `is_empty(&self) -> bool`
- `offset(&self) -> u64`
- `parent(&self) -> &MemoryMappedFile`

---

### segment::SegmentMut::new(parent, offset, len) -> Result<SegmentMut>
Parameters:
- `parent: Arc<MemoryMappedFile>`
- `offset: u64`
- `len: u64`

Description:
Mutable view into a region. Provides `as_slice_mut` and `write`.

Errors:
- `OutOfBounds` if invalid

Methods:
- `as_slice_mut(&self) -> Result<crate::mmap::MappedSliceMut<'_>>`
- `write(&self, data: &[u8]) -> Result<()>` — Partial writes allowed (writes `data.len()`)

Accessors:
- `len(&self) -> u64`
- `is_empty(&self) -> bool`
- `offset(&self) -> u64`
- `parent(&self) -> &MemoryMappedFile`

Example:
```rust
use std::sync::Arc;
use mmap_io::segment::SegmentMut;

let parent = Arc::new(mmap_io::MemoryMappedFile::open_rw("data.bin")?);
let seg = SegmentMut::new(parent, 0, 16)?;
seg.write(b"segment payload")?;
```

---

## Manager (High-level helpers)

These wrap low-level operations for convenience.

### manager::create_mmap(path, size) -> Result<MemoryMappedFile>
Description:
Create/truncate and map RW.

Example:
```rust
let mmap = mmap_io::create_mmap("file.bin", 4096)?;
```

---

### manager::load_mmap(path, mode) -> Result<MemoryMappedFile>
Parameters:
- `mode: MmapMode`

Example:
```rust
let ro = mmap_io::load_mmap("file.bin", mmap_io::MmapMode::ReadOnly)?;
let rw = mmap_io::load_mmap("file.bin", mmap_io::MmapMode::ReadWrite)?;
```

---

### manager::write_mmap(path, offset, data) -> Result<()>
Description:
Open RW and write `data` at `offset`.

---

### manager::update_region(mmap, offset, data) -> Result<()>
Description:
Call through to `MemoryMappedFile::update_region`.

---

### manager::flush(mmap) -> Result<()>

---

### utils::slice_range(offset, len, total) -> Result<(usize, usize)>
Description:
Computes a safe byte slice range tuple `(start, end)` as `usize` given `offset`, `len`, and `total`. Performs bounds checks via `ensure_in_bounds`.

Parameters:
- `offset: u64` — start position
- `len: u64` — byte length
- `total: u64` — total available length

Errors:
- `OutOfBounds` if the requested range exceeds `total`

Notes:
- Low-level helper; most users should call higher-level APIs (`as_slice`, `read_into`, segments) instead of manually computing ranges.

Example:
```rust
let (start, end) = mmap_io::utils::slice_range(10, 5, 100)?;
assert_eq!((start, end), (10, 15));
```

---

## Root Re-exports (crate::)

The following items are re-exported at the crate root for convenience:
- `MmapIoError`
- `MemoryMappedFile`
- `MmapMode`
- `create_mmap`, `load_mmap`, `write_mmap`, `update_region`, `flush`, `copy_mmap`, `delete_mmap`

Example:
```rust
use mmap_io::{create_mmap, load_mmap, MmapMode, MemoryMappedFile, MmapIoError};
```

## Notes on Length Caching

`MemoryMappedFile::len()` returns a cached length for performance. APIs that modify the file size (e.g., `resize`) update this cache. If external processes change the file length, the cached length may not reflect that until remapping; such external changes are outside the safety guarantees of this crate.
Description:
Flush mapping.

---

### manager::copy_mmap(src, dst) -> Result<()>
Description:
Filesystem copy of the underlying file.

---

### manager::delete_mmap(path) -> Result<()>
Description:
Remove backing file (ensure mappings are dropped beforehand).

---

## Async (feature = "async")

### manager::r#async::create_mmap_async(path, size) -> Result<MemoryMappedFile>
Description:
Create and size a file asynchronously (Tokio), then open RW mapping.

Example:
```rust
#[tokio::main]
async fn main() -> Result<(), mmap_io::MmapIoError> {
    let mmap = mmap_io::manager::r#async::create_mmap_async("data.bin", 4096).await?;
    Ok(())
}
```

---

### manager::r#async::copy_mmap_async(src, dst) -> Result<()>
Description:
Async filesystem copy.

---

### manager::r#async::delete_mmap_async(path) -> Result<()>
Description:
Async file delete.

---

## Utilities

### utils::page_size() -> usize
Description:
Returns OS page size in bytes.

---

### utils::align_up(value, alignment) -> u64
Description:
Align `value` up to `alignment`. Optimized fast path for power-of-two alignments.

---

### utils::ensure_in_bounds(offset, len, total) -> Result<()>
Description:
Validates `[offset, offset+len)` is within `[0, total)`. Returns `OutOfBounds` error otherwise.

---

## Notes and Best Practices

- For RW mappings, prefer using `read_into` for reading bytes to avoid holding lock guards.
- Drop write guards before calling `flush` to prevent deadlocks.
- Use segments for composing larger algorithms over subranges without copying data.
