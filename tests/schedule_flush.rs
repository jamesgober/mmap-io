//! `schedule_flush` / `schedule_flush_range` (1.1.0): start write-back
//! without waiting. Not durable; `pending_bytes` is left alone.

use std::sync::Arc;
use std::thread;

use mmap_io::{MemoryMappedFile, MmapIoError};

fn rw(dir: &tempfile::TempDir, len: u64) -> MemoryMappedFile {
    MemoryMappedFile::create_rw(dir.path().join("s.bin"), len).expect("create_rw")
}

#[test]
fn schedules_without_resetting_pending_bytes() {
    let dir = tempfile::tempdir().expect("tempdir");
    let m = rw(&dir, 64 * 1024);
    m.update_region(100, b"scheduled").expect("write");
    assert_eq!(m.pending_bytes(), 9);
    m.schedule_flush().expect("schedule_flush");
    m.schedule_flush_range(0, 4096)
        .expect("schedule_flush_range");
    m.schedule_flush_range(100, 9).expect("unaligned range");
    m.schedule_flush_range(64 * 1024 - 1, 1).expect("last byte");
    assert_eq!(m.pending_bytes(), 9, "not durable, so still pending");
    // Visible through the page cache regardless of write-back.
    let on_disk = std::fs::read(dir.path().join("s.bin")).expect("read");
    assert_eq!(&on_disk[100..109], b"scheduled");
    m.flush().expect("durable flush");
    assert_eq!(m.pending_bytes(), 0);
}

#[test]
fn ranges_are_validated_like_flush_range() {
    let dir = tempfile::tempdir().expect("tempdir");
    let m = rw(&dir, 8192);
    for (off, len) in [
        (8192u64, 1u64),
        (0, 8193),
        (8191, 2),
        (u64::MAX, 1),
        (1, u64::MAX),
    ] {
        assert!(
            matches!(
                m.schedule_flush_range(off, len),
                Err(MmapIoError::OutOfBounds { .. })
            ),
            "({off}, {len})"
        );
    }
    // Zero-length: accepted anywhere, does nothing.
    m.schedule_flush_range(u64::MAX, 0).expect("empty");
    m.schedule_flush_range(8192, 0).expect("empty at end");
    // The length after a resize is what counts.
    m.resize(4096).expect("shrink");
    assert!(m.schedule_flush_range(4096, 1).is_err());
    m.schedule_flush_range(4095, 1)
        .expect("in range after shrink");
    m.schedule_flush().expect("whole mapping after shrink");
}

#[test]
fn read_only_and_copy_on_write_are_validated_no_ops() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("ro.bin");
    std::fs::write(&path, vec![3u8; 4096]).expect("write");
    let ro = MemoryMappedFile::open_ro(&path).expect("open_ro");
    ro.schedule_flush().expect("ro no-op");
    ro.schedule_flush_range(0, 4096).expect("ro range no-op");
    assert!(matches!(
        ro.schedule_flush_range(4000, 200),
        Err(MmapIoError::OutOfBounds { .. })
    ));
    #[cfg(feature = "cow")]
    {
        let cow = MemoryMappedFile::open_cow(&path).expect("open_cow");
        cow.update_region(0, b"private").expect("cow write");
        cow.schedule_flush().expect("cow no-op");
        cow.schedule_flush_range(0, 7).expect("cow range no-op");
        assert!(cow.schedule_flush_range(4096, 1).is_err());
        drop(cow);
        assert_eq!(std::fs::read(&path).expect("read"), vec![3u8; 4096]);
    }
}

#[test]
fn concurrent_writers_and_schedulers() {
    let dir = tempfile::tempdir().expect("tempdir");
    let m = Arc::new(rw(&dir, 1 << 20));
    let handles: Vec<_> = (0..4u64)
        .map(|t| {
            let m = Arc::clone(&m);
            thread::spawn(move || {
                for i in 0..50u64 {
                    let off = (t * 256 + i) * 1024;
                    m.update_region(off, &[t as u8 + 1; 1024]).expect("write");
                    m.schedule_flush_range(off, 1024).expect("schedule");
                }
                m.schedule_flush().expect("schedule all");
            })
        })
        .collect();
    for h in handles {
        h.join().expect("join");
    }
    m.flush().expect("flush");
    let data = std::fs::read(dir.path().join("s.bin")).expect("read");
    assert!(data[..1024].iter().all(|&b| b == 1));
    assert!(data[3 * 256 * 1024..3 * 256 * 1024 + 1024]
        .iter()
        .all(|&b| b == 4));
}

/// Linux: `sync_file_range(SYNC_FILE_RANGE_WRITE)` really starts
/// write-back (unlike `msync(MS_ASYNC)`, a no-op there). Observed
/// through this mapping's own dirty page count in `/proc/self/smaps`,
/// which write-back clears, so other processes' I/O does not matter.
#[cfg(target_os = "linux")]
#[test]
fn linux_schedule_flush_starts_writeback() {
    use std::time::{Duration, Instant};

    fn dirty_kb(base: usize) -> Option<u64> {
        let smaps = std::fs::read_to_string("/proc/self/smaps").ok()?;
        let mut in_entry = false;
        let mut total = 0u64;
        let mut found = false;
        for line in smaps.lines() {
            let first = line.split_whitespace().next().unwrap_or("");
            if let Some((lo, hi)) = first.split_once('-') {
                if let (Ok(lo), Ok(hi)) =
                    (usize::from_str_radix(lo, 16), usize::from_str_radix(hi, 16))
                {
                    if in_entry {
                        break;
                    }
                    in_entry = base >= lo && base < hi;
                    found |= in_entry;
                    continue;
                }
            }
            if in_entry {
                for key in ["Shared_Dirty:", "Private_Dirty:"] {
                    if let Some(rest) = line.strip_prefix(key) {
                        total += rest.split_whitespace().next()?.parse::<u64>().ok()?;
                    }
                }
            }
        }
        found.then_some(total)
    }

    let dir = tempfile::tempdir().expect("tempdir");
    let len: usize = 16 << 20;
    let m = rw(&dir, len as u64);
    m.update_region(0, &vec![0xC3; len])
        .expect("dirty every page");
    // SAFETY: only the address is used, to find the smaps entry.
    let base = unsafe { m.as_ptr() } as usize;
    let before = dirty_kb(base).expect("smaps entry");
    if before < (len as u64 / 1024) / 2 {
        // The kernel already wrote the pages back (or the filesystem
        // does not track dirty pages); nothing to observe.
        eprintln!("skipping: only {before} KiB dirty before schedule_flush");
        return;
    }
    m.schedule_flush().expect("schedule_flush");
    assert_eq!(m.pending_bytes(), len as u64);
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let now = dirty_kb(base).expect("smaps entry");
        if now <= before / 4 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "write-back did not start: {before} KiB dirty before, {now} KiB after 20 s"
        );
        thread::sleep(Duration::from_millis(50));
    }
}
