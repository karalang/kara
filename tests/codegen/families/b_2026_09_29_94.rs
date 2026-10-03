//! B-2026-09-29-94 -- a negated unsuffixed literal (`-1`) takes its numeric
//! peer's type, exactly as a bare one (`1`) does.

use super::*;

/// The interpreter fixture's program (`tests/interpreter/families/
/// b_2026_09_29_94.rs`), compiled. Besides the type-check rejection both
/// backends shared, codegen handed a float literal argument of `min` / `max`
/// / `clamp` on an `f32` or `bf16` receiver to the intrinsic as a `double`
/// (module verification failed) and found no arm at all for an integer one.
#[test]
fn e2e_negated_literal_takes_its_numeric_peers_type() {
    let src = r#"fn main() {
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
    let Some(run) = run_program_capturing(src) else {
        return;
    };
    assert_eq!(
        run.stdout,
        "4 6 -10 2 -2 2\n-1 5 4 9 3\n-5 6 4\n0 -56 false true false\n-32768\n\
         0.5 -3 -1.25 true 1.25 2 1.5\n1 1.25 -2\n1.5 4 -1 true\n\
         -1 0.5 0.10009765625 0.10009765625\n\
         0.10000000149011612 -0.10000000149011612\n",
        "stderr: {}",
        run.stderr
    );
    assert!(run.status.success(), "stderr: {}", run.stderr);
}

/// `-iN::MIN - 1` through a promoted negated literal still traps: `-1` is an
/// `i8` here, so the subtraction is checked at `i8` width.
#[test]
fn e2e_negated_literal_at_narrow_peer_overflow_traps() {
    let src = r#"fn main() {
    let h: i8 = -128;
    let z = h + -1;
    println(f"{z}");
}
"#;
    let Some(run) = run_program_capturing(src) else {
        return;
    };
    assert!(!run.status.success(), "stdout: {}", run.stdout);
    assert!(
        run.stderr.contains("integer overflow"),
        "stderr: {}",
        run.stderr
    );
}
