# mmap-io - Safety Contract

This document describes how `mmap-io` keeps its `unsafe` code sound:
the locking model every accessor follows, each category of `unsafe`
block, and the limits of what the crate can guarantee. The
`// SAFETY:` comment at each block is the source of truth for that
block; this file explains the shape and the shared invariants. It
names functions rather than line numbers, so it does not drift as the
code moves.

Everything in the public API is safe to call except four `unsafe fn`
escape hatches, which carry their own contracts:
`MemoryMappedFile::as_ptr`, `MemoryMappedFile::as_mut_ptr`,
`AnonymousMmap::as_ptr`, and `AnonymousMmap::as_mut_ptr`.
The `mmap_io::raw` tier adds `unsafe fn` file-backed constructors
(`RawMmap::map`, `RawMmapMut::map_mut`, `RawMmapOptions::map`,
`map_mut`, `map_copy`) for the reason given in category 8 below.

## Locking model

### Which mappings are locked

- **ReadWrite** mappings live in `MapVariant::Rw(RwLock<RawMmapMut>)`
  (a `parking_lot::RwLock`). `resize()` replaces the `RawMmapMut`, so
  any access to the mapped bytes must hold a guard on this lock.
- **CopyOnWrite** mappings (writable since 1.1) live in
  `MapVariant::Cow(RwLock<RawMmapMut>)`, a private `map_copy` mapping.
  They are never resized, but they are written, so every access takes
  the same guards as `ReadWrite`.
- **ReadOnly** mappings live in an immutable `RawMmap` that is never
  replaced or written. A plain `&[u8]` borrow tied to
  `&MemoryMappedFile` is enough, and `as_slice_bytes` hands one out
  only for this mode.

### Who holds which guard

| Read guard (shared) | Write guard (exclusive) |
|---------------------|-------------------------|
| `MappedSlice` from `as_slice` / `Segment::as_slice` (RW only) | `update_region` (for the copy) |
| `chunks()` / `pages()` iterators **and every item they yield** | `as_slice_mut` / `SegmentMut::as_slice_mut` (`MappedSliceMut`) |
| `AtomicView` / `AtomicSliceView` | `chunks_mut().for_each_mut` |
| `read_into`, `touch_pages*`, `flush`, `flush_range`, `advise`, `lock`/`unlock` (for the duration of the call) | `resize` |

Consequences callers must know:

- Any live read view blocks **every** write method, whatever region
  it covers, and `resize()`. Writes to "disjoint" regions are not
  exempt.
- Calling a write method on a thread that holds a read view of the
  same mapping deadlocks. Drop the view first, or use the non-blocking
  `try_update_region` / `try_as_slice_mut` / `try_as_slice` (1.1),
  which use `try_write` / `try_read_recursive` and return "would
  block" instead of waiting.
- Read paths take the lock with `read_recursive()`, so a thread that
  already holds a view can take another one even while a writer is
  queued (a fair `read()` would deadlock there).

### Range validation happens under the guard

