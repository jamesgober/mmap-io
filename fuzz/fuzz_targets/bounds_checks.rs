//! Fuzz target: bounds-check helpers (`ensure_in_bounds`,
//! `slice_range`).
//!
//! These two functions are called from every bounds-checked public
//! method in the crate, so their correctness underwrites the
//! soundness of the whole library. The fuzzer hammers them with
//! arbitrary u64 triples (offset, len, total) to verify they never
//! panic on overflow and always return `OutOfBounds` for invalid
//! ranges.

#![no_main]

use libfuzzer_sys::fuzz_target;
use mmap_io::utils::{ensure_in_bounds, slice_range};

#[derive(arbitrary::Arbitrary, Debug)]
struct Input {
    offset: u64,
    len: u64,
    total: u64,
}

fuzz_target!(|input: Input| {
    // The contract: either Ok or OutOfBounds, never panic.
    let result = ensure_in_bounds(input.offset, input.len, input.total);
    let valid = input.offset <= input.total
        && input.offset.checked_add(input.len).map_or(false, |end| end <= input.total);

    match result {
        Ok(()) => assert!(valid, "ensure_in_bounds returned Ok for invalid range"),
        Err(_) => assert!(!valid, "ensure_in_bounds returned Err for valid range"),
    }

    // slice_range should agree with ensure_in_bounds on the
    // accept/reject decision.
    let slice_result = slice_range(input.offset, input.len, input.total);
    match (result.is_ok(), slice_result.is_ok()) {
        (true, true) => {
            let (start, end) = slice_result.unwrap();
            assert_eq!(start as u64, input.offset);
            assert_eq!(end as u64, input.offset + input.len);
        }
        (false, false) => {} // agree on rejection
        _ => panic!("ensure_in_bounds and slice_range disagree on accept/reject"),
    }
});
