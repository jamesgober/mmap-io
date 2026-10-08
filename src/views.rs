//! Run-time exclusion between atomic views and plain byte access.
//!
//! An atomic view (`AtomicView`, `AtomicSliceView`) and a plain view
//! (`MappedSlice`, iterator items) of a writable mapping both hold read
//! guards, so the mapping lock alone lets them coexist. If they cover
//! the same bytes, an atomic store races with the non-atomic reads of
//! the `&[u8]`, which is undefined behavior in the Rust memory model
//! (and a `&[u8]` asserts that its bytes do not change at all while it
//! is alive). Two atomic views of different element sizes over the
//! same bytes are mixed-size atomic accesses, which are undefined
//! behavior too.
//!
//! [`ViewRegistry`] tracks the byte ranges of every live view of one
//! mapping and refuses the combinations that would overlap:
//!
//! - a plain view over a range that overlaps a live atomic view;
//! - an atomic view over a range that overlaps a live plain view, or a
//!   live atomic view with a different element size.
//!
//! Disjoint ranges never conflict, so a header of atomic counters next
//! to plain data keeps working. Copying reads (`read_into`, snapshots
//! for iterator items, page touching) do not register: they hold the
//! atomic set's read lock for the duration of the copy, so no atomic
//! view can appear under them, and they read bytes covered by an
//! existing atomic view with atomic loads of that view's element size.
//!
//! Without the `atomic` feature no atomic view can exist, and the
//! registry compiles to nothing.
//!
//! # Protocol
//!
//! Plain views live in per-thread-ish shards (each a `Mutex<Slab>`);
//! atomic views live in one `RwLock<AtomicSet>`, with their number
//! mirrored in the `atomic_live` counter.
//!
//! - Plain registration inserts the range into its shard (under the
//!   shard mutex), then loads `atomic_live`. If it is zero, the
//!   registration is done without touching the atomic set. Otherwise it
//!   read-locks the atomic set and, on overlap, removes its entry again
//!   and fails.
//! - Atomic registration write-locks the atomic set, increments
//!   `atomic_live`, then locks every shard in turn and scans it. On
//!   overlap it decrements the counter and fails; otherwise it inserts
//!   its entry before releasing the write lock.
//!
//! Why a plain view P and an overlapping atomic view A can never both
//! succeed: both lock P's shard mutex, so one of them gets it first.
//! If P does, A's later lock of that mutex synchronizes with P's
//! unlock, so A's scan sees P's entry and A fails. If A does, A's
//! unlock synchronizes with P's later lock, so A's increment
//! happens-before P's load of `atomic_live`, which therefore reads a
//! non-zero value (A's increment is only undone when A fails or its
//! view is dropped). P then waits for the read lock of the atomic set,
//! which A holds for writing until its entry is in place, and finds the
//! entry. At most one side of a conflicting pair succeeds; occasionally
//! both fail, which is a spurious `InvalidMode`, never an overlap.
//!
//! Lock order: mapping lock, then the atomic set, then one plain shard.
//! None of these locks is held while user code runs.

#[cfg(feature = "atomic")]
pub(crate) use imp::AtomicReg;
pub(crate) use imp::{PlainReg, ViewRegistry};

/// Why a view could not be registered. Turned into
/// `MmapIoError::InvalidMode` by the callers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
// Without the `atomic` feature no atomic view exists, so registration
// never fails and the variants are never constructed.
#[cfg_attr(not(feature = "atomic"), allow(dead_code))]
pub(crate) enum Conflict {
    /// A plain view would overlap a live atomic view.
    AtomicLive,
    /// An atomic view would overlap a live plain view.
    PlainLive,
    /// An atomic view would overlap a live atomic view with a
    /// different element size.
    MixedSize,
}

