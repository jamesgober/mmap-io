//! Tests of the crate's pure logic: error formatting, the range and
//! alignment helpers in `utils`, the plain-data enums, and the
//! auto-trait guarantees of the public types.
//!
//! Nothing in this file maps memory, so the whole binary runs under
//! Miri (`cargo +nightly miri test --test pure_logic`). Keep it that
//! way: tests that need a mapping belong in another file.

use std::error::Error as _;
use std::io;

use mmap_io::errors::{MmapIoError, Result as MmapResult};
use mmap_io::flush::FlushPolicy;
use mmap_io::utils::{align_up, ensure_in_bounds, page_size, slice_range};
use mmap_io::{MmapMode, TouchHint};
use proptest::prelude::*;

// ---------------------------------------------------------------------
// errors
// ---------------------------------------------------------------------

/// One instance of every variant, with the exact `Display` output the
/// crate promises.
fn every_error() -> Vec<(MmapIoError, &'static str)> {
    vec![
        (
            MmapIoError::Io(io::Error::new(io::ErrorKind::NotFound, "nope")),
            "I/O error: nope",
        ),
        (
            MmapIoError::InvalidMode("read-only"),
            "invalid access mode: read-only",
        ),
        (
            MmapIoError::OutOfBounds {
                offset: 1,
                len: 2,
                total: 3,
            },
            "range out of bounds: offset=1, len=2, total=3",
        ),
        (
            MmapIoError::OutOfBounds {
                offset: u64::MAX,
                len: u64::MAX,
                total: 0,
            },
            "range out of bounds: offset=18446744073709551615, \
             len=18446744073709551615, total=0",
        ),
        (MmapIoError::FlushFailed("x".into()), "flush failed: x"),
        (MmapIoError::ResizeFailed("y".into()), "resize failed: y"),
        (MmapIoError::AdviceFailed("z".into()), "advice failed: z"),
        (MmapIoError::LockFailed("l".into()), "lock failed: l"),
        (MmapIoError::UnlockFailed("u".into()), "unlock failed: u"),
        (
            MmapIoError::Misaligned {
                required: 8,
                offset: 3,
            },
            "atomic alignment error: required=8, offset=3",
        ),
        (MmapIoError::WatchFailed("w".into()), "watch failed: w"),
        // Empty payloads still produce a well-formed message.
        (MmapIoError::FlushFailed(String::new()), "flush failed: "),
        (MmapIoError::InvalidMode(""), "invalid access mode: "),
    ]
}

#[test]
fn error_display_is_exact_for_every_variant() {
    for (err, expected) in every_error() {
        assert_eq!(err.to_string(), expected, "{err:?}");
    }
}

#[test]
fn error_debug_names_the_variant() {
    for (err, _) in every_error() {
        let dbg = format!("{err:?}");
        let variant = dbg.split(['(', ' ', '{']).next().unwrap_or_default();
        assert!(
            [
                "Io",
                "InvalidMode",
                "OutOfBounds",
                "FlushFailed",
                "ResizeFailed",
                "AdviceFailed",
                "LockFailed",
                "UnlockFailed",
                "Misaligned",
                "WatchFailed"
            ]
            .contains(&variant),
            "unexpected Debug output {dbg}"
        );
    }
}

#[test]
fn error_source_is_the_io_error_and_nothing_else() {
    for (err, _) in every_error() {
        match &err {
            MmapIoError::Io(inner) => {
                let src = err.source().expect("Io must expose its source");
                assert_eq!(src.to_string(), inner.to_string());
                let io_src = src
                    .downcast_ref::<io::Error>()
                    .expect("source is an io::Error");
                assert_eq!(io_src.kind(), inner.kind());
            }
            _ => assert!(err.source().is_none(), "{err:?} has a source"),
        }
    }
}

#[test]
fn from_io_error_keeps_kind_and_os_code() {
    for kind in [
        io::ErrorKind::NotFound,
        io::ErrorKind::PermissionDenied,
        io::ErrorKind::AlreadyExists,
        io::ErrorKind::InvalidInput,
        io::ErrorKind::Other,
    ] {
        let e: MmapIoError = io::Error::new(kind, "k").into();
        match e {
            MmapIoError::Io(inner) => assert_eq!(inner.kind(), kind),
            other => panic!("expected Io, got {other:?}"),
        }
    }
    let raw = io::Error::from_raw_os_error(2);
    let e = MmapIoError::from(raw);
    match e {
        MmapIoError::Io(inner) => assert_eq!(inner.raw_os_error(), Some(2)),
        other => panic!("expected Io, got {other:?}"),
    }
}

