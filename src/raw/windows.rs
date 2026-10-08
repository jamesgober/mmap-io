//! Windows backend for [`crate::raw`]: `CreateFileMappingW`,
//! `MapViewOfFile`, `FlushViewOfFile`, `UnmapViewOfFile`.
//!
//! The crate does not depend on `windows-sys`; the handful of
//! kernel32 entry points used here are declared by hand, following
//! the existing pattern in `src/utils.rs`, `src/lock.rs` and
//! `src/advise.rs`. kernel32 is always linked by `std` on Windows, so
//! no `#[link]` attribute is needed.
//!
//! References (MSDN):
//! - CreateFileMappingW: <https://learn.microsoft.com/en-us/windows/win32/api/memoryapi/nf-memoryapi-createfilemappingw>
//! - MapViewOfFile: <https://learn.microsoft.com/en-us/windows/win32/api/memoryapi/nf-memoryapi-mapviewoffile>
//! - FlushViewOfFile: <https://learn.microsoft.com/en-us/windows/win32/api/memoryapi/nf-memoryapi-flushviewoffile>
//! - UnmapViewOfFile: <https://learn.microsoft.com/en-us/windows/win32/api/memoryapi/nf-memoryapi-unmapviewoffile>
//! - GetSystemInfo: <https://learn.microsoft.com/en-us/windows/win32/api/sysinfoapi/nf-sysinfoapi-getsysteminfo>

use std::ffi::c_void;
use std::fs::File;
use std::io;
use std::mem::MaybeUninit;
use std::os::windows::io::AsRawHandle;
use std::ptr::{self, NonNull};
use std::sync::OnceLock;

use super::range::Layout;
use super::{Access, FlushMode, MapFlags, Protection};

type Handle = *mut c_void;
type Bool = i32;
type Dword = u32;

// Page protection and view access constants (winnt.h / memoryapi.h).
const PAGE_READONLY: Dword = 0x02;
const PAGE_READWRITE: Dword = 0x04;
const PAGE_WRITECOPY: Dword = 0x08;
const FILE_MAP_COPY: Dword = 0x0001;
const FILE_MAP_WRITE: Dword = 0x0002;
const FILE_MAP_READ: Dword = 0x0004;

/// `INVALID_HANDLE_VALUE`: passed to `CreateFileMappingW` to request a
/// section backed by the system paging file (an anonymous mapping).
const INVALID_HANDLE_VALUE: Handle = -1isize as Handle;

/// Layout of `SYSTEM_INFO` (sysinfoapi.h). Field names are snake_case
/// because `#[repr(C)]` layout depends only on order and types.
#[repr(C)]
struct SystemInfo {
    processor_architecture: u16,
    reserved: u16,
    page_size: Dword,
    minimum_application_address: *mut c_void,
    maximum_application_address: *mut c_void,
    active_processor_mask: usize,
    number_of_processors: Dword,
    processor_type: Dword,
    allocation_granularity: Dword,
    processor_level: u16,
    processor_revision: u16,
}

extern "system" {
    fn CreateFileMappingW(
        file: Handle,
        attributes: *mut c_void,
        protect: Dword,
        maximum_size_high: Dword,
        maximum_size_low: Dword,
        name: *const u16,
    ) -> Handle;
    fn MapViewOfFile(
        mapping: Handle,
        desired_access: Dword,
        offset_high: Dword,
        offset_low: Dword,
        bytes_to_map: usize,
    ) -> *mut c_void;
    fn FlushViewOfFile(base: *const c_void, bytes_to_flush: usize) -> Bool;
    fn UnmapViewOfFile(base: *const c_void) -> Bool;
    fn CloseHandle(object: Handle) -> Bool;
    fn GetSystemInfo(info: *mut SystemInfo);
    fn VirtualProtect(
        address: *mut c_void,
        size: usize,
        new_protect: Dword,
        old_protect: *mut Dword,
    ) -> Bool;
}