impl Conflict {
    /// Message for `MmapIoError::InvalidMode`.
    pub(crate) fn message(self) -> &'static str {
        match self {
            Conflict::AtomicLive => {
                "range overlaps a live atomic view of this mapping; read those bytes through \
                 the atomic view or copy them with read_into"
            }
            Conflict::PlainLive => {
                "range overlaps a live MappedSlice or iterator item of this mapping; drop it \
                 before creating an atomic view over the same bytes"
            }
            Conflict::MixedSize => {
                "range overlaps a live atomic view with a different element size; mixed-size \
                 atomic access to the same bytes is not allowed"
            }
        }
    }
}

impl From<Conflict> for crate::errors::MmapIoError {
    fn from(c: Conflict) -> Self {
        crate::errors::MmapIoError::InvalidMode(c.message())
    }
}

/// Plain copy of mapping bytes `[from, to)` into `dst` (which starts at
/// mapping offset `dst_start`).
///
/// # Safety
///
/// `[from, to)` must be valid for reads at `base`, inside
/// `[dst_start, dst_start + dst.len())`, and not concurrently written.
unsafe fn plain_copy(base: *const u8, from: usize, to: usize, dst_start: usize, dst: &mut [u8]) {
    let out = &mut dst[from - dst_start..to - dst_start];
    // SAFETY: caller contract; `out` has exactly `to - from` bytes and
    // is a distinct Rust allocation, so the ranges do not overlap.
    unsafe { std::ptr::copy_nonoverlapping(base.add(from), out.as_mut_ptr(), to - from) };
}

/// One volatile byte read, to fault a page in.
///
/// # Safety
///
/// `base + off` must be valid for reads and not concurrently written.
unsafe fn touch_byte(base: *const u8, off: usize) {
    // SAFETY: caller contract. `black_box` keeps the read.
    let byte = unsafe { std::ptr::read_volatile(base.add(off)) };
    std::hint::black_box(byte);
}

