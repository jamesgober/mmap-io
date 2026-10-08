//! Helpers shared by the integration test binaries.
//!
//! Every test file that touches the filesystem gets its paths from
//! [`tmp_path`], which places the file in a fresh, uniquely named
//! temporary directory. The directory (and everything in it) is
//! removed when the returned [`TmpPath`] is dropped, so tests never
//! write into the working directory, never collide with a parallel
//! test or a concurrent `cargo test` run, and never leave files
//! behind on a panic.

// Each integration test binary compiles its own copy of this module
// and uses a different subset of it; unused helpers in one binary are
// expected, so the lint is silenced for the whole module.
#![allow(dead_code)]

use std::ops::Deref;
use std::path::{Path, PathBuf};

/// A path inside a private temporary directory. Dereferences to
/// [`Path`]; the directory is deleted when this value is dropped.
///
/// Declare the `TmpPath` before any mapping of the file, so the
/// mapping is dropped first: Windows cannot delete a mapped file.
#[derive(Debug)]
pub struct TmpPath {
    path: PathBuf,
    dir: tempfile::TempDir,
}

impl TmpPath {
    /// The private directory that holds the file.
    pub fn dir(&self) -> &Path {
        self.dir.path()
    }

    /// Another path in the same private directory.
    pub fn sibling(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }
}

impl Deref for TmpPath {
    type Target = Path;

    fn deref(&self) -> &Path {
        &self.path
    }
}

impl AsRef<Path> for TmpPath {
    fn as_ref(&self) -> &Path {
        &self.path
    }
}

/// A not-yet-existing file called `name` in a new private temporary
/// directory.
pub fn tmp_path(name: &str) -> TmpPath {
    let dir = tempfile::Builder::new()
        .prefix("mmap-io-test-")
        .tempdir()
        .expect("create private temp dir");
    let path = dir.path().join(name);
    TmpPath { path, dir }
}

/// Deterministic, position-dependent byte pattern. `seed` shifts the
/// pattern so two writes of the same length differ.
pub fn pattern(len: usize, seed: u8) -> Vec<u8> {
    (0..len)
        .map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed) ^ ((i >> 8) as u8))
        .collect()
}

/// The OS page size, as `u64`.
pub fn page() -> u64 {
    mmap_io::utils::page_size() as u64
}

/// The OS offset granularity for file mappings (the page size on
/// Unix, the 64 KiB allocation granularity on Windows).
pub fn granularity() -> u64 {
    mmap_io::raw::offset_granularity().expect("offset granularity") as u64
}

/// Interesting mapping sizes around the page and granularity
/// boundaries, deduplicated and sorted.
pub fn boundary_sizes() -> Vec<u64> {
    let p = page();
    let g = granularity();
    let mut v = vec![1, 2, p - 1, p, p + 1, 2 * p, g - 1, g, g + 1, 3 * g + 7];
    v.sort_unstable();
    v.dedup();
    v
}

/// Read the whole file through a fresh `std::fs` handle.
pub fn read_file(path: &Path) -> Vec<u8> {
    std::fs::read(path).expect("read file")
}

/// Whether the current process ignores file permission bits (for
/// example when running as root). Permission tests are skipped then,
/// since the OS would let the "forbidden" operation through.
pub fn permissions_are_enforced(readonly_file: &Path) -> bool {
    std::fs::OpenOptions::new()
        .write(true)
        .open(readonly_file)
        .is_err()
}

/// Mark `path` read-only (or writable again).
pub fn set_readonly(path: &Path, readonly: bool) {
    let mut perms = std::fs::metadata(path).expect("metadata").permissions();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        perms.set_mode(if readonly { 0o444 } else { 0o644 });
    }
    #[cfg(not(unix))]
    perms.set_readonly(readonly);
    std::fs::set_permissions(path, perms).expect("set permissions");
}

/// Poll `pred` until it returns `true` or `timeout` elapses. Returns
/// the final value of `pred`. Used only where the event being waited
/// for has no synchronous hook (OS notifications, the `EveryMillis`
/// background thread); the timeout is generous so a slow CI machine
/// does not fail the test.
pub fn wait_until<F: FnMut() -> bool>(timeout: std::time::Duration, mut pred: F) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        if pred() {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    pred()
}

/// Block until the `EveryMillis` background flusher of `mmap` has
/// flushed every pending byte and its flush call has returned.
///
/// The flusher zeroes `pending_bytes()` just before it issues the
/// flush syscall, under a read guard. Taking (and dropping) a
/// zero-length mutable slice needs the write lock, so it waits for
/// that flush to finish.
pub fn wait_for_background_flush(mmap: &mmap_io::MemoryMappedFile) {
    assert!(
        wait_until(std::time::Duration::from_secs(10), || mmap.pending_bytes()
            == 0),
        "EveryMillis flusher did not run within 10 s (pending = {})",
        mmap.pending_bytes()
    );
    drop(mmap.as_slice_mut(0, 0).expect("write-lock barrier"));
}