/// How a view is backed, which decides what `flush` must do.
#[derive(Debug)]
pub(crate) enum Backing {
    /// Read-only, copy-on-write, or paging-file (anonymous) view:
    /// nothing to write back, so flush is a validated no-op.
    Private,
    /// Shared writable file view. Holds a duplicate of the caller's
    /// file handle (`File::try_clone`, i.e. `DuplicateHandle` with
    /// `DUPLICATE_SAME_ACCESS`) so a durable flush can call
    /// `FlushFileBuffers` after the caller's `File` is gone. The
    /// duplicate is closed when the mapping drops. (Approach follows
    /// memmap2, MIT/Apache-2.0.)
    Shared(File),
}

impl Backing {
    /// A read-only view comes from a `PAGE_READONLY` section, whose
    /// views can never be given write access (`VirtualProtect` fails),
    /// and it holds no file handle for a durable flush. Always fails.
    pub(crate) fn mark_shared_writable(&mut self) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "a read-only file mapping cannot be made writable on Windows: its section \
             was created with PAGE_READONLY; map the file with map_mut and use \
             make_read_only / make_mut to switch",
        ))
    }
}

#[derive(Clone, Copy)]
struct Granularity {
    page: usize,
    allocation: usize,
}

static GRANULARITY: OnceLock<Granularity> = OnceLock::new();

fn granularity() -> Granularity {
    *GRANULARITY.get_or_init(|| {
        let mut info = MaybeUninit::<SystemInfo>::uninit();
        // SAFETY: `GetSystemInfo` writes a complete SYSTEM_INFO into
        // caller-provided storage and has no failure mode (MSDN). The
        // pointer is to a correctly sized and aligned `MaybeUninit`
        // slot, so `assume_init` afterwards is sound.
        let info = unsafe {
            GetSystemInfo(info.as_mut_ptr());
            info.assume_init()
        };
        // u32 -> usize is lossless on every Windows target (32 or
        // 64-bit pointers). A zero or non-power-of-two value is
        // rejected by the callers below rather than divided by.
        Granularity {
            page: info.page_size as usize,
            allocation: info.allocation_granularity as usize,
        }
    })
}

/// System page size, validated to be a non-zero power of two.
pub(crate) fn page_size() -> io::Result<usize> {
    let page = granularity().page;
    if page.is_power_of_two() {
        Ok(page)
    } else {
        Err(io::Error::other(format!(
            "GetSystemInfo reported an invalid page size ({page})"
        )))
    }
}

/// Offset granularity for `MapViewOfFile`: the system allocation
/// granularity (typically 64 KiB), not the page size. MSDN: the
/// combination of the high and low offsets "must be a multiple of the
/// memory allocation granularity of the system".
pub(crate) fn offset_granularity() -> io::Result<usize> {
    let gran = granularity().allocation;
    if gran.is_power_of_two() {
        Ok(gran)
    } else {
        Err(io::Error::other(format!(
            "GetSystemInfo reported an invalid allocation granularity ({gran})"
        )))
    }
}

/// Split a 64-bit value into the (high, low) DWORD pair Win32 expects.
#[inline]
fn split_u64(value: u64) -> (Dword, Dword) {
    // Shifting right by 32 leaves at most 32 significant bits and the
    // mask keeps exactly 32, so both casts are lossless.
    ((value >> 32) as Dword, (value & 0xFFFF_FFFF) as Dword)
}

