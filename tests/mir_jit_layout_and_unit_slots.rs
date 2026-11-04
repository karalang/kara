//! Two faults LLVM 22 turned from latent into wrong output under
//! `__mir-llvm-run` (the corpus's mir-llvm lane):
//!
//! - The IR handed to the JIT carries the host's data layout. Without it
//!   the JIT's pre-passes read the module under LLVM's default layout, where
//!   an `i64` is 4-aligned, so a returned `{ i8, i64 }` pair had its `i64`
//!   at byte 4 while every load of the tuple reads byte 8: SROA split
//!   mini_astral's `parse_tz` offset into garbage.
//! - No store is wider than the stack slot it writes. A chars collect's
//!   reservation (`Vec.presize`) discards its reach into a unit, and the
//!   backend wrote the 8-byte reach into that zero-size slot anyway,
//!   overwriting a neighbouring spill once LLVM 22 packed the frame
//!   tighter (mini_spellchecker's `self`).
#![cfg(feature = "llvm")]

use std::collections::HashMap;

/// A `chars().collect()` beside other locals, as mini_spellchecker's
/// `known_edits` has it.
const CHARS: &str = r#"fn edits(w: ref String) -> i64 {
    let cs: Vec[char] = w.chars().collect();
    let n = cs.len() as i64;
    let mut total = 0;
    for i in 0..n {
        if cs[i] != 'x' {
            total += i;
        }
    }
    total
}
fn main() {
    println(edits("spelling".to_string()));
}
"#;

/// The bytes a store of `ty` writes, for the types the backend stores.
fn width(ty: &str) -> Option<u64> {
    Some(match ty {
        "i8" | "i1" => 1,
        "i16" => 2,
        "i32" | "float" => 4,
        "i64" | "ptr" | "double" => 8,
        "i128" => 16,
        _ => return None,
    })
}

/// Every `store` in `ir` wider than the `alloca [N x i8]` it writes to.
fn oversized_stores(ir: &str) -> Vec<String> {
    let mut slots: HashMap<&str, u64> = HashMap::new();
    let mut bad = Vec::new();
    for line in ir.lines() {
        if line.starts_with("define") {
            slots.clear();
        }
        let l = line.trim();
        if let Some((name, rest)) = l.split_once(" = alloca [") {
            if let Some((n, _)) = rest.split_once(" x i8]") {
                if let Ok(n) = n.parse() {
                    slots.insert(name, n);
                }
            }
            continue;
        }
        let Some(rest) = l.strip_prefix("store ") else {
            continue;
        };
        let Some((ty, rest)) = rest.split_once(' ') else {
            continue;
        };
        let Some((_, to)) = rest.split_once(", ptr ") else {
            continue;
        };
        let slot = to.split(',').next().unwrap_or("").trim();
        if let (Some(w), Some(&n)) = (width(ty), slots.get(slot)) {
            if w > n {
                bad.push(line.to_string());
            }
        }
    }
    bad
}

#[test]
fn mir_jit_ir_carries_the_host_data_layout() {
    let lowered = karac::mir::lower::build_source(CHARS).expect("build");
    let ir =
        karac::backend::mir::compile_mir_ir_for_jit(&lowered.program, &lowered.tys).expect("IR");
    let layout = ir
        .lines()
        .find(|l| l.starts_with("target datalayout"))
        .unwrap_or_else(|| panic!("no data layout in the JIT's IR"));
    #[cfg(target_arch = "x86_64")]
    assert!(layout.contains("i64:64"), "{layout}");
    assert!(ir.lines().any(|l| l.starts_with("target triple")));
}

#[test]
fn mir_no_store_is_wider_than_its_slot() {
    let lowered = karac::mir::lower::build_source(CHARS).expect("build");
    let ir = karac::backend::mir::compile_mir_ir(&lowered.program, &lowered.tys).expect("IR");
    assert!(ir.contains("alloca"), "no stack slots to check");
    let bad = oversized_stores(&ir);
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}
