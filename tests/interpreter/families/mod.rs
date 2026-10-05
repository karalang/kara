//! Per-family fixture files for `tests/interpreter.rs`.
//!
//! NEW FIXTURES FOR A BUG FAMILY GO IN THEIR OWN FILE HERE, not at the end of
//! an area file (`tests/interpreter/drop_order.rs` and friends). Several threads push
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
//! The test target is unchanged, so `cargo test --test interpreter` still runs
//! everything. One family alone: `--test interpreter families::b_2026_09_27_58::`.
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
mod b_2026_09_18_1;
mod b_2026_09_19_18;
mod b_2026_09_19_22;
mod b_2026_09_19_25;
mod b_2026_09_19_29;
mod b_2026_09_19_31;
mod b_2026_09_19_37;
mod b_2026_09_19_38;
mod b_2026_09_19_45;
mod b_2026_09_19_46;
mod b_2026_09_19_47;
mod b_2026_09_19_50;
mod b_2026_09_20_5;
mod b_2026_09_20_6;
mod b_2026_09_20_7;
mod b_2026_09_23_13;
mod b_2026_09_23_24;
mod b_2026_09_23_4;
mod b_2026_09_24_11;
mod b_2026_09_25_33;
mod b_2026_09_27_102;
mod b_2026_09_27_104;
mod b_2026_09_27_105;
mod b_2026_09_27_106;
mod b_2026_09_27_128;
mod b_2026_09_27_129;
mod b_2026_09_27_130;
mod b_2026_09_27_131;
mod b_2026_09_27_50;
mod b_2026_09_27_51;
mod b_2026_09_27_54;
mod b_2026_09_27_65;
mod b_2026_09_27_66;
mod b_2026_09_27_72;
mod b_2026_09_27_77;
mod b_2026_09_27_79;
mod b_2026_09_27_82;
mod b_2026_09_27_83;
mod b_2026_09_27_84;
mod b_2026_09_27_87;
mod b_2026_09_27_88;
mod b_2026_09_27_95;
mod b_2026_09_27_96;
mod b_2026_09_27_98;
mod b_2026_09_27_99;
mod b_2026_09_28_10;
mod b_2026_09_28_13;
mod b_2026_09_28_20;
mod b_2026_09_28_22;
mod b_2026_09_28_24;
mod b_2026_09_28_28;
mod b_2026_09_28_36;
mod b_2026_09_28_38;
mod b_2026_09_28_4;
mod b_2026_09_28_40;
mod b_2026_09_28_41;
mod b_2026_09_28_46;
mod b_2026_09_28_47;
mod b_2026_09_28_48;
mod b_2026_09_28_49;
mod b_2026_09_28_50;
mod b_2026_09_28_51;
mod b_2026_09_28_53;
mod b_2026_09_28_54;
mod b_2026_09_28_55;
mod b_2026_09_28_6;
mod b_2026_09_28_62;
mod b_2026_09_28_63;
mod b_2026_09_28_67;
mod b_2026_09_28_73;
mod b_2026_09_28_74;
mod b_2026_09_28_80;
mod b_2026_09_29_10;
mod b_2026_09_29_100;
mod b_2026_09_29_101;
mod b_2026_09_29_103;
mod b_2026_09_29_104;
mod b_2026_09_29_116;
mod b_2026_09_29_12;
mod b_2026_09_29_120;
mod b_2026_09_29_15;
mod b_2026_09_29_16;
mod b_2026_09_29_17;
mod b_2026_09_29_18;
mod b_2026_09_29_2;
mod b_2026_09_29_22;
mod b_2026_09_29_25;
mod b_2026_09_29_27;
mod b_2026_09_29_29;
mod b_2026_09_29_3;
mod b_2026_09_29_40;
mod b_2026_09_29_41;
mod b_2026_09_29_42;
mod b_2026_09_29_43;
mod b_2026_09_29_44;
mod b_2026_09_29_47;
mod b_2026_09_29_61;
mod b_2026_09_29_64;
mod b_2026_09_29_75;
mod b_2026_09_29_76;
mod b_2026_09_29_77;
mod b_2026_09_29_79;
mod b_2026_09_29_9;
mod b_2026_09_29_94;
mod b_2026_09_29_95;
mod b_2026_09_30_12;
mod b_2026_09_30_13;
mod b_2026_09_30_17;
mod b_2026_09_30_18;
mod b_2026_09_30_19;
mod b_2026_09_30_22;
mod b_2026_09_30_23;
mod b_2026_09_30_25;
mod b_2026_09_30_26;
mod b_2026_09_30_27;
mod b_2026_09_30_47;
mod b_2026_09_30_48;
mod b_2026_09_30_50;
mod b_2026_09_30_6;
mod b_2026_09_30_67;
mod b_2026_09_30_68;
mod b_2026_09_30_7;
mod b_2026_09_30_90;
mod b_2026_09_30_95;
mod b_2026_10_01_14;
mod b_2026_10_01_21;
mod b_2026_10_01_25;
mod b_2026_10_01_41;
mod b_2026_10_01_45;
mod b_2026_10_01_52;
mod b_2026_10_01_7;
mod b_2026_10_01_8;
mod b_2026_10_01_9;
mod b_2026_10_02_42;
mod b_2026_10_02_49;
mod b_2026_10_02_50;
mod b_2026_10_02_57;
mod b_2026_10_02_59;
mod b_2026_10_02_71;
mod b_2026_10_02_79;
mod b_2026_10_02_80;
mod b_2026_10_03_21;
mod b_2026_10_03_24;
mod b_2026_10_03_26;
mod b_2026_10_03_27;
mod b_2026_10_03_32;
mod b_2026_10_03_33;
mod b_2026_10_03_47;
mod b_2026_10_03_49;
mod b_2026_10_03_53;
mod b_2026_10_04_20;
mod b_2026_10_04_21;
mod b_2026_10_04_25;
mod b_2026_10_04_38;
mod b_2026_10_04_50;
mod b_2026_10_04_57;
mod b_2026_10_04_63;
mod b_2026_10_04_69;
mod b_2026_10_04_71;
mod b_2026_10_04_73;
mod b_2026_10_04_74;
mod b_2026_10_04_81;
mod b_2026_10_04_82;
mod b_2026_10_04_85;
mod b_2026_10_05_14;
mod b_2026_10_05_16;
mod b_2026_10_05_23;
mod b_2026_10_05_29;
mod b_2026_10_05_7;
mod b_2026_10_05_76;
mod b_2026_10_05_9;
