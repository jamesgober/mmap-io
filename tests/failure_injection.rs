//! Error paths that only an exhausted resource can reach: the kernel
//! refusing a mapping (address-space limit) and refusing to lock pages
//! (memlock limit).
//!
//! Each case runs in a child copy of this test binary started through
//! `sh -c 'ulimit ...; exec ...'`, so the limit applies to the child
//! only. The child role is selected with `MMAP_IO_FI_ROLE`; without it
//! the `child` test does nothing. Linux only: macOS does not enforce
//! `RLIMIT_AS`, and Windows has no `ulimit`.
//!
//! Sanitizer builds reserve terabytes of shadow memory and cannot start
//! under an address-space limit; when the child does not report that it
//! started, the parent test is skipped with a message instead of
//! failing.

#![cfg(all(target_os = "linux", target_pointer_width = "64"))]

use std::process::Command;

mod common;

const ROLE: &str = "MMAP_IO_FI_ROLE";
const DIR: &str = "MMAP_IO_FI_DIR";
const STARTED: &str = "FI-CHILD-STARTED";
const PASSED: &str = "FI-CHILD-PASSED";

/// Address-space limit for the child, in KiB. Large enough for the test
/// harness and a debug binary, far below the 4 GiB the child asks for.
const AS_LIMIT_KIB: u64 = 1 << 20; // 1 GiB
const TOO_BIG: u64 = 4 << 30; // 4 GiB