#[test]
fn question_mark_converts_io_errors() {
    fn inner() -> MmapResult<()> {
        Err(io::Error::new(io::ErrorKind::Interrupted, "q"))?;
        Ok(())
    }
    assert!(matches!(inner(), Err(MmapIoError::Io(e)) if e.kind() == io::ErrorKind::Interrupted));
}

#[test]
fn errors_box_into_dyn_error_and_downcast_back() {
    for (err, expected) in every_error() {
        let boxed: Box<dyn std::error::Error + Send + Sync + 'static> = Box::new(err);
        assert_eq!(boxed.to_string(), expected);
        assert!(boxed.downcast_ref::<MmapIoError>().is_some());
    }
}

// ---------------------------------------------------------------------
// utils::align_up
// ---------------------------------------------------------------------

#[test]
fn align_up_table() {
    const MAX: u64 = u64::MAX;
    let cases: &[(u64, u64, u64)] = &[
        // alignment 0 is "no alignment requested".
        (0, 0, 0),
        (5, 0, 5),
        (MAX, 0, MAX),
        // alignment 1 is the identity.
        (0, 1, 0),
        (7, 1, 7),
        (MAX, 1, MAX),
        // Powers of two.
        (0, 4096, 0),
        (1, 4096, 4096),
        (4095, 4096, 4096),
        (4096, 4096, 4096),
        (4097, 4096, 8192),
        (1, 8, 8),
        (8, 8, 8),
        (9, 8, 16),
        (1, 1 << 63, 1 << 63),
        (1 << 63, 1 << 63, 1 << 63),
        // Largest aligned value is reachable.
        (MAX - 4095, 4096, MAX - 4095),
        (MAX - 4096, 4096, MAX - 4095),
        // Saturation instead of overflow.
        (MAX, 4096, MAX),
        (MAX - 4094, 4096, MAX),
        ((1 << 63) + 1, 1 << 63, MAX),
        (MAX, 2, MAX),
        // Non-powers of two.
        (0, 3, 0),
        (9, 3, 9),
        (10, 3, 12),
        (1001, 1000, 2000),
        (1, MAX, MAX),
        (MAX - 1, MAX, MAX),
        (MAX, MAX, MAX),
        // 2^64 - 1 is divisible by 3, so it is already aligned.
        (MAX, 3, MAX),
        // The next multiple of 10 above u64::MAX does not fit.
        (MAX, 10, MAX),
        (MAX - 6, 10, MAX - 5),
        (18_446_744_073_709_551_610, 10, 18_446_744_073_709_551_610),
    ];
    for &(value, alignment, expected) in cases {
        assert_eq!(
            align_up(value, alignment),
            expected,
            "align_up({value}, {alignment})"
        );
    }
}

fn proptest_config(cases: u32) -> ProptestConfig {
    ProptestConfig {
        // Miri executes every case in its interpreter; keep it short.
        cases: if cfg!(miri) { 16 } else { cases },
        // Miri isolation forbids the file system, and these tests have
        // nothing worth persisting.
        failure_persistence: None,
        ..ProptestConfig::default()
    }
}

proptest! {
    #![proptest_config(proptest_config(2048))]

    /// `align_up` either returns the smallest multiple of `alignment`
    /// that is `>= value`, or saturates to `u64::MAX` exactly when that
    /// multiple does not fit in a `u64`.
    #[test]
    fn align_up_is_the_least_multiple_or_saturates(
        value in any::<u64>(),
        alignment in prop_oneof![
            1u64..=64,
            (0u32..64).prop_map(|s| 1u64 << s),
            any::<u64>().prop_filter("nonzero", |a| *a != 0),
        ],
    ) {
        let r = align_up(value, alignment);
        let exact = (u128::from(value)).div_ceil(u128::from(alignment)) * u128::from(alignment);
        if exact > u128::from(u64::MAX) {
            prop_assert_eq!(r, u64::MAX);
        } else {
            prop_assert_eq!(u128::from(r), exact);
        }
    }

    /// `align_up` is monotonic in `value` for a fixed alignment.
    #[test]
    fn align_up_is_monotonic(a in any::<u64>(), b in any::<u64>(), s in 0u32..64) {
        let alignment = 1u64 << s;
        let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
        prop_assert!(align_up(lo, alignment) <= align_up(hi, alignment));
    }
}