#[cfg(feature = "atomic")]
mod imp {
    use std::sync::atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering};

    use parking_lot::{Mutex, RwLock};

    use super::{plain_copy, touch_byte, Conflict};

    /// Number of plain-view shards. Each thread registers its plain
    /// views in one shard, so concurrent readers rarely share a lock.
    const SHARDS: usize = 8;

    /// Shard used by the current thread, assigned round-robin.
    fn shard_index() -> usize {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        thread_local! {
            static INDEX: usize = NEXT.fetch_add(1, Ordering::Relaxed) % SHARDS;
        }
        // `try_with` fails only during thread-local destruction; any
        // shard is correct, the index only spreads contention.
        INDEX.try_with(|i| *i).unwrap_or(0)
    }

    /// The ranges `[a0, a1)` and `[b0, b1)` share at least one byte.
    #[inline]
    fn overlaps(a0: usize, a1: usize, b0: usize, b1: usize) -> bool {
        a0 < b1 && b0 < a1
    }

    /// Live plain views of one shard: a slab of ranges with a free
    /// list, so registration and removal are O(1) and allocation-free
    /// once it has grown to the peak number of simultaneous views.
    #[derive(Default)]
    struct Slab {
        entries: Vec<Option<(usize, usize)>>,
        free: Vec<usize>,
    }

    impl Slab {
        fn insert(&mut self, range: (usize, usize)) -> usize {
            if let Some(slot) = self.free.pop() {
                self.entries[slot] = Some(range);
                slot
            } else {
                self.entries.push(Some(range));
                self.entries.len() - 1
            }
        }

        fn remove(&mut self, slot: usize) {
            if let Some(entry) = self.entries.get_mut(slot) {
                if entry.take().is_some() {
                    self.free.push(slot);
                }
            }
        }

        fn overlaps(&self, start: usize, end: usize) -> bool {
            self.entries
                .iter()
                .flatten()
                .any(|&(s, e)| overlaps(s, e, start, end))
        }
    }

    /// One shard, on its own cache line.
    #[repr(align(64))]
    #[derive(Default)]
    struct Shard {
        slab: Mutex<Slab>,
    }

    /// A live atomic view: byte range, element size (4 or 8), id.
    #[derive(Clone, Copy)]
    struct AtomicEntry {
        start: usize,
        end: usize,
        size: usize,
        id: u64,
    }

    #[derive(Default)]
    struct AtomicSet {
        entries: Vec<AtomicEntry>,
        next_id: u64,
    }

    /// Live views of one writable mapping. See the module docs.
    #[derive(Default)]
    pub(crate) struct ViewRegistry {
        atomics: RwLock<AtomicSet>,
        /// Number of entries in `atomics`; read without the lock by the
        /// plain fast path.
        atomic_live: AtomicUsize,
        plain: [Shard; SHARDS],
    }

    impl ViewRegistry {
        pub(crate) fn new() -> Self {
            Self::default()
        }

        /// Register a plain view of `[start, end)`. Fails if the range
        /// overlaps a live atomic view.
        pub(crate) fn register_plain(
            &self,
            start: usize,
            end: usize,
        ) -> Result<PlainReg<'_>, Conflict> {
            let shard = shard_index();
            let slot = self.plain[shard].slab.lock().insert((start, end));
            let reg = PlainReg {
                registry: self,
                shard,
                slot,
            };
            // The insert above was published by the shard mutex; see
            // "Protocol" in the module docs for why this load cannot
            // miss an atomic registration that missed the insert.
            if self.atomic_live.load(Ordering::SeqCst) == 0 {
                return Ok(reg);
            }
            let atomics = self.atomics.read();
            if atomics
                .entries
                .iter()
                .any(|a| overlaps(a.start, a.end, start, end))
            {
                drop(atomics);
                // Dropping `reg` removes the entry again.
                return Err(Conflict::AtomicLive);
            }
            Ok(reg)
        }

        /// Register an atomic view of `[start, end)` with elements of
        /// `size` bytes. Fails if the range overlaps a live plain view
        /// or a live atomic view with a different element size. An
        /// empty range registers nothing.
        pub(crate) fn register_atomic(
            &self,
            start: usize,
            end: usize,
            size: usize,
        ) -> Result<AtomicReg<'_>, Conflict> {
            if start == end {
                return Ok(AtomicReg {
                    registry: self,
                    id: None,
                });
            }
            let mut atomics = self.atomics.write();
            if atomics
                .entries
                .iter()
                .any(|a| a.size != size && overlaps(a.start, a.end, start, end))
            {
                return Err(Conflict::MixedSize);
            }
            // Announce before scanning; see "Protocol" in the module
            // docs. Plain views registered after this point either show
            // up in the scan below or see the count and wait for the
            // write lock.
            self.atomic_live.fetch_add(1, Ordering::SeqCst);
            if self
                .plain
                .iter()
                .any(|shard| shard.slab.lock().overlaps(start, end))
            {
                self.atomic_live.fetch_sub(1, Ordering::SeqCst);
                return Err(Conflict::PlainLive);
            }
            let id = atomics.next_id;
            atomics.next_id = atomics.next_id.wrapping_add(1);
            atomics.entries.push(AtomicEntry {
                start,
                end,
                size,
                id,
            });
            Ok(AtomicReg {
                registry: self,
                id: Some(id),
            })
        }

        /// Copy `dst.len()` bytes starting at mapping offset `start`
        /// into `dst`. Bytes covered by a live atomic view are read with
        /// atomic loads of that view's element size; all other bytes
        /// are copied plainly.
        ///
        /// # Safety
        ///
        /// `base` must be the base of a mapping that is valid for reads
        /// of `[start, start + dst.len())`, and the caller must hold a
        /// read guard on that mapping's lock for the duration of the
        /// call, so no writer and no resize can run. Every atomic view
        /// of those bytes must be registered in `self` (the atomic
        /// constructors register, at element-aligned offsets).
        pub(crate) unsafe fn copy_out(&self, base: *const u8, start: usize, dst: &mut [u8]) {
            let end = start + dst.len();
            // Held for the whole copy: no atomic view can be created
            // over the bytes being copied plainly.
            let atomics = self.atomics.read();
            let mut spans: Vec<AtomicEntry> = Vec::new();
            if !atomics.entries.is_empty() {
                spans.extend(
                    atomics
                        .entries
                        .iter()
                        .filter(|a| overlaps(a.start, a.end, start, end))
                        .copied(),
                );
                spans.sort_unstable_by_key(|a| a.start);
            }
            let mut pos = start;
            for span in &spans {
                if span.start > pos {
                    let plain_to = span.start.min(end);
                    // SAFETY: `[pos, plain_to)` is inside the caller's
                    // valid range and covered by no live atomic view
                    // (spans are sorted and `pos` only moves forward past
                    // them); the read guard excludes writers, so this is
                    // a race-free plain read.
                    unsafe { plain_copy(base, pos, plain_to, start, dst) };
                    pos = plain_to;
                }
                let atomic_to = span.end.min(end);
                if pos < atomic_to {
                    // SAFETY: `[pos, atomic_to)` lies in a registered
                    // atomic view, so its elements are aligned elements of
                    // `span.size` inside the valid range, read here only
                    // with atomic loads of exactly that size.
                    unsafe { atomic_copy(base, pos, atomic_to, span.size, start, dst) };
                    pos = atomic_to;
                }
            }
            if pos < end {
                // SAFETY: as for the plain span above.
                unsafe { plain_copy(base, pos, end, start, dst) };
            }
        }

        /// Fault in every page of `[start, start + len)` by reading one
        /// byte (or, inside a live atomic view, one element) per page.
        ///
        /// # Safety
        ///
        /// Same as [`copy_out`](Self::copy_out) for the range.
        pub(crate) unsafe fn touch(&self, base: *const u8, start: usize, len: usize, page: usize) {
            if len == 0 || page == 0 {
                return;
            }
            let atomics = self.atomics.read();
            let end = start + len;
            let mut off = start;
            while off < end {
                match atomics
                    .entries
                    .iter()
                    .find(|a| a.start <= off && off < a.end)
                {
                    Some(a) => {
                        let elem = off - off % a.size;
                        // SAFETY: `elem` is an aligned element of a live
                        // atomic view inside the valid range, read with
                        // an atomic load of the view's element size.
                        let v = unsafe { atomic_load(base, elem, a.size) };
                        std::hint::black_box(v);
                    }
                    // SAFETY: `off` is inside the valid range, under no
                    // live atomic view, and the read guard excludes
                    // writers.
                    None => unsafe { touch_byte(base, off) },
                }
                off += page;
            }
        }
    }

    /// Copy mapping bytes `[from, to)` of an atomic view with element
    /// size `size` into `dst`, one atomic load per element.
    ///
    /// # Safety
    ///
    /// Every element of `size` bytes intersecting `[from, to)` must be
    /// an aligned element of a live atomic view, valid for reads.
    unsafe fn atomic_copy(
        base: *const u8,
        from: usize,
        to: usize,
        size: usize,
        dst_start: usize,
        dst: &mut [u8],
    ) {
        let mut elem = from - from % size;
        while elem < to {
            // SAFETY: forwarded caller contract for this element.
            let bytes = unsafe { atomic_load(base, elem, size) };
            let lo = elem.max(from);
            let hi = (elem + size).min(to);
            dst[lo - dst_start..hi - dst_start].copy_from_slice(&bytes[lo - elem..hi - elem]);
            elem += size;
        }
    }

    /// Atomically load the element of `size` (4 or 8) bytes at mapping
    /// offset `elem`, in memory byte order (the low `size` bytes of the
    /// result are meaningful).
    ///
    /// # Safety
    ///
    /// `base + elem` must be aligned for and valid as an `AtomicU32`
    /// (`size == 4`) or `AtomicU64` (`size == 8`).
    unsafe fn atomic_load(base: *const u8, elem: usize, size: usize) -> [u8; 8] {
        let mut out = [0u8; 8];
        // SAFETY: caller contract: an aligned, live atomic element of
        // exactly this size; reading it through a shared reference to
        // the atomic type is the access the atomic view itself performs.
        // `AtomicU32` / `AtomicU64` have the layout of `u32` / `u64`.
        unsafe {
            if size == 4 {
                let a = &*base.add(elem).cast::<AtomicU32>();
                out[..4].copy_from_slice(&a.load(Ordering::Acquire).to_ne_bytes());
            } else {
                let a = &*base.add(elem).cast::<AtomicU64>();
                out.copy_from_slice(&a.load(Ordering::Acquire).to_ne_bytes());
            }
        }
        out
    }

    /// Registration of a live plain view; removes itself on drop.
    pub(crate) struct PlainReg<'a> {
        registry: &'a ViewRegistry,
        shard: usize,
        slot: usize,
    }

    impl Drop for PlainReg<'_> {
        fn drop(&mut self) {
            self.registry.plain[self.shard]
                .slab
                .lock()
                .remove(self.slot);
        }
    }

    /// Registration of a live atomic view; removes itself on drop.
    pub(crate) struct AtomicReg<'a> {
        registry: &'a ViewRegistry,
        id: Option<u64>,
    }

    impl Drop for AtomicReg<'_> {
        fn drop(&mut self) {
            if let Some(id) = self.id {
                let mut atomics = self.registry.atomics.write();
                if let Some(i) = atomics.entries.iter().position(|a| a.id == id) {
                    atomics.entries.swap_remove(i);
                    self.registry.atomic_live.fetch_sub(1, Ordering::SeqCst);
                }
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn plain_and_atomic_exclude_each_other_only_on_overlap() {
            let r = ViewRegistry::new();
            let a = r.register_atomic(0, 8, 8).expect("atomic");
            assert_eq!(r.register_plain(4, 12).err(), Some(Conflict::AtomicLive));
            let p = r.register_plain(8, 64).expect("disjoint plain");
            assert_eq!(
                r.register_atomic(16, 24, 8).err(),
                Some(Conflict::PlainLive)
            );
            let a3 = r.register_atomic(0, 8, 8).expect("same size, same bytes");
            assert_eq!(r.register_atomic(0, 4, 4).err(), Some(Conflict::MixedSize));
            assert_eq!(r.atomic_live.load(Ordering::SeqCst), 2);
            drop((a, a3));
            assert_eq!(r.atomic_live.load(Ordering::SeqCst), 0);
            drop(p);
            let _a = r.register_atomic(0, 64, 4).expect("all views gone");
            let _e = r.register_atomic(10, 10, 8).expect("empty range");
            assert_eq!(r.atomic_live.load(Ordering::SeqCst), 1);
        }

        #[test]
        fn failed_plain_registration_leaves_no_entry() {
            let r = ViewRegistry::new();
            let a = r.register_atomic(0, 8, 8).expect("atomic");
            assert!(r.register_plain(0, 8).is_err());
            drop(a);
            r.register_atomic(0, 8, 8).expect("no stale plain entry");
        }

        #[test]
        fn slab_reuses_slots() {
            let r = ViewRegistry::new();
            let regs: Vec<_> = (0..100)
                .map(|i| r.register_plain(i, i + 1).expect("plain"))
                .collect();
            drop(regs);
            let _again: Vec<_> = (0..100)
                .map(|i| r.register_plain(i, i + 1).expect("plain"))
                .collect();
            let total: usize = r.plain.iter().map(|s| s.slab.lock().entries.len()).sum();
            assert!(total <= 100, "slots were not reused: {total}");
        }

        #[test]
        fn racing_registrations_never_both_succeed() {
            use std::sync::{Arc, Barrier};
            for _ in 0..200 {
                let r = Arc::new(ViewRegistry::new());
                let barrier = Arc::new(Barrier::new(2));
                let (r2, b2) = (Arc::clone(&r), Arc::clone(&barrier));
                let t = std::thread::spawn(move || {
                    b2.wait();
                    r2.register_plain(0, 16).map(std::mem::forget).is_ok()
                });
                barrier.wait();
                let atomic_ok = r.register_atomic(8, 16, 8).map(std::mem::forget).is_ok();
                let plain_ok = t.join().expect("join");
                assert!(
                    !(atomic_ok && plain_ok),
                    "overlapping views both registered"
                );
            }
        }

        #[test]
        fn copy_out_uses_atomic_loads_inside_atomic_spans() {
            let words: Vec<AtomicU64> = (0..4).map(|_| AtomicU64::new(0)).collect();
            words[1].store(u64::from_ne_bytes(*b"ABCDEFGH"), Ordering::SeqCst);
            words[2].store(u64::from_ne_bytes(*b"IJKLMNOP"), Ordering::SeqCst);
            let base = words.as_ptr().cast::<u8>();
            let r = ViewRegistry::new();
            let _a = r.register_atomic(8, 24, 8).expect("atomic");
            let _b = r.register_atomic(16, 24, 8).expect("nested, same size");
            let mut out = [0xFFu8; 20];
            // SAFETY: `words` is 32 bytes of live atomics; 2..22 is
            // inside it and nothing writes it during the copy.
            unsafe { r.copy_out(base, 2, &mut out) };
            assert_eq!(&out[..6], &[0; 6]);
            assert_eq!(&out[6..20], b"ABCDEFGHIJKLMN");
            let mut whole = [0u8; 32];
            // SAFETY: as above.
            unsafe { r.copy_out(base, 0, &mut whole) };
            assert_eq!(&whole[8..24], b"ABCDEFGHIJKLMNOP");
            let mut mid = [0u8; 3];
            // SAFETY: as above.
            unsafe { r.copy_out(base, 10, &mut mid) };
            assert_eq!(&mid, b"CDE");
            // SAFETY: as above; touch only reads.
            unsafe { r.touch(base, 0, 32, 4) };
        }
    }
}

