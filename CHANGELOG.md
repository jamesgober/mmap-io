# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

Bug-fix release. Several of the fixes below are memory-safety bugs reachable from safe code (marked **soundness**); upgrading is recommended for every user. No public items were removed or renamed and no signatures changed; the behavior changes are listed under **Changed**.

### Security

- **`memmap2` removed; mapping now goes through the in-house `mmap_io::raw` layer** ([RUSTSEC-2026-0186](https://rustsec.org/advisories/RUSTSEC-2026-0186.html)). memmap2 before 0.9.11 did not validate `offset` / `len` in `flush_range` and `advise_range`. The memmap2 bump that first addressed this was contributed by **@merces** in #10; 1.1.0 goes further and drops the dependency. `mmap_io::raw` checks every range with overflow-checked arithmetic before any pointer math or syscall, and the managed API validates every range again under the mapping lock, pinned by `tests/range_validation_edges.rs`.
- **Removed the unused `anyhow` dependency** ([RUSTSEC-2026-0190](https://rustsec.org/advisories/RUSTSEC-2026-0190.html)).
- **`event-listener` 5.4.1 -> 5.4.2** in `Cargo.lock` ([RUSTSEC-2026-0221](https://rustsec.org/advisories/RUSTSEC-2026-0221.html)). Reached through `blocking` under the `async` feature.

### Added

- **`mmap_io::raw`**: a first-party, dependency-free memory-mapping layer (`RawMmap`, `RawMmapMut`, `RawMmapOptions`, `offset_granularity`) for Unix (`mmap` / `msync` / `munmap`) and Windows (`CreateFileMappingW` / `MapViewOfFile` / `FlushViewOfFile` + `FlushFileBuffers`), with a stub that returns `Unsupported` elsewhere. Its API follows memmap2's shape for the calls most code uses, so migrating is mostly an import change. Durable `flush` on every platform, offsets aligned to the OS granularity (64 KiB on Windows), mapping past end of file rejected up front instead of faulting on access, zero-length windows without an OS mapping. Measured never slower than memmap2 0.9.11; on Windows, map+drop is up to 2x faster (no write/exec probing, no handle duplication for read-only maps). Covered by unit, integration, concurrency, leak, property, Miri (range arithmetic) and fuzz (`raw_map`) tests. The managed `MemoryMappedFile` is built on it.
- **`raw` additions from memmap2's API.** `RawMmapOptions::populate()` (`MAP_POPULATE` on Linux / Android, ignored elsewhere) and `RawMmapOptions::huge()` (`MAP_HUGETLB` for `map_anon` on Linux / Android; the OS mapping is rounded up to whole huge pages and no fallback is attempted). `RawMmapMut::make_read_only(self) -> io::Result<RawMmap>` and `RawMmap::make_mut(self) -> io::Result<RawMmapMut>` (`mprotect` / `VirtualProtect`; a mapping made read-only gets its original shared, copy-on-write or anonymous access back; a mapping created read-only can be made writable on Unix if the file was opened for writing, and never on Windows, where it returns `Unsupported` because a `PAGE_READONLY` section cannot become writable). `advise` / `advise_range` (feature `advise`, reusing `MmapAdvice`; ranges validated before the syscall, start widened to a page; `DontNeed` refused with `InvalidInput` on copy-on-write and anonymous mappings, where it would discard private pages under live borrows) and `lock` / `unlock` (feature `locking`) on both `RawMmap` and `RawMmapMut`.
- **Non-blocking accessors** on `MemoryMappedFile`: `try_as_slice(offset, len) -> Result<Option<MappedSlice<'_>>>`, `try_as_slice_mut(offset, len) -> Result<Option<MappedSliceMut<'_>>>`, `try_update_region(offset, data) -> Result<bool>`; and on `AnonymousMmap`: `try_as_slice`, `try_as_mut_slice`, `try_update_region`. They return `Ok(None)` / `Ok(false)` instead of waiting for the mapping lock (`try_read_recursive` / `try_write`), and otherwise match the blocking versions (mode checks, validation under the lock, zero-length rule, atomic-overlap check, pending-bytes accounting and flush policy, the policy flush running under the already-held lock). This is the documented way to write from a thread that may hold a view, where the blocking methods deadlock. `tests/try_accessors.rs`.
- **`MemoryMappedFile::schedule_flush()` and `schedule_flush_range(offset, len)`**: start write-back without waiting for durability. Linux uses `sync_file_range(SYNC_FILE_RANGE_WRITE)` on the backing file (it really starts write-out, unlike `msync(MS_ASYNC)`, which Linux ignores); macOS and other Unix use `msync(MS_ASYNC)`; Windows uses `FlushViewOfFile` without `FlushFileBuffers`. **Not durable**: `flush()` / `flush_range()` remain the only durable calls, and `pending_bytes()` is not reset. Ranges are validated like `flush_range`; `ReadOnly` and `CopyOnWrite` mappings are validated no-ops. Measured in `docs/PERFORMANCE.md` (Linux, 4 KiB: 0.28 µs vs 4.7 ms for `flush_range`). `tests/schedule_flush.rs`, including a Linux check that dirty pages are actually written back.

### Fixed

- **Soundness: atomic views and plain views of the same bytes could coexist.** An `AtomicView` / `AtomicSliceView` and a `MappedSlice` (or iterator item, `Segment::as_slice`) both hold read guards, so a safe atomic `store` could race with the slice's plain reads, which is undefined behavior; two atomic views of different element sizes over the same bytes were mixed-size atomic accesses. 1.0 documented this as the caller's responsibility. Each writable mapping (`ReadWrite`, `CopyOnWrite`, `AnonymousMmap`) now tracks the byte ranges of its live views and refuses the overlapping combinations with `MmapIoError::InvalidMode`; disjoint ranges are unaffected. Copying reads (`read_into`, `read_bytes`, `MmapReader`, owned iterators, `touch_pages`) are never refused: they read bytes under a live atomic view with atomic loads of the view's element size. `chunks()` / `pages()` items that overlap a live atomic view are owned copies instead of borrows. See `docs/SAFETY.md` category 9 and `tests/atomic_plain_exclusion.rs`. Without the `atomic` feature the tracking compiles to nothing.
- **Soundness: use-after-free through `chunks()` / `pages()` items.** On RW mappings the read guard lived in the iterator while yielded items were plain borrows, so an item kept after the iterator was dropped pointed into memory that `resize()` could unmap. Every yielded item now holds its own read guard.
- **Soundness: atomic views on read-only pages.** `atomic_u32` / `atomic_u64` and the slice variants returned views into RO and COW mappings whose safe `store` / `fetch_add` fault the process. They now return `MmapIoError::InvalidMode` unless the mapping is `ReadWrite`.
- **Soundness: range checks against a stale length.** Accessors read the cached length, validated, and only then took the map lock, so a concurrent `resize()` could make `read_into` / `as_slice` index past the new mapping (panic, or `SIGBUS` on Linux). Every accessor now validates against the length of the mapping its guard protects. `advise()` and `lock()` / `unlock()` keep the guard alive across the syscall instead of releasing it before using the pointer.
- **Soundness: `resize()` truncated before locking.** A shrink called `set_len` before taking the write lock, so live views of the tail faulted with `SIGBUS`. `resize()` now takes the write lock first.
- **Soundness: `Send` impls relied on parking_lot internals.** `ChunkIterator`, `AtomicView`, and `AtomicSliceView` claimed parking_lot read guards are `Send`; they are not unless the `send_guard` feature is on. The feature is now enabled and a compile-time assertion guards it.
- **Explicit `flush()` never flushed.** Under the default `FlushPolicy::Never` / `Manual` the dirty counter was never incremented, and `flush()` / `flush_range()` returned early when it was zero; writes through `as_slice_mut`, `chunks_mut`, atomics, `as_mut_ptr`, and `SegmentMut` never counted under any policy. On Linux the flush path used `msync(MS_ASYNC)`, which only schedules writeback. `flush()` now always performs a synchronous flush on RW mappings (`msync(MS_SYNC)` on Unix, `FlushViewOfFile` + `FlushFileBuffers` on Windows). See **Changed** for the counter semantics and **Performance** for the cost.
- **`flush_range()` debited the global counter by an unrelated range**, which could suppress the next `EveryBytes` flush. A partial range now leaves the counter unchanged; a range covering the whole mapping resets it.
- **Windows `resize()` shrink was virtual.** It only lowered the cached length: the file never shrank, and a later grow failed with os error 1224 or exposed the bytes that should have been cut off. The view is now unmapped, the file truncated, and the prefix remapped.
- **Same-thread deadlock with a queued writer.** Taking a second read view (or calling `read_into`) on a thread that already held one deadlocked once another thread was waiting for the write lock. Read paths now use recursive read locks.
- **`huge_pages(true)` pre-faulted the whole file.** The builder ran `madvise(MADV_POPULATE_WRITE)` over every mapping of 2 MiB or more, allocating every block of a sparse file and dirtying every page. It now only issues the `MADV_HUGEPAGE` hint.
- **Builder `open()` ignored `FlushPolicy::EveryMillis` and `TouchHint::Eager`** (no flusher thread was started), and `open_or_create()` inherited that on its open path. All builder RW paths now apply every option.
- **`open_or_create()` could truncate a file created concurrently** (`exists()` followed by a truncating create). It now opens without truncating and creates with `create_new`.
- **`create_mmap_async()` truncated the file before validating the size.** It now delegates to `create_rw`, which validates first.
- **`SegmentMut::write` wrote past the segment** when `data` was longer than the segment. It now returns `OutOfBounds` (segment-relative fields).
- **`advise()` failed with `EINVAL` on Linux and macOS for offsets that are not page multiples.** The range start is now widened down to a page boundary.
- **`MmapReader::seek(SeekFrom::End(i64::MIN))` panicked**, and seeking before position 0 silently clamped. Seeking now matches `std::io::Cursor`: `InvalidInput` for negative or overflowing targets, position unchanged.
- **`utils::align_up(u64::MAX, 4096)` overflowed** (panic in debug, 0 in release). It now saturates to `u64::MAX`.
- **`TimeBasedFlusher` and `WatchHandle` drop.** Both spawned a throwaway thread to join their worker, so the worker could still run after the drop returned. They now join directly (skipping the join when dropped on the worker itself), and the flusher sleeps on a condition variable instead of waking every 50 ms. `Drop` never panics.

### Changed

- **Atomic and plain views of the same bytes are refused at run time** (see **Fixed**). `as_slice`, `try_as_slice` and `Segment::as_slice` return `InvalidMode` for a range that overlaps a live atomic view; `atomic_u32` / `atomic_u64` and the slice variants return `InvalidMode` for a range that overlaps a live `MappedSlice` or iterator item, or a live atomic view of the other element size (checked after alignment and bounds). Iterator items that overlap a live atomic view are owned copies (one allocation). Code that keeps atomic and plain regions disjoint, as 1.0 required, is unaffected. With the `atomic` feature on, each RW / COW plain view costs one extra lock round trip (see **Performance**).
- **`AnonymousMmap` read paths use recursive read locks** like `MemoryMappedFile`, so a thread holding a view cannot deadlock behind a queued writer.
- **`MmapMode::CopyOnWrite` mappings are writable.** `open_cow` (and builder / `from_file` with `CopyOnWrite`) now maps the file privately writable (`MAP_PRIVATE` / `PAGE_WRITECOPY` through `raw::RawMmapOptions::map_copy`) instead of read-only. `update_region`, `as_slice_mut`, `SegmentMut`, `chunks_mut`, `as_mut_ptr` and the atomic views work and write private pages: visible through the mapping and its clones, never written to the file, lost on drop. They returned `InvalidMode` before. `flush` / `flush_range` stay `Ok` no-ops (ranges validated), `pending_bytes()` stays 0, `resize` still returns `InvalidMode`. COW mappings now take the same locks as `ReadWrite`, so a live view blocks the write methods; `as_slice_bytes` returns `InvalidMode` on COW (it cannot guard a plain `&[u8]` against writers); `advise(.., DontNeed)` on COW takes the write lock, because on Linux it discards the private copies (the range reads the file again). Covered by `tests/cow_writable.rs` (writes never reach the file, read back with `std::fs::read`).
- **`advise` and `lock` / `unlock` go through the raw layer** (`madvise` / `PrefetchVirtualMemory`, `mlock` / `VirtualLock`), and `lock` / `unlock` widen the start down to a page boundary like `advise`. Error variants and messages are unchanged.
- **Zero-length range rule.** A zero-length request is accepted at any offset and does nothing, on every range API. Before, `as_slice`, `as_slice_mut`, `read_into`, and `Segment::new` rejected a zero-length request past the end while `flush_range`, `advise`, and the rest accepted it. Atomic views are not range requests and are unchanged. Documented in the crate docs and `docs/API.md`.
- **`pending_bytes()` counts every write path under every policy**: `update_region`, `MappedSliceMut` (on drop), `chunks_mut`, atomic views (their size, on drop), and `as_mut_ptr` (the whole mapping). Under `EveryWrites` it now reports bytes, not the call count. It only drives automatic flushes; explicit `flush()` ignores it.
- **Atomic views require a `ReadWrite` mapping** (`InvalidMode` on RO / COW, see **Fixed**). Checks run in the order mode, alignment, bounds.
- **`resize()` truncates on Windows** and fails with `MmapIoError::Io` if another independent mapping of the same file is open, since Windows cannot truncate a mapped file. The mapping is restored at its old length in that case.
- **`open_or_create()` extends an existing zero-length file** to `default_size` instead of failing, and never leaves an empty file behind when it errors. Builder `open_or_create()` defaults to `ReadWrite` on both paths, as documented; it opened existing files read-only before.
- **`huge_pages(true)` is a hint**: `madvise(MADV_HUGEPAGE)` on Linux RW mappings (including after `resize`), no effect elsewhere. `MAP_HUGETLB` and Windows large pages were documented but never attempted; the docs now say so.
- **parking_lot's `send_guard` feature is enabled.** parking_lot rejects `send_guard` together with its `deadlock_detection` feature, so a dependency graph that turns on `parking_lot/deadlock_detection` no longer compiles with mmap-io.
- **`MappedSlice` and `MappedSliceMut` are now `Send`** (additive auto-trait change; `public-api.txt` updated). `MappedSliceMut` gained a `Drop` impl (pending-bytes accounting).
- **`chunks()`, `pages()`, `chunks_owned()`, `pages_owned()`, and `chunks_mut()` no longer contain unreachable `expect` calls**; their incorrect `# Panics` sections are gone. `ChunkIteratorMut` reads the mapping length under its write lock.
- **Dependencies:** `thiserror` and `cfg-if` removed (`MmapIoError` implements `Display` / `Error` / `From<io::Error>` by hand with byte-identical messages, pinned by a test); `libc` is a Unix-only dependency; docs.rs metadata drops the redundant `features = ["async"]`.
- **Internals:** the cached length and the flush counters are `AtomicU64` instead of `RwLock<u64>`; `MappedSlice` caches its slice pointer at construction instead of re-indexing the guard on every deref.
- **CI:** tests run for the default feature set, all features, no features, and each feature alone on Linux, macOS, and Windows; a new MSRV job builds the library on 1.75 on all three OSes; the bench-regression gate (which could never fail) parses critcmp's table and uses the REPS.md 10% limit; `actions/cache` v6 and `actions/upload-artifact` v7; tool installs no longer come from never-expiring caches.

### Performance

Measured with `cargo bench --all-features --bench mmap_bench` on the reference machines in `docs/PERFORMANCE.md` (Windows 11 / NTFS on SSD, and WSL2 Ubuntu / ext4 on the same SSD), 1.0.0 vs this release. REPS.md section 7 requires flush changes to state their measured cost and any regression above 10% to be justified:

- **Flush paths are much slower because they now flush.** `flush_range` after a small write: 36-67 ns -> 0.43-1.28 ms (Windows), 35-61 ns -> 1.1-2.1 ms (Linux). `update_region` + `flush()`: 43 ns-16 µs -> 0.44-0.87 ms (Windows), 66 ns-14 µs -> 0.87-1.75 ms (Linux). The 1.0 numbers measured a flush that never reached the OS; there was no fast flush to preserve. Justified: the old behavior was a durability bug.
- **`EveryBytes(64 KiB)` sequential write on Linux: 62 µs -> 58 ms** (`msync(MS_ASYNC)` -> `MS_SYNC`, 64 flushes). Unchanged on Windows (19 ms), where the flush was already synchronous. **`EveryBytes(n)` threshold bench** (one flush per write) on Linux: 0.15-15 µs -> 0.95-1.7 ms. Justified: same reason.
- **`EveryMillis(10)` sequential write on Linux: 55 µs -> 80 µs**, because the background flush now holds the read lock for a synchronous `msync` that writers wait for. Windows: 71 µs -> 58 µs (noise).
- **`resize` grow + shrink on Windows: 24 µs -> 77 µs**, because a shrink now really unmaps, truncates, and remaps (1.0 only lowered the cached length, which was the bug). Linux: 6.9 µs -> 6.1 µs.
- **Faster:** zero-copy `chunks()` / `pages()` about 3x (16 MiB file, 4 KiB chunks: 26 µs -> 8-10 µs), random `as_slice` 4-10x (e.g. 64 B: 95 ns -> 11 ns Windows, 77 ns -> 15 ns Linux), `as_slice` on RO 2.5x (14 ns -> 5.5 ns), small `read_into` 10-25% (one lock acquisition instead of two), from the cached slice pointer, inlined accessors, and the atomic length.
- **Within noise, flagged for the record:** `sequential_read/as_slice` at 16 MiB measured 1.2-1.45x slower on Linux and 0.9-1.33x on Windows across three reruns, while two runs of the unchanged 1.0 build differed by up to 1.3x on the same benches. The RO `as_slice` path does the same work as in 1.0 (one bounds check, no lock). `create_rw` and `open_cow` on Windows swung more than 2x between runs of the same build.

### Documentation

- **`docs/SAFETY.md` rewritten** around the locking model: who holds which guard, validation under the guard, the per-platform `resize` protocol, every `unsafe` category by function name, and what the crate cannot guarantee (other processes, mixing atomic and plain access to the same bytes, raw pointers).
- **README, `docs/API.md`, rustdoc, examples:** `flush()` semantics and durability vs visibility; any live read view blocks every write (not only `resize`) and a same-thread write deadlocks; COW mappings are read-only in practice; `TouchHint::Lazy` equals `Never`; async helpers are runtime-agnostic and `update_region_async` allocates; corrected the README iterator example and version strings.
- **`docs/PERFORMANCE.md`** re-measured for every flush path; the "36 ns microflush" figure measured a flush that never ran.
- **`REPS.md`:** section 4 matches the real signatures, 4.3 describes the huge-page hint, 5.1 / 5.2 describe the locking and range rules, section 10 lists the actual dependencies, section 11 lists the four fuzz targets and what CI runs on MSRV.

### Notes

- MSRV unchanged at Rust 1.75 (library). The test suite's dev-dependencies (`proptest` 1.11, `half` 2.6 via `criterion`) need a newer toolchain.
- New regression tests: `tests/soundness_regressions.rs` and `tests/behavior_regressions.rs`, plus unit tests for the flusher shutdown and a hugepages sparse-file test.

<br>

<!-- VERSION: 1.0.0 -->
## [1.0.0] - 2026-05-18

The stable release. API surface is now locked under SemVer: breaking
changes require a major-version bump (`2.0.0`), additive features ship
as minor bumps (`1.1.0`+), bug fixes ship as patch bumps (`1.0.1`+).
CI enforces this via `cargo-semver-checks` and a new `cargo public-api`
diff workflow.

API-compatible with `0.9.11`. Callers on `0.9.11` upgrade by bumping
the version string. See [`docs/MIGRATION_0.9_TO_1.0.md`](docs/MIGRATION_0.9_TO_1.0.md)
for the full upgrade story, including the recovery path for callers
still on `0.9.6` or earlier.

### Added

- **`AnonymousMmap`** (new module `mmap_io::anonymous`). Process-local memory mapping with no backing file. Useful for shared scratch memory between threads, large temporary allocations that should bypass the heap, or as the kernel-side substrate for fd-passing IPC patterns. Pages are zero-initialized on first touch; memory is released when the value drops. Methods: `new(size)`, `len`, `is_empty`, `read_into`, `update_region`, `as_slice`, `as_mut_slice`, `as_ptr` / `as_mut_ptr` (unsafe). 17 tests in `tests/v1_0_0_additions.rs`. Closes audit F1.
- **`MemoryMappedFile::is_hugepage_backed() -> Option<bool>`**. Runtime introspection for whether the kernel currently backs a mapping with huge pages. On Linux, parses `/proc/self/smaps` and inspects `AnonHugePages`, `Private_Hugetlb`, `Shared_Hugetlb` for the entry containing the mapping's base address. Returns `Some(true)` if any portion is huge-page backed, `Some(false)` for regular pages, `None` on non-Linux platforms or when the lookup fails. Closes audit F4.
- **Multi-process IPC integration test** (`tests/ipc_cross_process.rs`). Verifies bidirectional byte visibility: parent writes, spawns child via `std::process::Command::new(std::env::current_exe())`, child reads parent's writes and writes its own bytes, parent verifies child's writes. The test invokes itself with libtest's `--exact` filter so only the target test function runs in the child. Closes audit T6.
- **`public-api.txt`** committed to the repository. A `cargo public-api --simplified --all-features` snapshot of the locked 1.0.0 surface. A new CI workflow (`.github/workflows/public-api.yml`) regenerates the snapshot on every PR and fails the build if the diff is non-empty without an accompanying snapshot update. Catches accidental API changes at PR review time rather than at release.

### Changed

- **`# Errors` and `# Panics` rustdoc completeness pass.** Every `Result`-returning public method now documents the error conditions it can return; every public method that calls `.expect()` documents the panic conditions. Previously the crate root denied `missing_docs` (every item has a doc comment); now `clippy::missing_errors_doc` and `clippy::missing_panics_doc` also pass clean.
- **Sparse-file behavior documented** on `create_rw` and `open_or_create`. The `set_len(size)` call produces a sparse file on every supported platform; a 1 TB `default_size` does not consume 1 TB of free disk until pages are written. Closes audit F6.

### Documentation

- **README rewritten as a fresh 1.0.0 launch.** Migration-from-0.9.6 content moved out of the README and into `docs/MIGRATION_0.9_TO_1.0.md`. README now focuses on the current product and stability commitment, not on a transition story.
- **`docs/MIGRATION_0.9_TO_1.0.md`** new file. Covers the upgrade path from any 0.9.x version: direct version bump from 0.9.11, compat-shim recovery from 0.9.6 (or earlier), and the optional zero-copy migration to the modern API.
- **`docs/API.md`** updated with the new `AnonymousMmap`, `is_hugepage_backed`, and `MmapReader` types, plus the 0.9.11 `read_bytes` / `reader` / `as_slice_bytes` entries that were not yet documented there. Feature table updated; the `async` description no longer says "Tokio-based" (runtime-agnostic since 0.9.11).
- **`REPS.md`** version refs bumped to 1.0.0.

### Internals

- New bench warning fix: `Arc` import in `benches/mmap_bench.rs` gated behind `#[cfg(feature = "atomic")]` to match its sole usage site.
- `tempfile` remains a dev-dependency; not pulled into the production tree.

### Notes

- MSRV unchanged at Rust 1.75.
- No public API breaks vs 0.9.11; existing 0.9.11 code compiles unchanged.
- Total test count: 158 passing (up from 140 in 0.9.11): 17 new in `v1_0_0_additions.rs`, 1 new in `ipc_cross_process.rs`. 1 ignored (unrelated hugepages-fallback). 0 failed.
- `cargo build / test --all-features / clippy / doc / audit / semver-checks / fmt / public-api` all clean.

<br>

<!-- VERSION: 0.9.11 -->
## [0.9.11] - 2026-05-14

Patch release. Two issues from the field (semver violation flagged
by **bbqsrc** in #6, smol runtime support requested by **ararog**)
plus opportunistic ecosystem polish: `bytes::Bytes` integration,
`io::Read` + `io::Seek` cursor, and `AsRawFd` / `AsHandle` trait
impls. Everything is additive; no API breaks.

### Added

- **`MemoryMappedFile::as_slice_bytes(offset, len) -> Result<&[u8]>`** — migration shim mirroring the 0.9.6 `as_slice` signature. RO and COW mappings return `&[u8]` directly; RW returns `MmapIoError::InvalidMode` matching the 0.9.6 behavior. Callers broken by the 0.9.7 `as_slice` return-type change recover with a one-method-name rename. Prefer `as_slice` for new code.
- **`ChunkIteratorMut::for_each_mut_legacy<F, E>(F) -> Result<Result<(), E>>`** — migration shim mirroring the 0.9.6 nested-Result signature. Internally uses the same single-held-write-guard loop as the flattened `for_each_mut`, so the H2 perf win is preserved.
- **`feature = "bytes"`** — `bytes::Bytes` integration. New `MemoryMappedFile::read_bytes(offset, len) -> Result<bytes::Bytes>` plus `From<MappedSlice<'_>>` / `From<&MappedSlice<'_>>` for `bytes::Bytes`. One allocation + memcpy at the conversion boundary; the resulting `Bytes` is mapping-lifetime-independent and travels freely through hyper / tower / tonic / axum / reqwest. Opt-in via `--features bytes`.
- **`MemoryMappedFile::reader() -> MmapReader<'_>`** — cursor implementing `std::io::Read` + `std::io::Seek`. Plugs the mapping into every parser / decoder that takes a generic `R: Read`: `serde_json::from_reader`, `flate2::read::GzDecoder`, `tar::Archive::new`, `image::ImageReader::new`, etc. `MmapReader::position()` and `set_position()` for direct cursor manipulation.
- **`AsFd` + `AsRawFd`** (Unix) and **`AsHandle` + `AsRawHandle`** (Windows) trait impls on `MemoryMappedFile`. Standard Rust way to hand the underlying file descriptor / handle to FFI code or other crates (`nix`, `rustix`, `polling`) without going through `unmap`.

### Changed

- **`feature = "async"` is now runtime-agnostic.** Was tokio-only through 0.9.10; now uses the `blocking` crate under the hood. Existing tokio users see no API change — the async methods (`update_region_async`, `flush_async`, `flush_range_async`, `manager::async::*`) still return futures with the same signatures and behavior. The change is that those futures now run on any executor (tokio, smol, async-std, embassy on hosted, etc.), not just tokio. Fixes the ararog issue: smol-based callers can use the async surface without dragging tokio into their dep tree. The transitive dep tree under `--features async` shrinks: tokio + tokio's pile of deps out, the much smaller `blocking` crate in.

### Documentation

- **CHANGELOG explicit acknowledgement of the 0.9.7 semver violation.** The breaking signature changes to `as_slice`, the iterator `Item`, and `for_each_mut` shipped in 0.9.7 should have been a 0.10.0 bump per Rust's pre-1.0 semver convention (Cargo's `^0.9.6` resolver treats 0.9.7 as a compatible upgrade). The crate carried this break for four releases (0.9.7 through 0.9.10) without flagging it; the `cargo-semver-checks` workflow added in 0.9.10 would have caught this exact case at PR time. The compat shims in this release (`as_slice_bytes`, `for_each_mut_legacy`) give downstream callers a one-line recovery path. Apologies to bbqsrc and to anyone else whose 0.9.6 code stopped compiling on 0.9.7.
- README gains a **"Migrating from 0.9.6"** mini-section pointing at the compat shims.
- `docs/API.md` documents the new methods, the new `bytes` feature, and the `MmapReader` type.
- `REPS.md` section 4 lists the new public surface.

### Internals

- `tokio` removed from `[dependencies]`; added to `[dev-dependencies]` purely so the existing `#[tokio::test]` test suite continues to drive the runtime-agnostic async surface. Downstream consumers no longer pull tokio via `--features async`.
- New `tests/v0_9_11_additions.rs` covers every new method (13 tests), including a `block_on` built from `std::thread::park` to prove the async surface works without a tokio runtime.

### Notes

- MSRV unchanged at Rust 1.75.
- All 0.9.7-introduced API surface (`MappedSlice`, the new iterator items, the flattened `for_each_mut`) remains the recommended path. The compat shims are explicitly migration aids.
- Total test count: 140 passing (up from 127 in 0.9.10), 1 ignored (unrelated hugepages-fallback), 0 failed.

<br>

<!-- VERSION: 0.9.10 -->
## [0.9.10] - 2026-05-13

### Added

- **Focused example suite** (audit D1). Ten one-purpose examples
  under `examples/`, each runnable via `cargo run --example
  NN_name [--features feat]`. Files: `01_read_a_file`,
  `02_create_and_write`, `03_segment_views`, `04_atomic_counter`
  (`atomic`), `05_log_appender`, `06_chunked_processing`
  (`iterator`), `07_watch_for_changes` (`watch`),
  `08_huge_pages_simulation` (`hugepages`), `09_async_writes`
  (`async`), `10_ipc_shared_state` (`atomic`). Each demonstrates
  one concrete use case in under 100 lines.
- **`cargo-fuzz` scaffold** (audit D7-related). Four fuzz targets
  under `fuzz/fuzz_targets/`: `read_into`, `update_region`,
  `atomic_view`, `bounds_checks`. The fuzz crate is workspace-
  isolated (`fuzz/Cargo.toml` with `[workspace]`) so it does not
  affect `cargo build` from the repo root. Linux/WSL + nightly
  required to run; the maintainer drives one-hour runs per target
  on a Linux box before tagging. See `fuzz/README.md`.
- **`docs/PERFORMANCE.md`** (audit D8) with **measured** numbers
  from the workload-pattern benches added in 0.9.7. Concrete
  speedup tables for the H1 (iterator zero-copy: 13-475x), H4
  (`as_slice` on RW: 15-49x), and H2 (`touch_pages` 1 GiB in 2 ms)
  audit wins. Reference machine noted; reproduction instructions
  inline.
- **CI: `cargo-audit` workflow** (`.github/workflows/audit.yml`).
  Runs on every push, PR, and a daily 04:17 UTC cron. `--deny
  warnings` catches yanked crates at PR time instead of at
  `cargo publish` time. Companion `cargo-deny` job runs as
  `continue-on-error: true` until the maintainer commits a
  `deny.toml` policy.
- **CI: `cargo-semver-checks` workflow**
  (`.github/workflows/semver-checks.yml`). Runs on PRs against
  `main`. Detects accidental breaking changes to the public API
  by walking it against the version on crates.io. Pre-1.0 this
  surfaces information; post-1.0 it gates merges.
- **CI: bench-regression hard gate**
  (`.github/workflows/bench-regression.yml`). Was previously
  upload-artifact only. Now runs the bench against the PR's
  merge-base on the same runner (same CPU, same noise floor) and
  fails the PR if any bench group regresses more than 15%. Uses
  `critcmp` for the comparison.

### Documentation

- **MSRV decision: hold at Rust 1.75 for the foreseeable future.**
  No stable Rust feature in 1.76-1.85 is on the critical path for
  the crate; the C3 atomic wrappers, the `notify`-backed watch
  feature, and the `OnceLock` / `parking_lot` patterns all work
  on 1.75. Holding gives downstream users continuity. Documented
  in `clippy.toml` (already pinned) and `Cargo.toml`
  (`rust-version = "1.75"`).
- **REPS R1-R7 verification.** The REPS-correction items from
  the original audit (`docs/AUDIT.md` section 4) have landed
  across prior milestones. R1 (lock signature), R2
  (`atomic_u32_slice`), R3 (Segment/SegmentMut public-or-hidden:
  PUBLIC), R4 (SAFETY comments complete: 0.9.6), R5 (doctests
  per public method: covered through 0.9.9), R6 (`ChangeKind`
  enum listing matches code: `Modified`/`Metadata`/`Removed`),
  R7 (`MappedSliceMut` public-or-hidden: PUBLIC, re-exported in
  0.9.7).
- **D5 verification.** `# Safety` rustdoc headings appear only
  on `unsafe fn` declarations (`as_ptr`, `as_mut_ptr`); no safe
  function carries one. Conforms to the Rust API guidelines'
  reservation of `# Safety` for unsafe contracts.

### Fixed

- **Lockfile bump: `slab 0.4.10` → `0.4.12`.** `slab 0.4.10`
  (transitive via `tokio` under the `async` feature) was yanked
  from crates.io after we shipped 0.9.9. Lockfile already bumped
  in commit `b5167be` ahead of this release; the fix is folded
  into the 0.9.10 changelog so the warning timeline is clear in
  the public record.

### Notes

- **0.9.10 is the technical lockdown release.** All audit items
  through D8 + R7 are closed. The crate is structurally ready for
  1.0.0; 1.0.0 remains on indefinite hold pending the
  maintainer's cross-repo presentation pass (consistent
  headers/branding/SECURITY.md across the project family).
- No new runtime dependencies. The fuzz scaffold uses
  `libfuzzer-sys` + `arbitrary` but only inside the isolated
  `fuzz/` crate, never reachable from a downstream `cargo add
  mmap-io` build.
- MSRV unchanged at Rust 1.75. Verified via `cargo +1.75 build
  --all-features`.

<br>

<!-- VERSION: 0.9.9 -->
## [0.9.9] - 2026-05-12

### Changed (BREAKING for watch implementors only)

- **Native watch backends.** The polling-based watch implementation
  is gone; the `watch` feature now uses `notify 6` under the hood,
  which dispatches to `inotify` on Linux, FSEvents on macOS, and
  `ReadDirectoryChangesW` on Windows. The public surface is
  unchanged: `MemoryMappedFile::watch(callback) -> Result<WatchHandle>`
  with `ChangeEvent { offset, len, kind }` and
  `ChangeKind { Modified, Metadata, Removed }`. The breaking aspect
  is implementation-side: anyone depending on polling-specific
  timing (e.g. the previous ~100 ms polling interval as a debounce
  floor) sees different timing now. Latency drops from 100 ms+ to
  <10 ms on Linux/Windows and <50 ms on macOS (FSEvents
  coalescing). Note: mmap-side writes (`update_region` + `flush`)
  are not a reliable trigger for native FS watchers on any
  platform; they reach the watcher only at OS-decided writeback
  time. Reliable detection requires `std::fs` API writes from
  another handle / process, which is the real-world use case for
  the watch feature.

### Added

- `notify` 6.x as an optional dependency, gated on the `watch`
  feature with `default-features = false` and only the
  `macos_fsevent` feature enabled to keep the dep tree tight.
- `tests/watch_native.rs` — five new integration tests
  (`watch_modify_detected`, `watch_truncate_detected`,
  `watch_extend_detected`,
  `watch_rapid_sequence_coalesces_or_reports_each`,
  `watch_removed_event_terminates_dispatcher`) that exercise the
  native backends through `std::fs` API writes.
- `src/watch.rs` gains a `WatchHandle::is_active()` method (was
  previously gated behind `#[allow(dead_code)]`); useful for tests
  and diagnostics.

### Fixed

- **Three previously-ignored Windows watch tests now pass live:**
  `watch::tests::test_watch_file_changes`,
  `watch::tests::test_multiple_watchers`,
  `tests/feature_integration.rs::test_all_features_integration`.
  The `#[cfg_attr(windows, ignore = "...")]` markers were the
  symptom of Windows polling-watch unreliability; with
  `ReadDirectoryChangesW` they pass on every platform.

### Documentation

- `README.md`: watch feature description updated to "native
  inotify/FSEvents/RDCW" (not "polling fallback"). Added a note
  about mmap-write detection limitations.
- `docs/API.md`: full rewrite of the watch section. New
  platform-behavior table (Linux <1 ms / macOS <50 ms / Windows
  <10 ms typical latencies), coalescing notes, error contract.
  Version history entry added. Install snippet bumped to 0.9.9.
- `REPS.md`: watch surface annotated `Since 0.9.9: backed by
  notify`.
- `.dev/ROADMAP.md`: **1.0.0 placed on indefinite hold** pending
  cross-repo presentation cleanup (consistent headers / branding /
  SECURITY.md / CONTRIBUTING.md across the project family). The
  previously-planned `1.0.0-rc.1` candidate phase is dropped:
  hyphenated release tags caused tooling issues in prior cycles,
  and the soak / hardening work happens on the last 0.9.x
  in real-world deployment instead. Versioning strategy through
  1.0.0 unblocks: continue with `0.9.x` minor / patch releases as
  needed.

### Notes

- MSRV unchanged at Rust 1.75.
- `notify 6.1.x` advertises MSRV 1.60; verified buildable on 1.75.
- The transitive dep set added by `notify` (with default features
  off and only `macos_fsevent` enabled) is bounded and stable:
  `crossbeam-channel`, `mio` on Linux, `filetime`, and the
  Windows-side `windows_x86_64_msvc` target shim. No surprise
  pulls.

<br>

<!-- VERSION: 0.9.8 -->
## [0.9.8] - 2026-05-12

### Added

- **(E1, E6)** `MemoryMappedFile::open_or_create(path, default_size)`
  and `MemoryMappedFileBuilder::open_or_create()` for the
  open-if-present / create-if-absent pattern in one call.
- **(F9)** `MemoryMappedFile::from_file(file, mode, path)` to wrap a
  pre-opened `std::fs::File` (e.g. one opened with custom
  `OpenOptions` flags like `O_DIRECT` / `O_NOATIME` or inherited
  from a parent process).
- **(F5)** `MemoryMappedFile::unmap(self) -> Result<File, Self>`
  consumes the mapping, drops the underlying mapping + background
  flusher in safe order, and returns the underlying `File`. Returns
  `Err(self)` unchanged if other clones of the mapping are alive.
- **(E7)** `flush_policy()` returns the configured `FlushPolicy`;
  `pending_bytes()` returns the live `EveryBytes` / `EveryWrites`
  accumulator value. Both are `#[inline]` and `O(1)`; useful for
  diagnostics and observability dashboards.
- **(E2)** `unsafe fn as_ptr(&self) -> *const u8` and `unsafe fn
  as_mut_ptr(&self) -> Result<*mut u8>` expose raw base pointers
  to the mapping for FFI / advanced use. Full safety contract in
  the rustdoc.
- **(F2)** `prefetch_range(offset, len)` issues
  `posix_fadvise(POSIX_FADV_WILLNEED)` on the file descriptor on
  Linux (warms the page cache from the file side, complementary to
  `advise(MmapAdvice::WillNeed)` which warms via `madvise` on the
  VM side). No-op fallback on non-Linux. Bounds-checked.
- `tests/ergonomic_api.rs` (17 tests) covers every new method:
  open_or_create both paths, builder open_or_create, from_file
  RO/RW/zero-length, unmap unique/shared, flush_policy /
  pending_bytes, as_ptr / as_mut_ptr roundtrips, prefetch_range
  in-bounds / OOB / zero-length.

### Fixed

- **`flush::TimeBasedFlusher`** thread loop used `interval -
  elapsed` directly. If `thread::sleep` overshot under heavy
  scheduler contention `elapsed` could exceed `interval` and the
  subtraction would panic on Duration underflow. Switched to
  `interval.saturating_sub(elapsed)` so the next slice clamps to
  zero (immediate retry) instead of panicking.

### Performance

- **Bounds-check helpers `#[inline]`-ed.** `ensure_in_bounds` and
  `slice_range` are called from every bounds-checked public method
  (`as_slice`, `as_slice_mut`, `read_into`, `update_region`,
  `flush_range`, `touch_pages_range`, `prefetch_range`, advise,
  lock, segment access). Inlining removes a call/return boundary
  on every read/write. Also merged the two-branch bounds check
  into a single `saturating_add` comparison.
- **`len()` / `is_empty()` / `mode()` / `flush_policy()` /
  `pending_bytes()` marked `#[inline]`**: trivial accessors that
  the optimiser should fold into the call site every time.

### Documentation

- `docs/API.md`: full sections for all eight new methods, TOC
  updated, version snippets bumped to 0.9.8, Version History
  entry added.
- `REPS.md` section 4: new ergonomic methods + builder addition
  listed with `// Since 0.9.8` markers.
- `Cargo.toml` SEO: description leads with the unique selling
  point (zero-copy), names the supported platforms, and lists use
  cases concretely; keywords tightened to the highest-volume
  search terms (`mmap`, `memory-mapped`, `zero-copy`, `filesystem`,
  `io`); categories include `concurrency`.
- `README.md`: opening hook rewritten around the actual
  differentiators (zero-copy on every mode, zero-allocation
  iteration, lock-free atomic views, configurable durability).

### Notes

- No new runtime dependencies. The Linux `posix_fadvise` path uses
  the already-required `libc` crate.
- MSRV unchanged at Rust 1.75.
- **F1** (anonymous shared-memory mapping) remains open. The
  refactor (Inner.file: `Option<File>`, sentinel path handling,
  per-method "anonymous-aware" branches) is sized for a focused
  pass rather than rolled into this ergonomic milestone.

<br>

<!-- VERSION: 0.9.7 -->
## [0.9.7] - 2026-05-12

### Changed (BREAKING)

- **(H1, H4)** `MemoryMappedFile::as_slice(offset, len)` now returns
  `Result<MappedSlice<'_>>` for all three mapping modes (RO, COW,
  **and RW**). Previously RW returned `MmapIoError::InvalidMode`.
  `MappedSlice<'_>` is a wrapper that derefs to `&[u8]` and, on RW
  mappings, holds a read guard for its lifetime so concurrent
  `resize()` blocks until the slice is dropped. Callers that
  previously caught the `InvalidMode` error on RW should remove
  that branch; the call now succeeds and returns a zero-copy view.
  Callers that used `as_slice` on RO/COW and stored the result as
  `&[u8]` should change the binding to `MappedSlice<'_>` or call
  `.as_slice()` / `&*slice` / `slice.as_ref()` at the use site.
- **(H1)** Iterator `Item` types changed. `ChunkIterator::Item` and
  `PageIterator::Item` are now `MappedSlice<'a>` (was
  `Result<Vec<u8>>`). The iterators no longer allocate or copy per
  chunk; they yield direct views into the mapped region. For a
  1 GiB file at 4 KiB chunks this eliminates 262,144 heap
  allocations per scan and roughly 2x of the previous memory
  bandwidth. Callers that genuinely need owned `Vec<u8>` buffers
  should migrate to `chunks_owned()` / `pages_owned()` (added
  below).
- **(audit E4)** `ChunkIteratorMut::for_each_mut` flattened. New
  signature: `fn for_each_mut<F>(self, F) -> Result<()>` where
  `F: FnMut(u64, &mut [u8]) -> Result<()>`. The previous
  triple-nested `Result<Result<(), E>>` is gone. Callers that
  returned `Ok::<(), std::io::Error>(())` should return `Ok(())`
  with `Result<()> = Result<(), MmapIoError>` and map any foreign
  error into `MmapIoError::Io(...)` before returning. Iteration
  now acquires the write guard ONCE for the entire iteration
  instead of per-chunk.

### Added

- `MappedSlice<'a>` public wrapper type: derefs to `[u8]`,
  implements `AsRef<[u8]>`, `Debug`, `PartialEq` (against itself,
  `[u8]`, `&[u8]`, `[u8; N]`, `&[u8; N]`). Re-exported from the
  crate root.
- `MemoryMappedFile::chunks_owned(chunk_size)` returns
  `ChunkIteratorOwned<'_>` yielding `Result<Vec<u8>>`. Migration
  aid for callers that need owned chunks.
- `MemoryMappedFile::pages_owned()` returns `PageIteratorOwned<'_>`.
  Same as `chunks_owned` but page-sized.
- `benches/mmap_bench.rs` workload-pattern benches:
  - `sequential_read` at 1 MiB / 16 MiB / 256 MiB (`as_slice` vs
    `read_into`)
  - `random_read` with hand-rolled xorshift64 PRNG (no new dep) at
    64 B / 256 B / 4 KiB / 64 KiB request sizes
  - `sequential_write` under `Manual` / `EveryBytes(64 KiB)` /
    `EveryMillis(10)` (post-C2)
  - `iterator_throughput` at 4 KiB / 64 KiB chunks plus pages,
    comparing zero-copy `chunks()` to `chunks_owned()` to
    show the H1 win
  - `touch_pages_large` on 1 GiB (post-H2)
  - `atomic_contention` across 1 / 2 / 4 / 8 threads with
    `fetch_add` on a shared `AtomicU64`
- `.github/workflows/bench-regression.yml` runs the full bench
  suite on every push and PR, uploads the criterion JSON as an
  artifact for diffing against the checked-in baseline. The
  10%-regression hard-fail gate is deferred to 0.9.10 per
  ROADMAP; this workflow is the data plumbing.

### Performance

- **(H2)** `touch_pages` / `touch_pages_range` rewritten to acquire
  the underlying lock (RW) or base pointer (RO/COW) ONCE per call
  and walk pages in a tight `ptr::read_volatile` loop wrapped in
  `std::hint::black_box`. Previously each page took a separate
  `read_into(offset, &mut [0u8; 1])` call, which acquired the
  lock, validated bounds, and memcpy'd a byte. Expected speedup
  on a 1 GiB file: ~50-100x. Captured under the
  `bench_touch_pages_large` group.
- **(H1)** Iterator zero-copy: see "Changed" above. The yielded
  `MappedSlice<'a>` borrows from the mapping directly with no
  allocation and no per-chunk memcpy.
- **(audit E4 follow-on)** `chunks_mut().for_each_mut(...)` now
  acquires the write guard once for the entire iteration instead
  of per-chunk. Other writers and readers see the same total
  blocked window they did before; the change only eliminates the
  per-chunk lock-acquire overhead inside the iteration.

### Documentation

- `docs/API.md`: `as_slice` examples updated to reflect the
  unified `MappedSlice<'_>` return; iterator examples updated to
  zero-copy form; new sections call out `chunks_owned` /
  `pages_owned` as the migration path; install snippets bumped to
  0.9.7.
- `REPS.md` section 4 reflects the new return types and adds
  `MappedSlice<'a>` to the public surface.

### Internals

- `tests/feature_integration.rs`, `tests/proptest_bounds.rs`,
  `tests/segment_after_resize.rs`, `tests/basic.rs`,
  `tests/platform_parity.rs` all updated for the new API. The
  obsolete `as_slice_rw_invalid_mode` property test was rewritten
  to verify the new `as_slice` succeeds on RW.

<br>

<!-- VERSION: 0.9.6 -->
## [0.9.6] - 2026-05-12

### Added

- `proptest` 1.5 added to `[dev-dependencies]` (default-features off;
  `std`, `bit-set`, `fork`, `timeout` opted in). Holds MSRV 1.75.
- `tests/proptest_bounds.rs` exercises bounds-checking on `as_slice`,
  `as_slice_mut`, `read_into`, `update_region`, and `flush_range`
  across random `(offset, len)` pairs and explicit boundary picks
  (off-by-one, wrapping-add overflow, zero-length at end, etc.).
- `tests/proptest_atomic.rs` exercises alignment + bounds on
  `atomic_u32`, `atomic_u64`, `atomic_u32_slice`, and
  `atomic_u64_slice`. Verifies the correct `Misaligned` /
  `OutOfBounds` variant fires for every misaligned or out-of-range
  offset and that aligned in-bounds slots round-trip via
  store/load.
- `tests/proptest_flush.rs` exercises `FlushPolicy` state transitions
  including the C1 regression scenario under random mixed
  `update_region` + `flush_range` sequences, plus `EveryWrites` and
  `Manual` policies.
- Each property test runs at least 1,024 cases per property by
  default; set `PROPTEST_CASES=10000` for the deep sweep run before
  releases.

### Fixed

- CI: `tests/atomic_view_resize_safety.rs` (added in 0.9.5) now
  carries the `#![cfg(feature = "atomic")]` crate gate. The matrix
  CI runs `cargo test --no-default-features --features "<combo>"`
  across feature subsets that exclude `atomic`; before the gate the
  test failed to compile under every such combination. The
  `full-build` job (all-features) masked it during the 0.9.5 cycle.

### Changed

- CI: bumped `actions/checkout@v4` to `actions/checkout@v5` across
  `.github/workflows/CI.yml` (4 occurrences). The v4 action runs on
  Node 20, which GitHub deprecated on 2025-09-19 (forced to Node 24
  starting 2026-06-02; removed 2026-09-16). v5 supports Node 24
  natively. No behavior change in the workflow itself.

### Documentation

- `docs/SAFETY.md` added: the authoritative catalog of every
  `unsafe` block in the crate, grouped by category (mapping
  construction, advise, locking, atomic views, flush, platform
  shims, test helpers), with the invariants each block relies on
  and citations to the relevant man page / MSDN page.
- Every `unsafe` block in `src/advise.rs`, `src/lock.rs`, and the
  non-atomic paths of `src/mmap.rs` (open / create / resize / COW /
  hugepages / msync) now has a `// SAFETY:` comment that states the
  invariants the syscall requires, demonstrates how local context
  establishes them, and cites the platform spec. Closes audit
  findings **S2** and **S3**.
- The two `libc::utime` test helpers in `src/watch.rs` (gated on
  `#[cfg(test)]`) now have explicit SAFETY comments citing
  POSIX `utime(2)`.
- `docs/API.md`: corrected MSRV to 1.75; version examples bumped to
  0.9.6; atomic return types updated to reflect the C3 wrapper
  types (`AtomicView<'_, T>` / `AtomicSliceView<'_, T>`); stray
  character removed from the `SegmentMut` section; added 0.9.5 and
  0.9.6 entries to the Version History.
- `REPS.md` section 4 updated to match the actual implementation:
  `TouchHint` variants (`Never`, `Eager`, `Lazy`); `as_slice_mut`
  return type (`MappedSliceMut<'_>`); atomic API returns wrapper
  types and includes `atomic_u32_slice`; locking API matches
  `lock`/`unlock`/`lock_all`/`unlock_all`; `MmapAdvice::advise`
  signature carries `(offset, len, advice)`; `ChangeKind` variants
  align with the polling implementation (`Modified`/`Metadata`/
  `Removed`).

<br>

<!-- VERSION: 0.9.5 -->
## [0.9.5] - 2026-05-12

### Fixed

- **(C1)** `flush_range` no longer zeros the global dirty-byte
  accumulator after a partial-range flush. Previously, calling
  `flush_range` on a sub-region would clear the accumulator entirely,
  silently breaking `FlushPolicy::EveryBytes` for any caller that
  mixed `update_region` with `flush_range`. The accumulator is now
  debited by the actual flushed length (clamped at zero), so the
  policy correctly tracks unflushed pages. Regression test:
  `tests/flush_range_accumulator.rs`. See `.dev/AUDIT.md` C1.
- **(C2)** `FlushPolicy::EveryMillis` actually triggers automatic
  flushes. The previous implementation created a dangling
  `Weak::new()` and discarded the `TimeBasedFlusher` value, leaving
  the policy as a silent no-op despite the test
  `flush_policy_interval_is_manual_now` documenting it as
  intentional. The flusher is now stored on `Inner` with a real
  `Arc::downgrade` weak reference and a shutdown signal so the
  worker thread exits cleanly on drop. Regression tests:
  `tests/time_based_flush.rs`. The misnamed test in `tests/basic.rs`
  was rewritten to verify the actual fixed behavior. See
  `.dev/AUDIT.md` C2.
- **(H5)** `WatchHandle::drop` now signals the polling thread to
  exit via an `AtomicBool` shutdown flag. Previously, dropping the
  handle did nothing and the background thread continued polling
  indefinitely (until the watched file was deleted), causing a
  per-call thread leak. See `.dev/AUDIT.md` H5.
- **(H6)** `Segment::as_slice` and `SegmentMut::as_slice_mut` /
  `SegmentMut::write` now re-validate bounds on every call instead
  of relying on the construction-time check. Parent mappings can be
  resized between segment construction and use, so the previous
  "validated in constructor" claim was misleading. New helper
  `is_valid()` lets callers check cheaply without paying for an
  access. Regression test: `tests/segment_after_resize.rs`. See
  `.dev/AUDIT.md` H6.
- Removed three unused imports (`std::mem` and `std::ptr` in
  `advise.rs`, `std::ptr` in `lock.rs`) that triggered clippy
  warnings.
- Fixed `examples/critical_features_demo.rs` to not use
  `std::io::ErrorKind::IsADirectory` (stable since 1.83) so the
  example compiles on MSRV 1.75.
- Added clippy allow attributes for intentional uses of
  `Permissions::set_readonly(false)` in Windows test paths.

### Changed

- **(C3, BREAKING)** Atomic-view methods (`atomic_u64`, `atomic_u32`,
  `atomic_u64_slice`, `atomic_u32_slice`) now return wrapper types
  `AtomicView<'_, T>` and `AtomicSliceView<'_, T>` instead of bare
  references `&AtomicU64` etc. The wrappers implement `Deref`, so
  call sites that do `view.fetch_add(...)`, `slice.iter()`, etc.,
  keep working. The change fixes a use-after-free unsoundness: the
  old API released the read lock before returning the reference,
  letting a concurrent `resize()` unmap the memory under a live
  view. The wrappers now hold the read guard for the view's
  lifetime, so `resize()` blocks while any view is alive.

  **Migration**: callers must drop the view before calling
  `resize()` on the same mapping from the same thread (otherwise
  that thread self-deadlocks because the view holds the read lock
  and `resize()` needs the write lock). Pattern: take the view in
  a tight scope or call `drop(view)` explicitly before subsequent
  write operations. Regression tests:
  `tests/atomic_view_resize_safety.rs`. See `.dev/AUDIT.md` C3.
- MSRV dropped from 1.76 to 1.75 (no source changes required).
- Repository ownership transferred from `asotex/mmap-io` to
  `jamesgober/mmap-io`.
- README rewritten to remove Asotex branding and fix outdated API
  example.
- CHANGELOG header rebranded.
- `docs/API.md` header replaced with the standard `jamesgober`
  Triple Hexagon header to match `docs/README.md`. Footer reduced to
  a simple license-only copyright. All Asotex brand links and the
  `Copyright (c) 2025 Asotex Inc.` line removed.

### Performance

- **(H7)** `utils::page_size()` is now cached in a
  `OnceLock<usize>`, eliminating a `sysconf` / `GetSystemInfo`
  syscall on every call. Hot paths affected: `flush_range`
  microflush optimization, `touch_pages`, `touch_pages_range`, and
  the page-iterator. See `.dev/AUDIT.md` H7.

### Documentation

- Added `clippy.toml` with MSRV pin and breaking-API guard.
- Added `REPS.md` (project specification) covering design
  principles, module structure, public API surface, safety contract,
  MSRV policy, performance contract, stability guarantees,
  dependency policy, testing requirements, and out-of-scope items.
- Added `.dev/DIRECTIVES.md` (project standards: identity, language
  rules, code style, build matrix, CI policy, commit and release
  rules, test discipline, documentation discipline, banned-words
  enforcement, AI dev workflow).
- Added `.dev/AUDIT.md` (deep-dive audit findings catalog that
  drove this release's bug fixes).
- Added `.dev/ROADMAP.md` (precise milestone path covering `0.9.5`
  correctness bugfix release, `0.9.6` unsafe audit and property
  tests, `0.9.7` performance telemetry and iterator zero-copy
  redesign, `0.9.8` async polish, `0.9.9` native watch backends and
  ergonomic adds, `0.9.10` pre-1.0 stabilization, `1.0.0-rc.1`
  release candidate, `1.0.0` stable, and long-term post-1.0 work).
- Added `.dev/PROMPTS.md` (ready-to-paste bootstrap prompts for
  handing each roadmap milestone to an AI agent, plus generic
  prompts for CHANGELOG management and safety review). `.dev/` is
  gitignored.

### Known issues

- Two polling-based file-watch tests (`test_watch_file_changes`,
  `test_multiple_watchers`) and one integration test
  (`test_all_features_integration`) remain
  `#[cfg_attr(windows, ignore)]`. Windows mtime granularity makes
  polling-based change detection flaky without native
  `ReadDirectoryChangesW` integration. Tracked for `0.9.9` per
  `.dev/ROADMAP.md`.

<br>

## [0.9.4] - 2025-08-20

### Added

- Final update and publish under previous ownership.

<br>

<!-- VERSION: 0.9.3 -->
## [0.9.3] - 2025-08-20

### Added

- **Touch Pages Feature**: Added `touch_pages()` and `touch_pages_range()`
  methods to prewarm memory pages, eliminating page faults for
  benchmarking and performance-critical sections.
- **Page Fault Cost Benchmarks**: Added benchmarks to investigate
  allocator/page fault costs at 4K-64K block sizes.
- **Microflush Optimization Benchmarks**: Added benchmarks to measure
  microflush overhead and optimization effectiveness.
- **Time-Based Flushing**: Implemented `FlushPolicy::EveryMillis` with
  background thread for automatic time-based flushing.
- **Enhanced Flush Range Optimization**: Improved `flush_range()` with
  microflush detection and page-aligned batching for sub-page-size ranges.
- **Real Huge Page Retention**: Enhanced huge pages implementation with
  multi-tier approach (optimized mapping + THP + fallback).
- **TouchHint::Eager Option**: Added `TouchHint` enum with `Eager` option
  for pre-touching pages during creation, useful for benchmarking.
- **Fallback Documentation**: Clearly documented that `.huge_pages(true)`
  does not guarantee huge pages and the fallback behavior.

### Enhanced

- **Flush Performance**: Optimized microflush operations by expanding
  small ranges to page boundaries, reducing syscall overhead.
- **Benchmarking Suite**: Added `bench_touch_pages`,
  `bench_page_fault_costs`, and `bench_microflush_overhead` benchmarks.
- **Memory Management**: Enhanced page prewarming for better performance
  predictability.

### Performance

- **Microflush Optimization**: Ranges smaller than page size are now
  page-aligned for better cache locality and reduced syscall overhead.
- **Page Fault Elimination**: `touch_pages` API allows prewarming memory
  to eliminate page faults in critical sections.
- **Time-Based Flushing**: Background thread handles automatic flushing
  at configurable intervals.

### Developer experience

- **Benchmarks**: Detailed benchmarks comparing cold vs warm page
  performance across different block sizes.
- **Production Features**: All new features designed for high-performance,
  energy efficiency, and predictable behavior.

<br>

<!-- VERSION: 0.9.0 -->
## [0.9.0] - 2025-08-06

### Fixed

- Critical issues in `atomic.rs`.
- Critical issues in `mmap.rs`.
- Performance issues in `mmap.rs`.
- Efficiency issues in `iterator.rs`.
- Efficiency issues in `segment.rs`.
- Code quality in `watch.rs`.
- Code quality in `mmap.rs`.

<br>

<!-- VERSION: 0.8.0 -->
## [0.8.0] - 2025-08-06

### Added

- `hugepages` flag to `Cargo.toml` features.
- `Huge Pages` feature.
- Test case for `Huge Pages`.
- `Async-Only Flushing` support.
- `async_flush.rs` file for `Async-Only Flushing` support.
- Test case for `Async-Only Flushing`.
- `Platform Parity` support.
- Test case for `Platform Parity`.
- `Huge Pages`, `Async-Only Flushing`, and `Platform Parity` documentation
  in `API.md`.
- `Huge Pages`, `Async-Only Flushing`, and `Platform Parity` documentation
  in `README.md`.
- Smarter internal guards for `flush()`.

### Changed

- `Optional Features` in `README.md` to include `hugepages` flag.
- `Features` in `API.md` to include `hugepages` flag.

### Fixed

- Performance issues and errors in `watch.rs`.
- Performance issues and errors in `mmap.rs`.

<br>

<!-- VERSION: 0.7.5 -->
## [0.7.5] - 2025-08-06

### Added

- Benchmark added to `Cargo.toml`.
- Benchmark functionality created.
- `FlushPolicy` via `flush.rs`.
- Test case for `FlushPolicy`.

### Changed

- Extended `MmapFile` in `mmap.rs` to store the `flush_policy`.

### Fixed

- Fix build error (Windows) `[cannot find value 'current']` in `mmap.rs`.

<br>

<!-- VERSION: 0.7.3 -->
## [0.7.3] - 2025-08-06

### Changed

- Changed the header for `CHANGELOG.md`.

### Fixed

- Fixed build error in `mmap.rs`.
- Fixed build error in `advise.rs`.
- Fixed deprecated command in `ci.yml`.
- Fixed warning in `mmap.rs`.

<br>

<!-- VERSION: 0.7.2 -->
## [0.7.2] - 2025-08-05

### Added

- README now includes `Optional Features`.
- README now includes `Default Features`.
- README now includes `Example Usage`.
- README now includes `Safety Notes`.
- API Documentation now includes `Safety and Best Practices` section.
- This CHANGELOG.
- README now links to CHANGELOG.
- API Documentation now links to CHANGELOG.

### Changed

- Updated Cargo default features.
- Updated GitHub Actions (CI) to include basic test build with all features.

<br>

<!-- VERSION: 0.7.1 -->
## [0.7.1] - 2025-08-05

### Added

- Copy-On-Write feature.
- Advice feature.
- Iterator feature.
- Atomic feature.
- Locking feature.
- Watch feature.
- Cargo available features.
- API documentation.
- GitHub Actions (CI) test build.

### Changed

- Updated README.

<br>

<!-- VERSION: 0.2.0 -->
## [0.2.0] - 2025-08-05

### Added

- Initial APIs.
- Async support with Tokio.
- Basic README.

<!-- LINK REFERENCE -->
[Unreleased]: https://github.com/jamesgober/mmap-io/compare/v1.0.0...HEAD
[1.0.0]: https://github.com/jamesgober/mmap-io/compare/v0.9.11...v1.0.0
[0.9.11]: https://github.com/jamesgober/mmap-io/compare/v0.9.10...v0.9.11
[0.9.10]: https://github.com/jamesgober/mmap-io/compare/v0.9.9...v0.9.10
[0.9.9]: https://github.com/jamesgober/mmap-io/compare/v0.9.8...v0.9.9
[0.9.8]: https://github.com/jamesgober/mmap-io/compare/v0.9.7...v0.9.8
[0.9.7]: https://github.com/jamesgober/mmap-io/compare/v0.9.6...v0.9.7
[0.9.6]: https://github.com/jamesgober/mmap-io/compare/v0.9.5...v0.9.6
[0.9.5]: https://github.com/jamesgober/mmap-io/compare/v0.9.4...v0.9.5
[0.9.4]: https://github.com/jamesgober/mmap-io/compare/v0.9.3...v0.9.4
[0.9.3]: https://github.com/jamesgober/mmap-io/compare/v0.9.0...v0.9.3
[0.9.0]: https://github.com/jamesgober/mmap-io/compare/v0.8.0...v0.9.0
[0.8.0]: https://github.com/jamesgober/mmap-io/compare/v0.7.5...v0.8.0
[0.7.5]: https://github.com/jamesgober/mmap-io/compare/v0.7.3...v0.7.5
[0.7.3]: https://github.com/jamesgober/mmap-io/compare/v0.7.2...v0.7.3
[0.7.2]: https://github.com/jamesgober/mmap-io/compare/0.7.1...v0.7.2
[0.7.1]: https://github.com/jamesgober/mmap-io/compare/0.2.0...0.7.1
[0.2.0]: https://github.com/jamesgober/mmap-io/releases/tag/0.2.0
