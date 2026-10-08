//! Raw memory mappings: the thin platform layer under
//! [`MemoryMappedFile`].
//!
//! This module owns the operating-system calls that create, flush and
//! release a mapping (`mmap` / `msync` / `munmap` on Unix,
//! `CreateFileMappingW` / `MapViewOfFile` / `FlushViewOfFile` /
//! `UnmapViewOfFile` on Windows) and nothing else: no locks, no flush
//! policy, no path bookkeeping. It returns [`std::io::Result`] so it
//! can be used without [`MmapIoError`].
//!
//! The API follows the shape of the `memmap2` crate so code written
//! against it ports by renaming types:
//!
//! | `memmap2`     | `mmap_io::raw`                                      |
//! |---------------|-----------------------------------------------------|
//! | `Mmap`        | [`RawMmap`](crate::raw::RawMmap)                    |
//! | `MmapMut`     | [`RawMmapMut`](crate::raw::RawMmapMut)              |
//! | `MmapOptions` | [`RawMmapOptions`](crate::raw::RawMmapOptions)      |
//!
//! # Types
//!
//! - [`RawMmap`](crate::raw::RawMmap): read-only, shared view of a
//!   file. Derefs to `[u8]`.
//! - [`RawMmapMut`](crate::raw::RawMmapMut): writable view. Shared
//!   with the file
//!   ([`RawMmapOptions::map_mut`](crate::raw::RawMmapOptions::map_mut)),
//!   private copy-on-write
//!   ([`RawMmapOptions::map_copy`](crate::raw::RawMmapOptions::map_copy)),
//!   or anonymous ([`RawMmapMut::map_anon`](crate::raw::RawMmapMut::map_anon)).
//!   Derefs to `[u8]` and `mut [u8]`.
//! - [`RawMmapOptions`](crate::raw::RawMmapOptions): builder for an
//!   offset / length window.
//!
// Links in this overview use `crate::raw::...` paths: rustdoc
// resolves a module's inner docs in the parent scope when the `mod`
// item in lib.rs also carries outer docs.
//!
//! # Guarantees
//!
//! - Every `(offset, len)` is validated with checked arithmetic before
//!   any pointer is formed or any syscall is made. Invalid ranges
//!   return `io::ErrorKind::InvalidInput`; nothing in this module
//!   panics on caller input.
//! - A window that would extend past the end of the file is rejected
//!   up front instead of producing a mapping that faults (`SIGBUS`) on
//!   access.
//! - Arbitrary offsets are supported: the OS mapping starts at the
//!   offset rounded down to the OS granularity (page size on Unix,
//!   allocation granularity on Windows) and the extra leading bytes
//!   are hidden.
//! - Zero-length windows (empty file, `len(0)`, offset at end of file)
//!   create no OS mapping at all and deref to an empty slice.
//! - `flush` is durable: `msync(MS_SYNC)` on Unix,
//!   `FlushViewOfFile` + `FlushFileBuffers` on Windows.
//! - Access through `Deref` is a pointer and a length: no allocation,
//!   no lock, no syscall.
//! - A mapping stays valid after the `File` it was created from is
//!   dropped.
//!
//! # Platform support
//!
//! Unix (Linux, Android, macOS, iOS, FreeBSD and the other BSDs) and
//! Windows have native backends. On any other target every constructor
//! returns `io::ErrorKind::Unsupported`.
//!
//! # Example
//!
//! ```
//! use std::fs::OpenOptions;
//! use mmap_io::raw::{RawMmap, RawMmapMut};
//!
//! # fn main() -> std::io::Result<()> {
//! let dir = tempfile::tempdir()?;
//! let path = dir.path().join("raw.bin");
//! let file = OpenOptions::new()
//!     .read(true)
//!     .write(true)
//!     .create(true)
//!     .truncate(true)
//!     .open(&path)?;
//! file.set_len(16)?;
//!
//! // SAFETY: the file is private to this example; nothing else
//! // modifies or truncates it while the mappings are alive.
//! let mut rw = unsafe { RawMmapMut::map_mut(&file)? };
//! rw[..5].copy_from_slice(b"hello");
//! rw.flush()?;
//! drop(rw);
//!
//! let ro = unsafe { RawMmap::map(&file)? };
//! assert_eq!(&ro[..5], b"hello");
//! # Ok(())
//! # }
//! ```

use std::fmt;
use std::fs::File;
use std::io;
use std::ops::{Deref, DerefMut};
use std::ptr::NonNull;
use std::slice;

mod range;

#[cfg(unix)]
mod unix;
#[cfg(unix)]
use unix as os;

#[cfg(windows)]
mod windows;
#[cfg(windows)]
use windows as os;

#[cfg(not(any(unix, windows)))]
mod stub;
#[cfg(not(any(unix, windows)))]
use stub as os;

/// Requested access for a file-backed mapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Access {
    /// `PROT_READ` + `MAP_SHARED` / `PAGE_READONLY` + `FILE_MAP_READ`.
    Read,
    /// `PROT_READ|PROT_WRITE` + `MAP_SHARED` /
    /// `PAGE_READWRITE` + `FILE_MAP_WRITE`.
    Write,
    /// `PROT_READ|PROT_WRITE` + `MAP_PRIVATE` /
    /// `PAGE_WRITECOPY` + `FILE_MAP_COPY`.
    Copy,
}

/// What kind of OS mapping a `Mapping` is. Fixed at creation; decides
/// which protections it may switch between and whether `DontNeed`
/// advice would discard private data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    /// Shared file mapping created read-only.
    Read,
    /// Shared file mapping created writable.
    Write,
    /// Private copy-on-write file mapping.
    Copy,
    /// Private anonymous mapping.
    Anon,
}

impl Kind {
    /// Private memory: `MADV_DONTNEED` discards its contents.
    #[cfg(feature = "advise")]
    fn is_private(self) -> bool {
        matches!(self, Kind::Copy | Kind::Anon)
    }
}

/// Page protection for [`RawMmap::make_mut`] and
/// [`RawMmapMut::make_read_only`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Protection {
    /// `PROT_READ` / `PAGE_READONLY`.
    ReadOnly,
    /// `PROT_READ|PROT_WRITE` / `PAGE_READWRITE`.
    ReadWrite,
    /// `PROT_READ|PROT_WRITE` on a private mapping / `PAGE_WRITECOPY`.
    WriteCopy,
}

/// Optional mapping flags set through [`RawMmapOptions`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct MapFlags {
    /// `MAP_POPULATE` (Linux / Android).
    pub(crate) populate: bool,
    /// `MAP_HUGETLB` for anonymous mappings (Linux / Android).
    pub(crate) huge: bool,
}

/// Whether a flush waits for the data to reach stable storage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FlushMode {
    /// Block until durable (`MS_SYNC`, or `FlushViewOfFile` +
    /// `FlushFileBuffers`).
    Sync,
    /// Start write-back and return (`MS_ASYNC`, or `FlushViewOfFile`).
    Async,
}