Every accessor acquires its guard first and validates
`offset + len` against the length of the mapping that guard protects
(`guard.len()`), never against a length read earlier. The cached
length (`Inner::cached_len`, an `AtomicU64`) only serves `len()` and
`current_len()`; it is written by `resize()` while it holds the write
lock. A concurrent `resize()` therefore shows up as `OutOfBounds`,
never as an out-of-range access. Zero-length requests are accepted at
any offset and touch nothing (see the crate docs, "Range
validation").

### resize

`resize()` takes the write lock **before** touching the file, so no
view can observe a truncated file:

- **Grow** (all platforms): extend the file, map the new length, swap
  the mapping in (the old one is unmapped as it is dropped). If the
  new mapping fails, the file is set back to its old length.
- **Shrink on Unix**: map the surviving prefix of the still-long file,
  truncate, swap. A mapping failure leaves file and mapping unchanged.
- **Shrink on Windows**: Windows refuses to truncate a file with a
  mapped view (`ERROR_USER_MAPPED_FILE`), so the view is first
  replaced by an empty anonymous placeholder, then the file is
  truncated and the prefix mapped. If truncation fails, the old
  length is remapped. If only the final remap fails, the mapping
  stays empty (`len() == 0`) and every access returns `OutOfBounds`.

## Categories of `unsafe`

### 1. Mapping construction (`src/mmap.rs`)

`memmap2::Mmap::map`, `MmapMut::map_mut`, and `MmapOptions::map` /
`map_mut` are `unsafe` because the OS does not stop another process
from modifying or truncating the file under the mapping. Inside the
process, all access to RW mappings goes through the lock described
above (COW included), and RO mappings are never written.
Cross-process modification is out of scope (REPS.md section 5.1).

Sites: `create_rw`, `open_ro`, `open_rw`, `from_file`, `open_cow`,
`MemoryMappedFileBuilder::open_existing` (RO and COW), and
`map_file_rw`, which every builder RW path and `resize()` use. Callers
of `map_file_rw` never pass a length beyond the file's current length.

Reference: [`memmap2::MmapMut::map_mut`](https://docs.rs/memmap2/latest/memmap2/struct.MmapMut.html#method.map_mut)

### 2. Guarded slices (`MappedSlice`, `src/mmap.rs`)

For RW mappings a `MappedSlice` stores the read guard plus a raw
`*const [u8]` computed once at construction, so `Deref` is a pointer
dereference with no range arithmetic. Soundness: the slice was taken
from the guarded mapping, the guard lives exactly as long as the
`MappedSlice`, and the returned borrow is tied to `&self`.

`MappedSlice` has `unsafe impl Send + Sync`. The guard it carries is
`Send` because the crate enables parking_lot's `send_guard` feature;
the constant `_ASSERT_GUARDS_SEND_SYNC` makes the build fail if that
feature is ever dropped. (parking_lot rejects `send_guard` together
with its `deadlock_detection` feature at compile time.)

Iterator items take their own recursive read guard, so a chunk kept
after its iterator is dropped still pins the mapping.

On RW and COW mappings the slice also holds a `PlainReg`, its entry in
the mapping's view registry (category 9), so no atomic view of its
bytes can exist while it lives. The slice pointer is computed with
`RawMmapMut::as_ptr` plus an offset (`sub_slice_ptr`) rather than by
indexing `&guard[..]`, so no `&[u8]` over the whole mapping (which
could cover bytes under a live atomic view elsewhere) is ever formed.
A fourth variant, `Snapshot(Box<[u8]>)`, is an owned copy used for
iterator items that overlap a live atomic view.

### 3. Atomic views (`src/atomic.rs`)

`view_parts` casts `guard.as_ptr().add(offset)` to `*const AtomicU32`
or `*const AtomicU64`. It is sound because:

1. Only writable mappings are accepted (`ReadWrite`, `CopyOnWrite`
   since 1.1, and `AnonymousMmap`); RO mappings return `InvalidMode`,
   since a safe `store` on a read-only page faults.
2. The offset is checked to be a multiple of the type's alignment, and
   the mapping base is page-aligned.
3. `offset + count * size_of::<T>()` is checked against the guarded
   mapping's length.
4. `T` is restricted by a sealed trait to `AtomicU32` / `AtomicU64`,
   which have the layout of `u32` / `u64` and accept every bit
   pattern; `size == align`, so every element of a run is aligned.
5. The range is registered in the mapping's view registry (category
   9), which refuses it if a plain view of any of its bytes, or an
   atomic view of the other element size, is alive, and keeps such
   views from being created until the atomic view is dropped.

The view keeps the read guard for its lifetime. `AtomicView` and
`AtomicSliceView` are `Send + Sync` for `T: Sync`, on the same
`send_guard` basis as `MappedSlice`.

### 4. Kernel range calls (`src/advise.rs`, `src/lock.rs`, `src/mmap.rs`)

`madvise`, `PrefetchVirtualMemory`, `mlock` / `munlock`, and
`VirtualLock` / `VirtualUnlock` take an address range. The address
and length come from a subslice of the mapping that the method holds
read access to, and that access (the guard, for RW) stays alive until
the syscall returns. `posix_fadvise` (`prefetch_range`, Linux) takes a
file descriptor and a file range instead; it touches no memory, and
the range is still bounds-checked. `advise()` widens the start
down to a page boundary because `madvise` rejects unaligned
addresses. None of these calls read or write the bytes through Rust
references. `MADV_DONTNEED` on these file-backed mappings drops page
table entries; the next access re-faults from the page cache or file.

The `hugepages` hint (`advise_huge_pages`) calls `madvise` with
`MADV_HUGEPAGE` over a whole mapping it borrows for the call.

References: [`madvise(2)`](https://man7.org/linux/man-pages/man2/madvise.2.html),
[`mlock(2)`](https://man7.org/linux/man-pages/man2/mlock.2.html),
[`PrefetchVirtualMemory`](https://learn.microsoft.com/en-us/windows/win32/api/memoryapi/nf-memoryapi-prefetchvirtualmemory),
[`VirtualLock`](https://learn.microsoft.com/en-us/windows/win32/api/memoryapi/nf-memoryapi-virtuallock).

### 5. Page touching (`touch_range_with_ptr`, `src/mmap.rs`)

`touch_pages` and `touch_pages_range` do one `read_volatile` per page
over a range validated under the held read access. The volatile read
is wrapped in `black_box` so it is not optimized away.

### 6. Platform queries (`src/utils.rs`)

`GetSystemInfo` fills a caller-provided `SYSTEM_INFO` in a
`MaybeUninit` and has no failure mode. `sysconf(_SC_PAGESIZE)` takes
no pointers; a non-positive result falls back to 4096 so callers
never divide by zero.

### 7. Caller-facing `unsafe fn`

`as_ptr` / `as_mut_ptr` return the mapping base without holding a
lock. The caller must not use the pointer past `len()`, across a
`resize()` (which can move the mapping), or in a way that aliases a
live `&` / `&mut` the crate handed out. `MemoryMappedFile::as_mut_ptr`
adds the whole mapping length to `pending_bytes()`, since writes
through the pointer are invisible to the crate.

### 8. Raw mapping layer (`src/raw/`)

`mmap_io::raw` is the platform layer (`RawMmap`, `RawMmapMut`,
`RawMmapOptions`) intended to replace the `memmap2` dependency. It is
split so that the arithmetic and the syscalls can be reviewed apart:

- `range.rs`: pure, `unsafe`-free offset and length arithmetic. Runs
  under Miri.
- `unix.rs`: `mmap` / `msync` / `munmap` / `mprotect`, plus `madvise`
  (feature `advise`), `mlock` / `munlock` (feature `locking`) and
  `sync_file_range` (Linux), via `libc`.
- `windows.rs`: `CreateFileMappingW` / `MapViewOfFile` /
  `FlushViewOfFile` / `UnmapViewOfFile` / `VirtualProtect` /
  `GetSystemInfo`, plus `PrefetchVirtualMemory` and `VirtualLock` /
  `VirtualUnlock`, declared by hand with `extern "system"` (no
  `windows-sys`).
- `stub.rs`: every constructor returns `Unsupported`.
- `mod.rs`: the owning `Mapping` type, `Deref`, `Drop`, `Send`/`Sync`.

**Why the file-backed constructors are `unsafe fn`.** A mapping hands
out `&[u8]` (and `&mut [u8]` for `RawMmapMut`), and Rust assumes the
bytes behind a shared reference do not change while it is alive. The
OS cannot enforce that for a file: another process, another mapping
in this process, or a plain `write` can change the bytes, and a
truncation makes later accesses fault (`SIGBUS` on Unix,
`EXCEPTION_IN_PAGE_ERROR` on Windows). Only the caller can rule this
out, so the contract is pushed to the caller exactly as `memmap2`
does. `map_anon` is safe: anonymous memory has no outside writer.

**Invariants of `Mapping`** (established at construction, relied on by
`Deref`, `flush` and `Drop`):

1. `len == 0` if and only if no OS mapping exists. Zero-length windows
   never call `mmap` (POSIX: a zero length fails with `EINVAL`) or
   `CreateFileMappingW` (fails on empty files). The pointer is then a
   non-null, granularity-aligned address used only for zero-length
   slices and never unmapped.
2. Otherwise the OS mapping starts at `ptr - delta` and is `os_len`
   bytes long, with `delta` below the OS offset granularity (page size
   on Unix, allocation granularity on Windows) and
   `delta + len <= os_len <= isize::MAX`. `delta` and `delta + len` are
   computed by `range::layout` with checked arithmetic; `os_len` is
   larger only for `huge()` anonymous mappings, rounded up (checked) to
   the huge page size because `munmap` of a `MAP_HUGETLB` mapping needs
   a huge-page multiple. `Drop` unmaps `os_len` bytes.
3. The window `[offset, offset + len)` lies inside the file at mapping
   time (`range::resolve_len`): mapping past end of file is rejected
   up front instead of producing a mapping that faults on access.
4. The mapping is owned exclusively by one value, so `Drop` unmaps it
   exactly once and never panics (errors from `munmap` /
   `UnmapViewOfFile` are ignored, as there is no caller to report to).

**Protection changes.** `make_read_only` and `make_mut` call
`mprotect` / `VirtualProtect` on the whole OS mapping. Both take the
mapping by value, so no `&[u8]` or `&mut [u8]` into it can be alive
when the protection changes (a `&mut [u8]` to pages that just became
read-only would fault on write; a `&[u8]` to pages that just became
writable through another handle would break its immutability). The
mapping records its kind at creation (shared read, shared write,
copy-on-write, anonymous), so `make_mut` restores exactly the original
access; on Windows a view of a `PAGE_READONLY` section is never made
writable (the call fails before any OS call). On Unix, making a
read-only shared file mapping writable switches its backing so `flush`
calls `msync`. A failed call drops (unmaps) the mapping.

**Advice and locking.** `advise_range` and `lock` validate the range
with the same `range::flush_span` as `flush_range`, so the address is
page aligned and inside the mapping before `madvise` / `mlock` /
`PrefetchVirtualMemory` / `VirtualLock` sees it. None of these calls
reads or writes the bytes, except `MADV_DONTNEED` on private memory,
which replaces private pages with file contents (copy-on-write) or
zeros (anonymous) and would change bytes behind live `&[u8]` borrows.
The raw tier therefore refuses `DontNeed` on private mappings with
`InvalidInput`. On shared file mappings it only drops page table
entries.

**Bounds before pointers.** `flush_range` passes the caller's
`(offset, len)` through `range::flush_span`, which rejects
`offset > len`, `len > window - offset` and any overflow, and aligns
the start down to a page, before `ptr.add` or any syscall. This is the
bug class of RUSTSEC-2026-0186 in `memmap2` (unchecked offset and
length in `flush_range` / `advise_range`).

**Windows handle lifetime.** The section handle from
`CreateFileMappingW` is closed right after `MapViewOfFile`; MSDN
documents that a view holds its own reference to the section. Shared
writable views keep a duplicate of the file handle
(`File::try_clone`, i.e. `DuplicateHandle` with
`DUPLICATE_SAME_ACCESS`) so that a durable `flush` can call
`FlushFileBuffers` after the caller's `File` is gone; the duplicate is
closed when the mapping drops. Last-error values are captured before
`CloseHandle` can overwrite them.

**`Send` / `Sync`.** `Mapping` owns its OS mapping the way `Box<[u8]>`
owns an allocation; a mapping is valid from every thread and can be
released from any thread. Shared access only yields `&[u8]` and
validated flush syscalls, and `&mut [u8]` requires `&mut RawMmapMut`.

References:

- POSIX `mmap`: https://pubs.opengroup.org/onlinepubs/9799919799/functions/mmap.html
- POSIX `msync`: https://pubs.opengroup.org/onlinepubs/9799919799/functions/msync.html
- `CreateFileMappingW`: https://learn.microsoft.com/en-us/windows/win32/api/memoryapi/nf-memoryapi-createfilemappingw
- `MapViewOfFile`: https://learn.microsoft.com/en-us/windows/win32/api/memoryapi/nf-memoryapi-mapviewoffile
- `FlushViewOfFile`: https://learn.microsoft.com/en-us/windows/win32/api/memoryapi/nf-memoryapi-flushviewoffile

## Flushing

The crate has no `unsafe` on the flush path. `flush()` and
`flush_range()` call memmap2, which issues `msync(MS_SYNC)` on Unix
and `FlushViewOfFile` + `FlushFileBuffers` on Windows, while holding a
read guard so the mapping cannot be replaced during the call.

### 9. View registry (`src/views.rs`)

An atomic view and a plain view (`MappedSlice`, iterator item) both
hold read guards, so the lock alone would let them cover the same
bytes. An atomic store would then race with the slice's non-atomic
reads, and a `&[u8]` asserts that its bytes do not change at all while
it lives: undefined behavior. Two atomic views of different element
sizes over the same bytes are mixed-size atomic accesses, also
undefined. Before 1.1 this was only documented; since 1.1 each
writable mapping (RW, COW, `AnonymousMmap`) carries a `ViewRegistry`
that records the byte range of every live plain and atomic view and
refuses:

- a plain view overlapping a live atomic view (`as_slice`,
  `Segment::as_slice`, `try_as_slice` return `InvalidMode`; iterator
  items become owned snapshots instead);
- an atomic view overlapping a live plain view, or a live atomic view
  of the other element size (`InvalidMode`).

Disjoint ranges never conflict. Copying reads (`read_into`,
`read_bytes`, `MmapReader`, snapshots) do not register; they hold the
registry's atomic-set read lock for the copy, so no atomic view can
appear under them, and read bytes under an existing atomic view with
atomic loads of that view's element size (`copy_out`). `touch_pages`
does the same per page.

Registration protocol: plain views go into per-thread shards
(`Mutex<Slab>`) and then load an `atomic_live` counter; only when it
is non-zero do they read-lock the atomic set and check for overlap.
Atomic views write-lock the atomic set, increment the counter, then
lock and scan every shard. Because both sides take the plain view's
shard mutex, one of them goes first: if the plain view does, the
atomic scan sees it; if the atomic view does, its increment
happens-before the plain view's counter load, which sends the plain
view to the locked check. At most one of a conflicting pair succeeds
(rarely both fail, which is a spurious `InvalidMode`, never an
overlap). The module docs carry the full argument; unit tests race the
two sides.

Without the `atomic` feature no atomic view can exist and the registry
compiles to nothing, so plain views cost exactly what they did in 1.0.
With it, each RW / COW plain view costs one shard lock to register and
one to deregister (see `docs/PERFORMANCE.md`).

What it does not cover: raw pointers (`as_ptr` / `as_mut_ptr`), and
other `MemoryMappedFile` values that map the same file independently
(another `open_rw` of the same path, or another process). Those are
separate mappings with separate registries, the same class as
cross-process modification.

## What the crate cannot guarantee

- **Other processes.** If another process writes to or truncates the
  file, readers here can see torn data or receive `SIGBUS`. Callers
  that share a file across processes must coordinate (REPS.md 5.1).
- **Atomic and plain access through independent mappings.** Within
  one mapping (and its clones) the view registry (category 9) keeps
  atomic and plain views of the same bytes apart. Two independently
  opened mappings of the same file, or a raw pointer, are not covered:
  an atomic store through one and a plain read through the other is a
  data race, like cross-process access.
- **Raw pointers** from `as_ptr` / `as_mut_ptr` follow the caller's
  contract above; the crate cannot check it.

## Audit history

- **S1** (`windows_page_size` lacked a SAFETY comment): closed in 0.9.5.
- **S2** (shallow SAFETY comments): closed in 0.9.6; every block cites
  the syscall contract it relies on.
- **S3** (guard released before using the pointer in `advise.rs`,
  `lock.rs`): 0.9.6 only documented it. Since 1.1 the guard is kept
  alive across the syscall, and ranges are validated under it.
- **S4** (COW write semantics): until 1.1 COW mappings were read-only
  at the API. Since 1.1 they are mapped writable and private
  (`map_copy`) and locked like `ReadWrite`, so the write methods and
  atomic views are sound on them; `as_slice_bytes` (an unguarded
  `&[u8]`) is refused on them, and `advise(DontNeed)`, which discards
  private pages, takes the write lock.
- **Atomic vs plain views** (documented as a caller obligation
  through 1.0): enforced at run time by the view registry since 1.1.
- **1.1 review**: iterator items outliving their guard
  (use-after-free on `resize`), atomic views on read-only pages,
  validation against a length read before the lock, truncation before
  locking in `resize`, and `Send` impls that relied on parking_lot
  internals. All fixed; `tests/soundness_regressions.rs` pins each.

## Verification

- Property tests: `tests/proptest_bounds.rs`, `tests/proptest_atomic.rs`,
  `tests/proptest_flush.rs` (`PROPTEST_CASES=10000` for a deep run).
- Fuzz targets under `fuzz/`: `atomic_view`, `bounds_checks`,
  `read_into`, `update_region`, `raw_map`.
- Regression tests for the soundness fixes:
  `tests/soundness_regressions.rs`.
- Raw layer: `tests/raw_mapping.rs`, `tests/raw_concurrency.rs`,
  `tests/raw_leak.rs`, `tests/raw_proptest.rs`.
- Miri runs the pure offset and length arithmetic in
  `src/raw/range.rs`; it cannot execute the `mmap` family of syscalls,
  so the FFI tests are skipped under Miri.