#[cfg(not(feature = "atomic"))]
mod imp {
    use std::marker::PhantomData;

    use super::{plain_copy, touch_byte, Conflict};

    /// Without the `atomic` feature no atomic view can exist, so there
    /// is nothing to track: registration always succeeds and costs
    /// nothing.
    pub(crate) struct ViewRegistry;

    /// Zero-sized stand-in for the registration of a plain view.
    pub(crate) struct PlainReg<'a>(PhantomData<&'a ViewRegistry>);

    impl ViewRegistry {
        pub(crate) fn new() -> Self {
            ViewRegistry
        }

        pub(crate) fn register_plain(
            &self,
            _start: usize,
            _end: usize,
        ) -> Result<PlainReg<'_>, Conflict> {
            Ok(PlainReg(PhantomData))
        }

        /// See the `atomic`-feature version; here every byte is plain.
        ///
        /// # Safety
        ///
        /// `[start, start + dst.len())` must be valid for reads at
        /// `base`, and the caller must hold a read guard on the mapping.
        pub(crate) unsafe fn copy_out(&self, base: *const u8, start: usize, dst: &mut [u8]) {
            // SAFETY: caller contract; no atomic views exist.
            unsafe { plain_copy(base, start, start + dst.len(), start, dst) };
        }

        /// See the `atomic`-feature version.
        ///
        /// # Safety
        ///
        /// Same as [`copy_out`](Self::copy_out) for the range.
        pub(crate) unsafe fn touch(&self, base: *const u8, start: usize, len: usize, page: usize) {
            if page == 0 {
                return;
            }
            let mut off = start;
            while off < start + len {
                // SAFETY: caller contract; no atomic views exist.
                unsafe { touch_byte(base, off) };
                off += page;
            }
        }
    }
}