/// Granularity at which the OS places the start of a file mapping:
/// the page size on Unix, the allocation granularity on Windows
/// (typically 64 KiB).
///
/// The raw constructors accept any offset and align it internally, so
/// callers do not need this value to create a mapping. It is useful
/// for choosing offsets that waste no address space and for tests
/// that probe alignment edges.
///
/// # Errors
///
/// Returns an error if the OS reports a value that is not a non-zero
/// power of two, and `io::ErrorKind::Unsupported` on targets without a
/// mapping backend.
///
/// # Example
///
/// ```
/// let gran = mmap_io::raw::offset_granularity()?;
/// assert!(gran.is_power_of_two());
/// # Ok::<(), std::io::Error>(())
/// ```
pub fn offset_granularity() -> io::Result<usize> {
    os::offset_granularity()
}

/// Apply `advice` to `count` bytes at `addr`: the platform call behind
/// [`RawMmap::advise_range`], for the managed layer, which validates
/// ranges against its own locked mapping.
///
/// # Safety
///
/// `addr` must be page aligned and `[addr, addr + count)` must lie
/// inside a live mapping created by this module, with `count > 0`, for
/// the duration of the call. `DontNeed` must not be applied to private
/// (copy-on-write or anonymous) memory while any reference into the
/// range may be alive.
#[cfg(feature = "advise")]
pub(crate) unsafe fn advise_span(
    addr: *mut u8,
    count: usize,
    advice: crate::advise::MmapAdvice,
) -> io::Result<()> {
    // SAFETY: forwarded caller contract, identical to `os::advise`'s.
    unsafe { os::advise(addr, count, advice) }
}

/// Lock or unlock `count` bytes at `addr`: the platform call behind
/// [`RawMmap::lock`], for the managed layer.
///
/// # Safety
///
/// `addr` must be page aligned and `[addr, addr + count)` must lie
/// inside a live mapping created by this module, with `count > 0`, for
/// the duration of the call.
#[cfg(feature = "locking")]
pub(crate) unsafe fn lock_span(addr: *mut u8, count: usize, lock: bool) -> io::Result<()> {
    // SAFETY: forwarded caller contract, identical to `os::lock`'s.
    unsafe { os::lock(addr, count, lock) }
}

/// Start write-back of the dirty page-cache pages of `file` in
/// `[offset, offset + len)` without waiting
/// (`sync_file_range(SYNC_FILE_RANGE_WRITE)`). Not durable. Linux only;
/// used by `MemoryMappedFile::schedule_flush_range`.
#[cfg(target_os = "linux")]
pub(crate) fn start_writeback(file: &File, offset: u64, len: u64) -> io::Result<()> {
    os::start_writeback(file, offset, len)
}

/// One OS mapping, or the empty placeholder.
///
/// Invariants:
/// - `len == 0` if and only if no OS mapping exists. Then `ptr` is a
///   non-null, granularity-aligned address that is never dereferenced
///   for more than zero bytes and never unmapped.
/// - Otherwise the OS mapping starts at `ptr - delta`, is `os_len`
///   bytes long with `delta + len <= os_len <= isize::MAX` (`os_len`
///   exceeds `delta + len` only for huge-page anonymous mappings, which
///   are rounded up to the huge page size), is readable for its whole
///   length, and is owned exclusively by this value.
/// - `kind` records how the mapping was created and never changes,
///   except that `make_mut` of a `Kind::Read` mapping turns it into
///   `Kind::Write` once the OS granted write access.
struct Mapping {
    ptr: NonNull<u8>,
    len: usize,
    delta: usize,
    os_len: usize,
    backing: os::Backing,
    kind: Kind,
}

// SAFETY: `Mapping` owns its OS mapping exclusively, in the same way a
// `Box<[u8]>` owns its allocation: the address is not shared with any
// other Rust value, a mapping is valid process-wide (from every
// thread), and `munmap` / `UnmapViewOfFile` may be called from any
// thread. The Windows `Backing::Shared` variant holds a `File`, which
// is itself `Send + Sync`. Through `&Mapping` the public types only
// produce `&[u8]` and raw pointers; `&mut [u8]` is produced only from
// `&mut RawMmapMut`, so the borrow checker rules out data races inside
// the process. Modification from outside the process is excluded by
// the `# Safety` contract of the file-backed constructors.
unsafe impl Send for Mapping {}
// SAFETY: see the `Send` impl above; shared references only permit
// reads of the mapped bytes and validated flush / advise / lock
// syscalls, all of which are thread-safe and leave the bytes unchanged
// (`DontNeed` on private memory, which would change them, is refused).
unsafe impl Sync for Mapping {}

impl Mapping {
    /// The zero-length placeholder. No OS call is made.
    fn empty(align: usize, kind: Kind) -> Self {
        // Use an aligned non-null address with no allocation behind it,
        // as `NonNull::dangling` does but aligned to the mapping
        // granularity so pointer arithmetic by callers on an empty
        // mapping sees the same alignment as on a real one. Valid for
        // zero-length slices, which is the only way it is used.
        let ptr = NonNull::new(align as *mut u8).unwrap_or(NonNull::dangling());
        Self {
            ptr,
            len: 0,
            delta: 0,
            os_len: 0,
            backing: os::Backing::Private,
            kind,
        }
    }

    /// Wrap a fresh OS mapping described by `layout`.
    ///
    /// # Safety
    ///
    /// `base` must be the start of a live OS mapping of exactly
    /// `os_len >= layout.map_len` bytes, with `layout.len > 0`, that
    /// nothing else owns.
    unsafe fn from_os(
        base: NonNull<u8>,
        layout: &range::Layout,
        os_len: usize,
        backing: os::Backing,
        kind: Kind,
    ) -> Self {
        // SAFETY: `layout.delta < layout.map_len <= os_len` because
        // `map_len == delta + len` and `len > 0` (caller contract), so
        // `base + delta` stays inside the mapping and cannot be null
        // (a mapping never wraps around the address space).
        let ptr = unsafe { NonNull::new_unchecked(base.as_ptr().add(layout.delta)) };
        Self {
            ptr,
            len: layout.len,
            delta: layout.delta,
            os_len,
            backing,
            kind,
        }
    }

    /// Start of the OS mapping (equal to `ptr` when `delta == 0`).
    #[inline]
    fn base(&self) -> *mut u8 {
        // `delta <= ptr - base` by construction, so this stays in
        // bounds; `wrapping_sub` keeps it free of `unsafe`.
        self.ptr.as_ptr().wrapping_sub(self.delta)
    }

    #[inline]
    fn as_slice(&self) -> &[u8] {
        // SAFETY: per the struct invariants `ptr` is non-null and, when
        // `len > 0`, valid for reads of `len` bytes for as long as
        // `self` is alive; when `len == 0` any non-null aligned pointer
        // is valid. `len <= isize::MAX` (range::MAX_LEN). File pages
        // and anonymous pages are initialised by the kernel. No `&mut`
        // to the same bytes can coexist with this `&self` borrow inside
        // safe code; outside writers are excluded by the constructors'
        // `# Safety` contract.
        unsafe { slice::from_raw_parts(self.ptr.as_ptr(), self.len) }
    }

    #[inline]
    fn as_mut_slice(&mut self) -> &mut [u8] {
        // SAFETY: as in `as_slice`, plus: only called for writable
        // mappings (`RawMmapMut`), and `&mut self` guarantees no other
        // borrow of the bytes exists in safe code.
        unsafe { slice::from_raw_parts_mut(self.ptr.as_ptr(), self.len) }
    }

