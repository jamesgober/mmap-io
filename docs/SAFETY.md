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

## Locking model

### Which mappings are locked

- **ReadWrite** mappings live in `MapVariant::Rw(RwLock<MmapMut>)`
  (a `parking_lot::RwLock`). `resize()` replaces the `MmapMut`, so
  any access to the mapped bytes must hold a guard on this lock.
- **ReadOnly** and **CopyOnWrite** mappings live in an immutable
  `memmap2::Mmap` that is never replaced (resize is rejected for both
  modes, and COW is exposed read-only). A plain `&[u8]` borrow tied to
  `&MemoryMappedFile` is enough.

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
  same mapping deadlocks. Drop the view first.
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
above, and RO/COW mappings are never written through Rust references.
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

### 3. Atomic views (`src/atomic.rs`)

`view_parts` casts `guard.as_ptr().add(offset)` to `*const AtomicU32`
or `*const AtomicU64`. It is sound because:

1. Only `ReadWrite` mappings are accepted; RO and COW mappings return
   `InvalidMode`, since a safe `store` on a read-only page faults.
2. The offset is checked to be a multiple of the type's alignment, and
   the mapping base is page-aligned.
3. `offset + count * size_of::<T>()` is checked against the guarded
   mapping's length.
4. `T` is restricted by a sealed trait to `AtomicU32` / `AtomicU64`,
   which have the layout of `u32` / `u64` and accept every bit
   pattern; `size == align`, so every element of a run is aligned.

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

## Flushing

The crate has no `unsafe` on the flush path. `flush()` and
`flush_range()` call memmap2, which issues `msync(MS_SYNC)` on Unix
and `FlushViewOfFile` + `FlushFileBuffers` on Windows, while holding a
read guard so the mapping cannot be replaced during the call.

## What the crate cannot guarantee

- **Other processes.** If another process writes to or truncates the
  file, readers here can see torn data or receive `SIGBUS`. Callers
  that share a file across processes must coordinate (REPS.md 5.1).
- **Atomic and plain access to the same bytes.** An `AtomicView` and a
  `MappedSlice` both hold read guards and can coexist. Reading bytes
  through the slice while another thread stores to them through the
  atomic view is a data race under the Rust memory model. Keep
  atomic regions and plain-byte regions disjoint.
- **Raw pointers** from `as_ptr` / `as_mut_ptr` follow the caller's
  contract above; the crate cannot check it.

## Audit history

- **S1** (`windows_page_size` lacked a SAFETY comment): closed in 0.9.5.
- **S2** (shallow SAFETY comments): closed in 0.9.6; every block cites
  the syscall contract it relies on.
- **S3** (guard released before using the pointer in `advise.rs`,
  `lock.rs`): 0.9.6 only documented it. Since 1.1 the guard is kept
  alive across the syscall, and ranges are validated under it.
- **S4** (COW write semantics): COW mappings are read-only at the API;
  every write method returns `InvalidMode`, and atomic views are
  refused on them since 1.1.
- **1.1 review**: iterator items outliving their guard
  (use-after-free on `resize`), atomic views on read-only pages,
  validation against a length read before the lock, truncation before
  locking in `resize`, and `Send` impls that relied on parking_lot
  internals. All fixed; `tests/soundness_regressions.rs` pins each.

## Verification

- Property tests: `tests/proptest_bounds.rs`, `tests/proptest_atomic.rs`,
  `tests/proptest_flush.rs` (`PROPTEST_CASES=10000` for a deep run).
- Fuzz targets under `fuzz/`: `atomic_view`, `bounds_checks`,
  `read_into`, `update_region`.
- Regression tests for the soundness fixes:
  `tests/soundness_regressions.rs`.
- Miri is not run: it cannot execute the `mmap` family of syscalls.
