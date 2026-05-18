# Migrating from 0.9.x to 1.0.0

`mmap-io 1.0.0` is **API-compatible with `0.9.11`**. Code that compiles against `0.9.11` compiles against `1.0.0` without changes.

This guide covers two scenarios:

1. **You're already on `0.9.11`**: Bump the version, you're done. See [§ Upgrading from 0.9.11](#upgrading-from-0911).
2. **You're on `0.9.6` or earlier and skipped `0.9.7`-`0.9.11`**: You need to address the 0.9.7 signature change. See [§ Upgrading from 0.9.6 or earlier](#upgrading-from-096-or-earlier).

For users coming from versions `0.9.7` through `0.9.10`: the 0.9.11 compat shims (`as_slice_bytes`, `for_each_mut_legacy`, `chunks_owned`) are preserved in 1.0.0. No migration required beyond bumping the version.

---

## Upgrading from 0.9.11

```toml
[dependencies]
- mmap-io = "0.9"
+ mmap-io = "1.0"
```

That's it. No source changes needed.

### New surface available in 1.0.0

You can optionally adopt these additions:

| Addition | Use case |
|----------|----------|
| `AnonymousMmap::new(size)` | Process-local memory without a backing file. Shared scratch buffers between threads, large temporary allocations, kernel-side IPC substrate. |
| `mmap.is_hugepage_backed()` | Confirm whether the kernel actually backed a mapping with huge pages (Linux). Returns `Option<bool>`. |

Both are purely additive; existing code does not need to use them.

### What the version bump itself signals

`1.0.0` commits to API stability under SemVer. From this point on:

- Breaking changes require a major-version bump (`2.0.0`).
- Additive features ship as minor bumps (`1.1.0`, `1.2.0`).
- Bug fixes and internal changes ship as patch bumps (`1.0.1`, `1.0.2`).

CI enforces this via `cargo-semver-checks` and the new `cargo-public-api` diff workflow. Any PR that changes the public API surface must update the committed `public-api.txt` snapshot, surfacing intent in code review.

---

## Upgrading from 0.9.6 or earlier

`0.9.7` shipped three signature changes as a patch release. This was a SemVer violation (the `^0.9.6` resolver constraint did not protect callers). It carried for four releases (`0.9.7` through `0.9.10`) before `0.9.11` introduced compat shims, and `1.0.0` preserves them.

Pick the path that fits your code:

### Path A: minimal-edit recovery (compat shims)

Each broken call site needs a one-method-name rename:

| 0.9.6 call                                                | 1.0.0 drop-in replacement |
|-----------------------------------------------------------|---------------------------|
| `mmap.as_slice(off, len)?` returning `&[u8]`              | `mmap.as_slice_bytes(off, len)?` |
| `mmap.chunks(N)` yielding `Result<Vec<u8>>`               | `mmap.chunks_owned(N)` |
| `for_each_mut(...)` with `Result<Result<(), E>>`          | `for_each_mut_legacy(...)` |

These shims match the 0.9.6 signatures exactly. They are not deprecated; we will keep them indefinitely for stability.

```rust
// 0.9.6 code
let slice: &[u8] = mmap.as_slice(0, 16)?;

// 1.0.0 minimal-edit fix
let slice: &[u8] = mmap.as_slice_bytes(0, 16)?;
```

### Path B: zero-copy migration (recommended for new code)

The current `as_slice`, `chunks`, and `for_each_mut` are zero-copy and faster. Migrating to them removes per-chunk allocations and is the path the rest of the crate's surface is designed around.

```rust
// 0.9.6 code (allocating)
let slice: &[u8] = mmap.as_slice(0, 16)?;
let bytes_owned: Vec<u8> = mmap.chunks(4096).next().unwrap()?;

// 1.0.0 recommended (zero-copy)
let slice: MappedSlice<'_> = mmap.as_slice(0, 16)?;
let bytes: MappedSlice<'_> = mmap.chunks(4096).next().unwrap()?;
// Both deref to &[u8], so existing &[u8]-consuming code "just works"
// when the call sites are adjusted to bind through Deref.
```

`MappedSlice` and `MappedSliceMut` implement `Deref<Target = [u8]>` and `AsRef<[u8]>`, so anywhere the 0.9.6 code passed `&[u8]` to a function, the new types pass through unchanged. The difference is at the binding site: hold the `MappedSlice<'_>` for the duration of its use, then let it drop.

### `for_each_mut` flattening

The 0.9.6 `for_each_mut` signature had a triple-nested `Result<Result<(), E>>` shape:

```rust
// 0.9.6
let r: Result<Result<(), io::Error>, MmapIoError> =
    mmap.chunks_mut(4096).for_each_mut(|off, c| Ok::<(), io::Error>(()));
r??;  // double unwrap
```

The current version flattens to `Result<()>` with the closure returning the crate's own `Result`:

```rust
// 1.0.0 recommended
mmap.chunks_mut(4096).for_each_mut(|_off, c| {
    c.fill(0);
    Ok(())
})?;
```

For codebases that need to keep the old shape (e.g. when carrying a foreign error type), `for_each_mut_legacy` preserves the 0.9.6 signature exactly. The internal implementation uses the same single-held-write-guard loop as the modern path, so the perf win is preserved either way.

---

## Behavioral differences in 1.0.0

None compared to `0.9.11`. The full list of behavioral changes since `0.9.6` is documented in [CHANGELOG.md](../CHANGELOG.md).

Notable items if you skipped the `0.9.x` series:

- **Flush policies actually work.** `EveryMillis(N)` runs a background thread bound to the mapping's lifetime. Partial flushes correctly debit the byte accumulator. (`0.9.5`)
- **Atomic views are sound across resize.** `atomic_u32` / `atomic_u64` return wrapper types that hold the read lock for their lifetime; concurrent `resize` blocks until they drop. (`0.9.5`)
- **Native FS watchers.** `feature = "watch"` uses inotify / FSEvents / ReadDirectoryChangesW directly, not polling. (`0.9.9`)
- **Runtime-agnostic async.** `feature = "async"` works on tokio, smol, async-std, and any executor; no tokio runtime dependency. (`0.9.11`)
- **`bytes::Bytes` integration.** `feature = "bytes"` provides `read_bytes` and `From<MappedSlice<'_>>` conversions for the hyper/tower/tonic/axum/reqwest ecosystem. (`0.9.11`)
- **`io::Read` + `io::Seek` cursor.** `mmap.reader()` plugs the mapping into any parser expecting `R: Read`. (`0.9.11`)
- **`AsFd` / `AsRawFd` / `AsHandle` / `AsRawHandle`.** Standard OS handle trait impls for FFI bridging. (`0.9.11`)

---

## Getting help

If you hit a migration question this guide doesn't cover, open an issue at <https://github.com/jamesgober/mmap-io/issues> with the relevant 0.9.x code snippet and what you expected the 1.0.0 equivalent to look like.