    /// Validate `[offset, offset + len)` against the window and return
    /// the page-aligned span of the OS mapping that covers it, as a
    /// start address and byte count, or `None` for an empty range.
    ///
    /// A returned span is non-empty, page aligned, and inside the live
    /// OS mapping: the precondition of every ranged `os::` call
    /// (`flush`, `advise`, `lock`).
    fn span(&self, offset: usize, len: usize) -> io::Result<Option<(*mut u8, usize)>> {
        // All validation happens in `flush_span` before any pointer
        // arithmetic. An empty mapping only accepts (0, 0) and returns
        // `None` without touching `ptr`.
        let page = os::page_size()?;
        let Some((start, count)) = range::flush_span(self.delta, self.len, offset, len, page)?
        else {
            return Ok(None);
        };
        // SAFETY: `count > 0` implies `self.len > 0`, so a real OS
        // mapping of `os_len >= delta + len` bytes starts at `base()`.
        // `flush_span` guarantees `start + count <= delta + len`, so
        // `base + start` is inside the mapping, and `start` is a
        // multiple of the page size, so the address is page aligned
        // (the mapping base is aligned to at least a page).
        let addr = unsafe { self.base().add(start) };
        Ok(Some((addr, count)))
    }

    fn flush(&self, offset: usize, len: usize, mode: FlushMode) -> io::Result<()> {
        let Some((addr, count)) = self.span(offset, len)? else {
            return Ok(());
        };
        // SAFETY: `span` returned a page-aligned, non-empty range inside
        // this live mapping, which is `os::flush`'s contract.
        unsafe { os::flush(addr, count, &self.backing, mode) }
    }

    #[cfg(feature = "advise")]
    fn advise(
        &self,
        advice: crate::advise::MmapAdvice,
        offset: usize,
        len: usize,
    ) -> io::Result<()> {
        // Validate first so a bad range is reported as a range error.
        let span = self.span(offset, len)?;
        if advice == crate::advise::MmapAdvice::DontNeed && self.kind.is_private() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "DontNeed is refused on private (copy-on-write or anonymous) mappings: \
                 it would discard the private pages while they may be borrowed",
            ));
        }
        let Some((addr, count)) = span else {
            return Ok(());
        };
        // SAFETY: page-aligned, non-empty range inside this live
        // mapping (`span`), and DontNeed on private memory was refused
        // above, which is `os::advise`'s contract.
        unsafe { os::advise(addr, count, advice) }
    }

    #[cfg(feature = "locking")]
    fn lock(&self, lock: bool) -> io::Result<()> {
        let Some((addr, count)) = self.span(0, self.len)? else {
            return Ok(());
        };
        // SAFETY: page-aligned, non-empty range inside this live
        // mapping (`span`), which is `os::lock`'s contract.
        unsafe { os::lock(addr, count, lock) }
    }

    /// Change the protection of the whole OS mapping. Takes `self` by
    /// value, so no borrow of the bytes can be alive. On error the
    /// mapping is dropped (unmapped) and the error returned.
    fn protect(self, prot: Protection) -> io::Result<Self> {
        if self.len == 0 {
            return Ok(self);
        }
        // SAFETY: `len > 0`, so `base()` / `os_len` describe exactly the
        // live OS mapping this value owns, and it is held by value, so
        // no Rust reference into it exists (`os::protect`'s contract).
        unsafe { os::protect(self.base(), self.os_len, prot)? };
        Ok(self)
    }

    /// Give the mapping write access again (or for the first time, for
    /// a `Kind::Read` shared file mapping).
    fn into_writable(mut self) -> io::Result<Self> {
        let prot = match self.kind {
            Kind::Read => {
                // Unix: flush must call msync from now on. Windows: a
                // PAGE_READONLY section cannot become writable; fails
                // here before any OS call.
                if self.len > 0 {
                    self.backing.mark_shared_writable()?;
                }
                self.kind = Kind::Write;
                Protection::ReadWrite
            }
            Kind::Write | Kind::Anon => Protection::ReadWrite,
            Kind::Copy => Protection::WriteCopy,
        };
        self.protect(prot)
    }
}

impl Drop for Mapping {
    fn drop(&mut self) {
        if self.len == 0 {
            return;
        }
        // SAFETY: `len > 0`, so `base()` is the start of a live OS
        // mapping of `os_len` bytes (at most isize::MAX, checked at
        // creation) owned by this value. `drop` runs at most once and
        // no borrow of the bytes can outlive `self`.
        unsafe { os::unmap(self.base(), self.os_len, &self.backing) };
    }
}

/// Builder for a mapping window: file offset, length, and optional
/// flags ([`populate`](Self::populate), [`huge`](Self::huge)).
///
/// Mirrors `memmap2::MmapOptions` for the options mmap-io uses
/// (`offset`, `len`, `populate`, `huge`). The constructors that map a file are `unsafe`;
/// see [`RawMmapOptions::map`] for the contract.
///
/// # Example
///
/// ```
/// use std::io::Write;
/// use mmap_io::raw::RawMmapOptions;
///
/// # fn main() -> std::io::Result<()> {
/// let mut file = tempfile::tempfile()?;
/// file.write_all(b"0123456789")?;
///
/// // SAFETY: the temporary file is private to this example.
/// let window = unsafe { RawMmapOptions::new().offset(3).len(4).map(&file)? };
/// assert_eq!(&window[..], b"3456");
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone, Default)]
pub struct RawMmapOptions {
    offset: u64,
    len: Option<usize>,
    flags: MapFlags,
}

impl RawMmapOptions {
    /// Options for a window starting at offset 0 and running to the end
    /// of the file.
    ///
    /// # Example
    ///
    /// ```
    /// let opts = mmap_io::raw::RawMmapOptions::new();
    /// # let _ = opts;
    /// ```
    #[must_use]
    pub const fn new() -> Self {
        Self {
            offset: 0,
            len: None,
            flags: MapFlags {
                populate: false,
                huge: false,
            },
        }
    }

    /// Start the window at byte `offset` of the file. Any value is
    /// accepted here; it is validated against the file size when the
    /// mapping is created. Ignored by [`map_anon`](Self::map_anon).
    ///
    /// # Example
    ///
    /// ```
    /// let mut opts = mmap_io::raw::RawMmapOptions::new();
    /// opts.offset(4096);
    /// ```
    pub fn offset(&mut self, offset: u64) -> &mut Self {
        self.offset = offset;
        self
    }

    /// Make the window `len` bytes long. Without this call the window
    /// runs from the offset to the end of the file. Required (else
    /// zero) for [`map_anon`](Self::map_anon).
    ///
    /// # Example
    ///
    /// ```
    /// let mut opts = mmap_io::raw::RawMmapOptions::new();
    /// opts.offset(10).len(20);
    /// ```
    pub fn len(&mut self, len: usize) -> &mut Self {
        self.len = Some(len);
        self
    }

