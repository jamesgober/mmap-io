//! Fallback backend for targets that are neither Unix nor Windows
//! (for example `wasm32-unknown-unknown`). Every mapping constructor
//! returns `io::ErrorKind::Unsupported`, including zero-length
//! windows (the constructors query [`offset_granularity`] first), so
//! the crate still compiles and callers get an error instead of a
//! link failure.

use std::fs::File;
use std::io;
use std::ptr::NonNull;

use super::range::Layout;
use super::{Access, FlushMode};

/// No mapping can exist on this target, so there is nothing to back.
#[derive(Debug)]
pub(crate) enum Backing {
    /// The only variant; never constructed for a real mapping.
    Private,
}

fn unsupported() -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        "memory mapping is not supported on this target",
    )
}

/// Always fails with `Unsupported`.
pub(crate) fn page_size() -> io::Result<usize> {
    Err(unsupported())
}

/// Always fails with `Unsupported`; every constructor calls this
/// before anything else, so no mapping value is ever created.
pub(crate) fn offset_granularity() -> io::Result<usize> {
    Err(unsupported())
}

/// Always fails with `Unsupported`.
///
/// # Safety
///
/// Trivially safe; `unsafe` only to match the other backends.
pub(crate) unsafe fn map_file(
    _file: &File,
    _access: Access,
    _layout: &Layout,
) -> io::Result<(NonNull<u8>, Backing)> {
    Err(unsupported())
}

/// Always fails with `Unsupported`.
///
/// # Safety
///
/// Trivially safe; `unsafe` only to match the other backends.
pub(crate) unsafe fn map_anon(_map_len: usize) -> io::Result<(NonNull<u8>, Backing)> {
    Err(unsupported())
}

/// Never reached: no non-empty mapping exists on this target.
///
/// # Safety
///
/// Trivially safe; `unsafe` only to match the other backends.
pub(crate) unsafe fn flush(
    _addr: *mut u8,
    _count: usize,
    _backing: &Backing,
    _mode: FlushMode,
) -> io::Result<()> {
    Err(unsupported())
}

/// Never reached: no non-empty mapping exists on this target.
///
/// # Safety
///
/// Trivially safe; `unsafe` only to match the other backends.
pub(crate) unsafe fn unmap(_base: *mut u8, _map_len: usize, _backing: &Backing) {}
