# mmap-io — Project Specification (REPS)

> Rust Engineering Project Specification.
> Normative language follows RFC 2119.

## 1. Purpose

`mmap-io` is a safe, high-performance memory-mapped file I/O library
for Rust. It MUST provide zero-copy reads and efficient writes over
memory-mapped files with cross-platform support (Linux, macOS,
Windows), thread-safe interior mutability, and a clean opt-in feature
matrix.

The crate is the foundation layer for use cases where the typical
`std::fs::File` + buffer abstraction is too slow or imprecise:
database engines, game runtimes, caches, log-structured storage, and
real-time data pipelines.

## 2. Design principles

The crate's design adheres to these principles, listed in priority
order. When two principles conflict, the higher-priority one wins.

1. **Safety.** Public API MUST be safe. Every `unsafe` block in the
   implementation MUST have a `// SAFETY:` comment justifying its
   soundness.
2. **Performance.** Hot paths MUST avoid unnecessary allocations,
   syscalls, and locks. Where a tradeoff exists between safety and
   performance, safety wins, but the implementation MUST measure and
   document the cost.
3. **Cross-platform parity.** Every public feature MUST work on
   Linux, macOS, and Windows, or MUST be clearly documented as
   platform-specific. Platform fallbacks (e.g., polling-based watch
   when native event sources are unavailable) MUST be documented.
4. **Opt-in surface.** Optional features (async, advise, iterator,
   etc.) MUST be gated behind Cargo features. The default feature
   set MUST be minimal and safe for any caller.
5. **Zero ratchet.** The crate MUST NOT pull in dependencies that
   force MSRV bumps without strong cause. New dependencies require
   review against the MSRV policy in section 6.

## 3. Module structure

```
mmap_io
├── errors      — error types for all operations
├── utils       — alignment and bounds helpers
├── mmap        — core MemoryMappedFile, MmapMode, TouchHint
├── segment     — segmented (offset + length) views
├── manager     — high-level create_mmap / load_mmap / update_region /
│                 flush / copy_mmap / delete_mmap / write_mmap
├── flush       — FlushPolicy, time-based flushing
├── advise      — madvise hints                     [feature: advise]
├── iterator    — ChunkIterator, PageIterator       [feature: iterator]
├── lock        — page-level mlock / VirtualLock    [feature: locking]
├── atomic      — atomic u32/u64 memory views       [feature: atomic]
└── watch       — file change notifications         [feature: watch]
```

## 4. Public API surface

The following items are part of the public, stable-through-0.9.x
surface. Breaking changes to these items MUST be batched into a
0.10.0 or 1.0.0 release with a CHANGELOG entry under `### Changed`
and a migration note.

### 4.1 Always available