    /// Pre-fault the mapping when it is created (`MAP_POPULATE` on
    /// Linux and Android): the `mmap` call itself reads every page in
    /// (file) or allocates it (anonymous), so first accesses do not
    /// fault. Creation takes longer and commits memory up front. On
    /// other platforms the option is accepted and has no effect.
    ///
    /// # Example
    ///
    /// ```
    /// let anon = mmap_io::raw::RawMmapOptions::new().len(8192).populate().map_anon()?;
    /// assert_eq!(anon.len(), 8192);
    /// # Ok::<(), std::io::Error>(())
    /// ```
    pub fn populate(&mut self) -> &mut Self {
        self.flags.populate = true;
        self
    }

    /// Back an anonymous mapping ([`map_anon`](Self::map_anon)) with
    /// explicit huge pages: `MAP_HUGETLB` on Linux and Android, using
    /// the default huge page size from `/proc/meminfo`. The OS mapping
    /// is rounded up to whole huge pages; [`RawMmapMut::len`] still
    /// reports the requested length.
    ///
    /// Huge pages must be reserved by the administrator
    /// (`vm.nr_hugepages`); without them `map_anon` fails with the OS
    /// error (usually `ENOMEM`) and does not fall back. Use
    /// `AnonymousMmap::with_huge_pages`
    /// (feature `hugepages`) for a mapping that falls back to normal
    /// pages. Ignored for file mappings and on other platforms (Windows
    /// large pages need `SeLockMemoryPrivilege` and are not used).
    ///
    /// # Example
    ///
    /// ```
    /// let mut opts = mmap_io::raw::RawMmapOptions::new();
    /// opts.len(2 * 1024 * 1024).huge();
    /// // On Linux this fails unless huge pages are reserved.
    /// if let Ok(map) = opts.map_anon() {
    ///     assert_eq!(map.len(), 2 * 1024 * 1024);
    /// }
    /// ```
    pub fn huge(&mut self) -> &mut Self {
        self.flags.huge = true;
        self
    }

    /// Validate the window against `file` and create the OS mapping.
    ///
    /// # Safety
    ///
    /// Same contract as [`RawMmapOptions::map`].
    unsafe fn map_impl(&self, file: &File, access: Access) -> io::Result<Mapping> {
        let gran = os::offset_granularity()?;
        let file_len = file.metadata()?.len();
        let len = range::resolve_len(file_len, self.offset, self.len, range::MAX_LEN)?;
        let layout = range::layout(self.offset, len, gran, range::MAX_LEN)?;
        let kind = match access {
            Access::Read => Kind::Read,
            Access::Write => Kind::Write,
            Access::Copy => Kind::Copy,
        };
        if layout.len == 0 {
            return Ok(Mapping::empty(gran, kind));
        }
        // SAFETY: `layout.len > 0` so `map_len > 0`; the window lies
        // inside the file (`resolve_len`) and `layout` aligned the
        // offset to `offset_granularity()`. The mapping is handed
        // straight to `Mapping`, which unmaps it exactly once.
        let (base, backing) = unsafe { os::map_file(file, access, &layout, self.flags)? };
        // SAFETY: `base` is a fresh mapping of `layout.map_len` bytes
        // with `layout.len > 0`, owned by nobody else.
        Ok(unsafe { Mapping::from_os(base, &layout, layout.map_len, backing, kind) })
    }

    /// Create a read-only, shared mapping of the window.
    ///
    /// Writes to the file made by other mappings or processes become
    /// visible through this mapping (it shares the page cache).
    ///
    /// # Safety
    ///
    /// The returned value hands out `&[u8]` that Rust assumes nothing
    /// else changes while it is borrowed. The OS cannot enforce that
    /// for a file, so the caller must guarantee that, for the lifetime
    /// of the mapping, the mapped range is neither modified nor
    /// truncated by:
    ///
    /// - another mapping of the same file in this process, while a
    ///   slice borrowed from this mapping is alive;
    /// - `write` / `set_len` / any other I/O on the file in this
    ///   process;
    /// - any other process.
    ///
    /// Concurrent modification is undefined behaviour under Rust's
    /// aliasing rules (in practice: torn or changing reads). Truncation
    /// below the mapped range makes later accesses raise `SIGBUS` on
    /// Unix or an `EXCEPTION_IN_PAGE_ERROR` on Windows (Windows refuses
    /// to shrink a file with a live view, with `ERROR_USER_MAPPED_FILE`,
    /// but other processes may still write to it). Typical ways to meet
    /// the contract are file permissions, advisory locks honoured by
    /// all writers, or a private temporary file.
    ///
    /// # Errors
    ///
    /// - `InvalidInput` if the offset is past the end of the file, if
    ///   `offset + len` overflows or exceeds the file size, or if the
    ///   window does not fit in this target's address space
    ///   (`isize::MAX`, relevant on 32-bit targets).
    /// - The OS error if reading the file metadata or creating the
    ///   mapping fails (for example a file not opened for reading).
    /// - `Unsupported` on targets without a mapping backend.
    ///
    /// # Example
    ///
    /// ```
    /// use std::io::Write;
    /// use mmap_io::raw::RawMmapOptions;
    ///
    /// # fn main() -> std::io::Result<()> {
    /// let mut file = tempfile::tempfile()?;
    /// file.write_all(b"abcdef")?;
    /// // SAFETY: the temporary file is private to this example.
    /// let map = unsafe { RawMmapOptions::new().offset(2).map(&file)? };
    /// assert_eq!(&map[..], b"cdef");
    /// # Ok(())
    /// # }
    /// ```
    pub unsafe fn map(&self, file: &File) -> io::Result<RawMmap> {
        // SAFETY: forwarded caller contract.
        unsafe { self.map_impl(file, Access::Read) }.map(|map| RawMmap { map })
    }

    /// Create a writable mapping of the window that is shared with the
    /// file: writes reach the file (after [`RawMmapMut::flush`] for
    /// durability) and are visible to other mappings of it.
    ///
    /// The file must be opened for reading and writing.
    ///
    /// # Safety
    ///
    /// Same contract as [`map`](Self::map). In addition, writes through
    /// this mapping are visible to other mappings of the file, so the
    /// caller must not write here while a slice borrowed from another
    /// mapping of the same range is alive.
    ///
    /// # Errors
    ///
    /// Same as [`map`](Self::map); a file opened read-only fails with
    /// the OS permission error.
    ///
    /// # Example
    ///
    /// ```
    /// use mmap_io::raw::RawMmapOptions;
    ///
    /// # fn main() -> std::io::Result<()> {
    /// let file = tempfile::tempfile()?;
    /// file.set_len(8192)?;
    /// // SAFETY: the temporary file is private to this example.
    /// let mut map = unsafe { RawMmapOptions::new().offset(4096).map_mut(&file)? };
    /// map[0] = 7;
    /// map.flush()?;
    /// # Ok(())
    /// # }
    /// ```
    pub unsafe fn map_mut(&self, file: &File) -> io::Result<RawMmapMut> {
        // SAFETY: forwarded caller contract.
        unsafe { self.map_impl(file, Access::Write) }.map(|map| RawMmapMut { map })
    }

