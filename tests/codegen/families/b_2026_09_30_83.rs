//! B-2026-09-30-83 -- a field store through an `Array[T, N]` element place
//! (`a[0].n = 9`) lands in the array, and a place shape codegen still cannot
//! lower is a build error rather than a dropped write.

use super::*;

/// B-2026-09-30-83 — `let mut a: Array[P, 1] = [P { n: 5 }]; a[0].n = 9`
/// read back `5` on every compiled surface, `karac check` clean and the
/// interpreter printing `9`. The nested-store resolvers only knew `Vec` and
/// slice containers, so an annotated array local, an array FIELD, a `mut ref`
/// array or struct parameter and a compound `+=` all fell out
/// `compile_field_store`'s `Ok(())` tail with nothing emitted, and so did a
/// `Vec` field of `self` (`self.v[i].n = 9` in a `mut ref self` method, the
/// self-hosted resolver's `self.binds[i].line = line`). (An unannotated
/// array literal is laid out as a `Vec`, which is why `control:` cells built
/// that way were right before and after.)
#[test]
fn store_through_array_element_field_lands() {
    let cells: &[(&str, &str, &str)] = &[
        (
            "local",
            "struct P { n: i64 }
fn main() { let mut a: Array[P, 1] = [P { n: 5 }]; a[0].n = 9; println(f\"r:{a[0].n}\") }
",
            "r:9\n",
        ),
        (
            "second-field-second-element",
            "struct P { n: i64, m: i64 }
fn main() { let mut a: Array[P, 2] = [P { n: 5, m: 1 }, P { n: 6, m: 2 }]; a[1].m = 9; println(f\"r:{a[1].m} {a[1].n} {a[0].m}\") }
",
            "r:9 6 1\n",
        ),
        (
            "loop-read-modify-write",
            "struct P { n: i64 }
fn main() { let mut a: Array[P, 2] = [P { n: 5 }, P { n: 6 }]; let mut i = 0; while i < 2 { a[i].n = a[i].n + 10; i = i + 1; } println(f\"r:{a[0].n} {a[1].n}\") }
",
            "r:15 16\n",
        ),
        (
            "compound-assign",
            "struct P { n: i64 }
fn main() { let mut a: Array[P, 1] = [P { n: 5 }]; a[0].n += 4; println(f\"r:{a[0].n}\") }
",
            "r:9\n",
        ),
        (
            "struct-array-field",
            "struct P { n: i64 }
struct B { a: Array[P, 2], k: i64 }
fn main() { let mut b = B { a: [P { n: 5 }, P { n: 6 }], k: 1 }; b.a[0].n = 9; println(f\"r:{b.a[0].n} {b.a[1].n} {b.k}\") }
",
            "r:9 6 1\n",
        ),
        (
            "mut-ref-struct-param",
            "struct P { n: i64 }
struct B { a: Array[P, 2], k: i64 }
fn bump(b: mut ref B) { b.a[1].n = 42; }
fn main() { let mut b = B { a: [P { n: 5 }, P { n: 6 }], k: 1 }; bump(mut b); println(f\"r:{b.a[0].n} {b.a[1].n}\") }
",
            "r:5 42\n",
        ),
        (
            "mut-ref-array-param",
            "struct P { n: i64 }
fn fill(a: mut ref Array[P, 2]) { a[0].n = 77; }
fn main() { let mut a: Array[P, 2] = [P { n: 5 }, P { n: 6 }]; fill(mut a); println(f\"r:{a[0].n} {a[1].n}\") }
",
            "r:77 6\n",
        ),
        (
            "mut-ref-self-vec-and-array-fields",
            "struct P { n: i64, s: String }
struct S { a: Array[P, 2], v: Vec[P], t: (Array[P, 1], i64) }
impl S { fn set(mut ref self, i: i64) { self.a[i].n = 9; self.v[i].s = f\"new{i}\"; self.v[0].n = 8; self.t.0[0].n = 4; } }
fn main() {
    let mut v: Vec[P] = Vec.new(); v.push(P { n: 5, s: f\"a{1}\" }); v.push(P { n: 6, s: f\"b{2}\" });
    let mut s = S { a: [P { n: 1, s: f\"c{1}\" }, P { n: 2, s: f\"d{2}\" }], v: v, t: ([P { n: 3, s: f\"e{3}\" }], 7) };
    s.set(1);
    println(f\"r:{s.a[1].n} {s.v[1].s} {s.v[0].n} {s.t.0[0].n} {s.a[0].n}\")
}
",
            "r:9 new1 8 4 1\n",
        ),
        (
            "string-field",
            "struct P { s: String, n: i64 }
fn main() { let mut a: Array[P, 2] = [P { s: \"aa\", n: 1 }, P { s: \"bb\", n: 2 }]; a[1].s = \"zz\"; println(f\"r:{a[0].s} {a[1].s}\") }
",
            "r:aa zz\n",
        ),
        (
            "control:unannotated-literal",
            "struct P { n: i64 }
fn main() { let mut a = [P { n: 5 }, P { n: 6 }]; a[1].n = 9; println(f\"r:{a[1].n}\") }
",
            "r:9\n",
        ),
        (
            "control:vec",
            "struct P { n: i64 }
fn main() { let mut v: Vec[P] = Vec.new(); v.push(P { n: 5 }); v[0].n = 9; println(f\"r:{v[0].n}\") }
",
            "r:9\n",
        ),
    ];
    for (label, prog, want) in cells {
        let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(prog);
        assert!(
            interp_errs.is_empty(),
            "[{label}] interp errored: {interp_errs:?}"
        );
        assert_eq!(interp_out.join(""), *want, "[{label}] interpreter");
        let Some(aot) = run_program(prog) else {
            continue;
        };
        assert_eq!(aot, *want, "[{label}] AOT");
    }
}

/// B-2026-09-30-83 — the store is bounds-checked like the array read: an
/// out-of-range runtime index panics instead of writing past the array. The
/// dropped store never reached memory at all, so before the fix this printed
/// `unreached`.
#[test]
fn store_through_array_element_field_is_bounds_checked() {
    let prog = "struct P { n: i64 }
fn main() { let mut a: Array[P, 2] = [P { n: 5 }, P { n: 6 }]; let i = 5 - 3; a[i].n = 9; println(\"unreached\") }
";
    let Some(run) = run_program_capturing(prog) else {
        return;
    };
    assert!(
        !run.status.success(),
        "expected a panic, got {:?}",
        run.stdout
    );
    assert!(
        !run.stdout.contains("unreached"),
        "stdout: {:?}",
        run.stdout
    );
    assert!(
        run.stderr.contains("array index out of bounds"),
        "stderr: {:?}",
        run.stderr
    );
}

/// B-2026-09-30-83 — a field store whose parent place codegen cannot lower is
/// refused at build time. `a.0[0].n = 9` through a `mut ref` TUPLE parameter
/// used to compile to nothing and read back `5`; B-2026-09-30-89 lowered that
/// specimen, so this pins the refusal on a nested index (`v[0][0].n = 9`),
/// which still has no resolver. PREDICTS B-2026-10-04-59: this cell must flip
/// to a lowered store when that row is fixed.
#[test]
fn unlowered_field_store_place_is_a_build_error() {
    let msg = codegen_error(
        "struct P { n: i64 }
fn main() { let mut v: Vec[Vec[P]] = [[P { n: 5 }]]; v[0][0].n = 9; println(f\"r:{v[0][0].n}\") }
",
    );
    assert!(
        msg.contains("assignment to field 'n' through this place is not yet lowered"),
        "{msg}"
    );
}

/// B-2026-09-30-84 — an annotated `Array` element handed to a `mut ref`
/// parameter (`bump(mut a[0])`) was copied into a temp, so the callee grew
/// the copy and the array kept `x1` (and the temp's free doubled the array's
/// own). The element is now borrowed in place, as a `Vec` local's element
/// already was; so is the element of a container in a struct field (through
/// `self` too) and of a `mut ref` array parameter.
#[test]
fn array_element_mut_ref_argument_mutates_in_place() {
    let cells: &[(&str, &str, &str)] = &[
        (
            "local",
            "fn bump(s: mut ref String) { s.push_str(\"yy\"); }
fn main() { let mut a: Array[String, 1] = [f\"x{1}\"]; bump(mut a[0]); println(a[0]); }
",
            "x1yy\n",
        ),
        (
            "struct-element-field",
            "struct P { s: String, n: i64 }
fn bump(s: mut ref String) { s.push_str(\"yy\"); }
fn main() { let mut a: Array[P, 2] = [P { s: f\"x{1}\", n: 1 }, P { s: f\"z{2}\", n: 2 }]; bump(mut a[1].s); println(f\"{a[0].s} {a[1].s}\"); }
",
            "x1 z2yy\n",
        ),
        (
            "vec-field",
            "struct O { v: Vec[String] }
fn bump(s: mut ref String) { s.push_str(\"yy\"); }
fn main() { let mut v: Vec[String] = Vec.new(); v.push(f\"v{6}\"); let mut o = O { v: v }; bump(mut o.v[0]); println(o.v[0]); }
",
            "v6yy\n",
        ),
        (
            "self-array-field",
            "struct O { a: Array[String, 2] }
impl O { fn g(mut ref self) { bump(mut self.a[0]); } }
fn bump(s: mut ref String) { s.push_str(\"yy\"); }
fn main() { let mut o = O { a: [f\"a{1}\", f\"b{1}\"] }; o.g(); println(f\"{o.a[0]} {o.a[1]}\"); }
",
            "a1yy b1\n",
        ),
        (
            "mut-ref-array-param",
            "fn g(a: mut ref Array[String, 2]) { bump(a[1]); }
fn bump(s: mut ref String) { s.push_str(\"yy\"); }
fn main() { let mut a: Array[String, 2] = [f\"a{1}\", f\"b{1}\"]; g(mut a); println(a[1]); }
",
            "b1yy\n",
        ),
        (
            "scalar-ref",
            "fn ri(x: ref i64) -> i64 { x + 1 }
fn main() { let a: Array[i64, 2] = [5, 6]; println(f\"{ri(a[1])}\"); }
",
            "7\n",
        ),
    ];
    for (label, prog, want) in cells {
        let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(prog);
        assert!(
            interp_errs.is_empty(),
            "[{label}] interp errored: {interp_errs:?}"
        );
        assert_eq!(interp_out.join(""), *want, "[{label}] interpreter");
        let Some(aot) = run_program(prog) else {
            continue;
        };
        assert_eq!(aot, *want, "[{label}] AOT");
    }
}