```rust
// errors
pub enum MmapIoError { /* variants */ }

// mmap (core)
pub struct MemoryMappedFile { /* private */ }
pub enum MmapMode { ReadOnly, ReadWrite, CopyOnWrite }
pub enum TouchHint { Never, Eager, Lazy }

// Read-side wrapper. Holds the RW read guard for its lifetime (None
// for RO/COW). Derefs to `[u8]`. Implements Debug + PartialEq with
// byte slices for ergonomic test/assert use. Since 0.9.7.
pub struct MappedSlice<'a> { /* private */ }

// Write-side wrapper. Holds the RW write lock for its lifetime. Use
// `.as_mut()` or DerefMut to access `&mut [u8]`.
pub struct MappedSliceMut<'a> { /* private */ }

impl MemoryMappedFile {
    pub fn create_rw<P: AsRef<Path>>(path: P, size: u64) -> Result<Self>;
    pub fn open_ro<P: AsRef<Path>>(path: P) -> Result<Self>;
    pub fn open_rw<P: AsRef<Path>>(path: P) -> Result<Self>;
    // Since 0.9.8: ergonomic constructors and teardown.
    pub fn open_or_create<P: AsRef<Path>>(path: P, default_size: u64) -> Result<Self>;
    pub fn from_file<P: AsRef<Path>>(file: File, mode: MmapMode, path: P) -> Result<Self>;
    pub fn unmap(self) -> std::result::Result<File, Self>;
    // Since 0.9.7: as_slice works uniformly on RO, COW, AND RW.
    pub fn as_slice(&self, offset: u64, len: u64) -> Result<MappedSlice<'_>>;
    pub fn as_slice_mut(&self, offset: u64, len: u64) -> Result<MappedSliceMut<'_>>;
    pub fn read_into(&self, offset: u64, dst: &mut [u8]) -> Result<()>;
    pub fn update_region(&self, offset: u64, data: &[u8]) -> Result<()>;
    pub fn flush(&self) -> Result<()>;
    pub fn flush_range(&self, offset: u64, len: u64) -> Result<()>;
    pub fn resize(&self, new_size: u64) -> Result<()>;
    pub fn touch_pages(&self) -> Result<()>;
    pub fn touch_pages_range(&self, offset: u64, len: u64) -> Result<()>;
    pub fn len(&self) -> u64;
    pub fn is_empty(&self) -> bool;
    pub fn path(&self) -> &Path;
    pub fn mode(&self) -> MmapMode;
    // Since 0.9.8: introspection accessors.
    pub fn flush_policy(&self) -> FlushPolicy;
    pub fn pending_bytes(&self) -> u64;
    // Since 0.9.8: FFI escape hatches.
    pub unsafe fn as_ptr(&self) -> *const u8;
    pub unsafe fn as_mut_ptr(&self) -> Result<*mut u8>;
    // Since 0.9.8: kernel-side prefetch hint (posix_fadvise on
    // Linux; no-op elsewhere). Complementary to MmapAdvice::WillNeed
    // (which is a VM-side hint via madvise).
    pub fn prefetch_range(&self, offset: u64, len: u64) -> Result<()>;
}

// Builder additions since 0.9.8:
impl MemoryMappedFileBuilder {
    pub fn open_or_create(self) -> Result<MemoryMappedFile>;
}

// manager (high-level)
pub fn create_mmap<P: AsRef<Path>>(path: P, size: u64) -> Result<MemoryMappedFile>;
pub fn load_mmap<P: AsRef<Path>>(path: P) -> Result<MemoryMappedFile>;
pub fn write_mmap<P: AsRef<Path>>(path: P, data: &[u8]) -> Result<()>;
pub fn update_region(mmap: &MemoryMappedFile, offset: u64, data: &[u8]) -> Result<()>;
pub fn flush(mmap: &MemoryMappedFile) -> Result<()>;
pub fn copy_mmap<P, Q>(src: P, dst: Q) -> Result<()>;
pub fn delete_mmap<P: AsRef<Path>>(path: P) -> Result<()>;

// flush
pub enum FlushPolicy {
    Never,             // (default) no implicit flush; user calls flush()
    Manual,            // alias for Never
    Always,            // flush after every update_region
    EveryBytes(usize), // flush after >= N dirty bytes
    EveryWrites(usize),// flush after every W update_region calls
    EveryMillis(u64),  // background-thread time-based flush
}
```

### 4.2 Feature-gated

