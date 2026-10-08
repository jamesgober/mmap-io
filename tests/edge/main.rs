//! Edge-case matrix for the public API.
//!
//! One test binary, split by topic. Every module is table-driven where
//! that makes sense: sizes around the page and allocation-granularity
//! boundaries, offsets and lengths at 0 / 1 / len-1 / len / len+1 /
//! `u32::MAX` / `i64::MAX` / `u64::MAX`, every mapping mode, and the
//! same checks again after a growing and a shrinking `resize`.
//!
//! Conventions:
//! - Errors are matched by exact variant (and exact fields where the
//!   API documents them), never just `is_err()`.
//! - Copy-on-write mappings are only checked for read and no-op flush
//!   semantics; their write behavior is about to change.
//! - Atomic views and plain read views are never held at the same time
//!   on one mapping.
//! - Tests that pin a known bug are `#[ignore = "BUG: ..."]` and fail
//!   when run with `--ignored`.

#[path = "../common/mod.rs"]
mod common;

mod anonymous;
mod construct;
mod flush_policy;
mod manager;
mod misc;
mod ranges;
mod reader;
mod resize;
mod segments;

#[cfg(feature = "atomic")]
mod atomic;
#[cfg(feature = "iterator")]
mod iterators;
