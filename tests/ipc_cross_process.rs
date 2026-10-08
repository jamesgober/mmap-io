//! T6: multi-process IPC integration test.
//!
//! Two processes mmap the same file. Parent writes magic value A,
//! spawns child. Child opens the same path, reads magic A (proving
//! parent->child visibility), writes magic B at a different offset,
//! exits with status 0. Parent waits for child, asserts status, then
//! reads magic B (proving child->parent visibility).
//!
//! Implementation note: the child is the same test binary invoked
//! recursively with an environment variable set, plus the libtest
//! `--exact` filter so only this test function runs. This avoids
//! adding a separate bin target or example for the IPC dance.

use std::env;
use std::process::Command;

const ENV_ROLE: &str = "MMAP_IO_T6_ROLE";
const ENV_PATH: &str = "MMAP_IO_T6_PATH";

const MAGIC_PARENT: &[u8] = b"FROM_PAR";
const MAGIC_CHILD: &[u8] = b"FROM_CHL";

const OFFSET_PARENT: u64 = 0;
const OFFSET_CHILD: u64 = 8;

#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
#[test]
fn ipc_cross_process_byte_visibility() {
    // Child branch: detect env var, do the child dance, exit.
    if let Ok(role) = env::var(ENV_ROLE) {
        let path = env::var(ENV_PATH).expect("child: MMAP_IO_T6_PATH not set");
        run_child(&role, &path);
        return;
    }

    // Parent branch.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("ipc.bin");

    let mmap = mmap_io::MemoryMappedFile::create_rw(&path, 4096).expect("parent create_rw");
    mmap.update_region(OFFSET_PARENT, MAGIC_PARENT)
        .expect("parent write");
    mmap.flush().expect("parent flush");

    let output = Command::new(env::current_exe().expect("current_exe"))
        .arg("--exact")
        .arg("ipc_cross_process_byte_visibility")
        .arg("--nocapture")
        .env(ENV_ROLE, "child")
        .env(ENV_PATH, path.to_str().expect("path utf-8"))
        .output()
        .expect("spawn child");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "child failed (status {:?})\nstdout:\n{stdout}\nstderr:\n{stderr}",
        output.status.code(),
    );

    // Parent: verify child's writes are visible.
    let mut buf = [0u8; 8];
    mmap.read_into(OFFSET_CHILD, &mut buf)
        .expect("parent read child region");
    assert_eq!(
        &buf, MAGIC_CHILD,
        "parent did not see child's write at offset {OFFSET_CHILD}: got {buf:?}"
    );
}

fn run_child(role: &str, path: &str) {
    assert_eq!(role, "child", "unknown child role: {role}");

    // 1. Open the same file the parent created.
    let mmap = mmap_io::MemoryMappedFile::open_rw(path).expect("child open_rw");

    // 2. Verify parent's writes are visible.
    let mut buf = [0u8; 8];
    mmap.read_into(OFFSET_PARENT, &mut buf)
        .expect("child read parent region");
    assert_eq!(
        &buf, MAGIC_PARENT,
        "child did not see parent's write at offset {OFFSET_PARENT}: got {buf:?}"
    );

    // 3. Write child's magic for the parent to verify.
    mmap.update_region(OFFSET_CHILD, MAGIC_CHILD)
        .expect("child write");
    mmap.flush().expect("child flush");

    // 4. Exit success. The test harness in the child treats this test
    //    function returning normally as a passing test; we explicitly
    //    exit to avoid running any test-harness post-amble that might
    //    decide otherwise.
    std::process::exit(0);
}
