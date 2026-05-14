<div id="doc-top" align="center">
    <img width="99" alt="Rust logo" src="https://raw.githubusercontent.com/jamesgober/rust-collection/72baabd71f00e14aa9184efcb16fa3deddda3a0a/assets/rust-logo.svg">
    <h1>
        <strong>mmap-io</strong>
        <sup><br><sub>API REFERENCE</sub><br></sup>
    </h1>
</div>
<br>

Complete reference for public-facing APIs. Each item lists its signature, parameters, description, errors, and examples.

<br>

## Table of Contents
- **[Prerequisites](#prerequisites)**
- **[Features](#features)**
  - [Default Features](#default-features)
- **[Installation](#installation)**
- **[Core Types](#core-types)**
  - [MemoryMappedFile](#memorymappedfile)
  - [MmapMode](#mmapmode)
  - [MmapIoError](#mmapioerror)
- **[Manager Functions](#manager-functions)**
  - [create_mmap](#create_mmap)
  - [load_mmap](#load_mmap)
  - [update_region](#update_region)
  - [flush](#flush)
  - [copy_mmap](#copy_mmap)
  - [delete_mmap](#delete_mmap)
- **[MemoryMappedFile Methods](#memorymappedfile-methods)**
  - [create_rw](#create_rw)
  - [open_ro](#open_ro)
  - [open_rw](#open_rw)
  - [open_cow](#open_cow) (feature = "cow")
  - [open_or_create](#open_or_create) (0.9.8)
  - [from_file](#from_file) (0.9.8)
  - [unmap](#unmap) (0.9.8)
  - [as_slice](#as_slice)
  - [as_slice_mut](#as_slice_mut)
  - [read_into](#read_into)
  - [update_region](#update_region-1)
  - [flush](#flush-1)
  - [flush_range](#flush_range)
  - [resize](#resize)
  - [len](#len)
  - [is_empty](#is_empty)
  - [path](#path)
  - [mode](#mode)
  - [flush_policy](#flush_policy) (0.9.8)
  - [pending_bytes](#pending_bytes) (0.9.8)
  - [as_ptr](#as_ptr) (0.9.8, unsafe)
  - [as_mut_ptr](#as_mut_ptr) (0.9.8, unsafe)
  - [prefetch_range](#prefetch_range) (0.9.8)
- **[Feature-Gated APIs](#feature-gated-apis)**
  - [Memory Advise](#memory-advise-feature--advise)
    - [advise](#advise)
    - [MmapAdvice](#mmapadvice)
  - [Iterator-Based Access](#iterator-based-access-feature--iterator)
    - [chunks](#chunks)
    - [pages](#pages)
    - [chunks_mut](#chunks_mut)
  - [Atomic Operations](#atomic-operations-feature--atomic)
    - [atomic_u64](#atomic_u64)
    - [atomic_u32](#atomic_u32)
    - [atomic_u64_slice](#atomic_u64_slice)
    - [atomic_u32_slice](#atomic_u32_slice)
  - [Memory Locking](#memory-locking-feature--locking)
    - [lock](#lock)
    - [unlock](#unlock)
    - [lock_all](#lock_all)
    - [unlock_all](#unlock_all)
  - [File Watching](#file-watching-feature--watch)
    - [watch](#watch)
    - [ChangeEvent](#changeevent)
    - [ChangeKind](#changekind)
- **[Segment Types](#segment-types)**
  - [Segment](#segment)
  - [SegmentMut](#segmentmut)
- **[Async Operations](#async-operations-feature--async)**
  - [update_region_async](#update_region_async)
  - [flush_async](#flush_async)
  - [flush_range_async](#flush_range_async)
  - [create_mmap_async](#create_mmap_async)
  - [copy_mmap_async](#copy_mmap_async)
  - [delete_mmap_async](#delete_mmap_async)
- **[Utility Functions](#utility-functions)**
  - [page_size](#page_size)
  - [align_up](#align_up)
- **[Safety and Best Practices](#safety-and-best-practices)**
- **[Flush Policy](#flush-policy)**
  - [Mapped Memory Access](#mapped-memory-access)
  - [Copy-On-Write Mode](#copy-on-write-cow-mode)
  - [Flushing Behavior](#flushing-behavior)
  - [Thread Safety](#thread-safety)
  - [Performance Tips](#performance-tips)
  - [Common Pitfalls](#common-pitfalls)
  - [Error Handling](#error-handling)
- **[Examples](#examples)**
  - [Database-like Usage](#database-like-usage)
  - [Game Asset Loading](#game-asset-loading)
  - [Log File Processing](#log-file-processing)
  - [Concurrent Counter](#concurrent-counter)
- **[Version History](#version-history)**

<br><br>

## Prerequisites:
- **MSRV: 1.75**
- **Default (*sync*) APIs**: *always available*.
- **Feature-gated APIs**: *require enabling specific features*.

<br><br>


## Features

The following optional Cargo features enable extended functionality:

| Feature    | Description                                                                                         |
|------------|-----------------------------------------------------------------------------------------------------|
| `async`    | Enables **Tokio-based async helpers** for asynchronous file and memory operations.                 |
| `advise`   | Enables memory hinting using **`madvise`/`posix_madvise` (Unix)** or **Prefetch (Windows)**.       |
| `iterator` | Provides **iterator-based access** to memory chunks or pages with zero-copy read access.           |
| `hugepages` | **Best-effort Huge Pages Support**: Reduces TLB misses for large memory regions through a multi-tier approach:<br/>**Tier 1**: Optimized mapping with immediate MADV_HUGEPAGE + MADV_POPULATE_WRITE<br/>**Tier 2**: Standard mapping with MADV_HUGEPAGE hint<br/>**Tier 3**: Silent fallback to regular pages<br/>⚠️ **Not Guaranteed**: Requires system configuration and adequate privileges. The mapping will function correctly regardless of huge page availability. |
| `cow`      | Enables **Copy-on-Write (COW)** mapping mode using private memory views (per-process isolation).   |
| `locking`  | Enables page-level memory locking via **`mlock`/`munlock` (Unix)** or **`VirtualLock` (Windows)**. |
| `atomic`   | Exposes **atomic views** into memory as aligned `u32` / `u64`, with strict safety guarantees.      |
| `watch`    | Enables **file change notifications** via platform-specific APIs with polling fallback.            |

<br>

- **Huge Pages** (`feature = "hugepages"`): Best-effort large-page mappings on supported platforms to reduce TLB misses. Falls back safely when unavailable or lacking privileges.

- **Async-Only Flushing** (`feature = "async"`): Async write helpers auto-flush after each write to ensure post-await visibility across platforms.

- **Platform Parity**: After `flush()` or `flush_range()`, newly opened RO mappings observe persisted bytes across supported OSes.

<br> 

### Default Features

By default, the following features are enabled:

- `advise` – Memory access hinting for performance
- `iterator` – Iterator-based chunk/page access


<hr>
<div align="right"><a href="#doc-top">&uarr; TOP</a></div>
<br>


## Installation

### 1: Basic Installation:
> Add the following to your Cargo.toml file:
```toml
[dependencies]
mmap-io = { version = "0.9.11" }
```

> Or install using Cargo:
```bash
cargo add mmap-io
```

<br>

### 2: Custom Install:
Enable additional features by using the pre-defined [features flags](#features) as shown above.

> ##### Manual Install with Features:
```toml
[dependencies]
mmap-io = { version = "0.9.11", features = ["cow", "locking"] }
```
> ##### Cargo Install with Features:
```bash
cargo add mmap-io --features async,advise,iterator,cow,locking,atomic,watch
```

<br>

### 3: Minimal Install:
If you're building for minimal environments or want total control over feature flags, you can disable the [default features](#default-features).

> ##### Manual Install without Default Features:
```toml
[dependencies]
mmap-io = { version = "0.9.11", default-features = false, features = ["locking"] }
```

> ##### Cargo Install without Default Features:
```bash
cargo add mmap-io --no-default-features --features locking
```

<hr>
<div align="right"><a href="#doc-top">&uarr; TOP</a></div>
<br>


## Core Types

<br>

### MemoryMappedFile

The main type for memory-mapped file operations.

```rust
pub struct MemoryMappedFile { /* private fields */ }
```

**Description**: Provides safe, zero-copy access to memory-mapped files with concurrent access support through interior mutability.

**Example**:
```rust
use mmap_io::MemoryMappedFile;

let mmap = MemoryMappedFile::create_rw("data.bin", 1024)?;
```

<br>

### TouchHint

Enum representing when to touch (prewarm) memory pages during mapping creation.

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TouchHint {
    Never,   // Don't touch pages during creation (default)
    Eager,   // Eagerly touch all pages during creation
    Lazy,    // Touch pages lazily on first access (same as Never for now)
}
```

**Variants**:
- `Never`: Don't touch pages during creation (default)
- `Eager`: Eagerly touch all pages during creation to prewarm page tables and improve first-access latency. Useful for benchmarking scenarios where you want consistent timing without page fault overhead.
- `Lazy`: Touch pages lazily on first access (same as Never for now)

<br>

### MmapMode

Enum representing the access mode for memory-mapped files.

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MmapMode {
    ReadOnly,
    ReadWrite,
    CopyOnWrite, // Available with feature = "cow"
}
```

**Variants**:
- `ReadOnly`: Read-only access to the file
- `ReadWrite`: Read and write access to the file
- `CopyOnWrite`: Private copy-on-write mapping (feature-gated)

<br>

### MmapIoError

Error type for all mmap-io operations.

```rust
#[derive(Debug, Error)]
pub enum MmapIoError {
    Io(#[from] io::Error),
    InvalidMode(&'static str),
    OutOfBounds { offset: u64, len: u64, total: u64 },
    FlushFailed(String),
    ResizeFailed(String),
    AdviceFailed(String),    // feature = "advise"
    LockFailed(String),      // feature = "locking"
    UnlockFailed(String),    // feature = "locking"
    Misaligned { required: u64, offset: u64 }, // feature = "atomic"
    WatchFailed(String),     // feature = "watch"
}
```
<hr>
<div align="right"><a href="#doc-top">&uarr; TOP</a></div>
<br>

## Manager Functions

High-level convenience functions for common operations.

<br>

### create_mmap

```rust
pub fn create_mmap<P: AsRef<Path>>(path: P, size: u64) -> Result<MemoryMappedFile>
```

**Description**: Creates a new memory-mapped file with the specified size. Truncates if the file already exists.

**Parameters**:
- `path`: Path to the file to create
- `size`: Size of the file in bytes (must be > 0)

**Returns**: `Result<MemoryMappedFile>` - The created memory-mapped file

**Errors**:
- `MmapIoError::ResizeFailed` if size is 0
- `MmapIoError::Io` if file creation fails

**Example**:
```rust
use mmap_io::create_mmap;

let mmap = create_mmap("new_file.bin", 1024 * 1024)?; // 1MB file
```

<br>

### load_mmap

```rust
pub fn load_mmap<P: AsRef<Path>>(path: P, mode: MmapMode) -> Result<MemoryMappedFile>
```

**Description**: Opens an existing file and memory-maps it with the specified mode.

**Parameters**:
- `path`: Path to the file to open
- `mode`: Access mode (`ReadOnly`, `ReadWrite`, or `CopyOnWrite`)

**Returns**: `Result<MemoryMappedFile>` - The opened memory-mapped file

**Errors**:
- `MmapIoError::Io` if file doesn't exist or can't be opened
- `MmapIoError::ResizeFailed` if file is zero-length (for RW mode)

**Example**:
```rust
use mmap_io::{load_mmap, MmapMode};

let ro_mmap = load_mmap("existing.bin", MmapMode::ReadOnly)?;
let rw_mmap = load_mmap("data.bin", MmapMode::ReadWrite)?;
```

<br>

### update_region

```rust
pub fn update_region(mmap: &MemoryMappedFile, offset: u64, data: &[u8]) -> Result<()>
```

**Description**: Writes data to the memory-mapped file at the specified offset.

**Parameters**:
- `mmap`: The memory-mapped file to write to
- `offset`: Byte offset where to start writing
- `data`: Data to write

**Returns**: `Result<()>`

**Errors**:
- `MmapIoError::InvalidMode` if not in ReadWrite mode
- `MmapIoError::OutOfBounds` if offset + data.len() exceeds file size

**Example**:
```rust
use mmap_io::{create_mmap, update_region};

let mmap = create_mmap("data.bin", 1024)?;
update_region(&mmap, 100, b"Hello, World!")?;
```

<br>

### flush

```rust
pub fn flush(mmap: &MemoryMappedFile) -> Result<()>
```

**Description**: Flushes all changes to disk. No-op for read-only mappings.

**Parameters**:
- `mmap`: The memory-mapped file to flush

**Returns**: `Result<()>`

**Errors**:
- `MmapIoError::FlushFailed` if the flush operation fails

**Example**:
```rust
use mmap_io::{create_mmap, update_region, flush};

let mmap = create_mmap("data.bin", 1024)?;
update_region(&mmap, 0, b"data")?;
flush(&mmap)?; // Ensure data is persisted
```

<br>

### copy_mmap

```rust
pub fn copy_mmap<P: AsRef<Path>>(src: P, dst: P) -> Result<()>
```

**Description**: Copies a file using the filesystem. Does not copy the mapping, only file contents.

**Parameters**:
- `src`: Source file path
- `dst`: Destination file path

**Returns**: `Result<()>`

**Errors**:
- `MmapIoError::Io` if the copy operation fails

**Example**:
```rust
use mmap_io::copy_mmap;

copy_mmap("source.bin", "backup.bin")?;
```

<br>

### delete_mmap

```rust
pub fn delete_mmap<P: AsRef<Path>>(path: P) -> Result<()>
```

**Description**: Deletes the file at the specified path. The mapping should be dropped before calling this.

**Parameters**:
- `path`: Path to the file to delete

**Returns**: `Result<()>`

**Errors**:
- `MmapIoError::Io` if the delete operation fails

**Example**:
```rust
use mmap_io::{create_mmap, delete_mmap};

{
    let mmap = create_mmap("temp.bin", 1024)?;
    // Use mmap...
} // mmap dropped here

delete_mmap("temp.bin")?;
```
<hr>
<div align="right"><a href="#doc-top">&uarr; TOP</a></div>
<br>

## MemoryMappedFile Methods

<br>

### create_rw

```rust
pub fn create_rw<P: AsRef<Path>>(path: P, size: u64) -> Result<Self>
```

**Description**: Creates a new file and memory-maps it in read-write mode.

**Parameters**:
- `path`: Path to the file to create
- `size`: Size in bytes (must be > 0)

**Returns**: `Result<MemoryMappedFile>`

**Example**:
```rust
use mmap_io::MemoryMappedFile;

let mmap = MemoryMappedFile::create_rw("new.bin", 4096)?;
```

<br>

### open_ro

```rust
pub fn open_ro<P: AsRef<Path>>(path: P) -> Result<Self>
```

**Description**: Opens an existing file in read-only mode.

**Parameters**:
- `path`: Path to the file to open

**Returns**: `Result<MemoryMappedFile>`

**Example**:
```rust
use mmap_io::MemoryMappedFile;

let mmap = MemoryMappedFile::open_ro("data.bin")?;
```

<br>

### open_rw

```rust
pub fn open_rw<P: AsRef<Path>>(path: P) -> Result<Self>
```

**Description**: Opens an existing file in read-write mode.

**Parameters**:
- `path`: Path to the file to open

**Returns**: `Result<MemoryMappedFile>`

**Errors**:
- `MmapIoError::ResizeFailed` if file is zero-length

**Example**:
```rust
use mmap_io::MemoryMappedFile;

let mmap = MemoryMappedFile::open_rw("data.bin")?;
```

<br>

### open_cow

```rust
#[cfg(feature = "cow")]
pub fn open_cow<P: AsRef<Path>>(path: P) -> Result<Self>
```

**Description**: Opens an existing file in copy-on-write mode. Changes are private to this process.

**Parameters**:
- `path`: Path to the file to open

**Returns**: `Result<MemoryMappedFile>`

**Example**:
```rust
#[cfg(feature = "cow")]
use mmap_io::MemoryMappedFile;

let mmap = MemoryMappedFile::open_cow("shared.bin")?;
```

<br>

### as_slice

```rust
pub fn as_slice(&self, offset: u64, len: u64) -> Result<MappedSlice<'_>>
```

**Description**: Returns a zero-copy read-only view of `[offset, offset + len)`. Since 0.9.7 this works on **all** mapping modes (ReadOnly, CopyOnWrite, and ReadWrite). `MappedSlice<'_>` implements `Deref<Target = [u8]>` and `AsRef<[u8]>` so it can be used as a `&[u8]` directly (indexing, iteration, passing to functions that take `&[u8]` via `&*slice` or `slice.as_ref()`).

On ReadWrite mappings, the returned slice holds an internal read guard for its lifetime. Concurrent `resize()` (which requires the write lock) blocks until the slice is dropped. Other readers and disjoint writes are not blocked.

**Parameters**:
- `offset`: Starting byte offset
- `len`: Number of bytes to include

**Returns**: `Result<MappedSlice<'_>>` - Wrapper around the immutable byte slice

**Errors**:
- `MmapIoError::OutOfBounds` if range exceeds file bounds

**Example**:
```rust
let mmap = MemoryMappedFile::open_ro("data.bin")?;
let data = mmap.as_slice(100, 50)?;
let first_byte = data[0];
// pass to a function that wants `&[u8]`:
fn consume(_: &[u8]) {}
consume(&*data);
```

<br>

### as_slice_mut

```rust
pub fn as_slice_mut(&self, offset: u64, len: u64) -> Result<MappedSliceMut<'_>>
```

**Description**: Returns a mutable slice guard for the specified range. Only available in ReadWrite mode.

**Parameters**:
- `offset`: Starting byte offset
- `len`: Number of bytes to include

**Returns**: `Result<MappedSliceMut>` - Guard providing mutable access

**Errors**:
- `MmapIoError::InvalidMode` if not in ReadWrite mode
- `MmapIoError::OutOfBounds` if range exceeds file bounds

**Example**:
```rust
let mmap = MemoryMappedFile::open_rw("data.bin")?;
{
    let mut guard = mmap.as_slice_mut(0, 10)?;
    guard.as_mut().copy_from_slice(b"0123456789");
} // guard dropped, lock released
```

<br>

### read_into

```rust
pub fn read_into(&self, offset: u64, buf: &mut [u8]) -> Result<()>
```

**Description**: Reads bytes from the mapping into the provided buffer.

**Parameters**:
- `offset`: Starting byte offset
- `buf`: Buffer to read into (length determines how many bytes to read)

**Returns**: `Result<()>`

**Errors**:
- `MmapIoError::OutOfBounds` if range exceeds file bounds

**Example**:
```rust
let mmap = MemoryMappedFile::open_rw("data.bin")?;
let mut buffer = vec![0u8; 100];
mmap.read_into(50, &mut buffer)?;
```

<br>

### update_region

```rust
pub fn update_region(&self, offset: u64, data: &[u8]) -> Result<()>
```

**Description**: Writes data to the mapped file at the specified offset.

**Parameters**:
- `offset`: Starting byte offset
- `data`: Data to write

**Returns**: `Result<()>`

**Errors**:
- `MmapIoError::InvalidMode` if not in ReadWrite mode
- `MmapIoError::OutOfBounds` if range exceeds file bounds

**Example**:
```rust
let mmap = MemoryMappedFile::create_rw("data.bin", 1024)?;
mmap.update_region(100, b"Hello")?;
```

<br>

### flush

Platform Parity: A subsequent fresh read-only mapping observes persisted data after this call on all supported platforms.

```rust
pub fn flush(&self) -> Result<()>
```

**Description**: Flushes all changes to disk.

**Returns**: `Result<()>`

**Errors**:
- `MmapIoError::FlushFailed` if flush operation fails

<br>

### flush_range

Platform Parity: A subsequent fresh read-only mapping observes persisted data for the flushed range after this call. Other non-flushed regions may not be visible until a full `flush()` is performed.

```rust
pub fn flush_range(&self, offset: u64, len: u64) -> Result<()>
```

**Description**: Flushes a specific byte range to disk.

**Parameters**:
- `offset`: Starting byte offset
- `len`: Number of bytes to flush

**Returns**: `Result<()>`

**Errors**:
- `MmapIoError::OutOfBounds` if range exceeds file bounds
- `MmapIoError::FlushFailed` if flush operation fails

<br>

### resize

```rust
pub fn resize(&self, new_size: u64) -> Result<()>
```

**Description**: Resizes the mapped file. Only available in ReadWrite mode.

**Parameters**:
- `new_size`: New size in bytes (must be > 0)

**Returns**: `Result<()>`

**Errors**:
- `MmapIoError::InvalidMode` if not in ReadWrite mode
- `MmapIoError::ResizeFailed` if new size is 0

**Example**:
```rust
let mmap = MemoryMappedFile::create_rw("data.bin", 1024)?;
mmap.resize(2048)?; // Grow to 2KB
```

<br>

### len

```rust
pub fn len(&self) -> u64
```

**Description**: Returns the current length of the mapped file in bytes.

**Returns**: `u64` - File size in bytes

### is_empty

```rust
pub fn is_empty(&self) -> bool
```

**Description**: Returns true if the mapped file is empty (0 bytes).

**Returns**: `bool`

<br>

### path

```rust
pub fn path(&self) -> &Path
```

**Description**: Returns the path to the underlying file.

**Returns**: `&Path`

<br>

### mode

```rust
pub fn mode(&self) -> MmapMode
```

**Description**: Returns the current mapping mode.

**Returns**: `MmapMode`

<br>

### open_or_create

```rust
pub fn open_or_create<P: AsRef<Path>>(path: P, default_size: u64) -> Result<Self>
```

**Description**: Opens `path` for read-write if it exists; creates it at `default_size` bytes otherwise. The classic "open if there, create if not" pattern in one call. Since 0.9.8.

**Parameters**:
- `path`: Path to open or create
- `default_size`: Size used only on the create path; ignored when the file already exists

**Returns**: `Result<MemoryMappedFile>` in ReadWrite mode

**Errors**:
- `MmapIoError::ResizeFailed` if creating and `default_size` is zero
- `MmapIoError::Io` if the filesystem rejects the call

**Example**:
```rust
use mmap_io::MemoryMappedFile;
let mmap = MemoryMappedFile::open_or_create("data.bin", 1024 * 1024)?;
```

<br>

### from_file

```rust
pub fn from_file<P: AsRef<Path>>(file: File, mode: MmapMode, path: P) -> Result<Self>
```

**Description**: Construct a `MemoryMappedFile` from a pre-opened `std::fs::File`. The escape hatch for callers that need custom `OpenOptions` (e.g. `O_DIRECT`, `O_NOATIME`, a specific security context, or a file inherited from a parent process). Since 0.9.8.

**Parameters**:
- `file`: An open File with permissions matching `mode`
- `mode`: Access mode (ReadOnly / ReadWrite / CopyOnWrite)
- `path`: Informational path for `path()` and error messages

**Returns**: `Result<MemoryMappedFile>`

**Errors**:
- `MmapIoError::ResizeFailed` if the file is zero-length on ReadWrite or CopyOnWrite
- `MmapIoError::Io` if metadata or mapping fails

**Example**:
```rust
use std::fs::OpenOptions;
use mmap_io::{MemoryMappedFile, MmapMode};

let file = OpenOptions::new().read(true).write(true).open("data.bin")?;
let mmap = MemoryMappedFile::from_file(file, MmapMode::ReadWrite, "data.bin")?;
# Ok::<(), Box<dyn std::error::Error>>(())
```

<br>

### unmap

```rust
pub fn unmap(self) -> std::result::Result<File, Self>
```

**Description**: Consume the mapping and return the underlying `File`. The mapping is dropped (memory unmapped, background flusher stopped) before the file is returned. Since 0.9.8.

Returns the mapping unchanged (wrapped in `Err`) if other clones of this `MemoryMappedFile` exist; the underlying File cannot be extracted while other handles hold references.

**Returns**: `Ok(File)` on success, `Err(MemoryMappedFile)` if other clones are alive

**Example**:
```rust
use mmap_io::MemoryMappedFile;
use std::io::Write;

let mmap = MemoryMappedFile::create_rw("data.bin", 1024)?;
mmap.update_region(0, b"done")?;
mmap.flush()?;

let mut file = mmap.unmap().expect("no clones alive");
file.write_all(b"more bytes")?;
# Ok::<(), Box<dyn std::error::Error>>(())
```

<br>

### flush_policy

```rust
pub fn flush_policy(&self) -> FlushPolicy
```

**Description**: Returns the `FlushPolicy` this mapping was constructed with. Diagnostic accessor for introspection. Since 0.9.8.

**Returns**: `FlushPolicy`

<br>

### pending_bytes

```rust
pub fn pending_bytes(&self) -> u64
```

**Description**: Bytes written since the last successful flush. Mainly useful for diagnostics under `FlushPolicy::EveryBytes` / `EveryWrites`: poll to see how close you are to the next auto-flush. One atomic read, no I/O. Since 0.9.8.

**Returns**: `u64` accumulator value

<br>

### as_ptr

```rust
pub unsafe fn as_ptr(&self) -> *const u8
```

**Description**: Raw read-only pointer to the start of the mapped region, for FFI use cases that need to hand a `const void *` plus length to a C library. Since 0.9.8.

**Safety**: The caller MUST NOT dereference past `self.len()` bytes, MUST NOT hold the pointer across a `resize()` (which can move the mapping to a new virtual address), and MUST honour Rust aliasing rules at the FFI boundary.

**Returns**: `*const u8` to the base of the mapping

<br>

### as_mut_ptr

```rust
pub unsafe fn as_mut_ptr(&self) -> Result<*mut u8>
```

**Description**: Raw mutable pointer to the start of the mapped region (ReadWrite only). Since 0.9.8.

**Safety**: Same contract as `as_ptr`, plus the caller MUST NOT alias this pointer with any live Rust `&` reference to the same bytes (a `MappedSlice` would alias).

**Returns**: `Result<*mut u8>`

**Errors**:
- `MmapIoError::InvalidMode` if the mapping is not ReadWrite

<br>

### prefetch_range

```rust
pub fn prefetch_range(&self, offset: u64, len: u64) -> Result<()>
```

**Description**: Hint the kernel that the given range of the **backing file** will be read soon. On Linux issues `posix_fadvise(POSIX_FADV_WILLNEED)` against the file descriptor (warms the page cache from the file side). No-op on other platforms. Since 0.9.8.

This is complementary to `advise(offset, len, MmapAdvice::WillNeed)`, which operates on the **mapped virtual memory range** via `madvise`. Both can be issued for cold reads of huge files.

**Parameters**:
- `offset`: Starting byte offset
- `len`: Length of the range to prefetch

**Returns**: `Result<()>`

**Errors**:
- `MmapIoError::OutOfBounds` if range exceeds file bounds
- `MmapIoError::AdviceFailed` if the underlying syscall errors (Linux only)

<br>

### touch_pages

```rust
pub fn touch_pages(&self) -> Result<()>
```

**Description**: Prewarns (touches) all pages by reading the first byte of each page, forcing the OS to load all pages into physical memory. This eliminates page faults during subsequent access, which is useful for benchmarking and performance-critical sections.

**Returns**: `Result<()>`

**Errors**:
- `MmapIoError::Io` if memory access fails

**Performance**:
- **Time Complexity**: O(n) where n is the number of pages
- **Memory Usage**: Forces all pages into physical memory
- **I/O Operations**: May trigger disk reads for unmapped pages
- **Cache Behavior**: Optimizes subsequent access patterns

**Example**:
```rust
let mmap = MemoryMappedFile::open_ro("data.bin")?;

// Prewarm all pages before performance-critical section
mmap.touch_pages()?;

// Now all subsequent accesses will be fast (no page faults)
let data = mmap.as_slice(0, 1024)?;
```

<br>

### touch_pages_range

```rust
pub fn touch_pages_range(&self, offset: u64, len: u64) -> Result<()>
```

**Description**: Prewarns a specific range of pages. Similar to `touch_pages()` but only affects the specified range.

**Parameters**:
- `offset`: Starting offset in bytes
- `len`: Length of range to touch in bytes

**Returns**: `Result<()>`

**Errors**:
- `MmapIoError::OutOfBounds` if range exceeds file bounds
- `MmapIoError::Io` if memory access fails

**Example**:
```rust
let mmap = MemoryMappedFile::create_rw("data.bin", 1024 * 1024)?;

// Prewarm only the first 64KB for immediate use
mmap.touch_pages_range(0, 64 * 1024)?;
```

<hr>
<div align="right"><a href="#doc-top">&uarr; TOP</a></div>
<br>

## Flush Policy

Configurable write flushing behavior for ReadWrite mappings.

Enum:
```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlushPolicy {
    Never,            // Manual control, no automatic flush
    Manual,           // Alias of Never
    Always,           // Flush after every write
    EveryBytes(usize),// Flush when N bytes written since last flush
    EveryWrites(usize), // Flush after W calls to update_region
    EveryMillis(u64), // Automatic time-based flushing every N milliseconds
}
```

Default: FlushPolicy::Never

Builder integration:
```rust
use mmap_io::{MemoryMappedFile, MmapMode};
use mmap_io::flush::FlushPolicy;

let mmap = MemoryMappedFile::builder("file.bin")
    .mode(MmapMode::ReadWrite)
    .size(1_000_000)
    .flush_policy(FlushPolicy::EveryBytes(64 * 1024))
    .create()?;
```

Behavior:
- Never/Manual: internal counters are not used; user must call flush() explicitly.
- Always: flush() is invoked after each update_region() call.
- EveryBytes(n): increments a byte counter by the number of bytes written per update_region; when it reaches n, counter resets and flush() is called.
- EveryWrites(w): increments a write counter per update_region; when it reaches w, counter resets and flush() is called.
- EveryMillis(ms): Enables automatic time-based flushing using a background thread. Writes are tracked and the background thread flushes pending changes every `ms` milliseconds when there are dirty pages. The background thread automatically stops when the MemoryMappedFile is dropped.

Notes:
- Flush is best-effort and may not imply fsync semantics on all platforms.
- COW mappings treat flush() as a no-op.

<hr>
<div align="right"><a href="#doc-top">&uarr; TOP</a></div>
<br>

## Feature-Gated APIs

<br>

### Memory Advise (feature = "advise")

#### advise

```rust
#[cfg(feature = "advise")]
pub fn advise(&self, offset: u64, len: u64, advice: MmapAdvice) -> Result<()>
```

**Description**: Provides hints to the OS about expected access patterns for better performance.

**Parameters**:
- `offset`: Starting byte offset
- `len`: Number of bytes the advice applies to
- `advice`: Type of advice to give

**Returns**: `Result<()>`

**Errors**:
- `MmapIoError::OutOfBounds` if range exceeds file bounds
- `MmapIoError::AdviceFailed` if the system call fails

**Example**:
```rust
#[cfg(feature = "advise")]
use mmap_io::MmapAdvice;

mmap.advise(0, 1024 * 1024, MmapAdvice::Sequential)?;
```

<br>

#### MmapAdvice

```rust
#[cfg(feature = "advise")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MmapAdvice {
    Normal,      // Default access pattern
    Random,      // Random access expected
    Sequential,  // Sequential access expected
    WillNeed,    // Will need this range soon
    DontNeed,    // Won't need this range soon
}
```

<br>

### Iterator-Based Access (feature = "iterator")

> Since 0.9.7 the chunk and page iterators are zero-copy: they yield
> `MappedSlice<'a>` items directly from the mapped region with no
> allocation and no memcpy. Callers who genuinely need owned `Vec<u8>`
> buffers can use `chunks_owned()` / `pages_owned()` as a migration
> aid.

#### chunks

```rust
#[cfg(feature = "iterator")]
pub fn chunks(&self, chunk_size: usize) -> ChunkIterator<'_>
```

**Description**: Zero-copy iterator over fixed-size chunks. The iterator holds a read guard (on RW mappings) for its lifetime; concurrent `resize()` blocks until the iterator is dropped.

**Parameters**:
- `chunk_size`: Size of each chunk in bytes (final chunk may be shorter)

**Returns**: `ChunkIterator` yielding `MappedSlice<'a>` (derefs to `&[u8]`)

**Example**:
```rust
#[cfg(feature = "iterator")]
for chunk in mmap.chunks(4096) {
    let _len = chunk.len();
    let _first = chunk[0];
}
```

<br>

#### pages

```rust
#[cfg(feature = "iterator")]
pub fn pages(&self) -> PageIterator<'_>
```

**Description**: Zero-copy iterator over page-aligned chunks. Same lifetime guarantees as `chunks()`.

**Returns**: `PageIterator` yielding `MappedSlice<'a>`

**Example**:
```rust
#[cfg(feature = "iterator")]
for page in mmap.pages() {
    let _ = page.len();
}
```

<br>

#### chunks_owned / pages_owned

```rust
#[cfg(feature = "iterator")]
pub fn chunks_owned(&self, chunk_size: usize) -> ChunkIteratorOwned<'_>
#[cfg(feature = "iterator")]
pub fn pages_owned(&self) -> PageIteratorOwned<'_>
```

**Description**: Migration-aid iterators that yield `Result<Vec<u8>>`. Each item is allocated and the chunk's bytes are copied into it. Prefer the zero-copy `chunks()` / `pages()` for performance; reach for the owned variants only when you must hand off ownership.

**Example**:
```rust
#[cfg(feature = "iterator")]
for chunk in mmap.chunks_owned(4096) {
    let bytes: Vec<u8> = chunk?;
    let _ = bytes;
}
```

<br>

#### chunks_mut

```rust
#[cfg(feature = "iterator")]
pub fn chunks_mut(&self, chunk_size: usize) -> ChunkIteratorMut<'_>
```

**Description**: Creates a mutable iterator that processes chunks via callback. Since 0.9.7 the write guard is acquired ONCE for the entire iteration (instead of per-chunk).

**Parameters**:
- `chunk_size`: Size of each chunk in bytes

**Returns**: `ChunkIteratorMut` with `for_each_mut` method

**Example**:
```rust
#[cfg(feature = "iterator")]
mmap.chunks_mut(1024).for_each_mut(|_offset, chunk| {
    chunk.fill(0);
    Ok(())
})?;
```

Note: since 0.9.7 the closure returns the crate's `Result<()>`
(was `std::result::Result<(), E>`); the outer `Result` is no longer
nested. If your closure needs to surface a foreign error type, map
it into `MmapIoError::Io(...)` before returning.

<br>

### Atomic Operations (feature = "atomic")

> Since 0.9.5, atomic methods return wrapper types
> (`AtomicView<'_, T>` for single atoms, `AtomicSliceView<'_, T>` for
> slices) instead of bare `&T` / `&[T]`. The wrappers `Deref` to the
> underlying atomic, so call sites that do
> `view.fetch_add(...)` / `slice.iter()` keep working unchanged. The
> wrapper holds the read lock for its lifetime, so a concurrent
> `resize()` blocks while the view is alive (C3 fix).

#### atomic_u64

```rust
#[cfg(feature = "atomic")]
pub fn atomic_u64(&self, offset: u64) -> Result<AtomicView<'_, AtomicU64>>
```

**Description**: Returns an atomic view of a u64 value at the specified offset.

**Parameters**:
- `offset`: Byte offset (must be 8-byte aligned)

**Returns**: `Result<AtomicView<'_, AtomicU64>>` - wrapper that derefs to `&AtomicU64`

**Errors**:
- `MmapIoError::Misaligned` if offset is not 8-byte aligned
- `MmapIoError::OutOfBounds` if offset + 8 exceeds file bounds

**Example**:
```rust
#[cfg(feature = "atomic")]
use std::sync::atomic::Ordering;

let counter = mmap.atomic_u64(0)?;
counter.fetch_add(1, Ordering::SeqCst);
```

<br>

#### atomic_u32

```rust
#[cfg(feature = "atomic")]
pub fn atomic_u32(&self, offset: u64) -> Result<AtomicView<'_, AtomicU32>>
```

**Description**: Returns an atomic view of a u32 value at the specified offset.

**Parameters**:
- `offset`: Byte offset (must be 4-byte aligned)

**Returns**: `Result<AtomicView<'_, AtomicU32>>` - wrapper that derefs to `&AtomicU32`

**Errors**:
- `MmapIoError::Misaligned` if offset is not 4-byte aligned
- `MmapIoError::OutOfBounds` if offset + 4 exceeds file bounds

<br>

#### atomic_u64_slice

```rust
#[cfg(feature = "atomic")]
pub fn atomic_u64_slice(&self, offset: u64, count: usize) -> Result<AtomicSliceView<'_, AtomicU64>>
```

**Description**: Returns a slice of atomic u64 values.

**Parameters**:
- `offset`: Starting byte offset (must be 8-byte aligned)
- `count`: Number of u64 values

**Returns**: `Result<AtomicSliceView<'_, AtomicU64>>` - wrapper that derefs to `&[AtomicU64]`

<br>

#### atomic_u32_slice

```rust
#[cfg(feature = "atomic")]
pub fn atomic_u32_slice(&self, offset: u64, count: usize) -> Result<AtomicSliceView<'_, AtomicU32>>
```

**Description**: Returns a slice of atomic u32 values.

**Parameters**:
- `offset`: Starting byte offset (must be 4-byte aligned)
- `count`: Number of u32 values

**Returns**: `Result<AtomicSliceView<'_, AtomicU32>>` - wrapper that derefs to `&[AtomicU32]`

<br>

### Memory Locking (feature = "locking")

#### lock

```rust
#[cfg(feature = "locking")]
pub fn lock(&self, offset: u64, len: u64) -> Result<()>
```

**Description**: Locks memory pages to prevent them from being swapped out. Requires appropriate privileges.

**Parameters**:
- `offset`: Starting byte offset
- `len`: Number of bytes to lock

**Returns**: `Result<()>`

**Errors**:
- `MmapIoError::OutOfBounds` if range exceeds file bounds
- `MmapIoError::LockFailed` if lock operation fails (often due to privileges)

**Example**:
```rust
#[cfg(feature = "locking")]
mmap.lock(0, 4096)?; // Lock first page
```

<br>

#### unlock

```rust
#[cfg(feature = "locking")]
pub fn unlock(&self, offset: u64, len: u64) -> Result<()>
```

**Description**: Unlocks previously locked memory pages.

**Parameters**:
- `offset`: Starting byte offset
- `len`: Number of bytes to unlock

**Returns**: `Result<()>`

<br>

#### lock_all

```rust
#[cfg(feature = "locking")]
pub fn lock_all(&self) -> Result<()>
```

**Description**: Locks all pages of the memory-mapped file.

**Returns**: `Result<()>`

<br>

#### unlock_all

```rust
#[cfg(feature = "locking")]
pub fn unlock_all(&self) -> Result<()>
```

**Description**: Unlocks all pages of the memory-mapped file.

**Returns**: `Result<()>`

<br>

### File Watching (feature = "watch")

> Since 0.9.9 the watcher uses the OS-native event source on every
> supported platform: `inotify` on Linux, FSEvents on macOS, and
> `ReadDirectoryChangesW` on Windows. The polling fallback used
> through 0.9.8 is gone, along with the Windows mtime granularity
> issue that previously forced three watch tests to be ignored.
>
> Note: mmap-side writes (`mmap.update_region(...)` + `mmap.flush()`)
> only reach the FS watcher at OS-decided writeback time and are
> not a reliable trigger for any platform's native event source.
> Reliable detection comes from `std::fs` API writes by another
> process / handle. This matches the actual real-world use case
> for `watch`: detect changes made by something other than the
> current mapping holder.

#### watch

```rust
#[cfg(feature = "watch")]
pub fn watch<F>(&self, callback: F) -> Result<WatchHandle>
where
    F: Fn(ChangeEvent) + Send + 'static
```

**Description**: Watch the backing file for changes using the OS-native event source. The callback runs on a dedicated dispatcher thread for each detected change. Drop the returned `WatchHandle` to stop watching and release the OS subscription.

**Parameters**:
- `callback`: `Fn(ChangeEvent) + Send + 'static` invoked once per detected change

**Returns**: `Result<WatchHandle>` - drop to stop watching

**Platform behavior**:

| Platform | Backend                       | Typical latency      |
|----------|-------------------------------|----------------------|
| Linux    | `inotify`                     | <1 ms                |
| macOS    | FSEvents                      | <50 ms (coalesced)   |
| Windows  | `ReadDirectoryChangesW`       | <10 ms               |

Event coalescing differs by platform: FSEvents on macOS batches at ~50 ms by design; `inotify` and RDCW deliver events as the kernel sees them. Callers that need to debounce should do so on top of the callback (e.g. wait 100 ms after the last event before reacting).

**Errors**:
- `MmapIoError::WatchFailed` if the OS subscription cannot be established (missing inotify support, exhausted per-process watch limit, path disappeared between the call and the kernel registration, etc.)

**Example**:
```rust
#[cfg(feature = "watch")]
use mmap_io::{MemoryMappedFile, watch::ChangeEvent};

let mmap = MemoryMappedFile::open_ro("data.bin")?;
let handle = mmap.watch(|event: ChangeEvent| {
    println!("File changed: {:?}", event.kind);
})?;
// ...handle dropped at end of scope stops the watch.
# Ok::<(), mmap_io::MmapIoError>(())
```

<br>

#### ChangeEvent

```rust
#[cfg(feature = "watch")]
#[derive(Debug, Clone)]
pub struct ChangeEvent {
    pub offset: Option<u64>,  // Offset where change occurred (if known)
    pub len: Option<u64>,     // Length of changed region (if known)
    pub kind: ChangeKind,     // Type of change
}
```

<br>

#### ChangeKind

```rust
#[cfg(feature = "watch")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeKind {
    Modified,  // File content was modified
    Metadata,  // File metadata changed
    Removed,   // File was removed
}
```
<hr>
<div align="right"><a href="#doc-top">&uarr; TOP</a></div>
<br>

## Segment Types

### Segment

```rust
pub struct Segment { /* private fields */ }
```

**Description**: Immutable view into a region of a memory-mapped file.

**Methods**:
- `new(parent: Arc<MemoryMappedFile>, offset: u64, len: u64) -> Result<Self>`
- `as_slice(&self) -> Result<&[u8]>`
- `len(&self) -> u64`
- `is_empty(&self) -> bool`
- `offset(&self) -> u64`
- `parent(&self) -> &MemoryMappedFile`

**Example**:
```rust
use std::sync::Arc;
use mmap_io::segment::Segment;

let mmap = Arc::new(MemoryMappedFile::open_ro("data.bin")?);
let segment = Segment::new(mmap.clone(), 100, 50)?;
let data = segment.as_slice()?;
```

<br>

### SegmentMut

```rust
pub struct SegmentMut { /* private fields */ }
```

**Description**: Mutable view into a region of a memory-mapped file.

**Methods**:
- `new(parent: Arc<MemoryMappedFile>, offset: u64, len: u64) -> Result<Self>`
- `as_slice_mut(&self) -> Result<MappedSliceMut<'_>>`
- `write(&self, data: &[u8]) -> Result<()>`
- `len(&self) -> u64`
- `is_empty(&self) -> bool`
- `offset(&self) -> u64`
- `parent(&self) -> &MemoryMappedFile`

<hr>
<div align="right"><a href="#doc-top">&uarr; TOP</a></div>
<br>

## Async Operations (feature = "async")

The crate exposes two layers of async helpers: manager-level
free functions for file lifecycle (`create_mmap_async`,
`copy_mmap_async`, `delete_mmap_async`) and instance methods on
`MemoryMappedFile` for write / flush operations. The instance
methods auto-flush after each call to guarantee post-await
durability across platforms (Async-Only Flushing).

### update_region_async

```rust
#[cfg(feature = "async")]
pub async fn update_region_async(&self, offset: u64, data: &[u8]) -> Result<()>
```

**Description**: Async write that also flushes after the write completes. The write itself runs on a `tokio::spawn_blocking` task; the flush is unconditional regardless of the configured `FlushPolicy`. This is the cross-platform-safe write path for async code.

**Parameters**:
- `offset`: Starting byte offset
- `data`: Bytes to write

**Returns**: `Result<()>`

**Errors**:
- `MmapIoError::InvalidMode` if not in `ReadWrite` mode
- `MmapIoError::OutOfBounds` if `offset + data.len()` exceeds file bounds
- `MmapIoError::FlushFailed` if the post-write flush fails

**Example**:
```rust
#[cfg(feature = "async")]
mmap.update_region_async(128, b"ASYNC-FLUSH").await?;
```

<br>

### flush_async

```rust
#[cfg(feature = "async")]
pub async fn flush_async(&self) -> Result<()>
```

**Description**: Async equivalent of `flush()`. Runs the underlying flush in a `spawn_blocking` task so the async scheduler is not blocked on disk I/O.

**Returns**: `Result<()>`

**Errors**:
- `MmapIoError::FlushFailed` if the flush operation fails

<br>

### flush_range_async

```rust
#[cfg(feature = "async")]
pub async fn flush_range_async(&self, offset: u64, len: u64) -> Result<()>
```

**Description**: Async equivalent of `flush_range()`. Same cancellation semantics as `flush_async`.

**Parameters**:
- `offset`: Starting byte offset
- `len`: Length of the range to flush

**Returns**: `Result<()>`

**Errors**:
- `MmapIoError::OutOfBounds` if range exceeds file bounds
- `MmapIoError::FlushFailed` if the flush operation fails

<br>

### create_mmap_async

```rust
#[cfg(feature = "async")]
pub async fn create_mmap_async<P: AsRef<Path>>(
    path: P, 
    size: u64
) -> Result<MemoryMappedFile>
```

**Description**: Asynchronously creates a new memory-mapped file.

**Parameters**:
- `path`: Path to the file to create
- `size`: Size in bytes

**Returns**: `Result<MemoryMappedFile>`

**Example**:
```rust
#[cfg(feature = "async")]
let mmap = mmap_io::manager::r#async::create_mmap_async("async.bin", 4096).await?;
```

<br>

### copy_mmap_async

```rust
#[cfg(feature = "async")]
pub async fn copy_mmap_async<P: AsRef<Path>>(src: P, dst: P) -> Result<()>
```

**Description**: Asynchronously copies a file.

**Parameters**:
- `src`: Source file path
- `dst`: Destination file path

**Returns**: `Result<()>`

<br>

### delete_mmap_async

```rust
#[cfg(feature = "async")]
pub async fn delete_mmap_async<P: AsRef<Path>>(path: P) -> Result<()>
```

**Description**: Asynchronously deletes a file.

**Parameters**:
- `path`: Path to the file to delete

**Returns**: `Result<()>`

<hr>
<div align="right"><a href="#doc-top">&uarr; TOP</a></div>
<br>

## Utility Functions

### page_size

```rust
pub fn page_size() -> usize
```

**Description**: Returns the system's memory page size in bytes.

**Returns**: `usize` - Page size (typically 4096 on most systems)

**Example**:
```rust
use mmap_io::utils::page_size;

let ps = page_size();
println!("System page size: {} bytes", ps);
```

<br>

### align_up

```rust
pub fn align_up(value: u64, alignment: u64) -> u64
```

**Description**: Aligns a value up to the nearest multiple of alignment.

**Parameters**:
- `value`: Value to align
- `alignment`: Alignment boundary

**Returns**: `u64` - Aligned value

**Example**:
```rust
use mmap_io::utils::align_up;

let aligned = align_up(1001, 1024); // Returns 1024
let aligned2 = align_up(2048, 1024); // Returns 2048
```

<hr>
<div align="right"><a href="#doc-top">&uarr; TOP</a></div>
<br>

## Safety and Best Practices

This crate uses `unsafe` code internally to interact with system memory mapping APIs:
- **Unix:** Uses `mmap`, `munmap`, `msync`, `madvise`, etc.
- **Windows:** Uses `CreateFileMappingW`, `MapViewOfFile`, `VirtualLock`, etc.

We expose only safe public APIs, but the following safety considerations apply:

<br>

### Mapped Memory Access
- You must not access memory after the file is closed, truncated, or deleted.
- Writing to `as_slice_mut()` is only allowed in `ReadWrite` or `CopyOnWrite` modes.
- Do not share mutable slices across threads without synchronization.

<br>

### Copy-On-Write (COW) Mode
- Writes are isolated per-process and never flushed to disk.
- `as_slice_mut()` is restricted in COW mode (future support planned).
- Flush in COW is a no-op for disk persistence.


<br>

### Flushing Behavior
- `flush()` ensures memory is flushed to the underlying file, but is a **best-effort** operation and may not guarantee disk sync unless `sync_all` or `fsync` is also called.
- Platform Parity: After `flush()`/`flush_range()`, opening a new RO mapping will observe the persisted bytes (entire file for `flush()`, specific region for `flush_range()`).
- Async-Only Flushing: When using async helpers, writes are auto-flushed after each async write to maintain cross-platform visibility guarantees.

<br>

### Thread Safety
`MemoryMappedFile` can be used across threads with `Arc`, but internal mutability requires synchronization if using `as_slice_mut()`.
- All operations are thread-safe through interior mutability
- Read operations can proceed concurrently
- Write operations are serialized through `RwLock`

<br>

### Performance Tips
1. Use `advise()` to hint access patterns for better OS optimization
2. Prefer page-aligned operations when possible
3. Use iterators for sequential processing of large files
4. Lock critical memory regions to prevent swapping
5. Batch writes and flush once rather than flushing frequently

<br>

### Common Pitfalls
1. Don't hold mutable guards across `flush()` calls (causes deadlock)
2. Ensure proper alignment when using atomic operations
3. Drop mappings before deleting files
4. Check privileges before using memory locking
5. Handle watch events promptly to avoid missing changes

<br>

### Error Handling
All operations return `Result<T, MmapIoError>`. Common error scenarios:
- `OutOfBounds`: Accessing beyond file boundaries
- `InvalidMode`: Operation not supported in current mode
- `Misaligned`: Atomic operations require proper alignment
- `LockFailed`: Usually due to insufficient privileges
- `Io`: Underlying filesystem errors

<hr>
<div align="right"><a href="#doc-top">&uarr; TOP</a></div>
<br>

## Examples

### Database-like Usage
```rust
use mmap_io::{MemoryMappedFile, MmapAdvice};
use std::sync::atomic::Ordering;

// Create a file for storing records
let db = MemoryMappedFile::create_rw("database.bin", 1024 * 1024)?;

// Advise random access pattern
db.advise(0, 1024 * 1024, MmapAdvice::Random)?;

// Use atomic counter for record count
let record_count = db.atomic_u64(0)?;
record_count.store(0, Ordering::SeqCst);

// Write records starting at offset 64
let record_data = b"First record";
db.update_region(64, record_data)?;
record_count.fetch_add(1, Ordering::SeqCst);

db.flush()?;
```

<br>

### Game Asset Loading
```rust
use mmap_io::{MemoryMappedFile, MmapAdvice};

// Load game assets read-only
let assets = MemoryMappedFile::open_ro("game_assets.dat")?;

// Hint that we'll need textures soon
assets.advise(0, 50 * 1024 * 1024, MmapAdvice::WillNeed)?;

// Load texture data
let texture_data = assets.as_slice(1024 * 1024, 2048 * 2048 * 4)?;
```

<br>

### Log File Processing
```rust
#[cfg(feature = "iterator")]
use mmap_io::MemoryMappedFile;

let log = MemoryMappedFile::open_ro("app.log")?;

// Process log file line by line using chunks
for chunk in log.chunks(4096) {
    let data = chunk?;
    // Process lines in chunk...
}
```

<br>

### Concurrent Counter
```rust
#[cfg(feature = "atomic")]
use mmap_io::MemoryMappedFile;
use std::sync::Arc;
use std::thread;
use std::sync::atomic::Ordering;

let mmap = Arc::new(MemoryMappedFile::create_rw("counters.bin", 64)?);

// Initialize counters
for i in 0..8 {
    let counter = mmap.atomic_u64(i * 8)?;
    counter.store(0, Ordering::SeqCst);
}

// Spawn threads to increment counters
let handles: Vec<_> = (0..4).map(|i| {
    let mmap = Arc::clone(&mmap);
    thread::spawn(move || {
        let counter = mmap.atomic_u64(i * 8).unwrap();
        for _ in 0..1000 {
            counter.fetch_add(1, Ordering::SeqCst);
        }
    })
}).collect();

for handle in handles {
    handle.join().unwrap();
}
```

<hr>
<div align="right"><a href="#doc-top">&uarr; TOP</a></div>
<br><br>

## Version History
- **0.9.11**: Patch release. Compat shims for the 0.9.7 semver violation (`as_slice_bytes`, `for_each_mut_legacy`). Runtime-agnostic async via `blocking` crate (smol/tokio/async-std all work). New `bytes::Bytes` integration (`feature = "bytes"`), `io::Read`+`io::Seek` cursor (`mmap.reader()`), and `AsFd`/`AsRawFd` (Unix) + `AsHandle`/`AsRawHandle` (Windows) trait impls.
- **0.9.10**: Pre-1.0 stabilization (Lockdown). Audit D1, D7, D8, R1-R7, D5 closed. Ten focused examples, `cargo-fuzz` scaffold, `docs/PERFORMANCE.md` with measured numbers, `cargo-audit` + `cargo-semver-checks` CI workflows, bench-regression hard gate. MSRV held at Rust 1.75.
- **0.9.9**: Native watch backends. `inotify` (Linux), FSEvents (macOS), `ReadDirectoryChangesW` (Windows) replace the polling implementation, backed by the `notify 6` crate gated on the `watch` feature. Three previously-ignored Windows watch tests now pass live; five new integration tests cover modify / truncate / extend / rapid-sequence / removed.
- **0.9.8**: Ergonomic API expansion (closes audit E1, E2, E6, E7, F2, F5, F9). Adds `open_or_create`, builder `open_or_create`, `from_file`, `unmap`, `flush_policy`, `pending_bytes`, `unsafe as_ptr` / `as_mut_ptr`, and `prefetch_range`. Hot-path bounds-check helpers (`ensure_in_bounds`, `slice_range`) and length/mode accessors marked `#[inline]`. Fixed a Duration underflow in the time-based flusher's slice arithmetic.
- **0.9.7**: Performance milestone (closes audit H1, H2, H4, E4). `as_slice` returns `MappedSlice<'_>` and works uniformly on RO / COW / RW (breaking). Iterators are zero-copy and yield `MappedSlice<'a>` directly (breaking); `chunks_owned` / `pages_owned` provided as migration aids. `touch_pages` rewritten as a tight `ptr::read_volatile` loop holding the lock once (~50-100x speedup on multi-GiB files). `chunks_mut().for_each_mut` flattened to `Result<()>` and holds the write guard once for the whole iteration. New workload-pattern benches and `bench-regression.yml` CI workflow.
- **0.9.6**: Unsafe audit (closes audit S2, S3); SAFETY comments rewritten with platform-spec citations; `docs/SAFETY.md` added; property-test suite (`tests/proptest_bounds.rs`, `tests/proptest_atomic.rs`, `tests/proptest_flush.rs`) added via `proptest 1.5`; CI matrix-feature gate fix.
- **0.9.5**: Correctness bugfix release. Closes audit C1 (`flush_range` accumulator), C2 (`FlushPolicy::EveryMillis` now actually flushes), C3 (atomic-view UAF; methods now return `AtomicView<'_, T>` / `AtomicSliceView<'_, T>` wrappers), H5 (`WatchHandle::drop` signals thread), H6 (`Segment::as_slice` re-validates bounds), H7 (`page_size()` cached via `OnceLock`).
- **0.9.4**: Production-Ready Performance 
- **0.9.3**: Final optimizations, cleaned codebase.
- **0.9.0**: Fixed Remaining Issues, Finalized Codebase for Stable Beta Release.
- **0.8.0**: Added Async-Only Flushing APIs; Platform Parity docs and tests; Huge Pages docs.
- **0.7.5**: Added Flush Policy.
- **0.7.3**: Fixed Build Errors.
- **0.7.2**: Added CHANGELOG and updated Documentation.
- **0.7.1**: Added atomic, locking, and watch features.
- **0.7.0**: Added advise and iterator features.
- **0.5.0**: Added copy-on-write mode support.
- **0.3.0**: Added async support with Tokio.
- **0.2.0**: basic mmap functionality with segment types.
- **0.1.0**: Initial release.

<br>

View the [CHANGELOG](../CHANGELOG.md).

<br>


<!--// LICENSE // -->
<div align="center">
    <br>
    <h2>LICENSE</h2>
    <p>
        Licensed under the <b>Apache License</b>, <b>Version 2.0</b>. 
        <br>
        See <b><a href="../LICENSE">LICENSE</a></b> file for details.
    </p>
</div>


<!--// COPYRIGHT // -->
<div align="center">
    <br>
    <h2></h2>
    <sub>Copyright &copy; 2026 James Gober.</sub>
</div>