//! Leak check for `mmap_io::raw`: thousands of map / drop cycles must
//! not grow the number of OS mappings or (on Windows) kernel handles.
//!
//! This file holds a single test so no other test in the same process
//! creates mappings or handles while the counts are taken.
//!
//! - Linux / Android: mappings are counted from `/proc/self/maps`.
//! - Windows: mapped views are counted by walking the address space
//!   with `VirtualQuery` (regions of type `MEM_MAPPED`), and handles
//!   with `GetProcessHandleCount`.
//! - Other Unix: no portable mapping count exists; the churn still runs
//!   so crashes or errors surface.
//!
//! Every file-backed mapping maps a private temporary file, which is
//! the raw constructors' `# Safety` contract.

use std::fs::OpenOptions;

use mmap_io::raw::{RawMmap, RawMmapMut, RawMmapOptions};

#[cfg(any(target_os = "linux", target_os = "android"))]
fn mapping_count() -> Option<usize> {
    let maps = std::fs::read_to_string("/proc/self/maps").ok()?;
    Some(maps.lines().count())
}

#[cfg(windows)]
mod win {
    use std::ffi::c_void;

    #[repr(C)]
    pub struct MemoryBasicInformation {
        pub base_address: *mut c_void,
        pub allocation_base: *mut c_void,
        pub allocation_protect: u32,
        #[cfg(target_pointer_width = "64")]
        pub partition_id: u16,
        pub region_size: usize,
        pub state: u32,
        pub protect: u32,
        pub kind: u32,
    }

    extern "system" {
        pub fn VirtualQuery(
            address: *const c_void,
            buffer: *mut MemoryBasicInformation,
            length: usize,
        ) -> usize;
        pub fn GetCurrentProcess() -> *mut c_void;
        pub fn GetProcessHandleCount(process: *mut c_void, count: *mut u32) -> i32;
    }

    pub const MEM_MAPPED: u32 = 0x0004_0000;
}

#[cfg(windows)]
fn mapping_count() -> Option<usize> {
    use std::mem::{size_of, MaybeUninit};
    let mut count = 0usize;
    let mut addr: usize = 0;
    loop {
        let mut info = MaybeUninit::<win::MemoryBasicInformation>::uninit();
        // SAFETY: VirtualQuery only reads the address-space layout and
        // writes at most `size_of` bytes into `info` (MSDN). Any
        // address is a valid query; a return of 0 ends the walk.
        let written = unsafe {
            win::VirtualQuery(
                addr as *const std::ffi::c_void,
                info.as_mut_ptr(),
                size_of::<win::MemoryBasicInformation>(),
            )
        };
        if written == 0 {
            break;
        }
        // SAFETY: a non-zero return means the structure was filled.
        let info = unsafe { info.assume_init() };
        if info.kind == win::MEM_MAPPED && info.base_address == info.allocation_base {
            count += 1;
        }
        match (info.base_address as usize).checked_add(info.region_size) {
            Some(next) if next > addr => addr = next,
            _ => break,
        }
    }
    Some(count)
}

#[cfg(windows)]
fn handle_count() -> Option<u32> {
    let mut n = 0u32;
    // SAFETY: GetCurrentProcess returns a pseudo-handle that needs no
    // closing; GetProcessHandleCount writes one u32 to `n` (MSDN).
    let ok = unsafe { win::GetProcessHandleCount(win::GetCurrentProcess(), &mut n) };
    (ok != 0).then_some(n)
}

#[cfg(not(windows))]
fn handle_count() -> Option<u32> {
    None
}

#[cfg(not(any(target_os = "linux", target_os = "android", windows)))]
fn mapping_count() -> Option<usize> {
    None
}

fn churn(file: &std::fs::File, rounds: usize) {
    for i in 0..rounds {
        // SAFETY: private temporary file.
        let ro = unsafe { RawMmap::map(file) }.expect("ro");
        assert_eq!(ro.len(), 3 * 65_536 + 1);
        drop(ro);
        // SAFETY: private temporary file.
        let mut rw =
            unsafe { RawMmapOptions::new().offset(1 + i as u64).map_mut(file) }.expect("rw");
        rw[0] = rw[0].wrapping_add(1);
        if i % 256 == 0 {
            rw.flush().expect("flush");
        }
        drop(rw);
        // SAFETY: private temporary file.
        let cow =
            unsafe { RawMmapOptions::new().offset(70_000).len(10).map_copy(file) }.expect("cow");
        drop(cow);
        // SAFETY: private temporary file; empty window, no OS mapping.
        let empty = unsafe { RawMmapOptions::new().len(0).map(file) }.expect("empty");
        drop(empty);
        let anon = RawMmapMut::map_anon(4096 * (1 + i % 4)).expect("anon");
        drop(anon);
    }
}

#[test]
fn map_drop_cycles_do_not_leak_mappings_or_handles() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("leak.bin");
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(&path)
        .expect("create");
    file.set_len(3 * 65_536 + 1).expect("set_len");

    // Warm up lazily initialised state (granularity cache, allocator).
    churn(&file, 16);
    let maps_before = mapping_count();
    let handles_before = handle_count();

    churn(&file, 4_000);

    let maps_after = mapping_count();
    let handles_after = handle_count();
    if let (Some(b), Some(a)) = (maps_before, maps_after) {
        assert!(a <= b + 4, "mapping count grew from {b} to {a}");
    }
    if let (Some(b), Some(a)) = (handles_before, handles_after) {
        assert!(a <= b + 4, "handle count grew from {b} to {a}");
    }

    // Positive control: live mappings ARE visible to the counters, so
    // the assertions above are meaningful.
    // SAFETY: private temporary file.
    let held: Vec<RawMmap> = (0..32)
        .map(|_| unsafe { RawMmap::map(&file) }.expect("held"))
        .collect();
    if let (Some(b), Some(a)) = (maps_after, mapping_count()) {
        assert!(a >= b + 32, "32 live mappings not counted: {b} -> {a}");
    }
    drop(held);
    if let (Some(b), Some(a)) = (maps_after, mapping_count()) {
        assert!(a <= b + 4, "mappings not released: {b} -> {a}");
    }
}
