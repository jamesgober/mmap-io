# mmap-io fuzz targets

`cargo-fuzz` targets for the read/write/atomic/bounds-check paths.

## Why a separate crate?

`cargo-fuzz` requires a dedicated workspace-isolated package because
the libFuzzer runtime linkage conflicts with normal Rust binaries.
This crate is not published to crates.io (`publish = false`) and is
not part of the workspace; it exists only to be driven by the
`cargo-fuzz` CLI.

## Prerequisites

- Linux or macOS (Windows is not supported by `libfuzzer-sys`; use
  WSL on Windows)
- A nightly Rust toolchain (`rustup install nightly`)
- The `cargo-fuzz` binary: `cargo install cargo-fuzz`

## Targets

| Target           | What it exercises                                         |
|------------------|-----------------------------------------------------------|
| `read_into`      | `MemoryMappedFile::read_into` with random offsets / sizes |
| `update_region`  | `update_region` + `flush_range` with random payloads      |
| `atomic_view`    | `atomic_u32`/`u64`/`u32_slice`/`u64_slice` alignment edges |
| `bounds_checks`  | `ensure_in_bounds` and `slice_range` directly             |

## Running

From the repository root:

```sh
cd fuzz
cargo +nightly fuzz run read_into       -- -runs=1000000
cargo +nightly fuzz run update_region   -- -runs=1000000
cargo +nightly fuzz run atomic_view     -- -runs=1000000
cargo +nightly fuzz run bounds_checks   -- -runs=1000000
```

`-runs=1000000` is a reasonable smoke pass (~1-2 minutes per
target). For deeper coverage, run with `-runs=0` (until
interrupted) overnight on a dedicated machine. The ROADMAP target
for 0.9.10 is a one-hour run per target before tagging.

## Contract under test

Every public bounds-checked method has the same minimal contract:
**never panic, never trigger UB.** Either succeed and return a
well-defined result, or return `MmapIoError::{OutOfBounds,
Misaligned, InvalidMode, ...}`. The fuzzers verify this by feeding
random / adversarial inputs and treating any panic, segfault, or
sanitizer trip as a corpus-worthy crash.

## Reproducing a crash

If a fuzzer reports a crash, the offending input is saved to
`fuzz/artifacts/<target>/crash-<hash>`. Reproduce locally:

```sh
cargo +nightly fuzz run <target> fuzz/artifacts/<target>/crash-<hash>
```

Add the crash input to `fuzz/corpus/<target>/` so future runs catch
the regression even if the codebase changes around it.