    /// Create a private copy-on-write mapping of the window
    /// (`MAP_PRIVATE` on Unix, `PAGE_WRITECOPY` / `FILE_MAP_COPY` on
    /// Windows). Writes stay in this process and never reach the file;
    /// [`RawMmapMut::flush`] is a no-op that returns `Ok`.
    ///
    /// The file only needs to be opened for reading.
    ///
    /// # Safety
    ///
    /// Same contract as [`map`](Self::map). Pages that have not been
    /// written yet may still reflect later changes to the file (POSIX
    /// leaves this unspecified; Windows shows them), so outside
    /// modification is excluded for the same reason.
    ///
    /// # Errors
    ///
    /// Same as [`map`](Self::map).
    ///
    /// # Example
    ///
    /// ```
    /// use std::io::Write;
    /// use mmap_io::raw::RawMmapOptions;
    ///
    /// # fn main() -> std::io::Result<()> {
    /// let mut file = tempfile::tempfile()?;
    /// file.write_all(b"original")?;
    /// // SAFETY: the temporary file is private to this example.
    /// let mut cow = unsafe { RawMmapOptions::new().map_copy(&file)? };
    /// cow[..4].copy_from_slice(b"EDIT");
    /// assert_eq!(&cow[..], b"EDITinal");
    /// # Ok(())
    /// # }
    /// ```
    pub unsafe fn map_copy(&self, file: &File) -> io::Result<RawMmapMut> {
        // SAFETY: forwarded caller contract.
        unsafe { self.map_impl(file, Access::Copy) }.map(|map| RawMmapMut { map })
    }

    /// Create a private, zero-filled anonymous mapping of the configured
    /// length (zero if [`len`](Self::len) was not called). The offset is
    /// ignored.
    ///
    /// # Errors
    ///
    /// - `InvalidInput` if the length exceeds `isize::MAX` (after
    ///   rounding up to the huge page size when [`huge`](Self::huge) is
    ///   set).
    /// - The OS error if the kernel refuses the allocation, including
    ///   a [`huge`](Self::huge) request with no huge pages available.
    /// - `Unsupported` on targets without a mapping backend.
    ///
    /// # Example
    ///
    /// ```
    /// let mut anon = mmap_io::raw::RawMmapOptions::new().len(4096).map_anon()?;
    /// assert!(anon.iter().all(|&b| b == 0));
    /// anon[0] = 1;
    /// # Ok::<(), std::io::Error>(())
    /// ```
    pub fn map_anon(&self) -> io::Result<RawMmapMut> {
        let gran = os::offset_granularity()?;
        let layout = range::layout(0, self.len.unwrap_or(0), gran, range::MAX_LEN)?;
        if layout.len == 0 {
            return Ok(RawMmapMut {
                map: Mapping::empty(gran, Kind::Anon),
            });
        }
        // SAFETY: `layout.map_len == layout.len > 0` and at most
        // isize::MAX. The mapping is handed straight to `Mapping`.
        let (base, backing, os_len) = unsafe { os::map_anon(layout.map_len, self.flags)? };
        // SAFETY: fresh mapping of `os_len >= map_len` bytes with
        // `len > 0`, owned by nobody else.
        Ok(RawMmapMut {
            map: unsafe { Mapping::from_os(base, &layout, os_len, backing, Kind::Anon) },
        })
    }
}

/// A read-only, shared memory mapping of a file window.
///
/// Derefs to `[u8]`. Created with [`RawMmap::map`] or
/// [`RawMmapOptions::map`]. The mapping is released when the value is
/// dropped; it does not keep the `File` open in Rust terms, but the OS
/// keeps the underlying file alive until the mapping is gone.
///
/// `RawMmap` is `Send + Sync`: any number of threads may read it
/// concurrently.
///
/// # Example
///
/// ```
/// use std::io::Write;
/// use mmap_io::raw::RawMmap;
///
/// # fn main() -> std::io::Result<()> {
/// let mut file = tempfile::tempfile()?;
/// file.write_all(b"data")?;
/// // SAFETY: the temporary file is private to this example.
/// let map = unsafe { RawMmap::map(&file)? };
/// drop(file); // the mapping stays valid
/// assert_eq!(&*map, b"data");
/// # Ok(())
/// # }
/// ```
pub struct RawMmap {
    map: Mapping,
}

impl RawMmap {
    /// Map the whole file read-only. Equivalent to
    /// `RawMmapOptions::new().map(file)`.
    ///
    /// # Safety
    ///
    /// See [`RawMmapOptions::map`].
    ///
    /// # Errors
    ///
    /// See [`RawMmapOptions::map`].
    ///
    /// # Example
    ///
    /// ```
    /// use std::io::Write;
    /// # fn main() -> std::io::Result<()> {
    /// let mut file = tempfile::tempfile()?;
    /// file.write_all(b"xyz")?;
    /// // SAFETY: the temporary file is private to this example.
    /// let map = unsafe { mmap_io::raw::RawMmap::map(&file)? };
    /// assert_eq!(map.len(), 3);
    /// # Ok(())
    /// # }
    /// ```
    pub unsafe fn map(file: &File) -> io::Result<Self> {
        // SAFETY: forwarded caller contract.
        unsafe { RawMmapOptions::new().map(file) }
    }

    /// Length of the window in bytes.
    ///
    /// # Example
    ///
    /// ```
    /// # fn main() -> std::io::Result<()> {
    /// let file = tempfile::tempfile()?;
    /// // SAFETY: the temporary file is private to this example.
    /// let map = unsafe { mmap_io::raw::RawMmap::map(&file)? };
    /// assert_eq!(map.len(), 0);
    /// # Ok(())
    /// # }
    /// ```
    #[inline]
    #[must_use]
    pub fn len(&self) -> usize {
        self.map.len
    }

    /// Whether the window is empty (no OS mapping exists).
    ///
    /// # Example
    ///
    /// ```
    /// # fn main() -> std::io::Result<()> {
    /// let file = tempfile::tempfile()?;
    /// // SAFETY: the temporary file is private to this example.
    /// let map = unsafe { mmap_io::raw::RawMmap::map(&file)? };
    /// assert!(map.is_empty());
    /// # Ok(())
    /// # }
    /// ```
    #[inline]
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.map.len == 0
    }

    /// Pointer to the first byte of the window. For an empty window
    /// this is a non-null, aligned address that must not be read.
    ///
    /// # Example
    ///
    /// ```
    /// use std::io::Write;
    /// # fn main() -> std::io::Result<()> {
    /// let mut file = tempfile::tempfile()?;
    /// file.write_all(b"q")?;
    /// // SAFETY: the temporary file is private to this example.
    /// let map = unsafe { mmap_io::raw::RawMmap::map(&file)? };
    /// // SAFETY: the window is one byte long and alive.
    /// assert_eq!(unsafe { *map.as_ptr() }, b'q');
    /// # Ok(())
    /// # }
    /// ```
    #[inline]
    #[must_use]
    pub fn as_ptr(&self) -> *const u8 {
        self.map.ptr.as_ptr().cast_const()
    }
}

