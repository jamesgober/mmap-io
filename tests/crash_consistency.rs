//! Process-crash consistency.
//!
//! A child copy of this test binary (selected by `MMAP_IO_CRASH_ROLE`)
//! maps a file, writes a known pattern region by region, makes each
//! region durable with `flush()` or `flush_range()`, and reports every
//! region it has flushed as a line on stdout. The child then dies
//! abruptly: `std::process::abort()` at a seed-chosen point, or
//! `Child::kill()` from the parent. The parent reopens the file and
//! requires every reported region to be on disk exactly.
//!
//! Scope: this tests durability across a *process* crash, where the OS
//! page cache survives. It does not simulate power loss or a kernel
//! crash; nothing here can observe whether the device itself persisted
//! the data.
//!
//! Expected OS behavior, also checked: writes to a shared mapping that
//! were never flushed are still visible in the file after the writing
//! process dies, because they live in the OS page cache, not in the
//! process. Flushing is what makes them survive a system crash, not a
//! process crash.

use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use mmap_io::{MemoryMappedFile, MmapMode};

mod common;

const ROLE: &str = "MMAP_IO_CRASH_ROLE";
const FILE: &str = "MMAP_IO_CRASH_FILE";
const SEED: &str = "MMAP_IO_CRASH_SEED";

/// Region size: not a multiple of the page size, so regions straddle
/// page boundaries and `flush_range` has to widen them.
const REGION: u64 = 1500;
const REGIONS: u64 = 2048;

fn region_bytes(index: u64) -> Vec<u8> {
    common::pattern(REGION as usize, (index % 251) as u8 ^ 0x5A)
        .into_iter()
        .map(|b| b | 1) // never zero, so "not written" is distinguishable
        .collect()
}

#[derive(Debug, Default)]
struct Report {
    flushed: Vec<u64>,
    /// Region written (completely) but deliberately not flushed.
    dirty: Option<u64>,
    /// Region half-written through a held `as_slice_mut` guard.
    half: Option<u64>,
    started: bool,
    done: bool,
}

fn parse_line(r: &mut Report, line: &str) {
    // The test harness prints `test child ... ` without a newline, so
    // the first report can share a line with it: find the keyword.
    let tokens: Vec<&str> = line.split_whitespace().collect();
    let Some(at) = tokens
        .iter()
        .position(|t| ["STARTED", "FLUSHED", "DIRTY", "HALF", "DONE"].contains(t))
    else {
        return;
    };
    let mut it = tokens[at..].iter().copied();
    match (it.next(), it.next().and_then(|v| v.parse::<u64>().ok())) {
        (Some("STARTED"), _) => r.started = true,
        (Some("FLUSHED"), Some(i)) => r.flushed.push(i),
        (Some("DIRTY"), Some(i)) => r.dirty = Some(i),
        (Some("HALF"), Some(i)) => r.half = Some(i),
        (Some("DONE"), _) => r.done = true,
        _ => {}
    }
}

fn spawn_child(role: &str, file: &Path, seed: u64) -> Child {
    Command::new(std::env::current_exe().expect("current_exe"))
        .args(["child", "--exact", "--nocapture", "--test-threads=1"])
        .env(ROLE, role)
        .env(FILE, file)
        .env(SEED, seed.to_string())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn child")
}

