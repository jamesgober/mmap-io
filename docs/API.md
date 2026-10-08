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
  - [AnonymousMmap](#anonymousmmap) (1.0.0)
  - [MmapMode](#mmapmode)
  - [MappedSlice](#mappedslice)
  - [MappedSliceMut](#mappedslicemut)
  - [TouchHint](#touchhint)
  - [MmapReader](#mmapreader) (0.9.11)
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
  - [try_as_slice / try_as_slice_mut / try_update_region](#try_as_slice--try_as_slice_mut--try_update_region) (1.1.0)
  - [flush](#flush-1)
  - [flush_range](#flush_range)
  - [schedule_flush / schedule_flush_range](#schedule_flush--schedule_flush_range) (1.1.0)
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
  - [as_slice_bytes](#as_slice_bytes) (0.9.11, 0.9.6-compat)
  - [read_bytes](#read_bytes) (0.9.11, feature = "bytes")
  - [reader](#reader) (0.9.11)
  - [is_hugepage_backed](#is_hugepage_backed) (1.0.0)
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
- **[Raw Mapping Tier](#raw-mapping-tier-mmap_ioraw)**
  - [When to use raw](#when-to-use-raw)
  - [RawMmapOptions](#rawmmapoptions)
  - [RawMmap](#rawmmap)
  - [RawMmapMut](#rawmmapmut)
  - [Protection changes, advice and locking](#protection-changes-advice-and-locking) (1.1.0)
  - [offset_granularity](#offset_granularity)
  - [Behavior and platform notes](#behavior-and-platform-notes)
  - [Performance](#performance)
- **[Utility Functions](#utility-functions)**
  - [page_size](#page_size)
  - [align_up](#align_up)
- **[Safety and Best Practices](#safety-and-best-practices)**
- **[Flush Policy](#flush-policy)**
  - [Mapped Memory Access](#mapped-memory-access)
  - [Range Validation](#range-validation)
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
| `async`    | Runtime-agnostic async helpers (drives on tokio, smol, async-std, custom executors).                |
| `bytes`    | `bytes::Bytes` conversions for the hyper/tower/tonic/axum/reqwest ecosystem.                        |
| `advise`   | Memory hinting via **`madvise`/`posix_madvise` (Unix)** or **Prefetch (Windows)**.                  |
| `iterator` | Iterator-based access to memory chunks or pages with zero-copy read access.                         |
| `hugepages` | Transparent huge page hint (`madvise(MADV_HUGEPAGE)`) on Linux RW mappings; no effect elsewhere. Use `is_hugepage_backed()` to confirm at runtime.|
| `cow`      | Copy-on-Write mapping mode using private memory views (per-process isolation).                       |
| `locking`  | Page-level memory locking via **`mlock`/`munlock` (Unix)** or **`VirtualLock` (Windows)**.           |
| `atomic`   | Atomic views into memory as aligned `u32` / `u64`, with strict alignment checking.                  |
| `watch`    | Native file-change notifications (inotify on Linux, FSEvents on macOS, ReadDirectoryChangesW on Windows). |

<br>

- **Huge Pages** (`feature = "hugepages"`): On Linux, `madvise(MADV_HUGEPAGE)` on `ReadWrite` mappings built with `.huge_pages(true)`. A hint the kernel may ignore (file-backed mappings on most disk filesystems stay on base pages); `MAP_HUGETLB` and Windows large pages are not used.

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
mmap-io = { version = "1.0.0" }
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
mmap-io = { version = "1.0.0", features = ["cow", "locking"] }
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
mmap-io = { version = "1.0.0", default-features = false, features = ["locking"] }
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

### AnonymousMmap

*(Since 1.0.0)*

Process-local memory mapping with no backing file. Useful for shared scratch memory between threads and as a kernel-side substrate for IPC patterns. Pages are zero-initialized on first touch; memory is released when the value is dropped.

```rust
pub struct AnonymousMmap { /* private fields */ }
```

**Differences from `MemoryMappedFile`**:
- No file descriptor / handle; no `AsFd`/`AsRawFd`/`AsHandle` impls.
- No `resize` (the underlying mapping does not support it).
- No `flush` (volatile memory; nothing to persist).
- No `path` (there is no path).

Everything else (read, write, slice access, the `try_` methods, and since 1.1.0 atomic views with feature `atomic`) works identically.

**Example**:
```rust
use mmap_io::AnonymousMmap;

let mmap = AnonymousMmap::new(4096)?;
mmap.update_region(0, b"hello")?;
let mut buf = [0u8; 5];
mmap.read_into(0, &mut buf)?;
assert_eq!(&buf, b"hello");
# Ok::<(), mmap_io::MmapIoError>(())
```

#### Methods

| Method | Signature | Notes |
|--------|-----------|-------|
| `new` | `fn new(size: u64) -> Result<Self>` | Allocate `size` bytes. Errors on zero/oversized. |
| `len` | `fn len(&self) -> u64` | Length in bytes. |
| `is_empty` | `fn is_empty(&self) -> bool` | Always `false` for a constructed mapping. |
| `read_into` | `fn read_into(&self, offset: u64, buf: &mut [u8]) -> Result<()>` | Copy bytes out of the mapping. |
| `update_region` | `fn update_region(&self, offset: u64, data: &[u8]) -> Result<()>` | Copy bytes into the mapping. |
| `as_slice` | `fn as_slice(&self, offset: u64, len: u64) -> Result<MappedSlice<'_>>` | Borrow a read-only slice (holds a read lock). |
| `as_mut_slice` | `fn as_mut_slice(&self, offset: u64, len: u64) -> Result<MappedSliceMut<'_>>` | Borrow a mutable slice (holds a write lock). |
| `try_as_slice` | `fn try_as_slice(&self, offset: u64, len: u64) -> Result<Option<MappedSlice<'_>>>` | 1.1.0. `Ok(None)` instead of waiting for a writer. |
| `try_as_mut_slice` | `fn try_as_mut_slice(&self, offset: u64, len: u64) -> Result<Option<MappedSliceMut<'_>>>` | 1.1.0. `Ok(None)` instead of waiting for views or writers. |
| `try_update_region` | `fn try_update_region(&self, offset: u64, data: &[u8]) -> Result<bool>` | 1.1.0. `Ok(false)` instead of waiting. |
| `atomic_u64` / `atomic_u32` | `fn atomic_u64(&self, offset: u64) -> Result<AtomicView<'_, AtomicU64>>` | 1.1.0, feature `atomic`. Same checks as on `MemoryMappedFile`. |
| `atomic_u64_slice` / `atomic_u32_slice` | `fn atomic_u64_slice(&self, offset: u64, count: usize) -> Result<AtomicSliceView<'_, AtomicU64>>` | 1.1.0, feature `atomic`. |
| `as_ptr` | `unsafe fn as_ptr(&self) -> *const u8` | Raw byte pointer for FFI. |
| `as_mut_ptr` | `unsafe fn as_mut_ptr(&self) -> *mut u8` | Raw mutable byte pointer for FFI. |

<br>

### MappedSlice

Read-only slice into a memory-mapped region. For RW and COW mappings it holds the read lock for its lifetime, so `resize` and every write method (`update_region`, `as_slice_mut`, `chunks_mut`) block until the slice is dropped. Calling one of those on the thread that holds the slice deadlocks; the `try_` methods return "would block" instead. `Send + Sync`.

```rust
pub struct MappedSlice<'a> { /* private fields */ }

impl<'a> MappedSlice<'a> {
    pub fn len(&self) -> usize;
    pub fn is_empty(&self) -> bool;
    pub fn as_slice(&self) -> &[u8];
}

impl std::ops::Deref for MappedSlice<'_> { type Target = [u8]; }
impl AsRef<[u8]> for MappedSlice<'_> { /* ... */ }
impl PartialEq for MappedSlice<'_> { /* ... */ }
impl PartialEq<[u8]> for MappedSlice<'_> { /* ... */ }
impl<const N: usize> PartialEq<[u8; N]> for MappedSlice<'_> { /* ... */ }
```

<br>

### MappedSliceMut

Mutable slice into a memory-mapped region. Holds the write lock for its lifetime; any concurrent reader or writer blocks until dropped.

```rust
pub struct MappedSliceMut<'a> { /* private fields */ }

impl<'a> MappedSliceMut<'a> {
    pub fn len(&self) -> usize;
    pub fn is_empty(&self) -> bool;
    pub fn as_mut(&mut self) -> &mut [u8];
}

impl std::ops::Deref for MappedSliceMut<'_> { type Target = [u8]; }
impl std::ops::DerefMut for MappedSliceMut<'_> { /* ... */ }
```

<br>

### MmapReader

*(Since 0.9.11)*

Cursor wrapper that implements `std::io::Read` and `std::io::Seek`. Plugs an `mmap-io` mapping into any parser or decoder that takes a generic `R: Read`: `serde_json::from_reader`, `flate2::read::GzDecoder`, `tar::Archive::new`, `BufReader`, etc.

```rust
pub struct MmapReader<'a> { /* private fields */ }

impl<'a> MmapReader<'a> {
    pub fn position(&self) -> u64;
    pub fn set_position(&mut self, pos: u64);
}

impl std::io::Read for MmapReader<'_> { /* ... */ }
impl std::io::Seek for MmapReader<'_> { /* ... */ }
```

Construct via [`MemoryMappedFile::reader`](#reader).

<br>

### TouchHint

Enum representing when to touch (prewarm) memory pages during mapping creation.

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TouchHint {
    Never,   // Don't touch pages during creation (default)
    Eager,   // Eagerly touch all pages during creation
    Lazy,    // Same as Never; kept for API compatibility
}
```

**Variants**:
- `Never`: Don't touch pages during creation (default)
- `Eager`: Eagerly touch all pages during creation to prewarm page tables and improve first-access latency. Useful for benchmarking scenarios where you want consistent timing without page fault overhead.
- `Lazy`: Same as `Never`: pages are faulted in by the OS on first access. No separate lazy prefetch exists; the variant is kept for API compatibility.

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
- `CopyOnWrite`: Private, writable mapping of an existing file (feature `cow`, writable since 1.1.0). Writes land in private pages, are visible through this mapping and its clones, and never reach the file. `flush` / `flush_range` are no-ops, `pending_bytes()` stays 0, `resize` returns `InvalidMode`.

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

**Description**: Same as `MemoryMappedFile::flush`: synchronously writes dirty pages back and waits for the OS to report them written. No-op for read-only and copy-on-write mappings.

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

**Description**: Opens an existing file in copy-on-write mode (`MAP_PRIVATE` / `PAGE_WRITECOPY`). The file only needs read permission. Since 1.1.0 the mapping is writable: `update_region`, `as_slice_mut`, `chunks_mut`, `as_mut_ptr` and the atomic views all work; each written page is copied on first write, and the changes are visible through this mapping (and its clones) only. They never reach the file and are lost when the mapping is dropped. `flush` and `flush_range` are `Ok` no-ops (ranges are still validated), `pending_bytes()` stays 0, and `resize` returns `InvalidMode`. Locking is the same as for `ReadWrite`: a live view blocks the write methods, and a write on the thread that holds a view deadlocks (the `try_` methods avoid that). `advise(.., DontNeed)` discards private copies on Linux, so on this mode it takes the write lock (see [advise](#advise)). Pages not yet written may still reflect later changes others make to the file (POSIX leaves this unspecified; Windows shows them).

**Behavior change in 1.1.0**: write methods returned `InvalidMode` on this mode before; `as_slice_bytes` now returns `InvalidMode` on it (it cannot hand out an unguarded `&[u8]` to writable memory).

**Parameters**:
- `path`: Path to the file to open

**Returns**: `Result<MemoryMappedFile>`

**Example**:
```rust
#[cfg(feature = "cow")]
use mmap_io::MemoryMappedFile;

let mmap = MemoryMappedFile::open_cow("shared.bin")?;
mmap.update_region(0, b"patched")?;           // private; the file is unchanged
assert_eq!(&*mmap.as_slice(0, 7)?, b"patched");
```

<br>

### as_slice

```rust
pub fn as_slice(&self, offset: u64, len: u64) -> Result<MappedSlice<'_>>
```

**Description**: Returns a zero-copy read-only view of `[offset, offset + len)`. Since 0.9.7 this works on **all** mapping modes (ReadOnly, CopyOnWrite, and ReadWrite). `MappedSlice<'_>` implements `Deref<Target = [u8]>` and `AsRef<[u8]>` so it can be used as a `&[u8]` directly (indexing, iteration, passing to functions that take `&[u8]` via `&*slice` or `slice.as_ref()`).

On ReadWrite mappings, the returned slice holds an internal read guard for its lifetime. Other readers are not blocked, but every operation that needs the write lock is, whatever region it touches: `resize()`, `update_region()`, `as_slice_mut()`, and `chunks_mut()` wait until the slice is dropped. Calling one of them on the thread that holds the slice deadlocks. A zero-length request returns an empty slice at any offset.

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

**Description**: Returns a mutable slice guard for the specified range. Only available in ReadWrite mode. The guard holds the write lock until dropped; every other reader and writer waits. When the guard drops, its length is added to `pending_bytes()`.

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

**Description**: Writes data to the mapped file at the specified offset under the write lock, then applies the flush policy. Empty `data` is accepted at any offset and does nothing.

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

On a thread that may hold a `MappedSlice`, iterator item, or atomic view of the same mapping, `update_region` deadlocks; use [`try_update_region`](#try_as_slice--try_as_slice_mut--try_update_region).

<br>

### try_as_slice / try_as_slice_mut / try_update_region

*(Since 1.1.0)*

```rust
pub fn try_as_slice(&self, offset: u64, len: u64) -> Result<Option<MappedSlice<'_>>>
pub fn try_as_slice_mut(&self, offset: u64, len: u64) -> Result<Option<MappedSliceMut<'_>>>
pub fn try_update_region(&self, offset: u64, data: &[u8]) -> Result<bool>
```

**Description**: Non-blocking versions of `as_slice`, `as_slice_mut` and `update_region`. Instead of waiting for the mapping's lock they report "would block": `Ok(None)` for the slice methods, `Ok(false)` for `try_update_region` (a `bool` because "written or not" is the only information; `Ok(true)` means the bytes are written). They exist because a live read view (`MappedSlice`, iterator item, atomic view) blocks every writer, including a write on the same thread, which deadlocks with the blocking methods.

| Method | Reports "would block" when |
|--------|----------------------------|
| `try_as_slice` | a writer holds the lock (`MappedSliceMut`, running `update_region` / `chunks_mut` / `resize`). Readers never block readers. `ReadOnly` mappings have no lock and always return `Some`. |
| `try_as_slice_mut` | any view or writer holds the lock, on any thread |
| `try_update_region` | any view or writer holds the lock, on any thread |

Otherwise they behave like the blocking versions: same mode checks (`InvalidMode` on `ReadOnly` for the write methods, checked before the lock), the range is validated under the lock (not checked when "would block" is returned), zero-length requests are accepted at any offset, `try_as_slice` refuses ranges that overlap a live atomic view, and `try_update_region` counts `pending_bytes()` and runs the flush policy. A policy flush runs under the lock already held (downgraded to a read guard), so the call never waits for another lock holder; it does wait for the disk when the policy flushes. The crate's internal view-tracking locks may be taken for a few instructions.

`AnonymousMmap` has the same three methods (`try_as_slice`, `try_as_mut_slice`, `try_update_region`); its length never changes, so ranges are validated before the lock.

**Example**:
```rust
use mmap_io::MemoryMappedFile;

let mmap = MemoryMappedFile::create_rw("data.bin", 1024)?;
let header = mmap.as_slice(0, 16)?;          // this thread holds a read view
// mmap.update_region(100, b"x")?;           // would deadlock
if !mmap.try_update_region(100, b"x")? {
    // Busy: retry after dropping our views, or hand the write to
    // another thread.
}
drop(header);
assert!(mmap.try_update_region(100, b"x")?);
# Ok::<(), mmap_io::MmapIoError>(())
```

<br>

### flush

```rust
pub fn flush(&self) -> Result<()>
```

**Description**: Writes all dirty pages back to the file and waits for the OS to report them written. On a ReadWrite mapping it always flushes, whatever `pending_bytes()` says (before 1.1 it skipped the flush when the counter was zero, which under the default policy was always). Per platform: `msync(MS_SYNC)` on Unix (on macOS this does not issue `F_FULLFSYNC`, so the drive cache may still hold data); `FlushViewOfFile` + `FlushFileBuffers` on Windows. Resets `pending_bytes()` to 0. No-op for ReadOnly and CopyOnWrite.

**Returns**: `Result<()>`

**Errors**:
- `MmapIoError::FlushFailed` if flush operation fails

<br>

### flush_range

```rust
pub fn flush_range(&self, offset: u64, len: u64) -> Result<()>
```

**Description**: Synchronously flushes the pages covering `[offset, offset + len)`, with the same per-platform calls as `flush()` (on Windows `FlushFileBuffers` covers the whole file). A range covering the whole mapping resets `pending_bytes()`; a partial range leaves it unchanged, since the crate does not track which bytes are dirty. A zero-length range is accepted at any offset and does nothing. Visibility is not the question here: other mappings of the file see writes immediately through the page cache; flushing makes them durable.

**Parameters**:
- `offset`: Starting byte offset
- `len`: Number of bytes to flush

**Returns**: `Result<()>`

**Errors**:
- `MmapIoError::OutOfBounds` if range exceeds file bounds
- `MmapIoError::FlushFailed` if flush operation fails

<br>

### schedule_flush / schedule_flush_range

*(Since 1.1.0)*

```rust
pub fn schedule_flush(&self) -> Result<()>
pub fn schedule_flush_range(&self, offset: u64, len: u64) -> Result<()>
```

**Description**: Start writing dirty pages back to the file **without waiting** for the write to finish. **Not durable**: when these return, the data may still be only in memory, and a crash or power loss can lose it. Only `flush()` / `flush_range()` make data durable. Use them to get write-back going early (for example after each batch, with a durable `flush()` at a commit point), or to keep dirty memory from piling up without paying for a synchronous flush. `pending_bytes()` is not changed.

| Platform | Call |
|----------|------|
| Linux | `sync_file_range(SYNC_FILE_RANGE_WRITE)` on the backing file: queues the dirty pages for write-out at once; no wait, no metadata, no device cache flush. (Linux treats `msync(MS_ASYNC)` as a no-op, so it is not used.) |
| macOS, other Unix | `msync(MS_ASYNC)`: schedules write-back and returns. |
| Windows | `FlushViewOfFile` without `FlushFileBuffers`: hands the pages to the file system cache and returns without waiting for the disk. |

The range is validated like `flush_range` (under the read guard; zero-length accepted at any offset; widened to whole pages by the kernel). On `ReadOnly` and `CopyOnWrite` mappings there is nothing to write back: the range is validated and the call returns `Ok`.

**Errors**:
- `MmapIoError::OutOfBounds` if the range exceeds the mapping length
- `MmapIoError::FlushFailed` if the OS rejects the request

**Example**:
```rust
let mmap = MemoryMappedFile::create_rw("journal.bin", 1 << 20)?;
for (i, record) in records.iter().enumerate() {
    let off = (i * 64) as u64;
    mmap.update_region(off, record)?;
    mmap.schedule_flush_range(off, 64)?; // start write-back, keep going
}
mmap.flush()?; // commit point: durable
```

Measured cost: see `docs/PERFORMANCE.md` ("Starting write-back without waiting").

<br>

### resize

```rust
pub fn resize(&self, new_size: u64) -> Result<()>
```

**Description**: Resizes the mapped file and remaps it. Only available in ReadWrite mode. Takes the write lock before touching the file, so it waits for every live slice, iterator item, and atomic view to drop (calling it while this thread holds one deadlocks). Shrinking truncates the file on every platform, including Windows since 1.1 (it used to shrink only the cached length); bytes cut off read back as zeros after a later grow. On Windows a shrink fails with `Io` if another independent mapping of the same file is open.

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

The file is never truncated. A non-empty existing file is mapped at its current length; an existing zero-length file is extended to `default_size`. Creation is exclusive (`create_new`), so if another process creates the file at the same moment, this call opens that file instead of overwriting it. (Before 1.1 an `exists()` check followed by a truncating create could wipe a file created in between.)

**Parameters**:
- `path`: Path to open or create
- `default_size`: Size used when the file is created or is empty; ignored for a non-empty existing file

**Returns**: `Result<MemoryMappedFile>` in ReadWrite mode

**Errors**:
- `MmapIoError::ResizeFailed` if the file must be created or extended and `default_size` is zero (no file is left behind)
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

**Description**: Bytes written since the last successful full flush, under every flush policy. Counts `update_region` (at the write), `MappedSliceMut` (its length, when dropped), `chunks_mut` (bytes handed to the closure), atomic views (their size, when dropped), and `as_mut_ptr` (whole mapping). Reset by `flush()` and by a `flush_range` covering the whole mapping. It drives `EveryBytes` and `EveryMillis`; explicit `flush()` ignores it. One atomic read, no I/O. Since 0.9.8.

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

**Description**: Prewarms (touches) all pages by reading the first byte of each page, forcing the OS to load all pages into physical memory. This eliminates page faults during subsequent access, which is useful for benchmarking and performance-critical sections.

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

<br>

### as_slice_bytes

*(Since 0.9.11, compat shim for the 0.9.6 signature)*

```rust
pub fn as_slice_bytes(&self, offset: u64, len: u64) -> Result<&[u8]>
```

**Description**: Returns a direct `&[u8]` borrow into the mapping. Mirrors the 0.9.6 `as_slice` signature for codebases that were broken by the 0.9.7 return-type change. Supported on `ReadOnly` mappings; returns `MmapIoError::InvalidMode` on `ReadWrite` and (since 1.1.0, when copy-on-write became writable) `CopyOnWrite` (use [`as_slice`](#as_slice), which returns `MappedSlice<'_>`, for those).

**Errors**:
- `MmapIoError::InvalidMode` on `ReadWrite` mappings.
- `MmapIoError::OutOfBounds` if range exceeds file bounds.

<br>

### read_bytes

*(Since 0.9.11; requires `feature = "bytes"`)*

```rust
pub fn read_bytes(&self, offset: u64, len: u64) -> Result<bytes::Bytes>
```

**Description**: Read `len` bytes from `offset` into a newly-allocated `bytes::Bytes`. One allocation + memcpy at the boundary; the resulting `Bytes` is mapping-lifetime-independent and can be sent through `hyper` / `tower` / `tonic` / `axum` / `reqwest`.

For true zero-copy networking, prefer `as_slice` and pass the borrowed `&[u8]` directly; `Bytes` is the right tool when ownership has to cross a thread or process boundary.

**Errors**:
- `MmapIoError::OutOfBounds` if range exceeds file bounds.

<br>

### reader

*(Since 0.9.11)*

```rust
pub fn reader(&self) -> MmapReader<'_>
```

**Description**: Returns an [`MmapReader`](#mmapreader) cursor over the mapping. Implements `std::io::Read` + `std::io::Seek`, so the mapping plugs directly into any parser or decoder expecting a generic `R: Read`.

**Example**:
```rust
use mmap_io::MemoryMappedFile;
use std::io::Read;

let mmap = MemoryMappedFile::open_ro("data.bin")?;
let mut reader = mmap.reader();
let mut buf = Vec::new();
reader.read_to_end(&mut buf)?;
# Ok::<(), Box<dyn std::error::Error>>(())
```

<br>

### is_hugepage_backed

*(Since 1.0.0)*

```rust
pub fn is_hugepage_backed(&self) -> Option<bool>
```

**Description**: Report whether the kernel currently backs this mapping with huge pages (transparent or explicit HugeTLB).

**Returns**:
- `Some(true)` if any portion of the mapping is backed by huge pages.
- `Some(false)` if the mapping is backed by regular pages only.
- `None` on non-Linux platforms, or when the status cannot be determined (e.g. `/proc/self/smaps` unreadable, no matching entry).

On Linux, parses `/proc/self/smaps` and inspects the `AnonHugePages`, `Private_Hugetlb`, and `Shared_Hugetlb` fields of the entry containing the mapping's base address.

**Notes**: Treat `None` as "unknown", not as "definitely regular pages". The result reflects state at the moment of the call; the kernel may promote or demote pages over time (Transparent Huge Pages).

<br>

## OS Handle Traits

*(Since 0.9.11)*

`MemoryMappedFile` implements the standard-library OS handle traits for file-backed mappings, letting you hand the underlying handle to FFI / `nix` / `rustix` / `polling` etc. without going through `unmap`.

```rust
// Unix (Linux, macOS, BSD):
impl AsFd for MemoryMappedFile { /* ... */ }
impl AsRawFd for MemoryMappedFile { /* ... */ }

// Windows:
impl AsHandle for MemoryMappedFile { /* ... */ }
impl AsRawHandle for MemoryMappedFile { /* ... */ }
```

The trait impls borrow the file handle for as long as the mapping is alive; the handle remains owned by the `MemoryMappedFile`. Use `unmap()` to retake ownership of the `File` explicitly.

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
- The policy only controls automatic flushes. An explicit `flush()` / `flush_range()` always flushes.
- Never/Manual: no automatic flushes; call `flush()` when you need durability. `pending_bytes()` still counts.
- Always: `flush()` runs after each `update_region()` call.
- EveryBytes(n): after an `update_region()` call, flushes if `pending_bytes()` (which also counts the other write paths) is at least n.
- EveryWrites(w): flushes after every w-th `update_region()` call.
- EveryMillis(ms): a background thread wakes every `ms` milliseconds and flushes if `pending_bytes()` is non-zero. It is started by every builder path (`create`, `open`, `open_or_create`) and is stopped and joined when the last handle to the mapping drops.

Notes:
- `flush()` is synchronous: `msync(MS_SYNC)` on Unix, `FlushViewOfFile` + `FlushFileBuffers` on Windows. macOS `msync` does not issue `F_FULLFSYNC`.
- ReadOnly and COW mappings treat flush() as a no-op (COW writes stay in private pages by design).

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

**Description**: Provides hints to the OS about expected access patterns for better performance. The start is widened down to a page boundary. Unix calls `madvise`; Windows calls `PrefetchVirtualMemory` for `WillNeed` and ignores the other hints. The call holds a read guard, except `DontNeed` on a `CopyOnWrite` mapping: there `MADV_DONTNEED` throws away the private copies (on Linux the range reads the file again and private changes are lost), which changes the mapped bytes, so it takes the write lock like a write method (it waits for live views; on the thread holding a view it deadlocks).

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

**Description**: Zero-copy iterator over fixed-size chunks. On RW mappings the iterator holds a read guard for its lifetime and every yielded item holds its own, so `resize()` and the write methods block until the iterator and all items it produced are dropped (before 1.1, an item kept after the iterator could outlive its guard).

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
> `resize()` (and every write method) blocks while the view is alive.
>
> Since 1.1, atomic views require a writable mapping: **ReadWrite**
> or **CopyOnWrite** (whose stores stay in private pages). On
> ReadOnly mappings the pages are not writable, so a safe `store`
> would fault; these methods return `MmapIoError::InvalidMode` there. Checks run in this order: mode,
> alignment, bounds. Dropping a view adds its size to
> `pending_bytes()`. Do not read the same bytes through a
> `MappedSlice` while another thread stores to them atomically; that
> would be a data race, so since 1.1 the mapping refuses it at run
> time (see "Atomic and plain views" below).

#### Atomic and plain views

Since 1.1.0 each writable mapping tracks the byte ranges of its live views. An atomic view and a plain byte view (`MappedSlice` from `as_slice` / `try_as_slice` / `Segment::as_slice`, or an iterator item) of the same bytes cannot be alive at the same time, because an atomic store racing with a plain read is undefined behavior:

| Live view | New request over overlapping bytes | Result |
|-----------|------------------------------------|--------|
| atomic view | `as_slice`, `try_as_slice`, `Segment::as_slice` | `InvalidMode` |
| atomic view | `chunks()` / `pages()` item | owned copy (atomic loads), not a borrow |
| atomic view | `read_into`, `read_bytes`, `MmapReader`, `touch_pages` | allowed; atomic bytes read with atomic loads |
| `MappedSlice` / iterator item | any atomic view | `InvalidMode` |
| `AtomicU64` view | `AtomicU32` view (or the reverse) | `InvalidMode` (mixed-size access) |
| `AtomicU64` view | `AtomicU64` view | allowed |

Disjoint ranges never conflict: counters in a header next to plain data work as before. The checks cover one mapping and its clones; an independent `MemoryMappedFile` of the same file, another process, or raw pointers are not tracked. Without the `atomic` feature nothing is tracked and plain views cost what they did in 1.0. With it, each RW / COW plain view adds one small lock round trip (numbers in `docs/PERFORMANCE.md`).

```rust
use std::sync::atomic::Ordering;
use mmap_io::MemoryMappedFile;

let mmap = MemoryMappedFile::create_rw("state.bin", 4096)?;
let counter = mmap.atomic_u64(0)?;          // header: bytes 0..8
counter.fetch_add(1, Ordering::SeqCst);
let body = mmap.as_slice(8, 64)?;           // disjoint: fine
assert!(mmap.as_slice(0, 16).is_err());     // overlaps the counter
let mut header = [0u8; 16];
mmap.read_into(0, &mut header)?;            // copies, counter read atomically
# drop(body);
# Ok::<(), mmap_io::MmapIoError>(())
```

<br>

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
- `MmapIoError::InvalidMode` if the mapping is not ReadWrite
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
- `MmapIoError::InvalidMode` if the mapping is not ReadWrite
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
- `as_slice(&self) -> Result<MappedSlice<'_>>`
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

**Description**: Mutable view into a region of a memory-mapped file. `write(data)` writes at the start of the segment; since 1.1 it returns `OutOfBounds` (segment-relative fields: `offset` 0, `len` = `data.len()`, `total` = segment length) when `data` is longer than the segment, instead of writing past the segment's end.

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

**Description**: Async write that also flushes after the write completes. The write and the flush run on the `blocking` crate's thread pool (any executor works); the flush is unconditional regardless of the configured `FlushPolicy`. `data` is copied into a `Vec` first (one allocation of `data.len()` bytes), because the blocking task must own its input.

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

**Description**: Async equivalent of `flush()`. Runs the underlying flush on the `blocking` crate's thread pool so the async scheduler is not blocked on disk I/O.

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

**Description**: Asynchronously creates a new memory-mapped file. Same behavior as `create_rw` (truncates an existing file), run on the `blocking` thread pool. Since 1.1 the size is validated before the file is touched; it used to truncate first.

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

## Raw Mapping Tier (`mmap_io::raw`)

`mmap_io::raw` is the platform layer underneath `MemoryMappedFile`:
`mmap` / `msync` / `munmap` on Unix and `CreateFileMappingW` /
`MapViewOfFile` / `FlushViewOfFile` / `UnmapViewOfFile` on Windows,
with checked offset and length handling. It returns
`std::io::Result`, has no locks, no flush policy and no path
bookkeeping, and depends on nothing but `libc` (Unix). Its shape
follows `memmap2` (`RawMmap` ~ `Mmap`, `RawMmapMut` ~ `MmapMut`,
`RawMmapOptions` ~ `MmapOptions`).

### When to use raw

| Need | Use |
|------|-----|
| Safe API, bounds-checked regions, concurrent readers and writers, flush policies, resize, atomics, watch | `MemoryMappedFile` |
| A bare mapping owned by your own type, already synchronised by your code | `raw::RawMmap` / `raw::RawMmapMut` |
| A mapping without the `MmapIoError` type (for example inside another crate's I/O layer) | `raw` |
| Anonymous scratch memory without locking | `raw::RawMmapMut::map_anon` (or `AnonymousMmap` for the locked, bounds-checked wrapper) |

The file-backed raw constructors are `unsafe fn`: the caller promises
that nothing modifies or truncates the mapped range for the lifetime
of the mapping (see `docs/SAFETY.md`, section 8). `MemoryMappedFile`
makes the same assumption internally (REPS section 5.1) but keeps the
public API safe.

### RawMmapOptions

```rust
#[derive(Debug, Clone, Default)]
pub struct RawMmapOptions { /* offset, len, populate, huge */ }

impl RawMmapOptions {
    pub const fn new() -> Self;
    pub fn offset(&mut self, offset: u64) -> &mut Self;
    pub fn len(&mut self, len: usize) -> &mut Self;
    pub fn populate(&mut self) -> &mut Self; // 1.1.0
    pub fn huge(&mut self) -> &mut Self;     // 1.1.0
    pub unsafe fn map(&self, file: &File) -> io::Result<RawMmap>;
    pub unsafe fn map_mut(&self, file: &File) -> io::Result<RawMmapMut>;
    pub unsafe fn map_copy(&self, file: &File) -> io::Result<RawMmapMut>;
    pub fn map_anon(&self) -> io::Result<RawMmapMut>;
}
```

**Description**: Builds a window `[offset, offset + len)` of a file.
Without `len` the window runs to the end of the file. Any offset is
accepted; the OS mapping starts at the offset rounded down to the OS
granularity and the leading bytes are hidden. `map` is read-only and
shared, `map_mut` is writable and shared with the file, `map_copy` is
private copy-on-write (writes never reach the file), `map_anon` is
zero-filled anonymous memory (offset ignored).

Since 1.1.0:

- `populate()` pre-faults the mapping at creation (`MAP_POPULATE` on
  Linux and Android, for file and anonymous maps). First accesses then
  do not page-fault; creation is slower and commits memory up front.
  Accepted and ignored on other platforms.
- `huge()` asks `map_anon` for explicit huge pages (`MAP_HUGETLB` on
  Linux and Android, default size from `/proc/meminfo`). The OS
  mapping is rounded up to whole huge pages; `len()` still reports the
  requested length. Without reserved huge pages (`vm.nr_hugepages`,
  0 on most systems) `map_anon` fails with the OS error, usually
  `ENOMEM`; the raw tier does not fall back. `AnonymousMmap::with_huge_pages`
  does. Ignored for file maps and on other platforms; Windows large
  pages need `SeLockMemoryPrivilege` and are not used.

**Errors**: `InvalidInput` when the offset is past the end of the
file, when `offset + len` overflows or exceeds the file size, or when
the window does not fit in the address space (`isize::MAX`, relevant
on 32-bit targets). OS errors (permissions, out of memory) pass
through. `Unsupported` on targets without a backend.

**Example**:
```rust
use mmap_io::raw::RawMmapOptions;

let file = std::fs::File::open("data.bin")?;
// SAFETY: data.bin is not modified while the windows are mapped.
let header = unsafe { RawMmapOptions::new().len(64).map(&file)? };
let body = unsafe { RawMmapOptions::new().offset(64).map(&file)? };
# Ok::<(), std::io::Error>(())
```

<br>

### RawMmap

```rust
pub struct RawMmap { /* private */ }

impl RawMmap {
    pub unsafe fn map(file: &File) -> io::Result<Self>;
    pub fn len(&self) -> usize;
    pub fn is_empty(&self) -> bool;
    pub fn as_ptr(&self) -> *const u8;
}
impl Deref<Target = [u8]> for RawMmap;
impl AsRef<[u8]> for RawMmap;
impl Debug for RawMmap; // ptr and len only, never the contents
// Send + Sync
```

**Description**: Read-only, shared mapping. Writes made to the file
through other mappings become visible. The mapping stays valid after
the `File` is dropped.

<br>

### RawMmapMut

```rust
pub struct RawMmapMut { /* private */ }

impl RawMmapMut {
    pub unsafe fn map_mut(file: &File) -> io::Result<Self>;
    pub fn map_anon(len: usize) -> io::Result<Self>;
    pub fn len(&self) -> usize;
    pub fn is_empty(&self) -> bool;
    pub fn as_ptr(&self) -> *const u8;
    pub fn as_mut_ptr(&mut self) -> *mut u8;
    pub fn flush(&self) -> io::Result<()>;
    pub fn flush_async(&self) -> io::Result<()>;
    pub fn flush_range(&self, offset: usize, len: usize) -> io::Result<()>;
    pub fn flush_async_range(&self, offset: usize, len: usize) -> io::Result<()>;
}
impl Deref<Target = [u8]> + DerefMut for RawMmapMut;
impl AsRef<[u8]> + AsMut<[u8]> for RawMmapMut;
impl Debug for RawMmapMut; // ptr and len only
// Send + Sync (mutation needs &mut self)
```

**Description**: Writable mapping: shared with the file
(`map_mut`), private copy-on-write (`RawMmapOptions::map_copy`) or
anonymous (`map_anon`). `flush` is durable; `flush_async` only starts
write-back. `flush_range` validates `offset <= len` and
`len <= self.len() - offset` before any pointer arithmetic and widens
the range to page boundaries. On copy-on-write and anonymous mappings
every flush is a validated no-op that returns `Ok`.

**Example**:
```rust
use mmap_io::raw::RawMmapMut;

let file = std::fs::OpenOptions::new().read(true).write(true).open("data.bin")?;
// SAFETY: no other writer touches data.bin while it is mapped.
let mut map = unsafe { RawMmapMut::map_mut(&file)? };
map[..4].copy_from_slice(b"MMIO");
map.flush_range(0, 4)?;
# Ok::<(), std::io::Error>(())
```

<br>

### Protection changes, advice and locking

Since 1.1.0. Additive, following `memmap2`'s method names.

```rust
impl RawMmap {
    pub fn make_mut(self) -> io::Result<RawMmapMut>;
    #[cfg(feature = "advise")]
    pub fn advise(&self, advice: MmapAdvice) -> io::Result<()>;
    #[cfg(feature = "advise")]
    pub fn advise_range(&self, advice: MmapAdvice, offset: usize, len: usize) -> io::Result<()>;
    #[cfg(feature = "locking")]
    pub fn lock(&self) -> io::Result<()>;
    #[cfg(feature = "locking")]
    pub fn unlock(&self) -> io::Result<()>;
}

impl RawMmapMut {
    pub fn make_read_only(self) -> io::Result<RawMmap>;
    #[cfg(feature = "advise")]
    pub fn advise(&self, advice: MmapAdvice) -> io::Result<()>;
    #[cfg(feature = "advise")]
    pub fn advise_range(&self, advice: MmapAdvice, offset: usize, len: usize) -> io::Result<()>;
    #[cfg(feature = "locking")]
    pub fn lock(&self) -> io::Result<()>;
    #[cfg(feature = "locking")]
    pub fn unlock(&self) -> io::Result<()>;
}
```

**`make_read_only` / `make_mut`** change the protection of the whole
mapping (`mprotect` on Unix, `VirtualProtect` on Windows) and consume
the value, so no borrow of the bytes can be alive across the change.
A mapping that went through `make_read_only` gets its original access
back from `make_mut`: shared writes reach the file, copy-on-write and
anonymous mappings stay private. A mapping created read-only with
`RawMmap::map`:

| Platform | `make_mut` |
|----------|------------|
| Unix | `mprotect(PROT_READ \| PROT_WRITE)`; needs a file opened for writing (`EACCES` otherwise). `flush` then writes back with `msync`. |
| Windows | Always `Unsupported`: the view belongs to a `PAGE_READONLY` section, which can never become writable. Map with `map_mut` and call `make_read_only` when a mapping must switch. |

`make_read_only` does not flush; call `flush` first when the data must
be durable. On error the mapping is released. Empty mappings convert
without a syscall.

**`advise` / `advise_range`** reuse `MmapAdvice` (feature `advise`).
The range is validated (`offset <= len`, `len <= self.len() - offset`)
before any syscall and its start is widened down to a page boundary.
Unix calls `madvise`; Windows calls `PrefetchVirtualMemory` for
`WillNeed` and ignores the other hints. `DontNeed` is refused with
`InvalidInput` on private mappings (copy-on-write and anonymous, also
after `make_read_only`): there it discards the private pages, which
would change bytes that `&self` borrows can be reading. On shared file
mappings it is allowed; the kernel drops page table entries and the
bytes read back unchanged from the page cache.

**`lock` / `unlock`** pin or unpin the whole window (`mlock` /
`munlock`, `VirtualLock` / `VirtualUnlock`; feature `locking`).
Locking usually needs privileges or a raised `RLIMIT_MEMLOCK`.
Unlocking pages that are not locked succeeds on every platform.

**Example**:
```rust
use mmap_io::{raw::RawMmapMut, MmapAdvice};

let mut scratch = RawMmapMut::map_anon(1 << 20)?;
scratch.advise(MmapAdvice::Sequential)?;
scratch[..5].copy_from_slice(b"ready");
let frozen = scratch.make_read_only()?;   // writes now fault
assert_eq!(&frozen[..5], b"ready");
let mut scratch = frozen.make_mut()?;     // writable again, still private
scratch[0] = b'R';
# Ok::<(), std::io::Error>(())
```

<br>

### offset_granularity

```rust
pub fn offset_granularity() -> io::Result<usize>
```

**Description**: The granularity at which the OS places a mapping's
file offset: the page size on Unix, the allocation granularity on
Windows (typically 64 KiB). The raw constructors align offsets
internally; use this value to choose offsets that waste no address
space.

<br>

### Behavior and platform notes

| Topic | Unix | Windows |
|-------|------|---------|
| Read-only map | `PROT_READ`, `MAP_SHARED` | `PAGE_READONLY`, `FILE_MAP_READ` |
| Read-write map | `PROT_READ \| PROT_WRITE`, `MAP_SHARED` | `PAGE_READWRITE`, `FILE_MAP_READ \| FILE_MAP_WRITE` |
| Copy-on-write | `MAP_PRIVATE` | `PAGE_WRITECOPY`, `FILE_MAP_COPY` |
| Anonymous | `MAP_PRIVATE \| MAP_ANON` | paging-file section |
| Offset alignment | page size | allocation granularity |
| Offsets above 2 GiB on 32-bit | `mmap64` (glibc, Android) or 64-bit `off_t` | high / low DWORD split |
| `flush` | `msync(MS_SYNC)` | `FlushViewOfFile` + `FlushFileBuffers` |
| `flush_async` | `msync(MS_ASYNC)` | `FlushViewOfFile` |
| `make_read_only` / `make_mut` | `mprotect` | `VirtualProtect` (read-only sections stay read-only) |
| `advise` | `madvise` | `PrefetchVirtualMemory` (`WillNeed` only) |
| `lock` / `unlock` | `mlock` / `munlock` | `VirtualLock` / `VirtualUnlock` |
| `populate()` | `MAP_POPULATE` (Linux, Android) | ignored |
| `huge()` (anonymous) | `MAP_HUGETLB` (Linux, Android) | ignored |
| Zero-length window | no syscall, empty slice | no syscall, empty slice |
| Handles held per mapping | none | none (read-only, COW, anonymous); one duplicated file handle (read-write) |

- **Past end of file.** A window that extends past the end of the file
  is an error, even with an explicit `len`. `memmap2` accepts an
  explicit length past the end on Unix, which produces a mapping that
  raises `SIGBUS` on access.
- **Durability.** `FlushViewOfFile` alone does not wait for the disk;
  `flush` therefore also calls `FlushFileBuffers`. On macOS, `msync`
  and `fsync` do not force the drive's write cache; use
  `F_FULLFSYNC` on the file when that matters.
- **Empty windows.** An empty file, `len(0)`, or an offset equal to the
  file length produce an empty mapping without any syscall. Flushing
  it with `(0, 0)` succeeds; any other range is an error.
- **Platforms.** Linux, Android, macOS, iOS, FreeBSD and the other BSDs
  use the Unix backend; Windows uses the Windows backend; any other
  target returns `Unsupported` from every constructor.

### Performance

Access through `Deref` is a pointer and a length: no allocation, no
lock, no syscall. Creating and releasing a mapping costs the same or
less than `memmap2` (criterion medians, `benches/raw_vs_memmap2.rs`):

| Operation | Windows 11 `memmap2` | Windows 11 `raw` | Linux (WSL2) `memmap2` | Linux (WSL2) `raw` |
|-----------|---------------------:|-----------------:|-----------------------:|-------------------:|
| map + drop, 1 MiB read-only | 32.1 us | 16.4 us | 1.65 us | 1.63 us |
| map + drop, 1 MiB read-write | 18.0 us | 17.0 us | 1.66 us | 1.59 us |
| map + drop, 256 KiB window at offset 4097 | 21.9 us | 15.8 us | 3.80 us | 2.04 us |
| map + drop, 1 MiB anonymous | 19.3 us | 12.1 us | 1.40 us | 1.24 us |
| drop only, 1 MiB read-write | 2.68 us | 1.55 us | 1.28 us | 1.11 us |
| dirty 1 page + durable flush, 4 KiB | 478 us | 481 us | 672 us | 661 us |
| dirty 256 pages + durable flush, 1 MiB | 1.10 ms | 1.12 ms | 1.52 ms | 1.39 ms |

On Windows the read-only and anonymous paths are cheaper because
`raw` creates the section with the final protection directly
(`memmap2` probes write and execute access with two extra
`CreateFileMappingW` calls and a `VirtualProtect`), does not duplicate
the file handle for mappings that never flush, and caches the system
granularity instead of calling `GetSystemInfo` on every drop. Flush
cost is dominated by the disk on both platforms and is unchanged.

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

**Description**: Aligns a value up to the nearest multiple of alignment. Returns `value` unchanged when `alignment` is 0, and saturates to `u64::MAX` when the result would overflow (it used to panic in debug builds).

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
- Another process must not truncate or modify the file while it is mapped here; readers can see torn data or receive `SIGBUS`.
- `as_slice_mut()` is only allowed in `ReadWrite` mode.
- Raw pointers from `as_ptr()` / `as_mut_ptr()` are invalidated by `resize()`.
- A `MappedSlice` and an atomic view of the same bytes cannot be alive together: since 1.1.0 the second one is refused with `InvalidMode` (see [Atomic and plain views](#atomic-and-plain-views)). Copying reads (`read_into`, `read_bytes`, `MmapReader`) are never refused and read atomic bytes with atomic loads.

See [SAFETY.md](SAFETY.md) for the full locking model.

<br>

### Range Validation
- A zero-length request is accepted at any offset, including past the end, and does nothing (slice methods return an empty slice).
- Any other range must satisfy `offset + len <= len()` (checked without overflow) or the call returns `OutOfBounds`. Nothing is clamped.
- On ReadWrite mappings the check runs under the mapping lock, against the mapping actually accessed, so a concurrent `resize()` yields `OutOfBounds`, never an out-of-range access.
- Atomic views are not range requests: they always require an aligned `offset <= len()`.

<br>

### Copy-On-Write (COW) Mode
- Writable since 1.1.0: every write method works, atomic views included, on private pages.
- The file is never modified through a COW mapping; changes are lost when the mapping is dropped.
- `flush()` / `flush_range()` are no-ops, `pending_bytes()` stays 0, `resize()` returns `InvalidMode`.
- Locking matches `ReadWrite`: live views block writers.

<br>

### Flushing Behavior
- `schedule_flush()` / `schedule_flush_range()` (1.1) start write-back and return without waiting; they are not durable.
- `flush()` / `flush_range()` are synchronous: `msync(MS_SYNC)` on Unix, `FlushViewOfFile` + `FlushFileBuffers` on Windows. On macOS, `msync` does not issue `F_FULLFSYNC`; call `File::sync_all` on a separate handle if you need the drive cache flushed too.
- Visibility is not durability: other mappings and `std::fs` readers of the same file see writes at once through the page cache. Flushing is what makes them survive a crash.
- Async helpers flush after each async write.

<br>

### Thread Safety
`MemoryMappedFile` is `Send + Sync` and can be shared with `Arc`.
- Read views (`as_slice`, iterator items, atomic views) share the lock; any number can coexist, including several on one thread.
- Writes (`update_region`, `as_slice_mut`, `chunks_mut`, `resize`) take the lock exclusively and wait for every live read view, whatever region it covers.
- Calling a write method on a thread that still holds a read view of the same mapping deadlocks. Drop the view first.

<br>

### Performance Tips
1. Use `advise()` to hint access patterns for better OS optimization
2. Prefer page-aligned operations when possible
3. Use iterators for sequential processing of large files
4. Lock critical memory regions to prevent swapping
5. Batch writes and flush once rather than flushing frequently: each flush is a synchronous write-back

<br>

### Common Pitfalls
1. Don't call `flush()` while holding a `MappedSliceMut`, or a write method while holding a `MappedSlice` / iterator item / atomic view, on the same thread (deadlock)
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
- **1.0.0**: Stable release. New surface: `AnonymousMmap` (process-local file-less mappings), `is_hugepage_backed()` runtime introspection, multi-process IPC integration test, sparse-file documentation. Doc-completeness pass: every `Result`-returning public method documents `# Errors`; every panicking method documents `# Panics`. `cargo public-api` snapshot committed and enforced via CI; `cargo-semver-checks` becomes a hard gate against unintended breaks. No API breaks vs 0.9.11.
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