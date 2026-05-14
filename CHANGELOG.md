# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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
[Unreleased]: https://github.com/jamesgober/mmap-io/compare/v0.9.11...HEAD
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
