<div align="center">
    <picture>
        <source media="(prefers-color-scheme: dark)" srcset="https://raw.githubusercontent.com/asotex/.github/refs/heads/main/media/asotex-icon-white.png">
        <img width="81px" alt="Asotex brand logo, featuring the Asotex A-Icon, followed by the word Asotex." src="https://raw.githubusercontent.com/asotex/.github/refs/heads/main/media/asotex-icon-dark.png">
    </picture>
    <h1>CHANGELOG</h1>
</div>
<br>

All notable changes to this project will be documented in this file.  

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).


## [Unreleased]


<br>

<!-- VERSION: 0.9.4 -->
## [0.9.4] - 2025-08-20

### Added
- Final update and publish.


<br>

<!-- VERSION: 0.9.3 -->
## [0.9.3] - 2025-08-20
### Added
- **Touch Pages Feature**: Added `touch_pages()` and `touch_pages_range()` methods to prewarm memory pages, eliminating page faults for benchmarking and performance-critical sections
- **Page Fault Cost Benchmarks**: Added comprehensive benchmarks to investigate allocator/page fault costs at 4K-64K block sizes
- **Microflush Optimization Benchmarks**: Added benchmarks to measure microflush overhead and optimization effectiveness
- **Time-Based Flushing**: Implemented `FlushPolicy::EveryMillis` with background thread for automatic time-based flushing
- **Enhanced Flush Range Optimization**: Improved `flush_range()` with microflush detection and page-aligned batching for sub-page-size ranges
- **Real Huge Page Retention**: Enhanced huge pages implementation with multi-tier approach (optimized mapping + THP + fallback)
- **TouchHint::Eager Option**: Added `TouchHint` enum with `Eager` option for pre-touching pages during creation, ideal for benchmarking
- **Comprehensive Fallback Documentation**: Clearly documented that `.huge_pages(true)` does not guarantee huge pages and fallback behavior

### Enhanced
- **Flush Performance**: Optimized microflush operations by expanding small ranges to page boundaries, reducing syscall overhead
- **Benchmarking Suite**: Added `bench_touch_pages`, `bench_page_fault_costs`, and `bench_microflush_overhead` benchmarks
- **Memory Management**: Enhanced page prewarming capabilities for better performance predictability

### Performance Improvements
- **Microflush Optimization**: Ranges smaller than page size are now page-aligned for better cache locality and reduced syscall overhead
- **Page Fault Elimination**: New touch_pages API allows prewarming memory to eliminate page faults in critical sections
- **Time-Based Flushing**: Background thread handles automatic flushing at configurable intervals

### Developer Experience
- **Comprehensive Benchmarks**: Added detailed benchmarks comparing cold vs warm page performance across different block sizes
- **Production-Ready Features**: All new features designed with high-performance, energy efficiency, and robustness in mind



<br>

<!-- VERSION: 0.9.0 -->
## [0.9.0] - 2025-08-06

### Fixed
- Critical Issues in `atomic.rs`.
- Critical Issues in `mmap.rs`.
- Performance Issues in `mmap.rs`.
- Efficiency Issues in `iterator.rs`.
- Efficiency Issues in `segment.rs`.
- Code Quality in `watch.rs`.
- Code Quality in `mmap.rs`.


<br>


<!-- VERSION: 0.8.0 -->
## [0.8.0] - 2025-08-06

### Added
- `hugepages` Flag to `Cargo.toml` Features.
- `Huge Pages` Feature.
- Test case for `Huge Pages`.
- `Async-Only Flushing` support.
- `async_flush.rs` file for `Async-Only Flushing` support.
- Test case for `Async-Only Flushing`.
- `Platform Parity` support.
- Test case for `latform Parity`.
- `Huge Pages`, `Async-Only Flushing`, & `Platform Parity` documentation to `API.md`.
- `Huge Pages`, `Async-Only Flushing`, & `Platform Parity` documentation to `README.md`.
- Smarter internal guards for `flush()`.

### Changed
- `Optional Features` in `README.md` to include `hugepages` Flag.
- `Features` in `API.md` to include `hugepages` Flag.

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
 - Fix Build Error (Windows)[cannot find value `current`] in `mmap.rs`.


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
- Updated Cargo Default Features.
- Updated GitHub Actions (CI) to include basic test build with all features.


<br>


<!-- VERSION: 0.7.1 -->
## [0.7.1] - 2025-08-05

### Added
- Copy-On-Write Feature.
- Advice Feature.
- Iterator Feature.
- Atomic Feature.
- Locking Feature.
- Watch Feature.
- Cargo Available Features.
- API Documentation.
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
[unreleased]: https://github.com/asotex/mmap-io/compare/v0.9.4...HEAD
[0.9.4]: https://github.com/asotex/mmap-io/compare/v0.9.3...v0.9.4
[0.9.3]: https://github.com/asotex/mmap-io/compare/v0.9.0...v0.9.3
[0.9.0]: https://github.com/asotex/mmap-io/compare/v0.8.0...v0.9.0
[0.8.0]: https://github.com/asotex/mmap-io/compare/v0.7.5...v0.8.0
[0.7.5]: https://github.com/asotex/mmap-io/compare/v0.7.3...v0.7.5
[0.7.3]: https://github.com/asotex/mmap-io/compare/v0.7.2...v0.7.3
[0.7.2]: https://github.com/asotex/mmap-io/compare/0.7.1...v0.7.2
[0.7.1]: https://github.com/asotex/mmap-io/compare/0.2.0...0.7.1
[0.2.0]: https://github.com/asotex/mmap-io/releases/tag/0.2.0