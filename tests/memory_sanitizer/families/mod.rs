//! Per-family fixture files for `tests/memory_sanitizer.rs`.
//!
//! NEW FIXTURES FOR A BUG FAMILY GO IN THEIR OWN FILE HERE, not at the end of
//! an area file (`tests/memory_sanitizer/drop_order.rs` and friends). Several threads push
//! to `main` at once, and two tests appended to the end of one shared file
//! always conflict on rebase; two new files never do.
//!
//! - Name the file after the ledger row that opens the family, lower-cased
//!   with `_` for `-`: `b_2026_09_27_58.rs`. Later fixtures for the same
//!   family (neighbour rows, follow-ups) go in that same file.
//! - Start it with `use super::*;` (the area-file helpers resolve through
//!   it) and a `//!` line naming the row(s).
//! - Declare it below with ONE line, `mod b_2026_09_27_58;`. This file is
//!   `merge=union` in `.gitattributes`, so two threads adding lines here
//!   at once merge with both lines kept. Keep it to `mod` lines only: union
//!   keeps both sides of ANY conflict, which is only safe for a pure list.
//!
//! The test target is unchanged, so `cargo test --test memory_sanitizer` still runs
//! everything. One family alone: `--test memory_sanitizer families::b_2026_09_27_58::`.
//! See CLAUDE.md, "Where a new fixture goes".

#[allow(unused_imports)]
use super::*;

// ── Family modules (one `mod` line each, any order) ──