impl RawMmap {
    /// Give the mapping write access, consuming it (`mprotect` on Unix,
    /// `VirtualProtect` on Windows).
    ///
    /// For a mapping that came from [`RawMmapMut::make_read_only`] this
    /// restores the original access: shared writes reach the file,
    /// copy-on-write and anonymous mappings stay private.
    ///
    /// For a mapping created read-only ([`RawMmap::map`] /
    /// [`RawMmapOptions::map`]):
    ///
    /// - **Unix**: the file must have been opened for writing, or the
    ///   call fails with the OS permission error (`EACCES`). The result
    ///   is a shared writable mapping; [`RawMmapMut::flush`] writes it
    ///   back. The writes are visible to every other mapping of the file,
    ///   which is covered by the `# Safety` contract the mapping was
    ///   created under: nothing else may hold a borrow of those bytes
    ///   while they are written.
    /// - **Windows**: always fails with `io::ErrorKind::Unsupported`. The
    ///   view belongs to a `PAGE_READONLY` section, which can never be
    ///   made writable. Map with [`RawMmapOptions::map_mut`] and use
    ///   [`RawMmapMut::make_read_only`] when a mapping has to switch.
    ///
    /// An empty mapping is converted without a syscall.
    ///
    /// # Errors
    ///
    /// The OS error from `mprotect` / `VirtualProtect`, or `Unsupported`
    /// as described above. The mapping is released on error.
    ///
    /// # Example
    ///
    /// ```
    /// # fn main() -> std::io::Result<()> {
    /// let mut anon = mmap_io::raw::RawMmapMut::map_anon(4096)?;
    /// anon[0] = 1;
    /// let ro = anon.make_read_only()?;
    /// assert_eq!(ro[0], 1);
    /// let mut rw = ro.make_mut()?;
    /// rw[0] = 2;
    /// assert_eq!(rw[0], 2);
    /// # Ok(())
    /// # }
    /// ```
    pub fn make_mut(self) -> io::Result<RawMmapMut> {
        let RawMmap { map } = self;
        map.into_writable().map(|map| RawMmapMut { map })
    }

    /// Advise the kernel about the expected access pattern for the
    /// whole mapping. See [`advise_range`](Self::advise_range).
    ///
    /// # Errors
    ///
    /// See [`advise_range`](Self::advise_range).
    ///
    /// # Example
    ///
    /// ```
    /// use std::io::Write;
    /// use mmap_io::MmapAdvice;
    /// # fn main() -> std::io::Result<()> {
    /// let mut file = tempfile::tempfile()?;
    /// file.write_all(&[7u8; 8192])?;
    /// // SAFETY: the temporary file is private to this example.
    /// let map = unsafe { mmap_io::raw::RawMmap::map(&file)? };
    /// map.advise(MmapAdvice::Sequential)?;
    /// # Ok(())
    /// # }
    /// ```
    #[cfg(feature = "advise")]
    pub fn advise(&self, advice: crate::advise::MmapAdvice) -> io::Result<()> {
        self.map.advise(advice, 0, self.map.len)
    }

    /// Advise the kernel about the expected access pattern for
    /// `[offset, offset + len)` of the window.
    ///
    /// The start is widened down to a page boundary, as `madvise`
    /// requires; the end is not widened past the window. A zero-length
    /// range is a validated no-op.
    ///
    /// - **Unix**: `madvise` with the matching `MADV_*` constant.
    /// - **Windows**: `WillNeed` calls `PrefetchVirtualMemory`; the
    ///   other hints are accepted and do nothing.
    ///
    /// `DontNeed` is refused with `InvalidInput` on private mappings
    /// (a [`RawMmap`] made read-only from a copy-on-write or anonymous
    /// [`RawMmapMut`]): on private memory it discards the pages, so the
    /// bytes behind live `&[u8]` borrows would change. On shared file
    /// mappings it only drops the page table entries and the bytes are
    /// read back unchanged.
    ///
    /// # Errors
    ///
    /// - `InvalidInput` if `offset > self.len()` or
    ///   `len > self.len() - offset` (checked before any syscall), or
    ///   for `DontNeed` on a private mapping.
    /// - The OS error from `madvise` / `PrefetchVirtualMemory`.
    ///
    /// # Example
    ///
    /// ```
    /// use std::io::Write;
    /// use mmap_io::MmapAdvice;
    /// # fn main() -> std::io::Result<()> {
    /// let mut file = tempfile::tempfile()?;
    /// file.write_all(&[7u8; 8192])?;
    /// // SAFETY: the temporary file is private to this example.
    /// let map = unsafe { mmap_io::raw::RawMmap::map(&file)? };
    /// map.advise_range(MmapAdvice::WillNeed, 100, 5000)?;
    /// assert!(map.advise_range(MmapAdvice::WillNeed, 8000, 500).is_err());
    /// # Ok(())
    /// # }
    /// ```
    #[cfg(feature = "advise")]
    pub fn advise_range(
        &self,
        advice: crate::advise::MmapAdvice,
        offset: usize,
        len: usize,
    ) -> io::Result<()> {
        self.map.advise(advice, offset, len)
    }

    /// Lock the pages of the window in RAM (`mlock` on Unix,
    /// `VirtualLock` on Windows), so they are not paged out. Usually
    /// needs privileges or a raised `RLIMIT_MEMLOCK` / working-set size.
    /// The pages are unlocked by [`unlock`](Self::unlock) or when the
    /// mapping is dropped. An empty mapping is a no-op.
    ///
    /// # Errors
    ///
    /// The OS error (for example `EPERM` / `ENOMEM` on Linux).
    ///
    /// # Example
    ///
    /// ```
    /// use std::io::Write;
    /// # fn main() -> std::io::Result<()> {
    /// let mut file = tempfile::tempfile()?;
    /// file.write_all(&[1u8; 4096])?;
    /// // SAFETY: the temporary file is private to this example.
    /// let map = unsafe { mmap_io::raw::RawMmap::map(&file)? };
    /// if map.lock().is_ok() {
    ///     map.unlock()?;
    /// }
    /// # Ok(())
    /// # }
    /// ```
    #[cfg(feature = "locking")]
    pub fn lock(&self) -> io::Result<()> {
        self.map.lock(true)
    }

    /// Unlock pages locked by [`lock`](Self::lock) (`munlock` /
    /// `VirtualUnlock`). Unlocking pages that are not locked succeeds.
    ///
    /// # Errors
    ///
    /// The OS error.
    ///
    /// # Example
    ///
    /// ```
    /// use std::io::Write;
    /// # fn main() -> std::io::Result<()> {
    /// let mut file = tempfile::tempfile()?;
    /// file.write_all(&[1u8; 16])?;
    /// // SAFETY: the temporary file is private to this example.
    /// let map = unsafe { mmap_io::raw::RawMmap::map(&file)? };
    /// map.unlock()?;
    /// # Ok(())
    /// # }
    /// ```
    #[cfg(feature = "locking")]
    pub fn unlock(&self) -> io::Result<()> {
        self.map.lock(false)
    }
}

impl Deref for RawMmap {
    type Target = [u8];

    #[inline]
    fn deref(&self) -> &[u8] {
        self.map.as_slice()
    }
}

impl AsRef<[u8]> for RawMmap {
    #[inline]
    fn as_ref(&self) -> &[u8] {
        self.map.as_slice()
    }
}

impl fmt::Debug for RawMmap {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RawMmap")
            .field("ptr", &self.as_ptr())
            .field("len", &self.len())
            .finish()
    }
}