```rust
// advise (feature = "advise", default-on)
pub enum MmapAdvice { Normal, Random, Sequential, WillNeed, DontNeed }
impl MemoryMappedFile {
    pub fn advise(&self, offset: u64, len: u64, advice: MmapAdvice) -> Result<()>;
}

// iterator (feature = "iterator", default-on)
// Since 0.9.7: chunks() and pages() are zero-copy and yield
// MappedSlice<'a>. chunks_owned() / pages_owned() preserve the old
// Vec<u8> ergonomics for callers that need owned buffers.
pub struct ChunkIterator<'a> { /* private */ }       // Item = MappedSlice<'a>
pub struct PageIterator<'a> { /* private */ }        // Item = MappedSlice<'a>
pub struct ChunkIteratorOwned<'a> { /* private */ }  // Item = Result<Vec<u8>>
pub struct PageIteratorOwned<'a> { /* private */ }   // Item = Result<Vec<u8>>
pub struct ChunkIteratorMut<'a> { /* private */ }
impl MemoryMappedFile {
    pub fn chunks(&self, chunk_size: usize) -> ChunkIterator<'_>;
    pub fn pages(&self) -> PageIterator<'_>;
    pub fn chunks_owned(&self, chunk_size: usize) -> ChunkIteratorOwned<'_>;
    pub fn pages_owned(&self) -> PageIteratorOwned<'_>;
    pub fn chunks_mut(&self, chunk_size: usize) -> ChunkIteratorMut<'_>;
}
impl<'a> ChunkIteratorMut<'a> {
    // Since 0.9.7: flattened return (was Result<Result<(), E>>).
    // Acquires the write guard ONCE for the entire iteration.
    pub fn for_each_mut<F>(self, f: F) -> Result<()>
        where F: FnMut(u64, &mut [u8]) -> Result<()>;
}

// cow (feature = "cow")
impl MemoryMappedFile {
    pub fn open_cow<P: AsRef<Path>>(path: P) -> Result<Self>;
}

// locking (feature = "locking")
impl MemoryMappedFile {
    pub fn lock(&self, offset: u64, len: u64) -> Result<()>;
    pub fn unlock(&self, offset: u64, len: u64) -> Result<()>;
    pub fn lock_all(&self) -> Result<()>;
    pub fn unlock_all(&self) -> Result<()>;
}

// atomic (feature = "atomic")
// AtomicView<'_, T> / AtomicSliceView<'_, T> hold the read lock for
// their lifetime; Deref<Target = T> / Deref<Target = [T]> so call
// sites use the wrappers as if they were `&T` / `&[T]`. resize()
// blocks until every live view is dropped (C3 fix).
pub struct AtomicView<'a, T> { /* private */ }
pub struct AtomicSliceView<'a, T> { /* private */ }
impl MemoryMappedFile {
    pub fn atomic_u32(&self, offset: u64) -> Result<AtomicView<'_, AtomicU32>>;
    pub fn atomic_u64(&self, offset: u64) -> Result<AtomicView<'_, AtomicU64>>;
    pub fn atomic_u32_slice(&self, offset: u64, count: usize) -> Result<AtomicSliceView<'_, AtomicU32>>;
    pub fn atomic_u64_slice(&self, offset: u64, count: usize) -> Result<AtomicSliceView<'_, AtomicU64>>;
}

// watch (feature = "watch")
// ChangeKind reflects what the polling backend can detect today.
// Native event backends (planned for 0.9.9) may enrich this set.
pub enum ChangeKind { Modified, Metadata, Removed }
pub struct ChangeEvent { /* private */ }
pub struct WatchHandle { /* private */ }
impl MemoryMappedFile {
    pub fn watch<F>(&self, callback: F) -> Result<WatchHandle>
        where F: FnMut(ChangeEvent) + Send + 'static;
}

// async (feature = "async")
impl MemoryMappedFile {
    pub async fn update_region_async(&self, offset: u64, data: &[u8]) -> Result<()>;
    pub async fn flush_async(&self) -> Result<()>;
    pub async fn flush_range_async(&self, offset: u64, len: u64) -> Result<()>;
}
// manager-level async helpers
pub async fn create_mmap_async<P: AsRef<Path>>(path: P, size: u64) -> Result<MemoryMappedFile>;
pub async fn copy_mmap_async<P: AsRef<Path>>(src: P, dst: P) -> Result<()>;
pub async fn delete_mmap_async<P: AsRef<Path>>(path: P) -> Result<()>;
```

### 4.3 Hugepages

`hugepages` is a Cargo feature that opts into best-effort huge-page
backing. The implementation MUST attempt `MAP_HUGETLB` on Linux and
`FILE_ATTRIBUTE_LARGE_PAGES` on Windows and MUST silently fall back
to standard 4 KiB pages on failure. `.huge_pages(true)` therefore
provides NO guarantee of huge-page backing and MUST be documented as
best-effort.

## 5. Safety contract

### 5.1 Thread safety

`MemoryMappedFile` MUST be `Send + Sync`. Concurrent reads from
disjoint slices MUST be safe. Concurrent writes to overlapping
regions are caller-managed; the crate MUST NOT silently serialize
them. Internal state (flush policy, watch handles, etc.) MUST be
protected by `parking_lot::RwLock` or atomic primitives.

### 5.2 Bounds checking

Every method that accepts an `offset` and `len` MUST validate that
`offset + len <= self.len()` before returning a slice. Out-of-bounds
requests MUST return `MmapIoError::OutOfBounds`, not panic, not UB.

### 5.3 Alignment

`atomic_u32` / `atomic_u64` / `atomic_u32_slice` / `atomic_u64_slice`
MUST verify that the offset is naturally aligned for the element type.
Misaligned access MUST return `MmapIoError::Misaligned { required,
offset }`, not unsafely transmute.

### 5.4 Unsafe blocks

Every `unsafe` block in the crate MUST be preceded by a
`// SAFETY:` comment that:

- States the invariants required for soundness.
- Demonstrates how the local context establishes them.
- References the source of any platform-specific guarantee
  (POSIX spec, MSDN page, kernel man page) where relevant.

Reviews of new `unsafe` blocks MUST verify the SAFETY comment is
accurate and complete.

## 6. MSRV policy

Current MSRV: **Rust 1.75**. Pinned in `Cargo.toml`
(`rust-version = "1.75"`) and in `clippy.toml` (`msrv = "1.75"`).

