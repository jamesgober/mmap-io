# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed

- MSRV dropped from 1.76 to 1.75 (no source changes required).
- Repository ownership transferred from `asotex/mmap-io` to `jamesgober/mmap-io`.
- README rewritten to remove Asotex branding and fix outdated API example.
- CHANGELOG header rebranded.

### Fixed

- Removed three unused imports (`std::mem` and `std::ptr` in `advise.rs`,
  `std::ptr` in `lock.rs`) that triggered clippy warnings.
- Fixed `examples/critical_features_demo.rs` to not use
  `std::io::ErrorKind::IsADirectory` (stable since 1.83) so the example
  compiles on MSRV 1.75.
- Added clippy allow attributes for intentional uses of
  `Permissions::set_readonly(false)` in Windows test paths.

### Documentation

- Added `clippy.toml` with MSRV pin and breaking-API guard.

### Known issues

- Two polling-based file-watch tests (`test_watch_file_changes`,
  `test_multiple_watchers`) and one integration test
  (`test_all_features_integration`) are marked
  `#[cfg_attr(windows, ignore)]`. Windows mtime granularity makes
  polling-based change detection flaky without native
  `ReadDirectoryChangesW` integration. Tracked for a future release.

<br>

<!-- VERSION: 0.9.4 -->
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
[Unreleased]: https://github.com/jamesgober/mmap-io/compare/v0.9.4...HEAD
[0.9.4]: https://github.com/jamesgober/mmap-io/compare/v0.9.3...v0.9.4
[0.9.3]: https://github.com/jamesgober/mmap-io/compare/v0.9.0...v0.9.3
[0.9.0]: https://github.com/jamesgober/mmap-io/compare/v0.8.0...v0.9.0
[0.8.0]: https://github.com/jamesgober/mmap-io/compare/v0.7.5...v0.8.0
[0.7.5]: https://github.com/jamesgober/mmap-io/compare/v0.7.3...v0.7.5
[0.7.3]: https://github.com/jamesgober/mmap-io/compare/v0.7.2...v0.7.3
[0.7.2]: https://github.com/jamesgober/mmap-io/compare/0.7.1...v0.7.2
[0.7.1]: https://github.com/jamesgober/mmap-io/compare/0.2.0...0.7.1
[0.2.0]: https://github.com/jamesgober/mmap-io/releases/tag/0.2.0