/// A writable memory mapping: shared with a file, private
/// copy-on-write, or anonymous.
///
/// Derefs to `[u8]` and `mut [u8]`. Created with
/// [`RawMmapMut::map_mut`], [`RawMmapMut::map_anon`],
/// [`RawMmapOptions::map_mut`], [`RawMmapOptions::map_copy`] or
/// [`RawMmapOptions::map_anon`].
///
/// `RawMmapMut` is `Send + Sync`. Mutable access requires `&mut self`,
/// so concurrent writers need external synchronisation (for example a
/// lock around the value, as `MemoryMappedFile` does).
///
/// # Example
///
/// ```
/// # fn main() -> std::io::Result<()> {
/// let file = tempfile::tempfile()?;
/// file.set_len(4)?;
/// // SAFETY: the temporary file is private to this example.
/// let mut map = unsafe { mmap_io::raw::RawMmapMut::map_mut(&file)? };
/// map.copy_from_slice(b"abcd");
/// map.flush()?;
/// # Ok(())
/// # }
/// ```
pub struct RawMmapMut {
    map: Mapping,
}

impl RawMmapMut {
    /// Map the whole file read-write, shared with the file. Equivalent
    /// to `RawMmapOptions::new().map_mut(file)`.
    ///
    /// # Safety
    ///
    /// See [`RawMmapOptions::map_mut`].
    ///
    /// # Errors
    ///
    /// See [`RawMmapOptions::map_mut`].
    ///
    /// # Example
    ///
    /// ```
    /// # fn main() -> std::io::Result<()> {
    /// let file = tempfile::tempfile()?;
    /// file.set_len(1)?;
    /// // SAFETY: the temporary file is private to this example.
    /// let mut map = unsafe { mmap_io::raw::RawMmapMut::map_mut(&file)? };
    /// map[0] = 0xAB;
    /// # Ok(())
    /// # }
    /// ```
    pub unsafe fn map_mut(file: &File) -> io::Result<Self> {
        // SAFETY: forwarded caller contract.
        unsafe { RawMmapOptions::new().map_mut(file) }
    }

    /// Create a private, zero-filled anonymous mapping of `len` bytes.
    /// Equivalent to `RawMmapOptions::new().len(len).map_anon()`. A
    /// length of zero returns an empty mapping without a syscall.
    ///
    /// # Errors
    ///
    /// See [`RawMmapOptions::map_anon`].
    ///
    /// # Example
    ///
    /// ```
    /// let mut anon = mmap_io::raw::RawMmapMut::map_anon(64)?;
    /// anon[63] = 1;
    /// assert_eq!(anon.len(), 64);
    /// # Ok::<(), std::io::Error>(())
    /// ```
    pub fn map_anon(len: usize) -> io::Result<Self> {
        RawMmapOptions::new().len(len).map_anon()
    }

    /// Length of the window in bytes.
    ///
    /// # Example
    ///
    /// ```
    /// let anon = mmap_io::raw::RawMmapMut::map_anon(10)?;
    /// assert_eq!(anon.len(), 10);
    /// # Ok::<(), std::io::Error>(())
    /// ```
    #[inline]
    #[must_use]
    pub fn len(&self) -> usize {
        self.map.len
    }

    /// Whether the window is empty (no OS mapping exists).
    ///
    /// # Example
    ///
    /// ```
    /// let anon = mmap_io::raw::RawMmapMut::map_anon(0)?;
    /// assert!(anon.is_empty());
    /// # Ok::<(), std::io::Error>(())
    /// ```
    #[inline]
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.map.len == 0
    }

    /// Pointer to the first byte of the window. For an empty window
    /// this is a non-null, aligned address that must not be accessed.
    ///
    /// # Example
    ///
    /// ```
    /// let anon = mmap_io::raw::RawMmapMut::map_anon(8)?;
    /// assert!(!anon.as_ptr().is_null());
    /// # Ok::<(), std::io::Error>(())
    /// ```
    #[inline]
    #[must_use]
    pub fn as_ptr(&self) -> *const u8 {
        self.map.ptr.as_ptr().cast_const()
    }

    /// Mutable pointer to the first byte of the window. Takes
    /// `&mut self`, matching `<[u8]>::as_mut_ptr`, so the pointer is
    /// derived from exclusive access.
    ///
    /// # Example
    ///
    /// ```
    /// let mut anon = mmap_io::raw::RawMmapMut::map_anon(8)?;
    /// let p = anon.as_mut_ptr();
    /// // SAFETY: the window is 8 bytes long and exclusively borrowed.
    /// unsafe { p.write(5) };
    /// assert_eq!(anon[0], 5);
    /// # Ok::<(), std::io::Error>(())
    /// ```
    #[inline]
    #[must_use]
    pub fn as_mut_ptr(&mut self) -> *mut u8 {
        self.map.ptr.as_ptr()
    }

    /// Durably write all modified pages of the window to the file.
    ///
    /// Unix: `msync(MS_SYNC)` over the page-aligned window. Windows:
    /// `FlushViewOfFile` followed by `FlushFileBuffers` on a duplicate
    /// of the file handle; `FlushViewOfFile` alone only hands the pages
    /// to the file system cache and does not wait for the disk. On
    /// return without error the data is on stable storage as far as
    /// the OS can tell (macOS: like `fsync`, this does not force the
    /// drive's own cache; use `F_FULLFSYNC` on the file for that).
    /// File metadata such as the modification time may not be updated.
    ///
    /// Copy-on-write and anonymous mappings have nothing to write back;
    /// for them this returns `Ok(())` without a syscall.
    ///
    /// # Errors
    ///
    /// The OS error if write-back fails.
    ///
    /// # Example
    ///
    /// ```
    /// # fn main() -> std::io::Result<()> {
    /// let file = tempfile::tempfile()?;
    /// file.set_len(16)?;
    /// // SAFETY: the temporary file is private to this example.
    /// let mut map = unsafe { mmap_io::raw::RawMmapMut::map_mut(&file)? };
    /// map[0] = 1;
    /// map.flush()?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn flush(&self) -> io::Result<()> {
        self.map.flush(0, self.map.len, FlushMode::Sync)
    }

    /// Start writing all modified pages of the window back to the file
    /// without waiting for completion (`msync(MS_ASYNC)` on Unix,
    /// `FlushViewOfFile` on Windows). Not durable on return.
    ///
    /// # Errors
    ///
    /// The OS error if the request fails.
    ///
    /// # Example
    ///
    /// ```
    /// # fn main() -> std::io::Result<()> {
    /// let file = tempfile::tempfile()?;
    /// file.set_len(16)?;
    /// // SAFETY: the temporary file is private to this example.
    /// let map = unsafe { mmap_io::raw::RawMmapMut::map_mut(&file)? };
    /// map.flush_async()?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn flush_async(&self) -> io::Result<()> {
        self.map.flush(0, self.map.len, FlushMode::Async)
    }

    /// Durably write the modified pages that overlap
    /// `[offset, offset + len)` of the window. Same mechanism and
    /// guarantees as [`flush`](Self::flush); the range is widened to
    /// page boundaries, so neighbouring bytes on the same pages may be
    /// written as well. `len == 0` is a validated no-op.
    ///
    /// # Errors
    ///
    /// - `InvalidInput` if `offset > self.len()` or
    ///   `len > self.len() - offset`. This is checked before any pointer
    ///   arithmetic or syscall.
    /// - The OS error if write-back fails.
    ///
    /// # Example
    ///
    /// ```
    /// # fn main() -> std::io::Result<()> {
    /// let file = tempfile::tempfile()?;
    /// file.set_len(10_000)?;
    /// // SAFETY: the temporary file is private to this example.
    /// let mut map = unsafe { mmap_io::raw::RawMmapMut::map_mut(&file)? };
    /// map[5000] = 9;
    /// map.flush_range(5000, 1)?;
    /// assert!(map.flush_range(9_999, 2).is_err());
    /// # Ok(())
    /// # }
    /// ```
    pub fn flush_range(&self, offset: usize, len: usize) -> io::Result<()> {
        self.map.flush(offset, len, FlushMode::Sync)
    }

    /// Asynchronous variant of [`flush_range`](Self::flush_range), with
    /// the semantics of [`flush_async`](Self::flush_async).
    ///
    /// # Errors
    ///
    /// Same as [`flush_range`](Self::flush_range).
    ///
    /// # Example
    ///
    /// ```
    /// # fn main() -> std::io::Result<()> {
    /// let file = tempfile::tempfile()?;
    /// file.set_len(64)?;
    /// // SAFETY: the temporary file is private to this example.
    /// let map = unsafe { mmap_io::raw::RawMmapMut::map_mut(&file)? };
    /// map.flush_async_range(8, 8)?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn flush_async_range(&self, offset: usize, len: usize) -> io::Result<()> {
        self.map.flush(offset, len, FlushMode::Async)
    }
}