MSRV MUST NOT be bumped without:
- A documented user-visible benefit (e.g., a stable API feature the
  crate would adopt).
- A CHANGELOG `### Changed` entry under the release that introduces
  the bump.
- CI verification on the new MSRV.

MSRV bumps are minor-version-bump events through 0.x.y; post-1.0
they MUST be major-version-bump events.

## 7. Performance contract

The crate MUST provide and maintain:

1. **`criterion`-based benchmarks** in `benches/mmap_bench.rs`
   covering: cold/warm reads, write-then-flush, microflush, page
   prewarming, huge-page mapping, iterator throughput.
2. **Touch-pages support** for benchmark page-fault elimination,
   exposed publicly via `touch_pages` / `touch_pages_range`.
3. **Documented flush-policy tradeoffs.** `FlushPolicy::Manual` is
   fastest; `FlushPolicy::EveryBytes` and `FlushPolicy::EveryMillis`
   trade latency for durability. The CHANGELOG entry for any
   flush-related change MUST document the measured impact.

Performance regressions of more than 10% on any documented benchmark
MUST be either fixed or explicitly justified in the CHANGELOG before
release.

## 8. Stability guarantees

Through `0.9.x`:
- The public API surface (section 4) is stable. Breaking changes
  require a `0.10.0` or `1.0.0` bump.
- Internal types and modules NOT re-exported from `lib.rs` are
  unstable and MAY change in any patch release.
- Optional features MAY be promoted from experimental to stable;
  experimental features MUST be marked as such in their rustdoc.

The `1.0.0` release MUST pin:
- The public API surface in section 4.
- The MSRV (1.75 or higher, decided at 1.0 cut).
- The feature flag set.

Post-1.0 breaking changes are limited per semver to major-version
bumps.

## 9. Out of scope

The crate intentionally does NOT provide:

- **Network-backed files** (NFS, SMB, S3, etc.). mmap semantics over
  network filesystems are unreliable; callers wanting that need a
  different abstraction.
- **In-process file caching beyond the OS page cache.** The OS does
  this better; duplicating it is wasteful.
- **Database semantics** (transactions, schemas, query languages).
  This crate is a building block; database semantics belong above it.
- **Async I/O for reads.** Memory-mapped reads ARE the async-friendly
  primitive — they bypass the kernel I/O path entirely once mapped.
  `async` feature covers flush operations only.
- **GC integration.** No tracking of slice lifetimes beyond what
  Rust's borrow checker already enforces.

## 10. Dependencies

The crate has a minimal direct dependency set:

| Dependency      | Purpose                                       | Required |
|-----------------|-----------------------------------------------|----------|
| `memmap2`       | Platform abstraction over `mmap` / `MapView`  | Yes      |
| `parking_lot`   | Faster `RwLock` for interior mutability       | Yes      |
| `libc`          | POSIX syscall declarations                    | Yes      |
| `anyhow`        | Error context in tests/examples               | Yes      |
| `thiserror`     | Derive-based error type definitions           | Yes      |
| `log`           | Standard logging facade                       | Yes      |
| `cfg-if`        | Cross-platform conditional compilation        | Yes      |
| `tokio`         | Async helpers                                 | `async`  |

New dependencies MUST be justified against:
- Does the crate already pull in equivalent functionality?
- Does the dependency hold MSRV at 1.75 or below?
- Is the dependency maintained and trusted?

## 11. Testing requirements

- Unit tests in each module's `#[cfg(test)] mod tests` block.
- Integration tests in `tests/` for cross-module behavior.
- Property-based tests (added in 0.9.6) via `proptest` for: bounds
  checking, alignment validation, flush-policy state transitions.
- Fuzz tests (target: 0.9.10) via `cargo-fuzz` for: `read_into`,
  `update_region`, atomic-view access patterns.
- CI MUST run on Linux, macOS, and Windows, on MSRV (1.75) and
  stable.
- Every public method MUST have at least one rustdoc example.
- Doctests are part of the test suite and MUST pass on every CI run.

## 12. Documentation

- README.md: project overview, quick start, feature matrix, install
  patterns, performance summary.
- docs/API.md: hand-written API reference with method-by-method
  signatures, errors, and examples.
- REPS.md: this document.
- CHANGELOG.md: Keep-a-Changelog format, dated releases, link
  footers, `[Unreleased]` section maintained.
- docs.rs build: `all-features = true` so every gated API renders.
- Inline rustdoc: every public item documented; no `// TODO` or
  empty rustdoc comments in shipped releases.
