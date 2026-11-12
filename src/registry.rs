//! Process-wide registry of the files mapped by [`MemoryMappedFile`]
//! (since 1.2.0).
//!
//! Every `MemoryMappedFile` keeps a [`Claim`] on the file it maps,
//! keyed by the file's identity (device and inode on Unix, volume
//! serial number and file id on Windows), not by its path. Clones of one
//! handle share one claim. The registry is what lets the safe
//! constructors refuse to create a second mapping that could change or
//! truncate the bytes behind a live `&[u8]` of the first:
//!
//! - a read claim (`ReadOnly`, `CopyOnWrite`) is refused while a write
//!   claim on the same file exists;
//! - a write claim (`ReadWrite`) is refused while any other claim on the
//!   same file exists.
//!
//! A claim taken through
//! [`MemoryMappedFileBuilder::allow_aliasing`](crate::mmap::MemoryMappedFileBuilder::allow_aliasing)
//! is not checked, but it is still recorded, so later checked opens see
//! it.
//!
//! The registry only knows about mappings made by this crate in this
//! process. Other processes, and `std::fs` writes in this process, are
//! outside it; see `docs/SAFETY.md`.
//!
//! [`MemoryMappedFile`]: crate::MemoryMappedFile

use std::collections::HashMap;
use std::fs::File;
use std::io;
use std::sync::{Mutex, MutexGuard, OnceLock, PoisonError};

/// Identity of an open file, independent of the path used to open it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct FileId {
    volume: u64,
    file: u128,
}

/// What a mapping can do to the bytes of its file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Access {
    /// The mapping never changes the file (`ReadOnly`, `CopyOnWrite`).
    Read,
    /// The mapping writes to the file and may resize it (`ReadWrite`).
    Write,
}

#[derive(Debug, Default)]
struct Counts {
    readers: usize,
    writers: usize,
}

impl Counts {
    fn total(&self) -> usize {
        self.readers + self.writers
    }
}

type Table = HashMap<FileId, Counts>;

fn table() -> MutexGuard<'static, Table> {
    static TABLE: OnceLock<Mutex<Table>> = OnceLock::new();
    TABLE
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        // The table holds plain counters and every update completes
        // before the guard is released, so a poisoned lock still holds a
        // consistent table.
        .unwrap_or_else(PoisonError::into_inner)
}

/// Why a checked claim was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Conflict {
    /// A writable mapping of the file exists.
    Writer,
    /// Another mapping of the file exists and a writable one was asked
    /// for.
    Mapped,
}

impl Conflict {
    pub(crate) fn message(self) -> &'static str {
        match self {
            Conflict::Writer => {
                "the file is already mapped read-write by another MemoryMappedFile in this \
                 process; a second mapping could see its bytes change under a borrow \
                 (open it through the existing handle, or see MemoryMappedFileBuilder::allow_aliasing)"
            }
            Conflict::Mapped => {
                "the file is already mapped by another MemoryMappedFile in this process; a \
                 read-write mapping needs exclusive access (open it through the existing \
                 handle, or see MemoryMappedFileBuilder::allow_aliasing)"
            }
        }
    }
}

/// Result of [`Claim::acquire`].
pub(crate) enum Acquired {
    Claimed(Claim),
    Refused(Conflict),
}

/// One mapping's registration. Released when dropped.
#[derive(Debug)]
pub(crate) struct Claim {
    /// `None` on targets where the file identity cannot be read; such
    /// claims are not tracked.
    id: Option<FileId>,
    access: Access,
    checked: bool,
}

impl Claim {
    /// Register a mapping of `file` with `access`. With `checked`, refuse
    /// it if it conflicts with a mapping that already exists.
    ///
    /// # Errors
    ///
    /// The OS error if the file's identity cannot be read.
    pub(crate) fn acquire(file: &File, access: Access, checked: bool) -> io::Result<Acquired> {
        let id = file_id(file)?;
        let Some(key) = id else {
            return Ok(Acquired::Claimed(Claim {
                id,
                access,
                checked,
            }));
        };
        let mut table = table();
        let counts = table.entry(key).or_default();
        if checked {
            let conflict = match access {
                Access::Read if counts.writers > 0 => Some(Conflict::Writer),
                Access::Write if counts.total() > 0 => Some(Conflict::Mapped),
                _ => None,
            };
            if let Some(conflict) = conflict {
                if counts.total() == 0 {
                    table.remove(&key);
                }
                return Ok(Acquired::Refused(conflict));
            }
        }
        match access {
            Access::Read => counts.readers += 1,
            Access::Write => counts.writers += 1,
        }
        Ok(Acquired::Claimed(Claim {
            id,
            access,
            checked,
        }))
    }

    /// Whether the claim was checked for conflicts when it was taken.
    pub(crate) fn is_checked(&self) -> bool {
        self.checked
    }

    /// Whether another mapping (any other `MemoryMappedFile`, not a clone
    /// of this one) of the same file is registered right now.
    pub(crate) fn has_others(&self) -> bool {
        let Some(key) = self.id else {
            return false;
        };
        table().get(&key).is_some_and(|c| c.total() > 1)
    }
}

impl Drop for Claim {
    fn drop(&mut self) {
        let Some(key) = self.id else {
            return;
        };
        let mut table = table();
        if let Some(counts) = table.get_mut(&key) {
            match self.access {
                Access::Read => counts.readers = counts.readers.saturating_sub(1),
                Access::Write => counts.writers = counts.writers.saturating_sub(1),
            }
            if counts.total() == 0 {
                table.remove(&key);
            }
        }
    }
}