// ---------------------------------------------------------------------
// utils::ensure_in_bounds / slice_range
// ---------------------------------------------------------------------

/// The specification both helpers implement: `[offset, offset + len)`
/// must lie inside `[0, total)`, computed without overflow. (The
/// helpers themselves also reject `offset > total` when `len == 0`;
/// the "zero length is accepted anywhere" rule lives in the callers.)
fn spec_in_bounds(offset: u64, len: u64, total: u64) -> bool {
    u128::from(offset) + u128::from(len) <= u128::from(total)
}

fn assert_oob<T: std::fmt::Debug>(r: MmapResult<T>, offset: u64, len: u64, total: u64) {
    match r {
        Err(MmapIoError::OutOfBounds {
            offset: o,
            len: l,
            total: t,
        }) => assert_eq!((o, l, t), (offset, len, total), "error fields"),
        other => panic!("({offset}, {len}, {total}): expected OutOfBounds, got {other:?}"),
    }
}

#[test]
fn ensure_in_bounds_table() {
    const MAX: u64 = u64::MAX;
    // (offset, len, total, ok)
    let cases: &[(u64, u64, u64, bool)] = &[
        (0, 0, 0, true),
        (0, 1, 0, false),
        (1, 0, 0, false),
        (0, 0, 10, true),
        (10, 0, 10, true),
        (11, 0, 10, false),
        (0, 10, 10, true),
        (0, 11, 10, false),
        (9, 1, 10, true),
        (9, 2, 10, false),
        (10, 1, 10, false),
        (5, 5, 10, true),
        (MAX, 0, 10, false),
        (MAX, 1, 10, false),
        (1, MAX, 10, false),
        (MAX, MAX, 10, false),
        (MAX, 0, MAX, true),
        (0, MAX, MAX, true),
        (MAX - 1, 1, MAX, true),
        (u64::from(u32::MAX), 1, u64::from(u32::MAX) + 1, true),
        (u64::from(u32::MAX), 2, u64::from(u32::MAX) + 1, false),
        (1 << 32, 1 << 32, 1 << 33, true),
        (1 << 32, (1 << 32) + 1, 1 << 33, false),
    ];
    for &(offset, len, total, ok) in cases {
        let r = ensure_in_bounds(offset, len, total);
        if ok {
            assert!(r.is_ok(), "({offset}, {len}, {total}) rejected: {r:?}");
        } else {
            assert_oob(r, offset, len, total);
        }
    }
}

#[test]
fn slice_range_returns_start_and_end() {
    assert_eq!(slice_range(0, 0, 0).unwrap(), (0, 0));
    assert_eq!(slice_range(3, 4, 10).unwrap(), (3, 7));
    assert_eq!(slice_range(10, 0, 10).unwrap(), (10, 10));
    assert_eq!(slice_range(0, 10, 10).unwrap(), (0, 10));
    assert_oob(slice_range(11, 0, 10), 11, 0, 10);
    assert_oob(slice_range(0, 11, 10), 0, 11, 10);
    assert_oob(slice_range(u64::MAX, u64::MAX, 10), u64::MAX, u64::MAX, 10);
}

/// On a 32-bit target a range that is in bounds for a `total` larger
/// than the address space still cannot become a `usize` range; it must
/// be rejected, not truncated. On 64-bit targets every `u64` fits.
#[test]
fn slice_range_handles_values_above_u32_max() {
    let total = 1u64 << 34;
    let r = slice_range(1 << 33, 1 << 32, total);
    if cfg!(target_pointer_width = "64") {
        let (s, e) = r.expect("fits in a 64-bit usize");
        assert_eq!(s as u64, 1 << 33);
        assert_eq!(e as u64, (1 << 33) + (1 << 32));
    } else {
        assert_oob(r, 1 << 33, 1 << 32, total);
    }
}

