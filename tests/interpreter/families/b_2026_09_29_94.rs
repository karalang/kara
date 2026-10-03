//! B-2026-09-29-94 -- a negated unsuffixed literal (`-1`) takes its numeric
//! peer's type, exactly as a bare one (`1`) does.

use super::*;

/// The program the interpreter and codegen fixtures share. Every line was
/// rejected at type-check before (`a + -1` over `a: i32`: "cannot mix integer
/// types 'i32' and 'i64'"; `a.min(-1)`: "`min` expects an argument of type
/// `i32`, got `i64`"), and `f > -1` over an `f32` type-checked but stopped the
/// interpreter on a `Float`/`Int` pair. The float `min` / `max` / `clamp`
/// lines also cover a bare literal argument, which codegen handed to
/// `llvm.minnum.f32` as a `double` and the interpreter had no arm for when it
/// was an integer.
const NEG_LITERAL_PEER_SRC: &str = r#"fn main() {
    let a: i32 = 5;
    println(f"{a + -1} {a - -1} {a * -2} {-3 + a} {a / -2} {a % -3}");
    println(f"{a.min(-1)} {a.max(-7)} {a.wrapping_add(-1)} {a.abs_diff(-4)} {a.clamp(-2, 3)}");
    let b: i128 = 5;
    println(f"{b * -1} {b - -1} {b.wrapping_add(-1)}");
    let c: i8 = 100;
    println(f"{c + -100} {c.wrapping_sub(-100)} {c < -1} {-1 < c} {c == -100}");
    let h: i16 = -32767;
    println(f"{h + -1}");
    let f: f32 = 1.5;
    println(f"{f + -1.0} {f * -2} {f.min(-1.25)} {f > -1} {f.min(1.25)} {f.max(2)} {f.max(-2)}");
    println(f"{f.clamp(0, 1)} {f.clamp(-1.0, 1.25)} {f.clamp(-3, -2)}");
    let g: f64 = 2.5;
    println(f"{g + -1} {g - -1.5} {g.min(-1)} {-1 < g}");
    let bb: bf16 = 1.5;
    println(f"{bb.min(-1)} {bb + -1} {bb.min(0.1)} {bb.clamp(-0.3, 0.1)}");
    let z: f32 = 0.0;
    println(f"{z.max(0.1)} {z.min(-0.1)}");
}
"#;

const NEG_LITERAL_PEER_OUT: &str = "4 6 -10 2 -2 2\n\
-1 5 4 9 3\n\
-5 6 4\n\
0 -56 false true false\n\
-32768\n\
0.5 -3 -1.25 true 1.25 2 1.5\n\
1 1.25 -2\n\
1.5 4 -1 true\n\
-1 0.5 0.10009765625 0.10009765625\n\
0.10000000149011612 -0.10000000149011612\n";

#[test]
fn test_negated_literal_takes_its_numeric_peers_type() {
    let parsed = karac::parse(NEG_LITERAL_PEER_SRC);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let resolved = karac::resolve(&parsed.program);
    let typed = karac::typecheck(&parsed.program, &resolved);
    let terrs: Vec<String> = typed.errors.iter().map(|e| e.to_string()).collect();
    assert!(terrs.is_empty(), "type errors: {terrs:?}");
    assert_eq!(run(NEG_LITERAL_PEER_SRC), NEG_LITERAL_PEER_OUT);
}

/// A negated literal OUTSIDE its peer's range is not promoted, and keeps the
/// diagnostic it had: promoting `-1` to a `u32` or `-200` to an `i8` would
/// trade the mixed-width error for a range error, and would change the
/// meaning of the mixed-width comparisons the checker accepts.
#[test]
fn test_out_of_range_negated_literal_keeps_its_default_type() {
    let src = r#"fn main() {
    let u: u32 = 5;
    println(f"{u + -1}");
    let c: i8 = 1;
    println(f"{c + -200}");
}
"#;
    let parsed = karac::parse(src);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let resolved = karac::resolve(&parsed.program);
    let typed = karac::typecheck(&parsed.program, &resolved);
    let terrs: Vec<String> = typed.errors.iter().map(|e| e.to_string()).collect();
    assert_eq!(terrs.len(), 2, "{terrs:?}");
    assert!(
        terrs[0].contains("cannot mix integer types 'u32' and 'i64'"),
        "{terrs:?}"
    );
    assert!(
        terrs[1].contains("cannot mix integer types 'i8' and 'i64'"),
        "{terrs:?}"
    );
}
