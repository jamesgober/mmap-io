# mmap-io - Safety Contract

This document is the authoritative description of every `unsafe`
block in the `mmap-io` source tree, grouped by category. It complements
the per-block `// SAFETY:` comments in source. Reviewers and curious
users should read this file alongside the code; the comments at each
`unsafe` block are the source of truth, this document explains the
shape and rationale.

The crate's public API is safe. Every `unsafe` block lives below the
public surface, behind one of the guarantees listed here. We do not
expose raw pointers or unsafe constructors at the user-facing API.

## Categories

### 1. File mapping construction (`src/mmap.rs`)

`memmap2::Mmap::map` and `memmap2::MmapMut::map_mut` are `unsafe`
because the operating system does not prevent another process from
concurrently modifying the backing file while a mapping is alive. In
Rust's memory model, that would be a data race if any thread in this
process held a Rust reference into the mapped region.

`mmap-io` handles this in two ways:

1. **Intra-process aliasing is sound.** Mutable access to a writable
   mapping goes through `parking_lot::RwLock<MmapMut>`, so the
   standard read-many/write-one invariants hold within the process.
2. **Inter-process aliasing is out-of-scope.** REPS.md section 5.1
   documents that callers sharing a file across processes are
   responsible for synchronization. The mapping's bytes are still
   visible to other processes; the crate does not pretend otherwise.

This category covers:

- `MemoryMappedFile::create_rw` (line ~194)
- `MemoryMappedFile::open_ro` (line ~222)
- `MemoryMappedFile::open_rw` (line ~256)
- `MemoryMappedFile::resize` (line ~628): additional invariant; the
  old mapping is not dropped until the write guard has been acquired,
  so live atomic views (C3 fix) prevent the swap.
- COW open (line ~910): read-only at the Rust API; aliasing trivially
  satisfied.
- Builder `create()` / `open()` paths (lines ~1118, ~1185, ~1210,
  ~1246, ~1273, ~1298): same justification as the constructors above.
- Huge-page helpers `map_mut_with_options` and
  `try_create_optimized_mapping` (lines ~810, ~835, ~849): Linux
  fallback chain when `MAP_HUGETLB` is unavailable.

