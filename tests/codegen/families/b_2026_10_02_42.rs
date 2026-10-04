//! B-2026-10-02-42: f-string format specs take the `+` sign flag

use super::*;

/// B-2026-10-02-42: `f"{d:+}"` was rejected, and the error called `+` an
/// unsupported TYPE. The flag now signs every non-negative number as Rust's
/// `{:+}` does: alongside zero-pad (the zeros go after the sign), alignment
/// and a custom fill, on a non-decimal radix (`+ff`, and a negative `i64`
/// reinterprets as its 64-bit pattern, signed), on `u64`, `i32` and `i128`
/// holes, and on floats. The expected text is Rust's own output for the
/// same specs. The `1e300` hole checks that a long float rendering is not
/// cut short on the compiled path, which formats a signed float in a
/// fixed buffer.
#[test]
fn e2e_format_spec_takes_the_plus_sign_flag() {
    let out = run_program(
        r#"fn twelve() -> i64 {
    return 12;
}

fn main() {
    let xs: Vec[i64] = vec![5, 0, -3];
    for d in xs.iter() {
        println(f"[{d:+}] [{d:+05}] [{d:<+5}] [{d:*^+7}] [{d:+x}]");
    }
    let u: u64 = 7;
    let w: i32 = -9;
    let big: i128 = 9223372036854775807 as i128 * 4;
    println(f"{u:+} {w:+} {big:+}");
    let fs: Vec[f64] = vec![1.5, 0.0, -0.25];
    for f in fs.iter() {
        println(f"[{f:+.2}] [{f:+08.2}] [{f:>+8.1}]");
    }
    let huge: f64 = 1e300;
    let s = f"{huge:+.2}";
    println(f"{s.len()} {s.starts_with("+100")} {s.ends_with("0.00")}");
    println(f"{twelve():+}");
}"#,
    );
    assert_eq!(
        out.as_deref(),
        Some("[+5] [+0005] [+5   ] [**+5***] [+5]\n[+0] [+0000] [+0   ] [**+0***] [+0]\n[-3] [-0003] [-3   ] [**-3***] [+fffffffffffffffd]\n+7 -9 +36893488147419103228\n[+1.50] [+0001.50] [    +1.5]\n[+0.00] [+0000.00] [    +0.0]\n[-0.25] [-0000.25] [    -0.2]\n305 true true\n+12\n")
    );
}