/// Create a section for `source` and map one view of it.
///
/// # Safety
///
/// `source` must be a valid file handle (or `INVALID_HANDLE_VALUE`
/// with a non-zero `max_size` for a paging-file section). `map_len`
/// must be non-zero and the view `[offset, offset + map_len)` must lie
/// inside the section. `offset` must be a multiple of the allocation
/// granularity.
unsafe fn map_view(
    source: Handle,
    protect: Dword,
    access: Dword,
    max_size: u64,
    offset: u64,
    map_len: usize,
) -> io::Result<NonNull<u8>> {
    let (max_high, max_low) = split_u64(max_size);
    // SAFETY: `source` is valid per the caller's contract. A null
    // security-attributes pointer and a null name are documented as
    // "default security" and "unnamed object". A maximum size of 0
    // means "the current size of the file" (the file must be non-empty,
    // which the caller guarantees by never mapping a zero-length
    // window). Failure returns NULL and sets the thread's last error.
    let section = unsafe {
        CreateFileMappingW(
            source,
            ptr::null_mut(),
            protect,
            max_high,
            max_low,
            ptr::null(),
        )
    };
    if section.is_null() {
        return Err(io::Error::last_os_error());
    }
    let (off_high, off_low) = split_u64(offset);
    // SAFETY: `section` is the live section handle created above.
    // `offset` is a multiple of the allocation granularity and the view
    // lies inside the section (caller contract), which are MSDN's
    // preconditions; violations would only make the call fail, not
    // corrupt memory. A successful call returns the base of a fresh
    // view that no other code references.
    let view = unsafe { MapViewOfFile(section, access, off_high, off_low, map_len) };
    // Capture the error before CloseHandle can overwrite the thread's
    // last-error value.
    let map_err = if view.is_null() {
        Some(io::Error::last_os_error())
    } else {
        None
    };
    // SAFETY: `section` is a handle this function owns and closes
    // exactly once. MSDN (CreateFileMappingW): mapped views hold their
    // own reference to the section, so closing the handle here does not
    // invalidate `view`; the section is destroyed when the last view is
    // unmapped. Closing early means no section handle is held for the
    // mapping's lifetime. A close failure is not actionable here.
    unsafe { CloseHandle(section) };
    if let Some(err) = map_err {
        return Err(err);
    }
    match NonNull::new(view.cast::<u8>()) {
        Some(base) => Ok(base),
        // Unreachable: null was handled above. Kept as an error path
        // so there is no panic and no unchecked conversion.
        None => Err(io::Error::other("MapViewOfFile returned a null view")),
    }
}

/// Map `layout` of `file` with the requested access.
///
/// `extra` is accepted for parity with the Unix backend; Windows has
/// no equivalent of `MAP_POPULATE` or `MAP_HUGETLB` for file views.
///
/// # Safety
///
/// `layout.map_len` must be non-zero and `layout` must have come from
/// [`super::range::layout`] for a window inside the file. The caller
/// owns the view and must release it with [`unmap`] exactly once.
pub(crate) unsafe fn map_file(
    file: &File,
    access: Access,
    layout: &Layout,
    extra: MapFlags,
) -> io::Result<(NonNull<u8>, Backing)> {
    let _ = extra;
    let (protect, view_access, backing) = match access {
        Access::Read => (PAGE_READONLY, FILE_MAP_READ, Backing::Private),
        Access::Write => (
            PAGE_READWRITE,
            FILE_MAP_READ | FILE_MAP_WRITE,
            // Duplicate before mapping so a failure here leaves
            // nothing to undo.
            Backing::Shared(file.try_clone()?),
        ),
        Access::Copy => (PAGE_WRITECOPY, FILE_MAP_COPY, Backing::Private),
    };
    // SAFETY: the handle is borrowed from a live `File`. The window
    // `[aligned_offset, aligned_offset + map_len)` lies inside the file
    // (resolve_len + layout), map_len is non-zero, and aligned_offset is
    // a multiple of `offset_granularity()`; together these satisfy
    // `map_view`'s contract. A maximum size of 0 maps the current file
    // size without growing the file.
    let base = unsafe {
        map_view(
            file.as_raw_handle(),
            protect,
            view_access,
            0,
            layout.aligned_offset,
            layout.map_len,
        )?
    };
    Ok((base, backing))
}