Reference: [`memmap2::MmapMut::map_mut`](https://docs.rs/memmap2/latest/memmap2/struct.MmapMut.html#method.map_mut)

### 2. Memory advise (`src/advise.rs`)

`libc::madvise` (Unix) and `PrefetchVirtualMemory` (Windows) are
`unsafe` because they take a raw pointer + length pair. The crate
establishes the invariants the kernel requires:

1. The range `[addr, addr + length)` lies within a mapped region of
   the calling process. Established by `slice_range` /
   `ensure_in_bounds` against the cached file length, which the
   resize path keeps consistent with the live mapping.
2. The flag is a documented constant. Each branch of the user-facing
   `MmapAdvice` enum selects exactly one libc constant.

Neither syscall reads or writes the memory contents. They communicate
with the kernel's VM subsystem about expected access patterns.
`MADV_DONTNEED` can discard anonymous pages, but our mappings are
file-backed; the next read re-faults from the file.

References:

- POSIX `madvise`: https://man7.org/linux/man-pages/man2/madvise.2.html
- Windows `PrefetchVirtualMemory`: https://learn.microsoft.com/en-us/windows/win32/api/memoryapi/nf-memoryapi-prefetchvirtualmemory

### 3. Page locking (`src/lock.rs`)

`libc::mlock` / `libc::munlock` (Unix) and `VirtualLock` /
`VirtualUnlock` (Windows) carry the same range-validity requirement
as `madvise`. The crate establishes it identically: bounds-checked via
`slice_range` against the cached file length.

Like `madvise`, these calls do not access the memory contents. They
update kernel bookkeeping that controls whether pages may be paged
out.

Failure modes the kernel reports:

- `EPERM` (no `CAP_IPC_LOCK` capability on Linux): surfaced as
  `MmapIoError::LockFailed`.
- `ENOMEM` (exceeding `RLIMIT_MEMLOCK`): same.
- `ERROR_NOT_LOCKED` on Windows `VirtualUnlock`: treated as a soft
  success (we silently allow unlocking a range that was never locked).

References:

- POSIX `mlock`: https://man7.org/linux/man-pages/man2/mlock.2.html
- Windows `VirtualLock`: https://learn.microsoft.com/en-us/windows/win32/api/memoryapi/nf-memoryapi-virtuallock

### 4. Atomic views (`src/atomic.rs`)

This category is the most subtle in the crate. The C3 audit finding
documented a use-after-free that existed before the 0.9.5 fix; the
current implementation closes it.

`atomic_u32`, `atomic_u64`, `atomic_u32_slice`, `atomic_u64_slice`
return wrapper types (`AtomicView<'a, T>`, `AtomicSliceView<'a, T>`)
that:

1. **Hold a read guard** on RW mappings for as long as the view is
   alive. This is what makes `resize()` block on the write lock while
   any view is live, which is the source of soundness against C3.
2. **Hold a lifetime tie** to the `&MemoryMappedFile` borrow on RO and
   COW mappings, which guarantees the underlying `Mmap` cannot be
   dropped while the view is alive.

The `unsafe` block that casts `*const u8` to `*const AtomicU{32,64}`
is sound because:

1. The offset is verified to satisfy the alignment requirement of the
   target type (`offset % align == 0`).
2. `AtomicU{32,64}` has the same memory layout as `u{32,64}` (same
   size, no padding), so any properly aligned byte sequence of the
   right length is a valid bit pattern. The Rust standard library
   documents this layout equivalence.
3. The pointed-to memory remains mapped for the view's lifetime per
   point (1) and (2) above.

The slice variant additionally relies on the fact that aligning the
first element implies alignment of all subsequent elements: for
`AtomicU64`, `size_of` == `align_of` == 8, so any 8-byte-aligned
offset + multiple of 8 stays 8-byte-aligned. Same for `AtomicU32` with
4.

`AtomicView` and `AtomicSliceView` carry `unsafe impl Send + Sync`
bounded on `T: Sync`. This is sound because (a) for the `Locked`
variant the read guard is `Send + Sync`, (b) for the `None` (RO/COW)
variant the lifetime tie is enough, and (c) the pointer targets
atomic memory which is `Send + Sync` by construction.

Reference: [`AtomicU64` layout](https://doc.rust-lang.org/std/sync/atomic/struct.AtomicU64.html)

### 5. Flush (`src/mmap.rs`)

`libc::msync(addr, len, MS_ASYNC)` is invoked under a held read guard
on the RW lock. The pointer is the mapping base; the length is the
cached mapping length. The read guard ensures the mapping cannot be
swapped out by `resize()` while the syscall is in flight. `MS_ASYNC`
schedules a kernel writeback and returns immediately without
accessing the memory.

This appears twice in `mmap.rs`:

- The fast-path `try_linux_async_flush` (line ~766): full flush.
- The `flush_range` Linux fast path (line ~543): range flush with
  the C1 accumulator-debit logic.

Reference: https://man7.org/linux/man-pages/man2/msync.2.html

### 6. Platform shims (`src/utils.rs`)

`windows_page_size` calls `GetSystemInfo` via `MaybeUninit`. The
soundness argument:

1. `GetSystemInfo` is a kernel32.dll export that takes a pointer to
   caller-allocated `SYSTEM_INFO` storage.
2. `MaybeUninit::<SYSTEM_INFO>::uninit().as_mut_ptr()` produces such a
   pointer with the right size and alignment.
3. The function unconditionally populates every field of the struct
   (no failure mode), so `assume_init` is sound on return.
4. The returned `dwPageSize` (a `u32`) casts losslessly to `usize` on
   every supported Windows target; page sizes are bounded above by
   64 KiB on all documented architectures.

`unix_page_size` calls `libc::sysconf(_SC_PAGESIZE)`. The `unsafe` is
required only because `sysconf` is `extern "C"`; the call has no
pointer arguments and no failure mode on supported platforms that
would produce a value requiring inspection.

References:

- `GetSystemInfo`: https://learn.microsoft.com/en-us/windows/win32/api/sysinfoapi/nf-sysinfoapi-getsysteminfo
- `sysconf`: https://man7.org/linux/man-pages/man3/sysconf.3.html

### 7. Test helpers (`src/watch.rs`)

Two `unsafe` blocks call `libc::utime(path, NULL)` to bump a file's
mtime in polling-watch tests. Both are inside `#[cfg(test)]`. The
soundness argument is trivial: the path is a valid NUL-terminated C
string held live across the call, and POSIX permits `NULL` for the
times argument to mean "use the current time."

These do not ship in the public surface and have no effect on
non-test builds.

Reference: https://man7.org/linux/man-pages/man2/utime.2.html

## Cross-cutting invariants

### Bounds invariant

Every `*ptr.add(offset)` call in `mmap-io` is preceded by either:

- `slice_range(offset, len, total)?` or
- `ensure_in_bounds(offset, len, total)?`

where `total` is the cached file length. The cached length is the
single source of truth for "current mapping size"; it is updated under
the write lock during `resize`, so callers cannot observe a length
that disagrees with the live mapping.

### Aliasing invariant

`MapVariant::Rw(RwLock<MmapMut>)` is the only writable variant. All
mutation goes through the write guard. All read-side access either:

1. Acquires the read guard explicitly (returning a wrapper that holds
   the guard, e.g. `AtomicView`, `MappedSliceMut`), or
2. Uses a raw pointer for a single kernel syscall (advise, lock,
   msync) that does not access the memory contents.

The `RwLock` provides the standard Rust aliasing guarantees within
the process. Cross-process aliasing is out-of-scope per REPS.md
section 5.1.

### Resize invariant

`resize()` holds the write guard while swapping the old `MmapMut` for
a new one. Any live `AtomicView` / `AtomicSliceView` / `MappedSliceMut`
holds a read or write guard and therefore blocks the swap until the
view is dropped. This is the C3 fix.

## Audit findings status (0.9.6)

The original audit (`.dev/AUDIT.md`) listed four safety findings,
S1 through S4:

- **S1.** `windows_page_size` missing SAFETY comment.
  **Closed in 0.9.5.** Comment now cites MSDN and walks through the
  `MaybeUninit` invariants.
- **S2.** Shallow `// SAFETY: validated bounds` comments throughout
  `advise.rs`, `lock.rs`, and the non-atomic parts of `mmap.rs`.
  **Closed in 0.9.6.** Every block now states the syscall contract
  it relies on and cites the man page or MSDN page.
- **S3.** Lock-then-release-then-use-pointer pattern in `advise.rs`,
  `lock.rs`, and `atomic.rs`. **Closed in 0.9.6 (documentation).**
  The atomic case was already closed in 0.9.5 via the
  `AtomicView` wrapper (C3); the advise/lock case is documented as
  sound because the syscalls operate on the address range without
  accessing memory, and `resize()` requires the write lock so cannot
  race with the read guard.
- **S4.** Phase-1 COW exposes `&[u8]` but write semantics are not
  yet wired. **Documented.** The rustdoc on `open_cow` notes that
  COW is currently read-only at the Rust API; writable COW is on the
  roadmap.

## Future work

- **Property tests** (`tests/proptest_*.rs`) exercise the bounds and
  alignment invariants against random inputs. The full sweep
  (`PROPTEST_CASES=10000`) is run before each release.
- **Fuzz tests** (`fuzz/`) are planned for 0.9.10 per ROADMAP.
- **MIRI runs** on the atomic module are planned for the same window.