/// `ensure_in_bounds` adds with saturation, so when `total == u64::MAX`
/// an overflowing `offset + len` saturates to `total` and is accepted.
/// `slice_range` then computes `offset + len` unchecked, which panics
/// in debug builds and wraps in release builds. Real mappings never
/// reach `total == u64::MAX`, but both helpers are public.
#[test]
#[ignore = "BUG: utils::ensure_in_bounds accepts offset+len overflow when total == u64::MAX, and slice_range then panics"]
fn bounds_helpers_reject_overflow_at_u64_max_total() {
    assert_oob(
        ensure_in_bounds(1, u64::MAX, u64::MAX),
        1,
        u64::MAX,
        u64::MAX,
    );
    assert_oob(
        ensure_in_bounds(u64::MAX, 1, u64::MAX),
        u64::MAX,
        1,
        u64::MAX,
    );
    assert_oob(
        ensure_in_bounds(u64::MAX, u64::MAX, u64::MAX),
        u64::MAX,
        u64::MAX,
        u64::MAX,
    );
    // Minimized from a `bounds_checks` fuzz crash.
    assert_oob(
        ensure_in_bounds(
            10_706_345_580_035_347_604,
            18_446_744_073_692_774_400,
            u64::MAX,
        ),
        10_706_345_580_035_347_604,
        18_446_744_073_692_774_400,
        u64::MAX,
    );
    let r = std::panic::catch_unwind(|| slice_range(2, u64::MAX - 1, u64::MAX));
    let r = r.expect("slice_range must not panic on caller input");
    assert_oob(r, 2, u64::MAX - 1, u64::MAX);
}

proptest! {
    #![proptest_config(proptest_config(4096))]

    /// Both helpers agree with the overflow-free specification for every
    /// `total` below `u64::MAX` (the `u64::MAX` case is the ignored bug
    /// test above), and never panic.
    #[test]
    fn bounds_helpers_match_the_spec(
        offset in prop_oneof![any::<u64>(), 0u64..=4096, Just(u64::MAX)],
        len in prop_oneof![any::<u64>(), 0u64..=4096, Just(u64::MAX)],
        total in prop_oneof![0u64..=8192, any::<u64>().prop_map(|t| t.saturating_sub(1))],
    ) {
        let expect_ok = spec_in_bounds(offset, len, total) && offset <= total;
        let r = ensure_in_bounds(offset, len, total);
        prop_assert_eq!(r.is_ok(), expect_ok, "ensure_in_bounds({}, {}, {})", offset, len, total);
        let r = slice_range(offset, len, total);
        if expect_ok && usize::try_from(offset + len).is_ok() {
            let (s, e) = r.expect("in bounds");
            prop_assert_eq!(s as u64, offset);
            prop_assert_eq!(e as u64, offset + len);
        } else {
            let is_oob = matches!(
                r,
                Err(MmapIoError::OutOfBounds { offset: o, len: l, total: t })
                    if o == offset && l == len && t == total
            );
            prop_assert!(is_oob);
        }
    }
}

// ---------------------------------------------------------------------
// utils::page_size
// ---------------------------------------------------------------------

#[test]
fn page_size_is_a_sane_power_of_two_and_stable() {
    let p = page_size();
    assert!(p.is_power_of_two(), "page size {p}");
    assert!((1024..=1 << 21).contains(&p), "page size {p}");
    let threads: Vec<_> = (0..4).map(|_| std::thread::spawn(page_size)).collect();
    for t in threads {
        assert_eq!(t.join().expect("thread"), p);
    }
    assert_eq!(page_size(), p);
}

// ---------------------------------------------------------------------
// Plain-data enums
// ---------------------------------------------------------------------

#[test]
fn flush_policy_defaults_and_equality() {
    assert_eq!(FlushPolicy::default(), FlushPolicy::Never);
    // `Manual` is documented as an alias in meaning, but it is a
    // distinct variant.
    assert_ne!(FlushPolicy::Manual, FlushPolicy::Never);
    assert_eq!(FlushPolicy::EveryBytes(0), FlushPolicy::EveryBytes(0));
    assert_ne!(FlushPolicy::EveryBytes(1), FlushPolicy::EveryWrites(1));
    assert_ne!(FlushPolicy::EveryMillis(1), FlushPolicy::EveryMillis(2));
    let p = FlushPolicy::EveryBytes(usize::MAX);
    let q = p; // Copy
    assert_eq!(p, q);
    for (policy, name) in [
        (FlushPolicy::Never, "Never"),
        (FlushPolicy::Manual, "Manual"),
        (FlushPolicy::Always, "Always"),
        (FlushPolicy::EveryBytes(7), "EveryBytes(7)"),
        (FlushPolicy::EveryWrites(8), "EveryWrites(8)"),
        (FlushPolicy::EveryMillis(9), "EveryMillis(9)"),
    ] {
        assert_eq!(format!("{policy:?}"), name);
    }
}

