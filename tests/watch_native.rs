//! Native file-watch integration tests (since 0.9.9).
//!
//! These tests exercise the native event-source path
//! (`inotify` on Linux, FSEvents on macOS, `ReadDirectoryChangesW`
//! on Windows) by modifying the watched file through the std::fs
//! API. mmap writes go through the page cache and only reach the
//! FS watcher at OS-decided writeback time, so they are not a
//! reliable trigger; the std::fs path is. This matches the real
//! use case for `watch`: detect changes made by another process
//! or by file-system operations.

#![cfg(feature = "watch")]

use mmap_io::{create_mmap, watch::ChangeEvent};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

fn tmp_path(name: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!(
        "mmap_io_watch_native_{}_{}",
        name,
        std::process::id()
    ));
    p
}

/// Spin until `pred()` returns true or `timeout` elapses.
fn wait_until<F: Fn() -> bool>(timeout: Duration, pred: F) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if pred() {
            return true;
        }
        thread::sleep(Duration::from_millis(10));
    }
    pred()
}

/// Write `payload` to `path` via std::fs (the trigger every native
/// FS watcher reliably observes across all three platforms).
fn write_external(path: &Path, payload: &[u8]) {
    let mut f = OpenOptions::new().write(true).open(path).expect("reopen");
    f.write_all(payload).expect("write");
    f.sync_all().expect("sync");
}

#[test]
fn watch_modify_detected() {
    let path = tmp_path("modify");
    let _ = fs::remove_file(&path);
    let mmap = create_mmap(&path, 64).expect("create");

    let saw = Arc::new(AtomicBool::new(false));
    let _handle = {
        let saw = Arc::clone(&saw);
        mmap.watch(move |_| saw.store(true, Ordering::SeqCst))
            .expect("watch")
    };
    thread::sleep(Duration::from_millis(200));

    write_external(&path, b"modified");

    assert!(
        wait_until(Duration::from_secs(3), || saw.load(Ordering::SeqCst)),
        "modify event must reach the watcher"
    );

    drop(mmap);
    let _ = fs::remove_file(&path);
}

#[test]
fn watch_truncate_detected() {
    let path = tmp_path("truncate");
    let _ = fs::remove_file(&path);
    let mmap = create_mmap(&path, 4096).expect("create");

    let count = Arc::new(AtomicUsize::new(0));
    let _handle = {
        let count = Arc::clone(&count);
        mmap.watch(move |_| {
            count.fetch_add(1, Ordering::SeqCst);
        })
        .expect("watch")
    };
    thread::sleep(Duration::from_millis(200));

    // Truncate via a fresh handle. Drop the mmap first on Windows
    // so the file is not held open by an active mapping (Windows
    // disallows truncating a file with a live mapping view).
    drop(mmap);
    OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(&path)
        .expect("truncate open")
        .sync_all()
        .expect("sync");

    assert!(
        wait_until(Duration::from_secs(3), || count.load(Ordering::SeqCst) > 0),
        "truncate must produce at least one event; saw {}",
        count.load(Ordering::SeqCst)
    );

    let _ = fs::remove_file(&path);
}

#[test]
fn watch_extend_detected() {
    let path = tmp_path("extend");
    let _ = fs::remove_file(&path);
    let mmap = create_mmap(&path, 256).expect("create");

    let count = Arc::new(AtomicUsize::new(0));
    let _handle = {
        let count = Arc::clone(&count);
        mmap.watch(move |_| {
            count.fetch_add(1, Ordering::SeqCst);
        })
        .expect("watch")
    };
    thread::sleep(Duration::from_millis(200));

    // Extend the file from a separate handle. The OpenOptions
    // append path doesn't truncate, so we can keep the mmap alive
    // on Linux/macOS; on Windows, however, append + set_len is
    // safer with the mapping dropped, so we drop it first to keep
    // the test cross-platform-clean.
    drop(mmap);
    let f = OpenOptions::new()
        .write(true)
        .open(&path)
        .expect("reopen for extend");
    f.set_len(8192).expect("set_len extend");
    f.sync_all().expect("sync");
    drop(f);

    assert!(
        wait_until(Duration::from_secs(3), || count.load(Ordering::SeqCst) > 0),
        "extend must produce at least one event; saw {}",
        count.load(Ordering::SeqCst)
    );

    let _ = fs::remove_file(&path);
}

#[test]
fn watch_rapid_sequence_coalesces_or_reports_each() {
    let path = tmp_path("rapid");
    let _ = fs::remove_file(&path);
    let mmap = create_mmap(&path, 64).expect("create");

    let count = Arc::new(AtomicUsize::new(0));
    let _handle = {
        let count = Arc::clone(&count);
        mmap.watch(move |_| {
            count.fetch_add(1, Ordering::SeqCst);
        })
        .expect("watch")
    };
    thread::sleep(Duration::from_millis(200));

    // Fire ten rapid external modifications. Different platforms
    // coalesce differently: macOS FSEvents batches at ~50ms, Linux
    // inotify delivers each, Windows RDCW may merge. The contract
    // is: AT LEAST ONE event reaches the watcher.
    for i in 0..10u8 {
        let payload = [b'A' + i; 8];
        write_external(&path, &payload);
        thread::sleep(Duration::from_millis(20));
    }

    assert!(
        wait_until(Duration::from_secs(3), || count.load(Ordering::SeqCst) > 0),
        "at least one event must reach the watcher from the rapid sequence; saw {}",
        count.load(Ordering::SeqCst)
    );

    drop(mmap);
    let _ = fs::remove_file(&path);
}

#[test]
fn watch_removed_event_terminates_dispatcher() {
    let path = tmp_path("removed");
    let _ = fs::remove_file(&path);
    let mmap = create_mmap(&path, 64).expect("create");

    let received_event: Arc<std::sync::Mutex<Option<ChangeEvent>>> =
        Arc::new(std::sync::Mutex::new(None));
    let handle = {
        let received = Arc::clone(&received_event);
        mmap.watch(move |event| {
            let mut slot = received.lock().expect("lock");
            *slot = Some(event);
        })
        .expect("watch")
    };
    thread::sleep(Duration::from_millis(200));

    // On Windows the mapping must be dropped before the file is
    // deletable. On Unix the unlink is OK with the mapping alive
    // (the inode persists until the last reference drops); for
    // cross-platform-clean behavior, drop first.
    drop(mmap);
    let _ = fs::remove_file(&path);

    // Give the OS time to deliver the removal event.
    let got_event = wait_until(Duration::from_secs(3), || {
        received_event.lock().expect("lock").is_some()
    });

    assert!(got_event, "removal must produce some event");

    drop(handle);
}
