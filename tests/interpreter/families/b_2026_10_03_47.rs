//! B-2026-10-03-47: a method call inside an f-string hole keeps its own type entry in the interpreter

use super::*;

/// B-2026-10-03-47: the f-string rebase moved every span of a hole's
/// expression to its real source position except a method call's
/// `args_close_span`, which stayed at the synthetic wrapper's coordinates.
/// The typechecker keys a bit intrinsic's receiver width on that span, so
/// two holes collided and the later one won: `{a.leading_zeros()}` on an
/// `i64` printed 31 when an `i32` hole followed it. The expected text is the
/// compiled program's output, which never read that table.
#[test]
fn interp_fstring_method_calls_keep_their_own_receiver_types() {
    let out = run(r#"fn main() {
    let a: i64 = 1;
    println(f"{a.leading_zeros()} {a.count_ones()} {a.trailing_zeros()}");
    let vals: Vec[i64] = vec![-1, 256];
    for v in vals.iter() {
        println(f"i64 {v}: {v.count_ones()} {v.leading_zeros()}");
    }
    let b: i32 = 1;
    println(f"{b.leading_zeros()} {b.count_ones()} {b.trailing_zeros()}");
    let n32: Vec[i32] = vec![-1, 256];
    for w in n32.iter() {
        println(f"i32 {w}: {w.count_ones()} {w.leading_zeros()}");
    }
    let c: u8 = 1;
    println(f"{c.leading_zeros()}");
}"#);
    assert_eq!(
        out,
        "63 1 0\ni64 -1: 64 0\ni64 256: 1 55\n31 1 0\ni32 -1: 32 0\ni32 256: 1 23\n7\n"
    );
}