/// Create a private, zero-filled read-write mapping of `map_len`
/// bytes backed by the system paging file. Returns the view, its
/// backing, and the view length (always `map_len`: large pages need
/// `SeLockMemoryPrivilege` and are not used, so `extra.huge` and
/// `extra.populate` are ignored).
///
/// # Safety
///
/// `map_len` must be non-zero. The caller owns the view and must
/// release it with [`unmap`] exactly once.
pub(crate) unsafe fn map_anon(
    map_len: usize,
    extra: MapFlags,
) -> io::Result<(NonNull<u8>, Backing, usize)> {
    let _ = extra;
    let size = u64::try_from(map_len).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("anonymous mapping length {map_len} does not fit in u64"),
        )
    })?;
    // SAFETY: INVALID_HANDLE_VALUE with an explicit non-zero maximum
    // size asks for a paging-file-backed section of exactly `size`
    // bytes (MSDN). Such sections are zero-initialised. The view covers
    // the whole section from offset 0, which is trivially aligned.
    let base = unsafe {
        map_view(
            INVALID_HANDLE_VALUE,
            PAGE_READWRITE,
            FILE_MAP_READ | FILE_MAP_WRITE,
            size,
            0,
            map_len,
        )?
    };
    Ok((base, Backing::Private, map_len))
}

/// Write back `count` bytes starting at `addr`.
///
/// `FlushMode::Async` calls `FlushViewOfFile` only. MSDN: that call
/// writes the dirty pages to the file system but "does not flush the
/// file metadata, and it does not wait to return until the changes are
/// flushed from the underlying hardware disk cache". `FlushMode::Sync`
/// therefore follows it with `FlushFileBuffers` on the duplicated file
/// handle (via `File::sync_data`), which is what makes the flush
/// durable.
///
/// # Safety
///
/// `count` must be non-zero and `[addr, addr + count)` must lie inside
/// a live view created by this module. Both hold when `addr`/`count`
/// come from [`super::range::flush_span`] applied to that view.
pub(crate) unsafe fn flush(
    addr: *mut u8,
    count: usize,
    backing: &Backing,
    mode: FlushMode,
) -> io::Result<()> {
    match backing {
        Backing::Private => Ok(()),
        Backing::Shared(file) => {
            // SAFETY: the range lies inside a live view (caller
            // contract) and `count` is non-zero; MSDN documents that a
            // count of zero means "to the end of the mapping", which
            // `flush_span` never produces. FlushViewOfFile only writes
            // pages back; it does not touch their contents.
            let ok = unsafe { FlushViewOfFile(addr.cast::<c_void>().cast_const(), count) };
            if ok == 0 {
                return Err(io::Error::last_os_error());
            }
            match mode {
                FlushMode::Async => Ok(()),
                // `File::sync_data` is `FlushFileBuffers` on Windows.
                FlushMode::Sync => file.sync_data(),
            }
        }
    }
}