impl RawMmapMut {
    /// Remove write access, consuming the mapping (`mprotect(PROT_READ)`
    /// on Unix, `VirtualProtect(PAGE_READONLY)` on Windows). The result
    /// can be made writable again with [`RawMmap::make_mut`], which
    /// restores the original access (shared, copy-on-write, or
    /// anonymous).
    ///
    /// Pending writes of a shared file mapping are not flushed by this
    /// call; call [`flush`](Self::flush) first if they must be durable.
    /// An empty mapping is converted without a syscall.
    ///
    /// # Errors
    ///
    /// The OS error from `mprotect` / `VirtualProtect`. The mapping is
    /// released on error.
    ///
    /// # Example
    ///
    /// ```
    /// # fn main() -> std::io::Result<()> {
    /// let mut anon = mmap_io::raw::RawMmapMut::map_anon(16)?;
    /// anon[..3].copy_from_slice(b"abc");
    /// let ro = anon.make_read_only()?;
    /// assert_eq!(&ro[..3], b"abc");
    /// # Ok(())
    /// # }
    /// ```
    pub fn make_read_only(self) -> io::Result<RawMmap> {
        let RawMmapMut { map } = self;
        map.protect(Protection::ReadOnly).map(|map| RawMmap { map })
    }

    /// Advise the kernel about the expected access pattern for the
    /// whole mapping. See [`advise_range`](Self::advise_range).
    ///
    /// # Errors
    ///
    /// See [`advise_range`](Self::advise_range).
    ///
    /// # Example
    ///
    /// ```
    /// use mmap_io::MmapAdvice;
    /// let anon = mmap_io::raw::RawMmapMut::map_anon(8192)?;
    /// anon.advise(MmapAdvice::Random)?;
    /// // DontNeed would discard private pages; it is refused.
    /// assert!(anon.advise(MmapAdvice::DontNeed).is_err());
    /// # Ok::<(), std::io::Error>(())
    /// ```
    #[cfg(feature = "advise")]
    pub fn advise(&self, advice: crate::advise::MmapAdvice) -> io::Result<()> {
        self.map.advise(advice, 0, self.map.len)
    }

    /// Advise the kernel about the expected access pattern for
    /// `[offset, offset + len)` of the window. Same validation and
    /// platform behavior as [`RawMmap::advise_range`].
    ///
    /// `DontNeed` is refused with `InvalidInput` on copy-on-write
    /// ([`RawMmapOptions::map_copy`]) and anonymous mappings, where it
    /// would discard private writes and change bytes that `&self`
    /// borrows may be reading. It is allowed on shared file mappings
    /// ([`RawMmapOptions::map_mut`]), where dirty data stays in the page
    /// cache and the bytes do not change.
    ///
    /// # Errors
    ///
    /// - `InvalidInput` for an out-of-range request (checked before any
    ///   syscall) or `DontNeed` on a private mapping.
    /// - The OS error from `madvise` / `PrefetchVirtualMemory`.
    ///
    /// # Example
    ///
    /// ```
    /// use mmap_io::MmapAdvice;
    /// let anon = mmap_io::raw::RawMmapMut::map_anon(3 * 4096)?;
    /// anon.advise_range(MmapAdvice::WillNeed, 4097, 100)?;
    /// assert!(anon.advise_range(MmapAdvice::WillNeed, 3 * 4096, 1).is_err());
    /// # Ok::<(), std::io::Error>(())
    /// ```
    #[cfg(feature = "advise")]
    pub fn advise_range(
        &self,
        advice: crate::advise::MmapAdvice,
        offset: usize,
        len: usize,
    ) -> io::Result<()> {
        self.map.advise(advice, offset, len)
    }

    /// Lock the pages of the window in RAM. Same behavior as
    /// [`RawMmap::lock`].
    ///
    /// # Errors
    ///
    /// The OS error (for example `EPERM` / `ENOMEM` on Linux).
    ///
    /// # Example
    ///
    /// ```
    /// let anon = mmap_io::raw::RawMmapMut::map_anon(4096)?;
    /// if anon.lock().is_ok() {
    ///     anon.unlock()?;
    /// }
    /// # Ok::<(), std::io::Error>(())
    /// ```
    #[cfg(feature = "locking")]
    pub fn lock(&self) -> io::Result<()> {
        self.map.lock(true)
    }

    /// Unlock pages locked by [`lock`](Self::lock). Unlocking pages
    /// that are not locked succeeds.
    ///
    /// # Errors
    ///
    /// The OS error.
    ///
    /// # Example
    ///
    /// ```
    /// let anon = mmap_io::raw::RawMmapMut::map_anon(4096)?;
    /// anon.unlock()?;
    /// # Ok::<(), std::io::Error>(())
    /// ```
    #[cfg(feature = "locking")]
    pub fn unlock(&self) -> io::Result<()> {
        self.map.lock(false)
    }
}

impl Deref for RawMmapMut {
    type Target = [u8];

    #[inline]
    fn deref(&self) -> &[u8] {
        self.map.as_slice()
    }
}

impl DerefMut for RawMmapMut {
    #[inline]
    fn deref_mut(&mut self) -> &mut [u8] {
        self.map.as_mut_slice()
    }
}

impl AsRef<[u8]> for RawMmapMut {
    #[inline]
    fn as_ref(&self) -> &[u8] {
        self.map.as_slice()
    }
}

impl AsMut<[u8]> for RawMmapMut {
    #[inline]
    fn as_mut(&mut self) -> &mut [u8] {
        self.map.as_mut_slice()
    }
}

impl fmt::Debug for RawMmapMut {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RawMmapMut")
            .field("ptr", &self.as_ptr())
            .field("len", &self.len())
            .finish()
    }
}

#[cfg(test)]
mod tests;
