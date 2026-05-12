# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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
[Unreleased]: https://github.com/jamesgober/mmap-io/compare/v0.9.5...HEAD
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