/// Change the protection of a whole view.
///
/// # Safety
///
/// `base` / `os_len` must describe exactly one live view created by
/// [`map_file`] or [`map_anon`]. The caller must own the view by value
/// with no Rust reference into it alive, because removing write access
/// while a `&mut [u8]` exists, or adding it while other code relies on
/// the bytes being read-only, would break those references.
pub(crate) unsafe fn protect(base: *mut u8, os_len: usize, prot: Protection) -> io::Result<()> {
    let new_protect = match prot {
        Protection::ReadOnly => PAGE_READONLY,
        Protection::ReadWrite => PAGE_READWRITE,
        Protection::WriteCopy => PAGE_WRITECOPY,
    };
    let mut old: Dword = 0;
    // SAFETY: MSDN (VirtualProtect): every page of
    // `[base, base + os_len)` must lie in one region reserved by
    // `MapViewOfFile`, which holds because `base` is the start of one
    // live view of `os_len` bytes (caller contract). `old` is a valid,
    // writable `DWORD` that the call fills in (it must not be null).
    // A protection the view's section does not allow (for example
    // write access on a view of a PAGE_READONLY section) makes the call
    // fail instead of changing anything. No Rust reference into the
    // view is alive (caller contract).
    let ok = unsafe { VirtualProtect(base.cast::<c_void>(), os_len, new_protect, &mut old) };
    if ok == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

/// Apply `advice` to `count` bytes starting at `addr`. Only `WillNeed`
/// does something on Windows (`PrefetchVirtualMemory`); the others are
/// accepted and ignored.
///
/// # Safety
///
/// `[addr, addr + count)` must lie inside a live view created by this
/// module, with `count > 0`.
#[cfg(feature = "advise")]
pub(crate) unsafe fn advise(
    addr: *mut u8,
    count: usize,
    advice: crate::advise::MmapAdvice,
) -> io::Result<()> {
    #[repr(C)]
    struct MemoryRangeEntry {
        virtual_address: *mut c_void,
        number_of_bytes: usize,
    }
    extern "system" {
        fn PrefetchVirtualMemory(
            process: Handle,
            number_of_entries: usize,
            virtual_addresses: *const MemoryRangeEntry,
            flags: Dword,
        ) -> Bool;
        fn GetCurrentProcess() -> Handle;
    }
    if advice != crate::advise::MmapAdvice::WillNeed {
        return Ok(());
    }
    let entry = MemoryRangeEntry {
        virtual_address: addr.cast::<c_void>(),
        number_of_bytes: count,
    };
    // SAFETY: `GetCurrentProcess` returns a pseudo-handle with full
    // rights. One entry is passed and `entry` lives for the call; it
    // describes a range inside a live view (caller contract). The
    // reserved flags argument is 0. The call only asks the memory
    // manager to read the pages in; it does not retain the pointer or
    // modify the bytes.
    // Reference: https://learn.microsoft.com/en-us/windows/win32/api/memoryapi/nf-memoryapi-prefetchvirtualmemory
    let ok = unsafe { PrefetchVirtualMemory(GetCurrentProcess(), 1, &entry, 0) };
    if ok == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

/// Lock (`lock == true`) or unlock `count` bytes at `addr` in the
/// working set (`VirtualLock` / `VirtualUnlock`). Unlocking pages that
/// are not locked (`ERROR_NOT_LOCKED`) is treated as success.
///
/// # Safety
///
/// `[addr, addr + count)` must lie inside a live view created by this
/// module, with `count > 0`.
#[cfg(feature = "locking")]
pub(crate) unsafe fn lock(addr: *mut u8, count: usize, lock: bool) -> io::Result<()> {
    extern "system" {
        fn VirtualLock(address: *const c_void, size: usize) -> Bool;
        fn VirtualUnlock(address: *const c_void, size: usize) -> Bool;
    }
    const ERROR_NOT_LOCKED: i32 = 158;
    let addr = addr.cast::<c_void>().cast_const();
    // SAFETY: the range is non-empty and inside one live view (caller
    // contract), as VirtualLock / VirtualUnlock require. Neither call
    // reads or writes the bytes; they only change working-set pinning.
    let ok = unsafe {
        if lock {
            VirtualLock(addr, count)
        } else {
            VirtualUnlock(addr, count)
        }
    };
    if ok != 0 {
        return Ok(());
    }
    let err = io::Error::last_os_error();
    if !lock && err.raw_os_error() == Some(ERROR_NOT_LOCKED) {
        Ok(())
    } else {
        Err(err)
    }
}

/// Release a view. Errors are ignored: this runs from `Drop`, and
/// `UnmapViewOfFile` only fails for addresses this module never
/// produces. Dropping `backing` afterwards closes the duplicated file
/// handle, if any.
///
/// # Safety
///
/// `base` must be the start of exactly one live view created by
/// [`map_file`] or [`map_anon`], and no reference into it may outlive
/// this call.
pub(crate) unsafe fn unmap(base: *mut u8, _map_len: usize, _backing: &Backing) {
    // SAFETY: forwarded from the caller's contract. `base` is the
    // address MapViewOfFile returned, which is what UnmapViewOfFile
    // requires; the whole view is released.
    unsafe { UnmapViewOfFile(base.cast::<c_void>().cast_const()) };
}
