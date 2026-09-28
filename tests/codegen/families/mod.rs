//! Per-family fixture files for `tests/codegen.rs`.
//!
//! NEW FIXTURES FOR A BUG FAMILY GO IN THEIR OWN FILE HERE, not at the end of
//! an area file (`tests/codegen/drop_order.rs` and friends). Several threads push
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
//!   rustfmt SORTS these lines and union keeps them in arrival order, so run
//!   `cargo fmt --all` after any rebase that brings in another thread's line.
//!
//! The test target is unchanged, so `cargo test --test codegen` still runs
//! everything. One family alone: `--test codegen families::b_2026_09_27_58::`.
//! See CLAUDE.md, "Where a new fixture goes".

#[allow(unused_imports)]
use super::*;

// ── Family modules (one `mod` line each, any order) ──
mod b_2026_09_17_11;
mod b_2026_09_17_12;
mod b_2026_09_17_13;
mod b_2026_09_17_14;
mod b_2026_09_17_16;
mod b_2026_09_17_17;
mod b_2026_09_17_20;
mod b_2026_09_17_23;
mod b_2026_09_17_24;
mod b_2026_09_17_26;
mod b_2026_09_17_27;
mod b_2026_09_17_28;
mod b_2026_09_17_32;
mod b_2026_09_17_33;
mod b_2026_09_19_52;
mod b_2026_09_19_59;
mod b_2026_09_20_2;
mod b_2026_09_20_21;
mod b_2026_09_20_38;
mod b_2026_09_23_36;
mod b_2026_09_24_11;
mod b_2026_09_27_102;
mod b_2026_09_27_104;
mod b_2026_09_27_105;
mod b_2026_09_27_106;
mod b_2026_09_27_113;
mod b_2026_09_27_128;
mod b_2026_09_27_129;
mod b_2026_09_27_130;
mod b_2026_09_27_131;
mod b_2026_09_27_50;
mod b_2026_09_27_51;
mod b_2026_09_27_54;
mod b_2026_09_27_65;
mod b_2026_09_27_66;
mod b_2026_09_27_82;
mod b_2026_09_27_94;
mod b_2026_09_27_96;
mod b_2026_09_27_98;
mod b_2026_09_27_99;
mod b_2026_09_28_10;
mod b_2026_09_28_20;
mod b_2026_09_28_22;
mod b_2026_09_28_24;
mod b_2026_09_28_36;
mod b_2026_09_28_4;
mod b_2026_09_28_40;
mod b_2026_09_28_41;
mod b_2026_09_28_46;
mod b_2026_09_28_49;
mod b_2026_09_28_5;
mod b_2026_09_28_6;
mod b_2026_09_28_8;