/// Run the `child` test of this binary with `role`, under `ulimit_args`.
/// Returns `None` if the child could not start under the limit.
fn run_child(role: &str, ulimit_args: &str) -> Option<String> {
    let dir = common::tmp_path("fi");
    let exe = std::env::current_exe().expect("current_exe");
    let out = Command::new("sh")
        .arg("-c")
        .arg(format!("ulimit {ulimit_args} && exec \"$0\" \"$@\""))
        .arg(&exe)
        .args(["child", "--exact", "--nocapture", "--test-threads=1"])
        .env(ROLE, role)
        .env(DIR, dir.dir())
        .output()
        .expect("spawn sh");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    if !stdout.contains(STARTED) {
        eprintln!("skipped: child could not start under `ulimit {ulimit_args}`:\n{stderr}");
        return None;
    }
    assert!(
        out.status.success() && stdout.contains(PASSED),
        "child role {role} failed\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    Some(stdout)
}

#[cfg_attr(miri, ignore = "spawns child processes, which Miri does not support")]
#[test]
fn grow_failure_restores_the_file_and_keeps_the_mapping() {
    run_child("grow", &format!("-v {AS_LIMIT_KIB}"));
}

#[cfg_attr(miri, ignore = "spawns child processes, which Miri does not support")]
#[test]
fn create_and_anonymous_failures_are_io_errors() {
    run_child("create", &format!("-v {AS_LIMIT_KIB}"));
}

#[cfg_attr(miri, ignore = "spawns child processes, which Miri does not support")]
#[test]
fn raw_layer_failures_are_io_errors() {
    run_child("raw", &format!("-v {AS_LIMIT_KIB}"));
}

#[cfg(feature = "locking")]
#[cfg_attr(miri, ignore = "spawns child processes, which Miri does not support")]
#[test]
fn lock_failure_is_lock_failed() {
    if let Some(out) = run_child("lock", "-l 0") {
        if out.contains("FI-LOCK-PRIVILEGED") {
            eprintln!("skipped: process may lock memory regardless of RLIMIT_MEMLOCK");
        }
    }
}

/// Entry point for the child process. A no-op in a normal test run.
#[cfg_attr(miri, ignore = "spawns child processes, which Miri does not support")]
#[test]
fn child() {
    let Ok(role) = std::env::var(ROLE) else {
        return;
    };
    let dir = std::path::PathBuf::from(std::env::var(DIR).expect("dir"));
    println!("{STARTED}");
    match role.as_str() {
        "grow" => child_grow(&dir),
        "create" => child_create(&dir),
        "raw" => child_raw(&dir),
        #[cfg(feature = "locking")]
        "lock" => child_lock(&dir),
        other => panic!("unknown role {other}"),
    }
    println!("{PASSED}");
}

fn child_grow(dir: &std::path::Path) {
    use mmap_io::{MemoryMappedFile, MmapIoError};
    let path = dir.join("grow.bin");
    let m = MemoryMappedFile::create_rw(&path, 4096).unwrap();
    m.update_region(0, b"before").unwrap();
    match m.resize(TOO_BIG) {
        Err(MmapIoError::Io(e)) => println!("resize failed as expected: {e}"),
        other => panic!("resize past the address-space limit: {other:?}"),
    }
    // The file is put back to the size the live mapping covers, and the
    // mapping is untouched and still usable.
    assert_eq!(std::fs::metadata(&path).unwrap().len(), 4096);
    assert_eq!(m.len(), 4096);
    assert_eq!(m.as_slice(0, 6).unwrap(), b"before");
    m.update_region(4090, b"after!").unwrap();
    m.flush().unwrap();
    m.resize(8192).unwrap();
    assert_eq!(m.as_slice(4090, 6).unwrap(), b"after!");
}

fn child_create(dir: &std::path::Path) {
    use mmap_io::{AnonymousMmap, MemoryMappedFile, MmapIoError};
    let path = dir.join("create.bin");
    match MemoryMappedFile::create_rw(&path, TOO_BIG) {
        Err(MmapIoError::Io(e)) => println!("create_rw failed as expected: {e}"),
        other => panic!("create_rw past the limit: {other:?}"),
    }
    match MemoryMappedFile::open_rw(&path) {
        Err(MmapIoError::Io(_)) => {}
        Err(MmapIoError::ResizeFailed(_)) => {}
        other => panic!("open_rw of a too-large file: {other:?}"),
    }
    match MemoryMappedFile::open_ro(&path) {
        Err(MmapIoError::Io(_)) => {}
        other => panic!("open_ro of a too-large file: {other:?}"),
    }
    match AnonymousMmap::new(TOO_BIG) {
        Err(MmapIoError::Io(e)) => println!("AnonymousMmap::new failed as expected: {e}"),
        other => panic!("AnonymousMmap past the limit: {other:?}"),
    }
    // Normal sizes still work in the same process.
    let ok = MemoryMappedFile::create_rw(dir.join("small.bin"), 4096).unwrap();
    ok.update_region(0, b"fine").unwrap();
    AnonymousMmap::new(4096).unwrap();
}

fn child_raw(dir: &std::path::Path) {
    use mmap_io::raw::{RawMmap, RawMmapMut, RawMmapOptions};
    let path = dir.join("raw.bin");
    let f = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(&path)
        .unwrap();
    f.set_len(TOO_BIG).unwrap();
    // SAFETY: private file in a private directory, not modified while
    // mapped (every mapping attempt here is expected to fail anyway).
    unsafe {
        assert!(RawMmap::map(&f).is_err());
        assert!(RawMmapMut::map_mut(&f).is_err());
        assert!(RawMmapOptions::new().map_copy(&f).is_err());
        // A small window of the same file maps fine.
        let w = RawMmapOptions::new()
            .offset(TOO_BIG - 10)
            .len(10)
            .map(&f)
            .unwrap();
        assert_eq!(&w[..], &[0u8; 10]);
    }
    assert!(RawMmapMut::map_anon(TOO_BIG as usize).is_err());
    assert!(RawMmapOptions::new()
        .len(TOO_BIG as usize)
        .map_anon()
        .is_err());
}

#[cfg(feature = "locking")]
fn child_lock(dir: &std::path::Path) {
    use mmap_io::{MemoryMappedFile, MmapIoError};
    let m = MemoryMappedFile::create_rw(dir.join("lock.bin"), 1 << 20).unwrap();
    match m.lock(0, 1 << 20) {
        Err(MmapIoError::LockFailed(msg)) => {
            assert!(msg.contains("mlock"), "{msg}");
            // Nothing was locked; unlocking is still fine.
            m.unlock(0, 1 << 20).unwrap();
        }
        Ok(()) => {
            println!("FI-LOCK-PRIVILEGED");
            m.unlock_all().unwrap();
        }
        Err(e) => panic!("unexpected {e}"),
    }
    match m.lock_all() {
        Err(MmapIoError::LockFailed(_)) | Ok(()) => {}
        Err(e) => panic!("unexpected {e}"),
    }
}