/// Device and inode of `file`.
#[cfg(unix)]
fn file_id(file: &File) -> io::Result<Option<FileId>> {
    use std::os::unix::fs::MetadataExt;
    let meta = file.metadata()?;
    Ok(Some(FileId {
        volume: meta.dev(),
        file: u128::from(meta.ino()),
    }))
}

/// Volume serial number and file id of `file`.
#[cfg(windows)]
fn file_id(file: &File) -> io::Result<Option<FileId>> {
    use std::ffi::c_void;
    use std::os::windows::io::AsRawHandle;

    /// `FILE_ID_INFO` (winbase.h).
    #[repr(C)]
    struct FileIdInfo {
        volume_serial_number: u64,
        file_id: [u8; 16],
    }

    /// `BY_HANDLE_FILE_INFORMATION` (fileapi.h). The three `FILETIME`s
    /// are two `DWORD`s each.
    #[repr(C)]
    struct ByHandleFileInformation {
        file_attributes: u32,
        times: [u32; 6],
        volume_serial_number: u32,
        file_size_high: u32,
        file_size_low: u32,
        number_of_links: u32,
        file_index_high: u32,
        file_index_low: u32,
    }

    /// `FileIdInfo` in `FILE_INFO_BY_HANDLE_CLASS`.
    const FILE_ID_INFO_CLASS: i32 = 18;

    extern "system" {
        fn GetFileInformationByHandleEx(
            file: *mut c_void,
            class: i32,
            info: *mut c_void,
            size: u32,
        ) -> i32;
        fn GetFileInformationByHandle(file: *mut c_void, info: *mut ByHandleFileInformation)
            -> i32;
    }

    let handle = file.as_raw_handle();
    let mut info = FileIdInfo {
        volume_serial_number: 0,
        file_id: [0; 16],
    };
    // SAFETY: `handle` is the open handle owned by `file`, which is
    // borrowed for the call. `info` is a writable `FILE_ID_INFO` and the
    // size passed is its exact size, as `GetFileInformationByHandleEx`
    // requires for the `FileIdInfo` class. The call only writes `info`.
    // Reference: https://learn.microsoft.com/en-us/windows/win32/api/winbase/nf-winbase-getfileinformationbyhandleex
    let ok = unsafe {
        GetFileInformationByHandleEx(
            handle.cast(),
            FILE_ID_INFO_CLASS,
            (&mut info as *mut FileIdInfo).cast(),
            std::mem::size_of::<FileIdInfo>() as u32,
        )
    };
    if ok != 0 {
        return Ok(Some(FileId {
            volume: info.volume_serial_number,
            file: u128::from_le_bytes(info.file_id),
        }));
    }
    // File systems without 128-bit ids (FAT, some network file
    // systems) still report the 64-bit index.
    let mut info = ByHandleFileInformation {
        file_attributes: 0,
        times: [0; 6],
        volume_serial_number: 0,
        file_size_high: 0,
        file_size_low: 0,
        number_of_links: 0,
        file_index_high: 0,
        file_index_low: 0,
    };
    // SAFETY: `handle` is the open handle owned by `file`, borrowed for
    // the call, and `info` is a writable `BY_HANDLE_FILE_INFORMATION`.
    // The call only writes `info`.
    // Reference: https://learn.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-getfileinformationbyhandle
    let ok = unsafe { GetFileInformationByHandle(handle.cast(), &mut info) };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(Some(FileId {
        volume: u64::from(info.volume_serial_number),
        file: (u128::from(info.file_index_high) << 32) | u128::from(info.file_index_low),
    }))
}

/// No file identity on this target; nothing can be mapped here anyway.
#[cfg(not(any(unix, windows)))]
fn file_id(_file: &File) -> io::Result<Option<FileId>> {
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn claim(file: &File, access: Access, checked: bool) -> Option<Claim> {
        match Claim::acquire(file, access, checked).expect("file id") {
            Acquired::Claimed(c) => Some(c),
            Acquired::Refused(_) => None,
        }
    }

    #[cfg_attr(miri, ignore = "file metadata syscalls are not supported by Miri")]
    #[test]
    fn readers_share_writers_exclude() {
        let file = tempfile::tempfile().expect("tempfile");
        let other = file.try_clone().expect("clone handle");
        let r1 = claim(&file, Access::Read, true).expect("first reader");
        let r2 = claim(&other, Access::Read, true).expect("readers share");
        assert!(r1.has_others());
        assert!(claim(&file, Access::Write, true).is_none());
        drop((r1, r2));
        let w = claim(&file, Access::Write, true).expect("alone");
        assert!(!w.has_others());
        assert!(claim(&other, Access::Read, true).is_none());
        assert!(claim(&other, Access::Write, true).is_none());
        let unchecked = claim(&other, Access::Read, false).expect("unchecked");
        assert!(!unchecked.is_checked());
        assert!(w.has_others());
        drop(unchecked);
        assert!(!w.has_others());
        drop(w);
        assert!(claim(&file, Access::Write, true).is_some());
    }

    #[cfg_attr(miri, ignore = "file metadata syscalls are not supported by Miri")]
    #[test]
    fn distinct_files_do_not_conflict() {
        let a = tempfile::tempfile().expect("tempfile");
        let b = tempfile::tempfile().expect("tempfile");
        let _wa = claim(&a, Access::Write, true).expect("a");
        let _wb = claim(&b, Access::Write, true).expect("b");
    }
}