/// Wait for `child` to exit, killing it if it outlives `limit`.
fn wait_with_limit(child: &mut Child, limit: Duration) -> std::process::ExitStatus {
    let deadline = Instant::now() + limit;
    loop {
        if let Some(status) = child.try_wait().expect("try_wait") {
            return status;
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            panic!("child did not exit within {limit:?}");
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Check the file against the report.
fn verify(file: &Path, report: &Report) {
    assert!(report.started, "child never started");
    let bytes = std::fs::read(file).expect("read file after crash");
    assert_eq!(bytes.len() as u64, REGIONS * REGION, "file length changed");
    let region = |i: u64| &bytes[(i * REGION) as usize..((i + 1) * REGION) as usize];
    for &i in &report.flushed {
        assert!(
            region(i) == region_bytes(i).as_slice(),
            "flushed region {i} is not on disk intact"
        );
    }
    if let Some(i) = report.dirty {
        assert!(
            region(i) == region_bytes(i).as_slice(),
            "unflushed region {i} was lost with the process (the page cache should keep it)"
        );
    }
    if let Some(i) = report.half {
        let want = region_bytes(i);
        let half = (REGION / 2) as usize;
        assert_eq!(&region(i)[..half], &want[..half], "half-written region {i}");
    }
    // Every other region is either untouched or one complete write
    // (the one in progress at kill time may be torn byte-wise).
    for i in 0..REGIONS {
        let got = region(i);
        let want = region_bytes(i);
        assert!(
            got.iter().zip(&want).all(|(&g, &w)| g == 0 || g == w),
            "region {i} holds bytes that were never written"
        );
    }
    // The file is still a valid mapping target.
    let m = MemoryMappedFile::open_ro(file).unwrap();
    assert_eq!(m.len(), REGIONS * REGION);
}

/// The child: write, flush, report; die where the role says.
#[cfg_attr(miri, ignore = "spawns child processes, which Miri does not support")]
#[test]
fn child() {
    let Ok(role) = std::env::var(ROLE) else {
        return;
    };
    let file = std::path::PathBuf::from(std::env::var(FILE).unwrap());
    let seed: u64 = std::env::var(SEED).unwrap().parse().unwrap();
    let m = MemoryMappedFile::builder(&file)
        .mode(MmapMode::ReadWrite)
        .size(REGIONS * REGION)
        .create()
        .unwrap();
    let say = |s: String| {
        use std::io::Write;
        let mut out = std::io::stdout().lock();
        writeln!(out, "{s}").unwrap();
        out.flush().unwrap();
    };
    say("STARTED".into());
    // Abort after this many regions (abort roles), 1..=24.
    let stop_after = 1 + seed % 24;
    let use_range = |i: u64| (i + seed) % 2 == 0;
    for i in 0..REGIONS {
        m.update_region(i * REGION, &region_bytes(i)).unwrap();
        if use_range(i) {
            m.flush_range(i * REGION, REGION).unwrap();
        } else {
            m.flush().unwrap();
        }
        say(format!("FLUSHED {i}"));
        if role != "until_killed" && i + 1 == stop_after {
            let next = i + 1;
            match role.as_str() {
                "abort_after_flush" => {}
                "abort_dirty" => {
                    m.update_region(next * REGION, &region_bytes(next)).unwrap();
                    say(format!("DIRTY {next}"));
                }
                "abort_holding_guard" => {
                    let mut g = m.as_slice_mut(next * REGION, REGION).unwrap();
                    let half = (REGION / 2) as usize;
                    g.as_mut()[..half].copy_from_slice(&region_bytes(next)[..half]);
                    say(format!("HALF {next}"));
                    std::process::abort();
                }
                other => panic!("unknown role {other}"),
            }
            std::process::abort();
        }
    }
    say("DONE".into());
}

/// Run an abort role with `seed` and verify.
fn abort_case(role: &str, seed: u64) {
    let file = common::tmp_path("crash.bin");
    let mut child = spawn_child(role, &file, seed);
    let stdout = child.stdout.take().unwrap();
    let mut report = Report::default();
    for line in BufReader::new(stdout).lines() {
        parse_line(&mut report, &line.unwrap());
    }
    let status = wait_with_limit(&mut child, Duration::from_secs(120));
    assert!(
        !status.success(),
        "{role}: child exited cleanly: {status:?}"
    );
    assert!(!report.done);
    assert_eq!(
        report.flushed.len() as u64,
        1 + seed % 24,
        "{role}: wrong abort point"
    );
    verify(&file, &report);
}

#[cfg_attr(miri, ignore = "spawns child processes, which Miri does not support")]
#[test]
fn abort_after_flush_keeps_every_flushed_region() {
    for seed in [0, 5, 13, 23] {
        abort_case("abort_after_flush", seed);
    }
}

#[cfg_attr(miri, ignore = "spawns child processes, which Miri does not support")]
#[test]
fn abort_with_an_unflushed_write_keeps_it_in_the_page_cache() {
    for seed in [1, 8, 22] {
        abort_case("abort_dirty", seed);
    }
}

#[cfg_attr(miri, ignore = "spawns child processes, which Miri does not support")]
#[test]
fn abort_while_holding_a_write_guard_keeps_the_partial_write() {
    for seed in [2, 11] {
        abort_case("abort_holding_guard", seed);
    }
}

/// The parent kills the child at an arbitrary moment after it has
/// reported a number of flushed regions.
#[cfg_attr(miri, ignore = "spawns child processes, which Miri does not support")]
#[test]
fn kill_at_an_arbitrary_point_keeps_every_reported_region() {
    for (round, after) in [3usize, 17, 40].into_iter().enumerate() {
        let file = common::tmp_path("killed.bin");
        let mut child = spawn_child("until_killed", &file, round as u64);
        let stdout = child.stdout.take().unwrap();
        let mut lines = BufReader::new(stdout).lines();
        let mut report = Report::default();
        while report.flushed.len() < after && !report.done {
            match lines.next() {
                Some(line) => parse_line(&mut report, &line.unwrap()),
                None => break,
            }
        }
        child.kill().expect("kill child");
        // Lines already in the pipe are valid reports too.
        for line in lines {
            parse_line(&mut report, &line.unwrap());
        }
        wait_with_limit(&mut child, Duration::from_secs(60));
        assert!(report.flushed.len() >= after || report.done);
        verify(&file, &report);
    }
}
