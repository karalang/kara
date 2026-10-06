//! B-2026-10-06-69 -- a `Set` / `SortedSet` copied by the generic map clone
//! helper got an 8-byte value slot, so a later `remove` wrote 8 bytes into a
//! one-byte stack slot and smashed the caller's frame compiled.

use super::*;

/// The row's program, then reads that prove the clones stayed independent.
#[test]
fn e2e_set_clone_then_remove_keeps_frame() {
    for (set, map) in [("Set", "Map"), ("SortedSet", "SortedMap")] {
        let src = format!(
            r#"struct Holder {{ m: {map}[i64, i64], s: {set}[i64] }}
fn main() {{
    let a: {map}[i64, i64] = {map}.new();
    let mut s: {set}[i64] = {set}.new();
    s.insert(5);
    let t = s.clone();
    let h = Holder {{ m: a.clone(), s: t.clone() }};
    let mut h2 = Holder {{ m: h.m.clone(), s: h.s.clone() }};
    h2.s.remove(5);
    h2.s.insert(7);
    println(f"{{h2.s.len()}} {{h2.s.contains(7)}} {{h2.s.contains(5)}} {{h.s.len()}} {{t.len()}}");
}}
"#
        );
        let want = "1 true false 1 1\n";
        let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(&src);
        assert!(
            interp_errs.is_empty(),
            "{set}: interp errored: {interp_errs:?}"
        );
        assert_eq!(interp_out.join(""), want, "{set}: interpreter");
        assert_eq!(run_program(&src).as_deref(), Some(want), "{set}: AOT");
    }
}

/// The crash is a frame-layout accident, so whether a given program dies is
/// not a reliable control (neither ASAN nor the e2e above saw it on the
/// unfixed tree). The cause is not an accident: the Set clone helper must
/// create its destination with a ZERO-byte value half, as every other Set
/// constructor does, and must not give that half an 8-byte slot.
#[test]
fn ir_set_clone_helper_has_zero_byte_value_half() {
    let src = r#"fn main() {
    let mut s: Set[i64] = Set.new();
    s.insert(5);
    let mut t = s.clone();
    t.remove(5);
    println(f"{t.len()} {s.len()}");
}
"#;
    let mut parsed = karac::parse(src);
    let resolved = karac::resolve(&parsed.program);
    let typed = karac::typecheck(&parsed.program, &resolved);
    karac::lower(&mut parsed.program, &typed);
    let ownership = karac::ownershipcheck(&parsed.program, &typed);
    let ir =
        karac::codegen::compile_to_ir(&parsed.program, Some(&ownership), None).expect("codegen");
    let body: Vec<&str> = ir
        .lines()
        .skip_while(|l| !(l.starts_with("define") && l.contains("@karac_clone_Map_i64_unit(")))
        .take_while(|l| *l != "}")
        .collect();
    assert!(!body.is_empty(), "no Set clone helper in the IR");
    let new_call = body
        .iter()
        .find(|l| l.contains("@karac_map_new("))
        .expect("the clone helper creates its destination map");
    // The key size is a `ptrtoint` constant expression; the value size, the
    // second argument, sits right before the hash fn pointer.
    assert!(
        new_call.contains(", i64 0, ptr @karac_hash_"),
        "Set clone value size must be 0: {new_call}"
    );
    assert!(
        !body.iter().any(|l| l.contains("%v.out = alloca i64")),
        "the unit value slot must not be 8 bytes"
    );
}