#[test]
fn mode_and_touch_hint_are_plain_values() {
    assert_eq!(TouchHint::default(), TouchHint::Never);
    assert_ne!(TouchHint::Eager, TouchHint::Lazy);
    assert_eq!(format!("{:?}", TouchHint::Eager), "Eager");
    let modes = [
        MmapMode::ReadOnly,
        MmapMode::ReadWrite,
        MmapMode::CopyOnWrite,
    ];
    for (i, a) in modes.iter().enumerate() {
        for (j, b) in modes.iter().enumerate() {
            assert_eq!(a == b, i == j);
        }
    }
    assert_eq!(format!("{:?}", MmapMode::CopyOnWrite), "CopyOnWrite");
}

#[cfg(feature = "advise")]
#[test]
fn advice_variants_are_distinct() {
    use mmap_io::MmapAdvice;
    let all = [
        MmapAdvice::Normal,
        MmapAdvice::Random,
        MmapAdvice::Sequential,
        MmapAdvice::WillNeed,
        MmapAdvice::DontNeed,
    ];
    for (i, a) in all.iter().enumerate() {
        for (j, b) in all.iter().enumerate() {
            assert_eq!(a == b, i == j);
        }
        assert!(!format!("{a:?}").is_empty());
    }
}

#[cfg(feature = "watch")]
#[test]
fn change_event_is_cloneable_data() {
    use mmap_io::{ChangeEvent, ChangeKind};
    let e = ChangeEvent {
        offset: Some(1),
        len: None,
        kind: ChangeKind::Metadata,
    };
    let c = e.clone();
    assert_eq!(c.offset, Some(1));
    assert_eq!(c.len, None);
    assert_eq!(c.kind, ChangeKind::Metadata);
    assert_ne!(ChangeKind::Modified, ChangeKind::Removed);
    assert!(format!("{e:?}").contains("Metadata"));
}

#[test]
fn raw_options_builder_is_plain_data() {
    let mut o = mmap_io::raw::RawMmapOptions::new();
    o.offset(u64::MAX).len(usize::MAX);
    let c = o.clone();
    let dbg = format!("{c:?}");
    assert!(dbg.contains(&u64::MAX.to_string()), "{dbg}");
}

// ---------------------------------------------------------------------
// Auto traits (compile-time checks)
// ---------------------------------------------------------------------

#[test]
fn public_types_have_the_documented_auto_traits() {
    fn send_sync<T: Send + Sync>() {}
    fn send<T: Send>() {}
    send_sync::<mmap_io::MemoryMappedFile>();
    send_sync::<mmap_io::AnonymousMmap>();
    send_sync::<mmap_io::MappedSlice<'static>>();
    send::<mmap_io::MappedSliceMut<'static>>();
    send_sync::<mmap_io::segment::Segment>();
    send_sync::<mmap_io::segment::SegmentMut>();
    send_sync::<mmap_io::mmap::MmapReader<'static>>();
    send_sync::<MmapIoError>();
    send_sync::<FlushPolicy>();
    send_sync::<mmap_io::raw::RawMmap>();
    send_sync::<mmap_io::raw::RawMmapMut>();
    #[cfg(feature = "iterator")]
    {
        send::<mmap_io::ChunkIterator<'static>>();
        send::<mmap_io::PageIterator<'static>>();
    }
    #[cfg(feature = "atomic")]
    {
        use std::sync::atomic::{AtomicU32, AtomicU64};
        send_sync::<mmap_io::atomic::AtomicView<'static, AtomicU64>>();
        send_sync::<mmap_io::atomic::AtomicView<'static, AtomicU32>>();
        send_sync::<mmap_io::atomic::AtomicSliceView<'static, AtomicU64>>();
        send_sync::<mmap_io::atomic::AtomicSliceView<'static, AtomicU32>>();
    }
    #[cfg(feature = "watch")]
    send::<mmap_io::WatchHandle>();
}
