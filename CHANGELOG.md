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

### Added
- Benchmark added to `Cargo.toml`.
- Benchmark functionality created.
- `FlushPolicy` via `flush.rs`.
- Test case for `FlushPolicy`.

### Changed
- Extended `MmapFile` in `mmap.rs` to store the `flush_policy`.

### Fix
 - Fix Build Error (Windows)[cannot find value `current`] in `mmap.rs`.


<br>


<!-- VERSION: 0.7.5 
## [0.7.5] - 2025-08-06
-->

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
[unreleased]: https://github.com/asotex/mmap-io/compare/v0.7.3...HEAD

[0.8.1]: https://github.com/asotex/mmap-io/compare/v0.8.0...v0.8.1
[0.8.0]: https://github.com/asotex/mmap-io/compare/v0.7.8...v0.8.0
[0.7.6]: https://github.com/asotex/mmap-io/compare/v0.7.5...v0.7.8
[0.7.5]: https://github.com/asotex/mmap-io/compare/v0.7.3...v0.7.5

[0.7.3]: https://github.com/asotex/mmap-io/compare/v0.7.2...v0.7.3
[0.7.2]: https://github.com/asotex/mmap-io/compare/0.7.1...v0.7.2
[0.7.1]: https://github.com/asotex/mmap-io/compare/0.2.0...0.7.1
[0.2.0]: https://github.com/asotex/mmap-io/releases/tag/0.2.0